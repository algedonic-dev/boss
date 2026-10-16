//! The private collector channel is separate from metadata IPC. This source
//! adapter installs no listener, TLS key, native authority or launch selector.
use super::{CacheError, files, ipc, scope};
use scope::BrokerRecords;
use serde_json::Value;
use std::sync::Arc;
pub(crate) mod tls;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    run: uuid::Uuid,
    nonce: uuid::Uuid,
    workspace_uid: String,
    image_witness: Value,
    trust_generation: uuid::Uuid,
    collector_account_uid: uuid::Uuid,
    original_job: Value,
    original_pod: Value,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    run: uuid::Uuid,
    nonce: uuid::Uuid,
    pod_uid: uuid::Uuid,
    workspace_uid: String,
}

fn context_matches(
    record: &scope::Record,
    plan: &Plan,
    caller: &ipc::Caller,
    identity: &Identity,
) -> Result<(), CacheError> {
    let p = &record.prepared;
    scope::protected_record(record)?;
    if identity.run.is_nil()
        || identity.nonce.is_nil()
        || identity.pod_uid.is_nil()
        || plan.run != identity.run
        || plan.nonce != identity.nonce
        || plan.workspace_uid != identity.workspace_uid
        || caller.pod_uid != identity.pod_uid
        || caller.service_account_uid != plan.collector_account_uid
        || caller.service_account_uid.is_nil()
        || caller.expires_at <= boss_clock_client::wall_now().timestamp()
        || p.run != identity.run.to_string()
        || record.original_workspace["metadata"]["uid"] != plan.workspace_uid
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(())
}
async fn attest(
    record: &scope::Record,
    plan: &Plan,
    caller: &ipc::Caller,
    identity: &Identity,
    metadata: &dyn scope::Metadata,
) -> Result<Value, CacheError> {
    use scope::Resource;
    use serde_json::json;
    context_matches(record, plan, caller, identity)?;
    let p = &record.prepared;
    let b = &record.bindings;
    let run = metadata.run(identity.run).await?;
    scope::registered_run(record, &run)?;
    let credential_original = metadata.credential(&b.credential).await?;
    let credential: boss_jobs::credentials::CredentialRow = serde_json::from_value(
        credential_original
            .get("data")
            .unwrap_or(&credential_original)
            .clone(),
    )
    .map_err(|_| CacheError::InvalidBinding)?;
    if !crate::launch_cluster::credential_matches(&credential, b) {
        return Err(CacheError::InvalidBinding);
    }
    let controller = metadata
        .native(Resource::Pod, &record.controller_pod)
        .await?;
    let controller_account = metadata
        .native(Resource::ControllerAccount, "dev-session")
        .await?;
    scope::native_identity(&controller, "Pod", &record.controller_pod)?;
    scope::native_identity(&controller_account, "ServiceAccount", "dev-session")?;
    if controller["metadata"]["uid"] != record.controller_pod_uid.to_string()
        || controller["spec"]["serviceAccountName"] != "dev-session"
        || controller["status"]["phase"] != "Running"
        || controller_account["metadata"]["uid"] != record.controller_sa_uid.to_string()
    {
        return Err(CacheError::InvalidBinding);
    }
    let account = metadata
        .native(Resource::CollectorAccount, "boss-managed-cache-collector")
        .await?;
    scope::native_identity(&account, "ServiceAccount", "boss-managed-cache-collector")?;
    if account["metadata"]["uid"] != caller.service_account_uid.to_string() {
        return Err(CacheError::InvalidBinding);
    }
    let compute_spec = crate::launch_job::Spec {
        run_id: &p.run,
        image: &p.image,
        login_secret: &p.login,
        bound: p.bound,
        grace: p.grace,
    };
    let compute =
        crate::launch_binding::JobBinding::from_created(&record.original_job, &compute_spec)
            .map_err(|_| CacheError::InvalidBinding)?;
    let original = compute
        .observe_running(
            &record.original_job,
            &scope::named_pod(&record.original_pod),
        )
        .map_err(|_| CacheError::InvalidBinding)?;
    let compute_absence = metadata
        .native(Resource::StoppedJob, compute.name())
        .await?;
    let pod_absence = metadata
        .native(Resource::StoppedPod, original.pod_name())
        .await?;
    for (observed, name, plural) in [
        (&compute_absence, compute.name(), "jobs"),
        (&pod_absence, original.pod_name(), "pods"),
    ] {
        if observed["apiVersion"] != "v1"
            || observed["kind"] != "Status"
            || observed["reason"] != "NotFound"
            || observed["code"].as_u64() != Some(404)
            || observed["details"]["name"] != name
            || observed["details"]["kind"] != plural
        {
            return Err(CacheError::InvalidBinding);
        }
    }
    let workspace_intent = crate::launch_job::workspace_manifest(&compute_spec);
    crate::launch_controller::workspace_identity(&record.original_workspace, &workspace_intent)
        .map_err(|_| CacheError::InvalidBinding)?;
    let workspace_name = workspace_intent["metadata"]["name"]
        .as_str()
        .ok_or(CacheError::InvalidBinding)?;
    let workspace = metadata.native(Resource::Workspace, workspace_name).await?;
    crate::launch_controller::workspace_identity(&workspace, &workspace_intent)
        .map_err(|_| CacheError::InvalidBinding)?;
    if workspace["metadata"]["uid"] != plan.workspace_uid {
        return Err(CacheError::InvalidBinding);
    }
    let spec = super::collector::Spec {
        run: plan.run,
        nonce: plan.nonce,
        workspace_uid: &plan.workspace_uid,
        image_witness: &plan.image_witness,
        trust_generation: plan.trust_generation,
    };
    let intent = super::collector::manifest(&spec)?;
    if plan.original_job["metadata"]["annotations"] != intent["metadata"]["annotations"] {
        return Err(CacheError::InvalidBinding);
    }
    let collector =
        crate::launch_binding::JobBinding::from_collector_created(&plan.original_job, &spec)
            .map_err(|_| CacheError::InvalidBinding)?;
    let bound = collector
        .observe_running(&plan.original_job, &scope::named_pod(&plan.original_pod))
        .map_err(|_| CacheError::InvalidBinding)?;
    if bound.identity()["pod_uid"] != caller.pod_uid.to_string() {
        return Err(CacheError::InvalidBinding);
    }
    let current_job = metadata.native(Resource::Job, collector.name()).await?;
    let current_pod = metadata.native(Resource::Pod, bound.pod_name()).await?;
    if current_job["metadata"]["annotations"] != intent["metadata"]["annotations"] {
        return Err(CacheError::InvalidBinding);
    }
    bound
        .recheck(&current_job, &scope::named_pod(&current_pod))
        .map_err(|_| CacheError::InvalidBinding)?;
    if metadata.run(identity.run).await? != run
        || metadata.credential(&b.credential).await? != credential_original
        || caller.expires_at <= boss_clock_client::wall_now().timestamp()
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(
        json!({"schema":1,"identity":identity,"registered_run":run,"credential":credential_original,
        "controller":controller,"controller_account":controller_account,"collector_account":account,
        "original_compute_binding":original.identity(),"compute_absence":compute_absence,"pod_absence":pod_absence,
        "workspace":workspace,"collector_job":current_job,"collector_pod":current_pod,"collector_binding":bound.identity()}),
    )
}

