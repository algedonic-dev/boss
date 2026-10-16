//! Broker-owned metadata resolution for design001fe8d0. No private cache transport,
//! installed authority, collector or activation is inferred from this source.
use super::{CacheError, ipc};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// Metadata resolution never qualifies private bytes. The separately reviewed
/// private verifier/channel must supply that effect before collection exists.
pub(crate) type MetadataOnly = super::files::AwaitingVerification;

/// Inputs belong to the protected broker installation/controller record, not
/// to the metadata request. Native originals are metadata, never login bytes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub prepared: crate::launch_controller::Prepared,
    pub bindings: crate::launch_cluster::Bindings,
    pub installed: crate::launch_cluster::Bindings,
    pub controller_pod: String,
    pub controller_pod_uid: uuid::Uuid,
    pub controller_sa_uid: uuid::Uuid,
    pub original_job: Value,
    pub original_pod: Value,
    pub original_workspace: Value,
    pub store_root: PathBuf,
    pub owner: u32,
    pub prompt_sha256: String,
}

#[async_trait]
pub(crate) trait BrokerRecords: Send + Sync {
    async fn read(&self, run: uuid::Uuid) -> Result<Record, CacheError>;
}
/// The installer must bind this broker-owned metadata directory to its exact
/// generation. This adapter reads only fixed metadata files, never auth.json.
pub(crate) struct FileRecords {
    pub root: PathBuf,
    pub owner: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    schema: u64,
    bindings: crate::launch_cluster::Bindings,
    store_root: PathBuf,
    owner: u32,
}
#[async_trait]
impl BrokerRecords for FileRecords {
    async fn read(&self, run: uuid::Uuid) -> Result<Record, CacheError> {
        use std::os::unix::fs::MetadataExt;
        if run.is_nil() {
            return Err(CacheError::InvalidBinding);
        }
        let root = self.root.clone();
        let owner = self.owner;
        tokio::task::spawn_blocking(move || {
            use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};
            let directory = std::fs::File::from(
                openat2(
                    rustix::fs::CWD,
                    &root,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(),
                    ResolveFlags::NO_SYMLINKS,
                )
                .map_err(|_| CacheError::Unavailable)?,
            );
            let meta = directory.metadata().map_err(|_| CacheError::Unavailable)?;
            if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o777 != 0o700 {
                return Err(CacheError::Unavailable);
            }
            let installation: Installation = read_metadata(&directory, "installation.json", owner)?;
            if installation.schema != 1
                || installation.owner != owner
                || !installation.store_root.is_absolute()
                || installation
                    .store_root
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(CacheError::InvalidBinding);
            }
            let mut record: Record = read_metadata(&directory, &format!("{run}.json"), owner)?;
            if record.prepared.run != run.to_string() {
                return Err(CacheError::InvalidBinding);
            }
            record.installed = installation.bindings;
            record.store_root = installation.store_root;
            record.owner = installation.owner;
            Ok(record)
        })
        .await
        .map_err(|_| CacheError::Unavailable)?
    }
}
pub(super) fn read_metadata<T: serde::de::DeserializeOwned>(
    directory: &std::fs::File,
    name: &str,
    owner: u32,
) -> Result<T, CacheError> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::{io::Read, os::unix::fs::MetadataExt};
    let file = std::fs::File::from(
        openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| CacheError::Unavailable)?,
    );
    let meta = file.metadata().map_err(|_| CacheError::Unavailable)?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o777 != 0o600 || meta.nlink() != 1 {
        return Err(CacheError::Unavailable);
    }
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CacheError::Unavailable)?;
    if bytes.len() > 256 * 1024 {
        return Err(CacheError::Unavailable);
    }
    serde_json::from_slice(&bytes).map_err(|_| CacheError::Unavailable)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resource {
    Job,
    Pod,
    Workspace,
    ControllerAccount,
    CollectorAccount,
    StoppedJob,
    StoppedPod,
}
#[async_trait]
pub(crate) trait Metadata: Send + Sync {
    async fn run(&self, run: uuid::Uuid) -> Result<Value, CacheError>;
    async fn credential(&self, id: &str) -> Result<Value, CacheError>;
    async fn native(&self, resource: Resource, name: &str) -> Result<Value, CacheError>;
    async fn observe(&self, query: Query) -> Observation {
        let result = match query {
            Query::Run(run) => self.run(run).await,
            Query::Credential(id) => self.credential(&id).await,
            Query::Native(resource, name) => self.native(resource, &name).await,
        };
        Observation {
            original: json!({"kind":"port-result","result":result}),
            result,
        }
    }
}
pub(crate) enum Query {
    Run(uuid::Uuid),
    Credential(String),
    Native(Resource, String),
}
/// No Debug or public serialization: full response originals belong only to
/// the broker's private descriptor archive, not metadata IPC diagnostics.
pub(crate) struct Observation {
    pub result: Result<Value, CacheError>,
    pub(super) original: Value,
}
async fn observed_request(request: reqwest::RequestBuilder) -> Observation {
    use base64::Engine;
    let mut response = match request
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Observation {
                result: Err(CacheError::Unavailable),
                original: json!({"kind":"http","status":null,"body_base64":"","complete":false,"error":error.to_string()}),
            };
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    let mut complete = false;
    let mut error = None;
    loop {
        match response.chunk().await {
            Ok(None) => {
                complete = true;
                break;
            }
            Err(failure) => {
                error = Some(failure.to_string());
                break;
            }
            Ok(Some(chunk)) => {
                let remaining = 256 * 1024 - bytes.len();
                if chunk.len() > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                    error = Some("native_body_bound_exceeded".into());
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
        }
    }
    let result = if status == reqwest::StatusCode::OK && complete {
        serde_json::from_slice(&bytes).map_err(|_| CacheError::InvalidBinding)
    } else {
        Err(CacheError::Unavailable)
    };
    Observation {
        result,
        original: json!({"kind":"http","status":status.as_u16(),"body_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),"complete":complete,"error":error}),
    }
}

pub(crate) struct Resolver {
    pub records: Arc<dyn BrokerRecords>,
    pub metadata: Arc<dyn Metadata>,
}

/// Clients and endpoints are bound by trusted broker installation. Native
/// reads never use the ordinary controller Exec/journal transport.
pub(crate) struct HttpMetadata {
    jobs: boss_core::machine_token::Client,
    native: reqwest::Client,
    jobs_base: reqwest::Url,
    native_base: reqwest::Url,
    actor: String,
}
impl HttpMetadata {
    pub(crate) fn new(
        jobs: boss_core::machine_token::Client,
        jobs_base: reqwest::Url,
        native_base: reqwest::Url,
        native_builder: reqwest::ClientBuilder,
        actor: String,
    ) -> Result<Self, CacheError> {
        for base in [&jobs_base, &native_base] {
            if !matches!(base.scheme(), "http" | "https")
                || base.host_str().is_none()
                || !base.username().is_empty()
                || base.password().is_some()
                || base.query().is_some()
                || base.fragment().is_some()
                || base.path() != "/"
            {
                return Err(CacheError::InvalidBinding);
            }
        }
        if native_base.scheme() != "https" || actor.trim().is_empty() {
            return Err(CacheError::InvalidBinding);
        }
        Ok(Self {
            jobs,
            jobs_base,
            native_base,
            actor,
            native: native_builder
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|_| CacheError::Unavailable)?,
        })
    }
    fn registry_request(&self, path: &str) -> Result<reqwest::RequestBuilder, CacheError> {
        let url = self
            .jobs_base
            .join(path)
            .map_err(|_| CacheError::InvalidBinding)?;
        Ok(self
            .jobs
            .get(url.as_str())
            .header("x-boss-user", crate::identity::header(&self.actor)))
    }
    fn native_request(
        &self,
        resource: Resource,
        name: &str,
    ) -> Result<reqwest::RequestBuilder, CacheError> {
        if !crate::launch_job::is_name(name)
            || resource == Resource::ControllerAccount && name != "dev-session"
            || resource == Resource::CollectorAccount && name != "boss-managed-cache-collector"
        {
            return Err(CacheError::InvalidBinding);
        }
        let (version, plural) = match resource {
            Resource::Job | Resource::StoppedJob => ("/apis/batch/v1", "jobs"),
            Resource::Pod | Resource::StoppedPod => ("/api/v1", "pods"),
            Resource::Workspace => ("/api/v1", "persistentvolumeclaims"),
            Resource::ControllerAccount | Resource::CollectorAccount => {
                ("/api/v1", "serviceaccounts")
            }
        };
        let url = self
            .native_base
            .join(&format!(
                "{version}/namespaces/{}/{plural}/{name}",
                crate::launch_job::NAMESPACE
            ))
            .map_err(|_| CacheError::InvalidBinding)?;
        Ok(self.native.get(url))
    }
}
#[cfg(test)]
async fn bounded_json(request: reqwest::RequestBuilder) -> Result<Value, CacheError> {
    observed_request(request).await.result
}
#[async_trait]
impl Metadata for HttpMetadata {
    async fn run(&self, run: uuid::Uuid) -> Result<Value, CacheError> {
        self.observe(Query::Run(run)).await.result
    }
    async fn credential(&self, id: &str) -> Result<Value, CacheError> {
        self.observe(Query::Credential(id.into())).await.result
    }
    async fn native(&self, r: Resource, name: &str) -> Result<Value, CacheError> {
        self.observe(Query::Native(r, name.into())).await.result
    }
    async fn observe(&self, query: Query) -> Observation {
        let request = match &query {
            Query::Run(run) if !run.is_nil() => self.registry_request(&format!("/api/jobs/{run}")),
            Query::Credential(id) if crate::launch_job::is_name(id) => {
                self.registry_request(&format!("/api/credentials/{id}"))
            }
            Query::Native(resource, name) => self.native_request(*resource, name),
            _ => Err(CacheError::InvalidBinding),
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                return Observation {
                    result: Err(error),
                    original: json!({"kind":"not-requested","error":error}),
                };
            }
        };
        let mut observed = observed_request(request).await;
        if let Query::Native(resource, name) = query
            && matches!(resource, Resource::StoppedJob | Resource::StoppedPod)
        {
            observed.result = judge_absence(
                &observed.original,
                &name,
                if resource == Resource::StoppedJob {
                    "jobs"
                } else {
                    "pods"
                },
            );
        }
        observed
    }
}

#[async_trait]
impl ipc::ScopePort<MetadataOnly> for Resolver {
    async fn resolve(
        &self,
        caller: &ipc::Caller,
        request: &ipc::Request,
    ) -> Result<ipc::Scope<MetadataOnly>, CacheError> {
        // Collection is a different authenticated private transport under design001.
        // Never open a local path or pretend its PVC is mounted in boss.
        if matches!(request.phase, ipc::Phase::Collect | ipc::Phase::Reconcile) {
            return Err(CacheError::Unavailable);
        }
        if request.schema != 1
            || request.run.is_nil()
            || request.nonce.is_nil()
            || caller.expires_at <= boss_clock_client::wall_now().timestamp()
        {
            return Err(CacheError::InvalidBinding);
        }
        let record = self.records.read(request.run).await?;
        let p = &record.prepared;
        let b = &record.bindings;
        protected_record(&record)?;
        if p.run != request.run.to_string()
            || b.credential != request.credential
            || record.controller_pod_uid != caller.pod_uid
            || record.controller_sa_uid != caller.service_account_uid
            || caller.pod_uid.is_nil()
            || caller.service_account_uid.is_nil()
        {
            return Err(CacheError::InvalidBinding);
        }
        if let ipc::Phase::Acquire { generation } = &request.phase
            && generation != &b.storage_generation
        {
            return Err(CacheError::InvalidBinding);
        }
        let run_original = self.metadata.run(request.run).await?;
        registered_run(&record, &run_original)?;
        let credential_original = self.metadata.credential(&b.credential).await?;
        let credential = credential_original
            .get("data")
            .unwrap_or(&credential_original);
        let row: boss_jobs::credentials::CredentialRow =
            serde_json::from_value(credential.clone()).map_err(|_| CacheError::InvalidBinding)?;
        if !crate::launch_cluster::credential_matches(&row, b) {
            return Err(CacheError::InvalidBinding);
        }
        let controller = self
            .metadata
            .native(Resource::Pod, &record.controller_pod)
            .await?;
        let sa = self
            .metadata
            .native(Resource::ControllerAccount, "dev-session")
            .await?;
        native_identity(&controller, "Pod", &record.controller_pod)?;
        native_identity(&sa, "ServiceAccount", "dev-session")?;
        if controller["metadata"]["uid"] != caller.pod_uid.to_string()
            || controller["spec"]["serviceAccountName"] != "dev-session"
            || controller["status"]["phase"] != "Running"
            || sa["metadata"]["uid"] != caller.service_account_uid.to_string()
        {
            return Err(CacheError::InvalidBinding);
        }
        let running = if matches!(request.phase, ipc::Phase::Acquire { .. })
            && record.original_job.is_null()
            && record.original_pod.is_null()
            && record.original_workspace.is_null()
        {
            // Lease acquisition precedes creating/mounting the writable run
            // cache. A lease supplies no compute incarnation; Bind does that.
            None
        } else {
            let spec = crate::launch_job::Spec {
                run_id: &p.run,
                image: &p.image,
                login_secret: &p.login,
                bound: p.bound,
                grace: p.grace,
            };
            let original =
                crate::launch_binding::JobBinding::from_created(&record.original_job, &spec)
                    .map_err(|_| CacheError::InvalidBinding)?;
            let original_pod_name = record.original_pod["metadata"]["name"]
                .as_str()
                .ok_or(CacheError::InvalidBinding)?;
            native_identity(&record.original_pod, "Pod", original_pod_name)?;
            let running = original
                .observe_running(&record.original_job, &named_pod(&record.original_pod))
                .map_err(|_| CacheError::InvalidBinding)?;
            let job = self.metadata.native(Resource::Job, original.name()).await?;
            let pod = self
                .metadata
                .native(Resource::Pod, original_pod_name)
                .await?;
            native_identity(&pod, "Pod", original_pod_name)?;
            running
                .recheck(&job, &named_pod(&pod))
                .map_err(|_| CacheError::InvalidBinding)?;
            let intent = crate::launch_job::workspace_manifest(&spec);
            crate::launch_controller::workspace_identity(&record.original_workspace, &intent)
                .map_err(|_| CacheError::InvalidBinding)?;
            let workspace = self
                .metadata
                .native(
                    Resource::Workspace,
                    intent["metadata"]["name"]
                        .as_str()
                        .ok_or(CacheError::InvalidBinding)?,
                )
                .await?;
            crate::launch_controller::workspace_identity(&workspace, &intent)
                .map_err(|_| CacheError::InvalidBinding)?;
            if workspace["metadata"]["uid"] != record.original_workspace["metadata"]["uid"] {
                return Err(CacheError::InvalidBinding);
            }
            Some(running)
        };
        // A changed protected generation or registry row while resolving is
        // not the generation just judged. These reads remain metadata-only.
        let after = self.records.read(request.run).await?;
        if serde_json::to_value(&record).map_err(|_| CacheError::Unavailable)?
            != serde_json::to_value(after).map_err(|_| CacheError::Unavailable)?
            || self.metadata.credential(&b.credential).await? != credential_original
            || self.metadata.run(request.run).await? != run_original
            || caller.expires_at <= boss_clock_client::wall_now().timestamp()
        {
            return Err(CacheError::InvalidBinding);
        }
        Ok(ipc::Scope {
            prepared: record.prepared,
            bindings: record.bindings,
            running,
            terminal: None,
            store_root: record.store_root,
            owner: record.owner,
            verifier: super::files::AwaitingVerification,
            retained: None,
            controller_pod_uid: record.controller_pod_uid,
            controller_service_account_uid: record.controller_sa_uid,
        })
    }
}

pub(super) fn native_identity(object: &Value, kind: &str, name: &str) -> Result<(), CacheError> {
    if object["apiVersion"] != "v1"
        || object["kind"] != kind
        || object["metadata"]["namespace"] != crate::launch_job::NAMESPACE
        || object["metadata"]["name"] != name
        || !object["metadata"]["deletionTimestamp"].is_null()
        || ["uid", "resourceVersion"].iter().any(|key| {
            object["metadata"][key]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.chars().any(char::is_whitespace))
        })
    {
        Err(CacheError::InvalidBinding)
    } else {
        Ok(())
    }
}
// Adapt one exact named GET to the existing single-Pod posture evaluator.
// This is a local envelope, not a claim that a native list was observed.
pub(super) fn named_pod(pod: &Value) -> Value {
    json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":pod["metadata"]["resourceVersion"]},"items":[pod]})
}