pub(crate) struct Endpoint {
    pub authentication: ipc::Authentication,
    pub records: scope::FileRecords,
    pub metadata: Arc<dyn scope::Metadata>,
}

/// Keep every native metadata value that the existing port returned, including
/// objects subsequently refused by attestation. Port failures remain UNKNOWN,
/// not fabricated HTTP responses. Only private descriptor storage consumes it.
struct Captured {
    metadata: Arc<dyn scope::Metadata>,
    originals: tokio::sync::Mutex<Vec<Value>>,
}
impl Captured {
    fn new(metadata: Arc<dyn scope::Metadata>) -> Self {
        Self {
            metadata,
            originals: tokio::sync::Mutex::new(Vec::new()),
        }
    }
    async fn retain(
        &self,
        query: Value,
        observation: scope::Observation,
    ) -> Result<Value, CacheError> {
        self.originals.lock().await.push(serde_json::json!({"query":query,"original":observation.original,"result":observation.result}));
        observation.result
    }
    async fn originals(&self) -> Value {
        serde_json::json!(*self.originals.lock().await)
    }
}
#[async_trait::async_trait]
impl scope::Metadata for Captured {
    async fn run(&self, run: uuid::Uuid) -> Result<Value, CacheError> {
        self.retain(
            serde_json::json!({"kind":"run","id":run}),
            self.metadata.observe(scope::Query::Run(run)).await,
        )
        .await
    }
    async fn credential(&self, id: &str) -> Result<Value, CacheError> {
        self.retain(
            serde_json::json!({"kind":"credential","id":id}),
            self.metadata
                .observe(scope::Query::Credential(id.into()))
                .await,
        )
        .await
    }
    async fn native(&self, resource: scope::Resource, name: &str) -> Result<Value, CacheError> {
        self.retain(
            serde_json::json!({"kind":"native","resource":format!("{resource:?}"),"name":name}),
            self.metadata
                .observe(scope::Query::Native(resource, name.into()))
                .await,
        )
        .await
    }
}

impl Endpoint {
    async fn plan(&self, run: uuid::Uuid) -> Result<(scope::Record, Plan), CacheError> {
        let record = self.records.read(run).await?;
        let root = self.records.root.clone();
        let owner = self.records.owner;
        let plan = tokio::task::spawn_blocking(move || {
            let directory = files::private_directory(&root, owner)?;
            scope::read_metadata(&directory, &format!("collection-{run}.json"), owner)
        })
        .await
        .map_err(|_| CacheError::Unavailable)??;
        Ok((record, plan))
    }
}

fn contract() -> Result<Value, CacheError> {
    serde_json::from_str(include_str!(
        "../../../../../../infra/agent/private-collector-contract.json"
    ))
    .map_err(|_| CacheError::Unavailable)
}
fn identity(headers: &axum::http::HeaderMap) -> Result<Identity, CacheError> {
    let one = |name: &str| -> Result<String, CacheError> {
        let values = headers.get_all(name).iter().collect::<Vec<_>>();
        let [value] = values.as_slice() else {
            return Err(CacheError::InvalidBinding);
        };
        value
            .to_str()
            .map(str::to_owned)
            .map_err(|_| CacheError::InvalidBinding)
    };
    let id = |name: &str| -> Result<uuid::Uuid, CacheError> {
        let text = one(name)?;
        let id = uuid::Uuid::parse_str(&text).map_err(|_| CacheError::InvalidBinding)?;
        if id.is_nil() || id.to_string() != text {
            return Err(CacheError::InvalidBinding);
        }
        Ok(id)
    };
    Ok(Identity {
        run: id("x-boss-run")?,
        nonce: id("x-boss-nonce")?,
        pod_uid: id("x-boss-pod-uid")?,
        workspace_uid: one("x-boss-workspace-uid")?,
    })
}