pub(super) fn registered_run(record: &Record, original: &Value) -> Result<(), CacheError> {
    let p = &record.prepared;
    let run = original.get("data").unwrap_or(original);
    let building = run["steps"]
        .as_array()
        .ok_or(CacheError::InvalidBinding)?
        .iter()
        .filter(|s| s["spec_slug"] == "building")
        .collect::<Vec<_>>();
    let [building] = building.as_slice() else {
        return Err(CacheError::InvalidBinding);
    };
    let expectation = &building["metadata"]["worker_start_expectation"];
    if run["id"] != p.run
        || run["kind"] != "agent-run"
        || run["status"] != "open"
        || building["status"] != "active"
        || building["assignee_id"] != p.actor
        || expectation["schema"] != 2
        || expectation["observer"] != p.observer
        || expectation["actor"] != p.actor
        || expectation["snapshot_sha256"] != p.snapshot.sha256
        || expectation["prompt_sha256"] != record.prompt_sha256
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(())
}
/// One owner for the existing metadata IPC and private collector's protected
/// installation shape. This validates declarations, never an installed mount.
pub(super) fn protected_record(record: &Record) -> Result<(), CacheError> {
    let p = &record.prepared;
    let b = &record.bindings;
    if b != &record.installed
        || b.schema != 1
        || b.harness != "codex-exec"
        || p.image != b.image
        || !crate::launch_job::is_pinned(&b.image)
        || p.login != b.secret
        || p.login_file != "auth.json"
        || b.login_file != "auth.json"
        || p.home_env != "CODEX_HOME"
        || p.observer.is_empty()
        || p.actor.is_empty()
        || record.controller_pod_uid.is_nil()
        || record.controller_sa_uid.is_nil()
        || record.prompt_sha256.len() != 64
        || !record.prompt_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !record.store_root.is_absolute()
        || record
            .store_root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(CacheError::InvalidBinding);
    }
    super::Installed::from_binding(b)?;
    Ok(())
}

// Only an actual404 on the trusted named native reader may prove absence.
// A200 Status/code404, route404, another resource or unreadable response refuses.
#[cfg(test)]
async fn bounded_absence(
    request: reqwest::RequestBuilder,
    name: &str,
    plural: &str,
) -> Result<Value, CacheError> {
    judge_absence(&observed_request(request).await.original, name, plural)
}
fn judge_absence(observed: &Value, name: &str, plural: &str) -> Result<Value, CacheError> {
    use base64::Engine;
    if observed["status"].as_u64() != Some(404) || observed["complete"] != true {
        return Err(CacheError::Unavailable);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            observed["body_base64"]
                .as_str()
                .ok_or(CacheError::Unavailable)?,
        )
        .map_err(|_| CacheError::Unavailable)?;
    let original: Value = serde_json::from_slice(&bytes).map_err(|_| CacheError::Unavailable)?;
    if original["apiVersion"] != "v1"
        || original["kind"] != "Status"
        || original["reason"] != "NotFound"
        || original["code"].as_u64() != Some(404)
        || original["details"]["name"] != name
        || original["details"]["kind"] != plural
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(original)
}

#[cfg(test)]
pub(crate) mod tests;