// Private data deliberately has no Debug or serialization implementation.
struct Upload {
    bytes: Vec<u8>,
    complete: bool,
    error: Option<String>,
    trailers: Vec<(String, String)>,
}
async fn read_upload(
    mut body: axum::body::Body,
    limit: usize,
    bound: std::time::Duration,
) -> Upload {
    use base64::Engine;
    let mut upload = Upload {
        bytes: Vec::new(),
        complete: false,
        error: None,
        trailers: Vec::new(),
    };
    let Some(deadline) = tokio::time::Instant::now().checked_add(bound) else {
        upload.error = Some("body_deadline_unavailable".into());
        return upload;
    };
    loop {
        let next = tokio::time::timeout_at(
            deadline,
            std::future::poll_fn(|cx| {
                axum::body::HttpBody::poll_frame(std::pin::Pin::new(&mut body), cx)
            }),
        )
        .await;
        let frame = match next {
            Err(_) => {
                upload.error = Some("body_timeout".into());
                break;
            }
            Ok(None) => {
                upload.complete = true;
                break;
            }
            Ok(Some(Err(error))) => {
                upload.error = Some(error.to_string());
                break;
            }
            Ok(Some(Ok(frame))) => frame,
        };
        match frame.into_data() {
            Ok(bytes) => {
                let remaining = limit - upload.bytes.len();
                if bytes.len() > remaining {
                    upload.bytes.extend_from_slice(&bytes[..remaining]);
                    upload.error = Some("body_bound_exceeded".into());
                    break;
                }
                upload.bytes.extend_from_slice(&bytes);
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    upload.trailers = trailers
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.to_string(),
                                base64::engine::general_purpose::STANDARD.encode(value.as_bytes()),
                            )
                        })
                        .collect();
                }
                upload.error = Some("body_trailers_refused".into());
                break;
            }
        }
    }
    upload
}

async fn handle(
    axum::extract::State(endpoint): axum::extract::State<Option<Arc<Endpoint>>>,
    request: axum::extract::Request,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use serde_json::json;

    let refused = || {
        (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(json!({"schema":1,"status":"refused"})),
        )
    };
    let Some(endpoint) = endpoint else {
        return refused();
    };
    let result=async {
        let contract=contract()?;
        if endpoint.authentication.terms.namespace!=crate::launch_job::NAMESPACE
            || endpoint.authentication.terms.service_account!=contract["account"].as_str().ok_or(CacheError::Unavailable)?
            || endpoint.authentication.terms.audience!=contract["audience"].as_str().ok_or(CacheError::Unavailable)? {
            return Err(CacheError::InvalidBinding);
        }
        let headers=request.headers();
        let values=headers.get_all(axum::http::header::AUTHORIZATION).iter().collect::<Vec<_>>();
        let [value]=values.as_slice() else {return Err(CacheError::InvalidBinding);};
        let token=value.to_str().map_err(|_|CacheError::InvalidBinding)?
            .strip_prefix("Bearer ").ok_or(CacheError::InvalidBinding)?.to_owned();
        let authentication=endpoint.clone();
        let caller=tokio::task::spawn_blocking(move ||ipc::authenticate(&token,&authentication.authentication,boss_clock_client::wall_now().timestamp()))
            .await.map_err(|_|CacheError::Unavailable)??;
        let identity=identity(headers)?;
        let values=headers.get_all("x-boss-collection-phase").iter().collect::<Vec<_>>();
        let [value]=values.as_slice() else {return Err(CacheError::InvalidBinding);};
        let phase=value.to_str().map_err(|_|CacheError::InvalidBinding)?.to_owned();
        if !matches!(phase.as_str(),"authorize"|"collect") {return Err(CacheError::InvalidBinding);}
        let (record,plan)=endpoint.plan(identity.run).await?;
        context_matches(&record,&plan,&caller,&identity)?;
        let compute_spec=crate::launch_job::Spec {run_id:&record.prepared.run,image:&record.prepared.image,login_secret:&record.prepared.login,bound:record.prepared.bound,grace:record.prepared.grace};
        let original=crate::launch_binding::JobBinding::from_created(&record.original_job,&compute_spec).map_err(|_|CacheError::InvalidBinding)?;
        let original_binding=original.observe_running(&record.original_job,&scope::named_pod(&record.original_pod)).map_err(|_|CacheError::InvalidBinding)?.identity();
        let root=record.store_root.clone();let owner=record.owner;
        let expected=super::Installed::from_binding(&record.bindings)?;
        let wanted=identity.clone();
        let (mut store,lease)=tokio::task::spawn_blocking(move || {
            let store=files::FileStore::open(&root,owner,files::AwaitingVerification)?;
            let super::State::Leased(lease)=store.state()? else {return Err(CacheError::Conflict);};
            if lease.run!=wanted.run.to_string() || lease.nonce!=wanted.nonce || lease.installed!=expected
                || lease.binding.as_ref()!=Some(&original_binding) {return Err(CacheError::InvalidBinding);}
            store.upload_available(&lease)?;
            Ok((store,lease))
        }).await.map_err(|_|CacheError::Unavailable)??;
        let captured=Captured::new(endpoint.metadata.clone());
        let before=attest(&record,&plan,&caller,&identity,&captured).await;
        let guard=json!({"schema":1,"record":record,"plan":plan,"identity":identity,"originals":captured.originals().await});
        let guard_phase=if phase=="authorize" {"authorize"} else {"before"};
        let (next_store,next_lease,before)=tokio::task::spawn_blocking(move || {
            let saved=store.retain_upload_guard(&lease,guard_phase,&guard);
            match (saved,before) {
                (Ok(()),Ok(before))=>Ok((store,lease,before)),
                _=>{let _=super::quarantine::<_,()>(&lease,&mut store);Err(CacheError::Unavailable)}
            }
        }).await.map_err(|_|CacheError::Unavailable)??;
        store=next_store;let lease=next_lease;
        let after_plan=endpoint.plan(identity.run).await?;
        if serde_json::to_value((&record,&plan)).map_err(|_|CacheError::Unavailable)?
            !=serde_json::to_value(after_plan).map_err(|_|CacheError::Unavailable)? {return Err(CacheError::InvalidBinding);}
        let mut reply=json!({"schema":1,"run":identity.run,"nonce":identity.nonce,"pod_uid":identity.pod_uid,"workspace_uid":identity.workspace_uid,"status":"authorized"});
        let limit=contract["limit"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or(CacheError::Unavailable)?;
        let bound=std::time::Duration::from_secs(contract["deadline_seconds"].as_u64().ok_or(CacheError::Unavailable)?);
        if phase=="authorize" {
            let body=tokio::time::timeout(bound,axum::body::to_bytes(request.into_body(),0)).await
                .map_err(|_|CacheError::Unavailable)?.map_err(|_|CacheError::InvalidBinding)?;
            if !body.is_empty() {return Err(CacheError::InvalidBinding);}
            return Ok(reply);
        }
        let intent=json!({"schema":1,"record":record,"plan":plan,"lease":lease,"before":before});
        let (next_store,next_lease)=tokio::task::spawn_blocking(move || {
            store.begin_upload(&lease,&intent)?; Ok((store,lease))
        }).await.map_err(|_|CacheError::Unavailable)??;
        store=next_store;let lease=next_lease;
        let upload=read_upload(request.into_body(),limit,bound).await;
        let transport=json!({"schema":1,"complete":upload.complete,"error":upload.error,"trailers":upload.trailers,
            "bytes":upload.bytes.len(),"sha256":crate::launch_controller::hash(&upload.bytes)});
        let (next_store,next_lease,upload)=tokio::task::spawn_blocking(move || {
            let saved=store.stage_upload(&lease,&upload.bytes,&transport);
            if saved.is_err() || !upload.complete || upload.bytes.is_empty() {
                let _=super::quarantine::<_,()>(&lease,&mut store);
                return Err(CacheError::Unavailable);
            }
            Ok((store,lease,upload))
        }).await.map_err(|_|CacheError::Unavailable)??;
        store=next_store;let lease=next_lease;
        let captured=Captured::new(endpoint.metadata.clone());
        let qualification=async {
            let after=attest(&record,&plan,&caller,&identity,&captured).await?;
            if serde_json::to_value((&record,&plan)).map_err(|_|CacheError::Unavailable)?
                !=serde_json::to_value(endpoint.plan(identity.run).await?).map_err(|_|CacheError::Unavailable)? {
                return Err(CacheError::InvalidBinding);
            }
            reply["status"]=json!("retained");reply["sha256"]=json!(crate::launch_controller::hash(&upload.bytes));reply["bytes"]=json!(upload.bytes.len());
            Ok(json!({"schema":1,"reply":reply,"after":after}))
        }.await;
        let after_guard=json!({"schema":1,"originals":captured.originals().await});
        tokio::task::spawn_blocking(move || {
            let guard=store.retain_upload_guard(&lease,"after",&after_guard);
            let saved=match (guard,qualification) {
                (Ok(()),Ok(receipt))=>store.retain_upload(&lease,&upload.bytes,&receipt),
                (Err(error),_)|(_,Err(error))=>Err(error),
            };
            if saved.is_err() {let _=super::quarantine::<_,()>(&lease,&mut store);}
            saved
        }).await.map_err(|_|CacheError::Unavailable)??;
        Ok(reply)
    }.await;
    match result {
        Ok(reply) => (axum::http::StatusCode::OK, axum::Json(reply)),
        Err(_) => refused(),
    }
}

/// The trusted TLS listener must mount this router privately; no plaintext
/// listener, broker installation, grant or key lifecycle is selected here.
fn router(endpoint: Option<Arc<Endpoint>>) -> axum::Router {
    use axum::routing::post;
    let path = contract()
        .ok()
        .and_then(|c| c["path"].as_str().map(str::to_owned));
    let Some(path) = path else {
        return axum::Router::new();
    };
    axum::Router::new()
        .route(&path, post(handle))
        .with_state(endpoint)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;
    use tower::ServiceExt;
    #[tokio::test]
    async fn an_interrupted_private_body_retains_received_bytes_and_refuses_completion() {
        let stream = futures::stream::iter([
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"synthetic private prefix")),
            Err(std::io::Error::other("synthetic private failure")),
        ]);
        let upload = read_upload(
            axum::body::Body::from_stream(stream),
            262144,
            std::time::Duration::from_secs(1),
        )
        .await;
        assert!(!upload.complete);
        assert_eq!(upload.bytes, b"synthetic private prefix");
    }
    #[derive(Clone)]
    struct Metadata {
        run: Value,
        credential: Value,
        objects: Vec<(scope::Resource, String, Value)>,
    }
    #[async_trait::async_trait]
    impl scope::Metadata for Metadata {
        async fn run(&self, _: uuid::Uuid) -> Result<Value, CacheError> {
            Ok(self.run.clone())
        }
        async fn credential(&self, _: &str) -> Result<Value, CacheError> {
            Ok(self.credential.clone())
        }
        async fn native(&self, r: scope::Resource, n: &str) -> Result<Value, CacheError> {
            self.objects
                .iter()
                .find(|(resource, name, _)| *resource == r && name == n)
                .map(|(_, _, value)| value.clone())
                .ok_or(CacheError::Unavailable)
        }
    }

    fn fixture() -> (scope::Record, Plan, ipc::Caller, Identity, Metadata) {
        let (record, run, mut objects, _, request) = scope::tests::fixture();
        let image_witness = json!({"image":format!("registry.invalid/agent@sha256:{}","a".repeat(64)),"generation":"b".repeat(40),"image_platform":"linux/amd64",
            "collector_helper_sha256":crate::launch_controller::hash(include_bytes!("../../../../../../infra/agent/private-collector.py")),
            "collector_contract_sha256":crate::launch_controller::hash(include_bytes!("../../../../../../infra/agent/private-collector-contract.json"))});
        let trust_generation = uuid::Uuid::from_u128(10);
        let account_uid = uuid::Uuid::from_u128(11);
        let pod_uid = uuid::Uuid::from_u128(12);
        let workspace_uid = record.original_workspace["metadata"]["uid"]
            .as_str()
            .unwrap()
            .to_string();
        let spec = super::super::collector::Spec {
            run: request.run,
            nonce: request.nonce,
            workspace_uid: &workspace_uid,
            image_witness: &image_witness,
            trust_generation,
        };
        let mut job = super::super::collector::manifest(&spec).unwrap();
        job["metadata"]["uid"] = json!("collector-job");
        job["metadata"]["resourceVersion"] = json!("2");
        let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"collector-pod","namespace":"boss-dev","uid":pod_uid,"resourceVersion":"2","labels":job["spec"]["template"]["metadata"]["labels"],
            "ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","name":job["metadata"]["name"],"uid":"collector-job","controller":true}]},"spec":job["spec"]["template"]["spec"],
            "status":{"phase":"Running","containerStatuses":[{"name":"collector","containerID":"containerd://collector","restartCount":0,"ready":true,"state":{"running":{"startedAt":"2026-10-14T17:00:00Z"}}}]}});
        for (resource, name, value) in &mut objects {
            if *resource == scope::Resource::Job
                || (*resource == scope::Resource::Pod && name == "pod")
            {
                let plural = if *resource == scope::Resource::Job {
                    *resource = scope::Resource::StoppedJob;
                    "jobs"
                } else {
                    *resource = scope::Resource::StoppedPod;
                    "pods"
                };
                *value = json!({"apiVersion":"v1","kind":"Status","reason":"NotFound","code":404,"details":{"name":name,"kind":plural}});
            }
        }
        objects.extend([
            (scope::Resource::Job,job["metadata"]["name"].as_str().unwrap().into(),job.clone()),
            (scope::Resource::Pod,"collector-pod".into(),pod.clone()),
            (scope::Resource::CollectorAccount,"boss-managed-cache-collector".into(),json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"namespace":"boss-dev","name":"boss-managed-cache-collector","uid":account_uid,"resourceVersion":"2"}}))]);
        let metadata = Metadata {
            run,
            credential: scope::tests::credential(),
            objects,
        };
        let plan = Plan {
            run: request.run,
            nonce: request.nonce,
            workspace_uid: workspace_uid.clone(),
            image_witness,
            trust_generation,
            collector_account_uid: account_uid,
            original_job: job,
            original_pod: pod,
        };
        let caller = ipc::Caller {
            pod_uid,
            service_account_uid: account_uid,
            expires_at: boss_clock_client::wall_now().timestamp() + 60,
        };
        let identity = Identity {
            run: request.run,
            nonce: request.nonce,
            pod_uid,
            workspace_uid,
        };
        (record, plan, caller, identity, metadata)
    }
    #[tokio::test]
    async fn an_exact_stopped_compute_and_bound_collector_are_independently_attested() {
        let (record, plan, caller, identity, metadata) = fixture();
        assert!(
            attest(&record, &plan, &caller, &identity, &metadata)
                .await
                .is_ok()
        );
        for key in [
            "nonce",
            "job_uid",
            "pod_uid",
            "container",
            "pvc_uid",
            "account_uid",
            "active_run",
            "generation",
            "controller_uid",
            "original_alive",
            "trust_generation",
            "writable_mount",
            "schema",
            "prompt_hash",
        ] {
            let (mut record, mut plan, mut caller, mut identity, mut metadata) = fixture();
            match key {
                "nonce" => identity.nonce = uuid::Uuid::new_v4(),
                "job_uid" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(r, n, _)| {
                            *r == scope::Resource::Job && n.starts_with("agent-private")
                        })
                        .unwrap()
                        .2["metadata"]["uid"] = json!("replacement")
                }
                "pod_uid" => caller.pod_uid = uuid::Uuid::new_v4(),
                "container" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(_, n, _)| n == "collector-pod")
                        .unwrap()
                        .2["status"]["containerStatuses"][0]["containerID"] =
                        json!("containerd://replacement")
                }
                "pvc_uid" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(r, _, _)| *r == scope::Resource::Workspace)
                        .unwrap()
                        .2["metadata"]["uid"] = json!("replacement")
                }
                "account_uid" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(r, _, _)| *r == scope::Resource::CollectorAccount)
                        .unwrap()
                        .2["metadata"]["uid"] = json!(uuid::Uuid::new_v4())
                }
                "active_run" => metadata.run["steps"][0]["status"] = json!("completed"),
                "generation" => record.installed.storage_generation = "changed".into(),
                "controller_uid" => record.controller_pod_uid = uuid::Uuid::new_v4(),
                "original_alive" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(r, _, _)| *r == scope::Resource::StoppedJob)
                        .unwrap()
                        .2 = record.original_job.clone()
                }
                "trust_generation" => plan.trust_generation = uuid::Uuid::new_v4(),
                "writable_mount" => {
                    metadata
                        .objects
                        .iter_mut()
                        .find(|(_, n, _)| n == "collector-pod")
                        .unwrap()
                        .2["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] = json!(false)
                }
                "schema" => {
                    record.bindings.schema = 2;
                    record.installed.schema = 2;
                }
                "prompt_hash" => {
                    record.prompt_sha256 = "not-a-sha256".into();
                    metadata.run["steps"][0]["metadata"]["worker_start_expectation"]["prompt_sha256"] =
                        json!(record.prompt_sha256);
                }
                _ => unreachable!(),
            }
            assert!(
                attest(&record, &plan, &caller, &identity, &metadata)
                    .await
                    .is_err(),
                "{key}"
            );
        }
    }

    fn write_private(path: &std::path::Path, value: &[u8]) {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            // mode-bits-ok: private synthetic broker fixture, never executable
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(value).unwrap();
        file.sync_all().unwrap();
    }
    pub(super) fn endpoint_fixture() -> (Arc<Endpoint>, String, Identity, std::path::PathBuf) {
        use base64::Engine;
        use boss_jobs::credentials::broker_stage::{StageKeys, Terms, TrustedKey};
        use rsa::signature::{SignatureEncoding, Signer};
        use std::os::unix::fs::PermissionsExt;
        struct Keys(rsa::RsaPublicKey);
        impl StageKeys for Keys {
            fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
                Ok(vec![TrustedKey {
                    kid: "fixture".into(),
                    key: self.0.clone(),
                }])
            }
        }
        let (mut record, plan, caller, identity, metadata) = fixture();
        let root = boss_testing::scratch_dir("private-collection-records");
        let store = boss_testing::scratch_dir("private-collection-storage");
        for directory in [&root, &store] {
            // mode-bits-ok: private synthetic broker directory, never executable
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owner = rustix::process::geteuid().as_raw();
        record.store_root = store.clone();
        record.owner = owner;
        write_private(
            &root.join("installation.json"),
            &serde_json::to_vec(
                &json!({"schema":1,"bindings":record.installed,"store_root":store,"owner":owner}),
            )
            .unwrap(),
        );
        write_private(
            &root.join(format!("{}.json", identity.run)),
            &serde_json::to_vec(&record).unwrap(),
        );
        write_private(
            &root.join(format!("collection-{}.json", identity.run)),
            &serde_json::to_vec(&plan).unwrap(),
        );
        let compute_spec = crate::launch_job::Spec {
            run_id: &record.prepared.run,
            image: &record.prepared.image,
            login_secret: &record.prepared.login,
            bound: record.prepared.bound,
            grace: record.prepared.grace,
        };
        let compute =
            crate::launch_binding::JobBinding::from_created(&record.original_job, &compute_spec)
                .unwrap();
        let bound = compute
            .observe_running(
                &record.original_job,
                &scope::named_pod(&record.original_pod),
            )
            .unwrap();
        let lease = super::super::Lease {
            installed: super::super::Installed::from_binding(&record.bindings).unwrap(),
            run: record.prepared.run.clone(),
            nonce: identity.nonce,
            binding: Some(bound.identity()),
        };
        write_private(
            &store.join("state.json"),
            &serde_json::to_vec(&super::super::State::Leased(lease)).unwrap(),
        );
        let terms = Terms {
            issuer: "https://fixture.invalid".into(),
            audience: "boss-managed-cache-collection".into(),
            namespace: "boss-dev".into(),
            service_account: "boss-managed-cache-collector".into(),
        };
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let now = boss_clock_client::wall_now().timestamp();
        let claims = json!({"iss":terms.issuer,"aud":[terms.audience],"sub":"system:serviceaccount:boss-dev:boss-managed-cache-collector","iat":now,"exp":now+600,
            "kubernetes.io":{"namespace":"boss-dev","serviceaccount":{"name":"boss-managed-cache-collector","uid":caller.service_account_uid},"pod":{"uid":caller.pod_uid}}});
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let signed = format!(
            "{}.{}",
            encode(br#"{"alg":"RS256","kid":"fixture"}"#),
            encode(&serde_json::to_vec(&claims).unwrap())
        );
        let signature = rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(key.clone())
            .sign(signed.as_bytes());
        let token = format!("{signed}.{}", encode(&signature.to_vec()));
        (
            Arc::new(Endpoint {
                authentication: ipc::Authentication {
                    terms,
                    service_account_uid: caller.service_account_uid,
                    keys: Arc::new(Keys(key.to_public_key())),
                },
                records: scope::FileRecords { root, owner },
                metadata: Arc::new(metadata),
            }),
            token,
            identity,
            store,
        )
    }
    pub(super) fn request(
        token: &str,
        identity: &Identity,
        phase: &str,
        body: axum::body::Body,
    ) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/collect")
            .header("Authorization", format!("Bearer {token}"))
            .header("x-boss-collection-phase", phase)
            .header("x-boss-run", identity.run.to_string())
            .header("x-boss-nonce", identity.nonce.to_string())
            .header("x-boss-pod-uid", identity.pod_uid.to_string())
            .header("x-boss-workspace-uid", &identity.workspace_uid)
            .body(body)
            .unwrap()
    }
    #[tokio::test]
    async fn the_fixed_http_channel_authorizes_then_privately_retains_and_closes_one_exact_upload()
    {
        use std::os::unix::fs::MetadataExt;
        let (endpoint, token, identity, store) = endpoint_fixture();
        let service = router(Some(endpoint));
        let response = service
            .clone()
            .oneshot(request(
                &token,
                &identity,
                "authorize",
                axum::body::Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let authorized: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            authorized,
            json!({"schema":1,"status":"authorized","run":identity.run,"nonce":identity.nonce,"pod_uid":identity.pod_uid,"workspace_uid":identity.workspace_uid})
        );
        assert!(
            !store
                .join(format!("upload-{}-intent.private", identity.nonce))
                .exists()
        );
        let private = b"synthetic private cache";
        let response = service
            .clone()
            .oneshot(request(
                &token,
                &identity,
                "collect",
                axum::body::Body::from(private.as_slice()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let receipt: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            receipt,
            json!({"schema":1,"status":"retained","run":identity.run,"nonce":identity.nonce,"pod_uid":identity.pod_uid,"workspace_uid":identity.workspace_uid,"bytes":private.len(),"sha256":crate::launch_controller::hash(private)})
        );
        assert_eq!(
            std::fs::read(store.join(format!("upload-{}-original.private", identity.nonce)))
                .unwrap(),
            private
        );
        let stored: Value = serde_json::from_slice(
            &std::fs::read(store.join(format!("upload-{}-receipt.private", identity.nonce)))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stored["reply"], receipt);
        assert!(stored["after"]["collector_job"].is_object());
        let replay = service
            .oneshot(request(
                &token,
                &identity,
                "collect",
                axum::body::Body::from("replacement"),
            ))
            .await
            .unwrap();
        assert_eq!(replay.status(), axum::http::StatusCode::FORBIDDEN);
        assert_eq!(
            std::fs::read(store.join(format!("upload-{}-original.private", identity.nonce)))
                .unwrap(),
            private
        );
        for suffix in ["intent", "original", "transport", "receipt"] {
            assert_eq!(
                std::fs::metadata(
                    store.join(format!("upload-{}-{suffix}.private", identity.nonce))
                )
                .unwrap()
                .mode()
                    & 0o777,
                0o600
            );
        }
        let state: super::super::State =
            serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap();
        assert!(matches!(state, super::super::State::Leased(_)));
    }
    fn counted_body(count: Arc<std::sync::atomic::AtomicUsize>) -> axum::body::Body {
        axum::body::Body::from_stream(futures::stream::poll_fn(move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(Some(Err::<axum::body::Bytes, _>(std::io::Error::other(
                "synthetic private body was polled",
            ))))
        }))
    }
    #[tokio::test]
    async fn authentication_scope_and_replay_refuse_before_polling_private_bytes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (mut endpoint, token, identity, store) = endpoint_fixture();
        let count = Arc::new(AtomicUsize::new(0));
        for failure in [
            "forged",
            "machine-identity",
            "nonce",
            "duplicate",
            "configuration",
        ] {
            let mut changed = identity.clone();
            let used = if matches!(failure, "forged" | "machine-identity") {
                "synthetic forged bearer"
            } else {
                &token
            };
            if failure == "nonce" {
                changed.nonce = uuid::Uuid::new_v4();
            }
            if failure == "configuration" {
                Arc::get_mut(&mut endpoint)
                    .unwrap()
                    .authentication
                    .terms
                    .audience = "not-the-collection-door".into();
            }
            let mut request = request(used, &changed, "collect", counted_body(count.clone()));
            if failure == "machine-identity" {
                request
                    .headers_mut()
                    .insert("x-boss-user", "synthetic-asserted-user".parse().unwrap());
                request.headers_mut().insert(
                    "x-boss-machine-token",
                    "synthetic-machine-token".parse().unwrap(),
                );
            }
            if failure == "duplicate" {
                request
                    .headers_mut()
                    .append("x-boss-nonce", identity.nonce.to_string().parse().unwrap());
            }
            let response = router(Some(endpoint.clone()))
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::FORBIDDEN,
                "{failure}"
            );
            assert_eq!(count.load(Ordering::SeqCst), 0, "{failure}");
        }
        assert!(
            !store
                .join(format!("upload-{}-intent.private", identity.nonce))
                .exists()
        );
    }
    #[tokio::test]
    async fn partial_oversized_and_failed_durability_uploads_keep_originals_and_never_acknowledge()
    {
        use std::os::unix::fs::symlink;
        for failure in ["partial", "oversized", "receipt"] {
            let (endpoint, token, identity, store) = endpoint_fixture();
            let body = match failure {
                "partial" => axum::body::Body::from_stream(futures::stream::iter([
                    Ok(axum::body::Bytes::from_static(b"synthetic private prefix")),
                    Err(std::io::Error::other("synthetic private error original")),
                ])),
                "oversized" => axum::body::Body::from(vec![b'x'; 262145]),
                _ => axum::body::Body::from("synthetic complete original"),
            };
            if failure == "receipt" {
                let outside = boss_testing::scratch_dir("private-upload-outside").join("unchanged");
                std::fs::write(&outside, b"unchanged synthetic outside").unwrap();
                symlink(
                    &outside,
                    store.join(format!("upload-{}-receipt.private", identity.nonce)),
                )
                .unwrap();
            }
            let response = router(Some(endpoint.clone()))
                .oneshot(request(&token, &identity, "collect", body))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::FORBIDDEN,
                "{failure}"
            );
            let reply: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(reply, json!({"schema":1,"status":"refused"}));
            let original =
                std::fs::read(store.join(format!("upload-{}-original.private", identity.nonce)))
                    .unwrap();
            match failure {
                "partial" => assert_eq!(original, b"synthetic private prefix"),
                "oversized" => assert_eq!(original, vec![b'x'; 262144]),
                _ => assert_eq!(original, b"synthetic complete original"),
            }
            let state: super::super::State =
                serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap();
            assert!(
                matches!(state, super::super::State::Quarantined(_)),
                "{failure}"
            );
            let replay = router(Some(endpoint))
                .oneshot(request(
                    &token,
                    &identity,
                    "collect",
                    axum::body::Body::from("retry"),
                ))
                .await
                .unwrap();
            assert_eq!(replay.status(), axum::http::StatusCode::FORBIDDEN);
            if failure == "partial" {
                let transport: Value = serde_json::from_slice(
                    &std::fs::read(
                        store.join(format!("upload-{}-transport.private", identity.nonce)),
                    )
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(transport["complete"], false);
                assert!(
                    transport["error"]
                        .as_str()
                        .unwrap()
                        .contains("synthetic private error original")
                );
            }
        }
    }
    #[tokio::test]
    async fn changed_native_incarnation_after_body_keeps_full_refused_original_and_quarantines() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Changing {
            source: Arc<dyn scope::Metadata>,
            changed: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl scope::Metadata for Changing {
            async fn run(&self, id: uuid::Uuid) -> Result<Value, CacheError> {
                self.source.run(id).await
            }
            async fn credential(&self, id: &str) -> Result<Value, CacheError> {
                self.source.credential(id).await
            }
            async fn native(&self, r: scope::Resource, n: &str) -> Result<Value, CacheError> {
                let mut value = self.source.native(r, n).await?;
                if n == "collector-pod" && self.changed.load(Ordering::SeqCst) {
                    value["status"]["containerStatuses"][0]["containerID"] =
                        json!("containerd://changed-after-body");
                }
                Ok(value)
            }
        }
        let (mut endpoint, token, identity, store) = endpoint_fixture();
        let changed = Arc::new(AtomicBool::new(false));
        let source = endpoint.metadata.clone();
        Arc::get_mut(&mut endpoint).unwrap().metadata = Arc::new(Changing {
            source,
            changed: changed.clone(),
        });
        let body = axum::body::Body::from_stream(futures::stream::once(async move {
            changed.store(true, Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                b"synthetic original before refusal",
            ))
        }));
        let response = router(Some(endpoint))
            .oneshot(request(&token, &identity, "collect", body))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        assert_eq!(
            std::fs::read(store.join(format!("upload-{}-original.private", identity.nonce)))
                .unwrap(),
            b"synthetic original before refusal"
        );
        let retained =
            std::fs::read(store.join(format!("upload-{}-after-native.private", identity.nonce)))
                .unwrap();
        assert!(
            String::from_utf8(retained)
                .unwrap()
                .contains("containerd://changed-after-body")
        );
        assert!(
            !store
                .join(format!("upload-{}-receipt.private", identity.nonce))
                .exists()
        );
        let state: super::super::State =
            serde_json::from_slice(&std::fs::read(store.join("state.json")).unwrap()).unwrap();
        assert!(matches!(state, super::super::State::Quarantined(_)));
    }

    #[tokio::test]
    async fn a_private_collector_endpoint_refuses_before_reading_an_untrusted_body() {
        let response = router(None)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/collect")
                    .body(axum::body::Body::from("synthetic private material"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"schema":1,"status":"refused"})
        );
    }
}
