//! Real launcher subprocesses, real BOSS jobs/files routers, and a
//! controlled Kubernetes HTTP adapter. No estate endpoint or env mutation.
use super::*;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use boss_jobs::JobsRepository;
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, Notify};

const FIXTURE_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
fn fixture_manifest(packet: &str, mode: &str) -> String {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/gate-runner/gate-runner.yaml"),
    )
    .unwrap()
    .replace("namespace: boss-dev", "namespace: fixture");
    render_job(&source, "fixture", packet, mode).unwrap()
}

#[derive(Clone, Serialize, Deserialize)]
struct Input {
    jobs: String,
    content: String,
    kubeconfig: String,
    cache: String,
    packet: String,
    #[serde(default)]
    create_program: Option<String>,
    #[serde(default)]
    tamper_bytes: bool,
    #[serde(default)]
    operation: String,
    #[serde(default)]
    checkout: Option<String>,
    #[serde(default)]
    manifest: Option<String>,
}

fn qualification_doors<'a>(
    client: &'a crate::machine_http::MachineClient,
    input: &Input,
) -> Doors<'a> {
    Doors {
        http: client,
        jobs_base: input.jobs.clone(),
        content_base: input.content.clone(),
        caller: Some(identity::Caller {
            id: if input.operation == "supervise" {
                identity::CONDUCTOR.into()
            } else {
                "agent-test".into()
            },
            source: identity::Source::Env,
        }),
        native_args: vec![
            format!("--kubeconfig={}", input.kubeconfig),
            format!("--cache-dir={}", input.cache),
        ],
        create_program: input.create_program.as_ref().map(PathBuf::from),
    }
}

#[test]
#[ignore = "subprocess entry; invoked only with an isolated qualification input"]
fn isolated_native_worker() {
    let path = std::env::var("BOSS_GATE_QUALIFICATION_INPUT").expect("explicit fixture input");
    let input: Input = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &input);
        let now = model::instant("2026-10-15T13:00:00Z").unwrap();
        if input.operation == "ordinary" || input.operation == "ordinary-submit" {
            crate::gate::run(
                "fixture",
                None,
                input.manifest.as_ref().map(PathBuf::from),
                "fixture",
                false,
                input.operation == "ordinary-submit",
                false,
                ParkIntent::default(),
                None,
                None,
                false,
                None,
                now,
            )
            .await
            .unwrap();
            return;
        }
        if input.operation == "conductor" {
            crate::train::qualification_launch(
                &input.jobs,
                input.manifest.as_deref().unwrap(),
                "fixture",
                &{
                    let output = std::process::Command::new("git")
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .unwrap();
                    assert!(output.status.success());
                    String::from_utf8(output.stdout).unwrap().trim().to_owned()
                },
            )
            .await
            .unwrap();
            return;
        }
        capability_on(&doors).await.unwrap();
        if input.operation == "observe" {
            let (_, receipt) = submission::original_on(&doors, &input.packet)
                .await
                .unwrap();
            eprintln!(
                "original durable acceptance {}",
                serde_json::to_string(&receipt).unwrap()
            );
            return;
        }
        if input.operation == "supervise" {
            submission::launch_on(&doors, &input.packet, now + chrono::Duration::days(1))
                .await
                .unwrap();
            return;
        }
        if existing_on(&doors, "fixture", &input.packet)
            .await
            .unwrap()
            .is_some()
        {
            return;
        }
        renew_on(&doors, &input.packet, now).await.unwrap();
        let manifest = fixture_manifest(&input.packet, "");
        let (intent, mut bytes) = prepare_on(
            &doors,
            &input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &manifest,
        )
        .await
        .unwrap();
        if input.operation == "submit" {
            submission::submit_on(&doors, &intent, &json!({}))
                .await
                .unwrap();
            return;
        }
        if input.tamper_bytes {
            bytes.push(b' ');
        }
        create_on(&doors, &intent, &bytes, now).await.unwrap();
    });
}

#[derive(Default)]
struct Native {
    submission_barrier: AtomicU8,
    policy_fault: AtomicU8,
    priority_fault: AtomicU8,
    execution: Mutex<Option<Execution>>,
    executions: Mutex<Vec<Value>>,
    jobs_requests: Mutex<Vec<String>>,
    claim_fault: AtomicU8,
    claim_commands: Mutex<Vec<Value>>,
    jobs: Mutex<std::collections::BTreeMap<String, Value>>,
    accepted: AtomicUsize,
    // 0 success; 1 reject; 2 accept but fail reply; 3 pause before
    // acceptance then reject; 4 accept then pause before reply.
    behavior: AtomicU8,
    // 0 echo the request; 1 add the exact server defaults the live API
    // server added to train 48bbc0b3's gate Job (971c9006); 2 those
    // defaults plus one admitted value that is NOT a default.
    defaults: AtomicU8,
    barrier_outcome: AtomicU8,
    reached: Notify,
    release: Notify,
    finished: Notify,
    requests: Mutex<Vec<String>>,
}
#[derive(Clone)]
struct Execution {
    origin: PathBuf,
    root: PathBuf,
}

/// Execute the shipped current run.sh checkout and gate invocation,
/// with only its mount paths relocated into this fixture. Host setup,
/// credentials, private caches and all other runner stages are excluded.
async fn execute_current_runner(execution: &Execution, job: &Value) -> Value {
    let name = job["metadata"]["name"].as_str().unwrap();
    let work = execution.root.join(name);
    std::fs::create_dir_all(&work).unwrap();
    let checkout = work.join("repo");
    let clone = tokio::process::Command::new("git")
        .args(["clone", "--quiet"])
        .arg(&execution.origin)
        .arg(&checkout)
        .output()
        .await
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/gate-runner/run.sh"),
    )
    .unwrap();
    let checkout_source = source
        .split_once("# --- gated sha (begin) ---\n")
        .unwrap()
        .1
        .split_once("# --- gated sha (end) ---")
        .unwrap()
        .0;
    let invocation = source
        .split_once("if BOSS_GATE_RECEIPT=\"$RECEIPT\" ./infra/gate.sh")
        .unwrap()
        .1;
    let invocation = format!(
        "if BOSS_GATE_RECEIPT=\"$RECEIPT\" ./infra/gate.sh{}\nfi\n",
        invocation.split_once("\nfi").unwrap().0
    )
    .replace("/gate-target", work.to_str().unwrap());
    let script = work.join("execute.sh");
    std::fs::write(
        &script,
        format!(
            "set -euo pipefail\nfail_lost() {{ echo \"$*\" >&2; exit 7; }}\n{}\nRECEIPT='{}'\n{}",
            checkout_source,
            work.join("receipt.json").display(),
            invocation
        ),
    )
    .unwrap();
    let gate = job["spec"]["template"]["spec"]["containers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|container| container["name"] == "gate")
        .unwrap();
    assert_eq!(
        gate["command"],
        json!(["bash", "/repo-script/run.sh"]),
        "this control covers the actual current launcher command"
    );
    let mut command = tokio::process::Command::new("bash");
    command.arg(&script).current_dir(&checkout);
    for key in ["GATE_BRANCH", "GATE_MODE", "GATE_SHA"] {
        let value = gate["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == key)
            .and_then(|value| value["value"].as_str())
            .unwrap_or("");
        command.env(key, value);
    }
    let output = command.output().await.unwrap();
    let receipt = std::fs::read(work.join("receipt.json")).unwrap_or_default();
    json!({"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr,
        "receipt":receipt,"job":job})
}

// A disconnected client can cancel its HTTP response future. That does not
// undo an already persisted Job, and must not strand the fixture's observer.
struct Barrier {
    native: Arc<Native>,
    released: bool,
}
impl Drop for Barrier {
    fn drop(&mut self) {
        self.native
            .barrier_outcome
            .store(if self.released { 1 } else { 2 }, Ordering::SeqCst);
        self.native.finished.notify_one();
    }
}
async fn acceptance_barrier(native: &Arc<Native>) {
    let mut guard = Barrier {
        native: native.clone(),
        released: false,
    };
    native.reached.notify_one();
    native.release.notified().await;
    guard.released = true;
}
/// What a real API server adds to the gate Job on admission, and ONLY
/// that: the sites the retained request/admitted pair from train 48bbc0b3
/// differed at (971c9006). Each is written only where the request left
/// it absent, as the server defaults it. Every image the gate manifest
/// names carries a non-latest tag, so an absent pull policy defaults to
/// IfNotPresent; the assertion keeps that premise honest.
fn admit_with_server_defaults(job: &mut Value) {
    job["metadata"]["generation"] = json!(1);
    for collection in ["initContainers", "containers"] {
        let Some(containers) = job["spec"]["template"]["spec"][collection].as_array_mut() else {
            continue;
        };
        for container in containers {
            let fields = container.as_object_mut().unwrap();
            fields
                .entry("terminationMessagePath")
                .or_insert_with(|| json!("/dev/termination-log"));
            fields
                .entry("terminationMessagePolicy")
                .or_insert_with(|| json!("File"));
            if collection == "initContainers" && !fields.contains_key("imagePullPolicy") {
                let image = fields["image"].as_str().unwrap();
                let tag = image.rsplit('/').next().unwrap().split_once(':');
                assert!(tag.is_some_and(|(_, tag)| tag != "latest"), "{image}");
                fields.insert("imagePullPolicy".into(), json!("IfNotPresent"));
            }
            for entry in fields
                .get_mut("env")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(field) = entry
                    .pointer_mut("/valueFrom/fieldRef")
                    .and_then(Value::as_object_mut)
                {
                    field.entry("apiVersion").or_insert_with(|| json!("v1"));
                }
            }
        }
    }
}

fn status(code: StatusCode, reason: &str) -> Response {
    (code,Json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":reason,"message":reason,"code":code.as_u16()}))).into_response()
}
async fn native_api(
    State(state): State<Arc<Native>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = uri.path();
    state.requests.lock().await.push(format!("{method} {uri}"));
    match path {
        "/openapi/v3"=>return Json(json!({"paths":{"apis/batch/v1":{"serverRelativeURL":"/openapi/v3/apis/batch/v1?hash=fixture"}}})).into_response(),
        "/openapi/v3/apis/batch/v1"=>return Json(json!({"openapi":"3.0.0","info":{"title":"isolated native qualification","version":"v1"},"paths":{"/apis/batch/v1/namespaces/{namespace}/jobs":{"patch":{"x-kubernetes-group-version-kind":{"group":"batch","version":"v1","kind":"Job"},"parameters":[{"name":"fieldValidation","in":"query","schema":{"type":"string"}}],"responses":{"201":{"description":"created"}}}}},"components":{"schemas":{}}})).into_response(),
        "/openapi/v2"=>return Json(json!({"swagger":"2.0","info":{"title":"isolated native qualification","version":"v1"},"paths":{},"definitions":{"io.k8s.api.batch.v1.Job":{"type":"object","x-kubernetes-group-version-kind":[{"group":"batch","version":"v1","kind":"Job"}],"properties":{"apiVersion":{"type":"string"},"kind":{"type":"string"},"metadata":{"type":"object","additionalProperties":true},"spec":{"type":"object","additionalProperties":true}}}}})).into_response(),
        "/api/v1/namespaces/fixture"=>return Json(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"fixture"}})).into_response(),
        "/api"=>return Json(json!({"apiVersion":"v1","kind":"APIVersions","versions":["v1"]})).into_response(),
        "/api/v1"=>return Json(json!({"apiVersion":"v1","kind":"APIResourceList","groupVersion":"v1","resources":[]})).into_response(),
        "/apis"=>return Json(json!({"apiVersion":"v1","kind":"APIGroupList","groups":[{"name":"batch","versions":[{"groupVersion":"batch/v1","version":"v1"}],"preferredVersion":{"groupVersion":"batch/v1","version":"v1"}}]})).into_response(),
        "/apis/batch/v1"=>return Json(json!({"apiVersion":"v1","kind":"APIResourceList","groupVersion":"batch/v1","resources":[{"name":"jobs","singularName":"job","namespaced":true,"kind":"Job","verbs":["create","get","list"]}]})).into_response(),
        _=>{}
    }
    let base = "/apis/batch/v1/namespaces/fixture/jobs";
    if path == base && method == Method::GET {
        let url = reqwest::Url::parse(&format!("http://fixture.invalid{uri}")).unwrap();
        let selector = url
            .query_pairs()
            .find(|(key, _)| key == "labelSelector")
            .map(|(_, value)| value.into_owned());
        let terms = selector
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|term| !term.is_empty())
            .map(|term| term.split_once('='))
            .collect::<Option<Vec<_>>>();
        let Some(terms) = terms else {
            return status(StatusCode::BAD_REQUEST, "UnsupportedFixtureSelector");
        };
        let items = state
            .jobs
            .lock()
            .await
            .values()
            .filter(|job| {
                terms.iter().all(|(key, value)| {
                    job.pointer("/metadata/labels")
                        .and_then(|labels| labels.get(*key))
                        .and_then(Value::as_str)
                        == Some(*value)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        return Json(json!({"apiVersion":"batch/v1","kind":"JobList","metadata":{"resourceVersion":"1"},"items":items})).into_response();
    }
    if path == base && method == Method::POST {
        if !uri.query().is_some_and(|query| {
            query
                .split('&')
                .any(|field| field == "fieldValidation=Strict")
        }) {
            return status(StatusCode::BAD_REQUEST, "StrictValidationRequired");
        }
        let mut job: Value = match serde_json::from_slice(&body) {
            Ok(job) => job,
            Err(_) => return status(StatusCode::BAD_REQUEST, "Invalid"),
        };
        let behavior = state.behavior.load(Ordering::SeqCst);
        if behavior == 3 {
            acceptance_barrier(&state).await;
            return status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "RejectedBeforeAcceptance",
            );
        }
        if behavior == 1 {
            return status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "RejectedBeforeAcceptance",
            );
        }
        let name = match job.pointer("/metadata/name").and_then(Value::as_str) {
            Some(name) => name.to_owned(),
            None => match job
                .pointer("/metadata/generateName")
                .and_then(Value::as_str)
            {
                Some(prefix) => format!("{prefix}{}", &Uuid::new_v4().simple().to_string()[..5]),
                None => return status(StatusCode::BAD_REQUEST, "MissingName"),
            },
        };
        job["metadata"]["name"] = json!(name);
        {
            let mut jobs = state.jobs.lock().await;
            if jobs.contains_key(&name) {
                return status(StatusCode::CONFLICT, "AlreadyExists");
            }
            job["metadata"]["uid"] = json!(Uuid::new_v4());
            job["metadata"]["creationTimestamp"] = json!("2026-10-15T13:00:01Z");
            job["metadata"]["resourceVersion"] = json!("1");
            if behavior == 5 {
                job["spec"]["template"]["spec"]["initContainers"] =
                    json!([{"name":"unexpected","image":"fixture.invalid/injected"}]);
            }
            match state.defaults.load(Ordering::SeqCst) {
                0 => {}
                changed => {
                    admit_with_server_defaults(&mut job);
                    if changed == 2 {
                        job["spec"]["template"]["spec"]["initContainers"][0]["terminationMessagePolicy"] =
                            json!("FallbackToLogsOnError");
                    }
                }
            }
            jobs.insert(name, job.clone());
            state.accepted.fetch_add(1, Ordering::SeqCst);
        }
        if behavior == 4 {
            acceptance_barrier(&state).await;
        }
        let execution = { state.execution.lock().await.clone() };
        if let Some(execution) = execution {
            let record = execute_current_runner(&execution, &job).await;
            state.executions.lock().await.push(record);
        }
        if behavior == 2 {
            return status(StatusCode::SERVICE_UNAVAILABLE, "ReplyLostAfterAcceptance");
        }
        return (StatusCode::CREATED, Json(job)).into_response();
    }
    if let Some(name) = path.strip_prefix(&format!("{base}/")) {
        return match state.jobs.lock().await.get(name) {
            Some(job) => Json(job.clone()).into_response(),
            None => status(StatusCode::NOT_FOUND, "NotFound"),
        };
    }
    status(StatusCode::NOT_FOUND, "UnknownFixtureRoute")
}

struct Fixture {
    input: Input,
    repo: Arc<boss_jobs::InMemoryJobs>,
    native: Arc<Native>,
    _files: tempfile::TempDir,
    servers: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (
        url,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn claim_reply_fault(
    State(native): State<Arc<Native>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    native
        .jobs_requests
        .lock()
        .await
        .push(format!("{} {}", request.method(), request.uri()));
    if native.priority_fault.load(Ordering::SeqCst) == 1
        && request.method() == Method::GET
        && request.uri().path() == "/api/jobs"
        && request
            .uri()
            .query()
            .is_some_and(|q| q.contains("kind=pr-train"))
    {
        return status(
            StatusCode::SERVICE_UNAVAILABLE,
            "agents registry could not answer; retry, role is unknown",
        );
    }
    if request.method() == Method::GET && request.uri().path().starts_with("/api/delivery/policy/")
    {
        let fault = native.policy_fault.load(Ordering::SeqCst);
        if fault == 3 {
            return status(StatusCode::SERVICE_UNAVAILABLE, "PolicyReadUnknown");
        }
        if fault == 5 {
            return status(StatusCode::NOT_FOUND, "PolicyReadAbsent");
        }
        let pinned = request.uri().path().contains("/versions/");
        let response = next.run(request).await;
        if !response.status().is_success() || fault == 0 {
            return response;
        }
        let (parts, body) = response.into_parts();
        let mut value: Value =
            serde_json::from_slice(&axum::body::to_bytes(body, 1024 * 1024).await.unwrap())
                .unwrap();
        match fault {
            1 => {
                value.as_object_mut().unwrap().remove("gate_queue_owner");
            }
            2 if pinned => value["gate_max_concurrent"] = json!(99),
            4 => value["gate_queue_owner"] = json!("unrecognized"),
            6 => value["gate_max_concurrent"] = json!(0),
            7 => value["gate_queue_owner"] = json!("resident"),
            _ => {}
        }
        return axum::response::Response::from_parts(
            parts,
            axum::body::Body::from(serde_json::to_vec(&value).unwrap()),
        );
    }
    if request.method() != Method::POST || !request.uri().path().ends_with("/metadata/conditional")
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
    let command: Value = serde_json::from_slice(&bytes).unwrap();
    let submitting = command.pointer("/patch/gate_submission").is_some();
    let sending = command
        .pointer("/patch/gate_launch_attempt/state")
        .and_then(Value::as_str)
        == Some("sending");
    let fault = if sending {
        native.claim_commands.lock().await.push(command);
        native.claim_fault.swap(0, Ordering::SeqCst)
    } else {
        0
    };
    if fault == 2 {
        return status(
            StatusCode::SERVICE_UNAVAILABLE,
            "BeforeCommitReplyUnavailable",
        );
    }
    let response = next
        .run(axum::extract::Request::from_parts(
            parts,
            axum::body::Body::from(bytes),
        ))
        .await;
    if submitting
        && response.status().is_success()
        && native.submission_barrier.swap(0, Ordering::SeqCst) == 1
    {
        acceptance_barrier(&native).await;
    }
    if fault == 1 && response.status().is_success() {
        return status(StatusCode::SERVICE_UNAVAILABLE, "CommittedReplyUnavailable");
    }
    response
}
impl Fixture {
    async fn new() -> Self {
        Self::configured(None, false).await
    }
    async fn configured(schema: Option<Value>, for_cli: bool) -> Self {
        use boss_jobs::registry::InMemoryWorkflows;
        // Hold both declared service ports on a unique loopback address.
        // This exercises live door address derivation without a shared
        // fixed-port race, real estate endpoint, token or global env edit.
        let listeners = if for_cli {
            let mut reserved = None;
            for _ in 0..100 {
                let id = Uuid::new_v4();
                let bytes = id.as_bytes();
                let ip = std::net::Ipv4Addr::new(
                    127,
                    bytes[0] % 250 + 1,
                    bytes[1] % 250 + 1,
                    bytes[2] % 250 + 1,
                );
                if let Ok(jobs) =
                    tokio::net::TcpListener::bind((ip, boss_ports::prod("jobs") as u16)).await
                    && let Ok(content) =
                        tokio::net::TcpListener::bind((ip, boss_ports::prod("content") as u16))
                            .await
                {
                    reserved = Some((jobs, content));
                    break;
                }
            }
            Some(reserved.expect("bounded unique loopback service sockets"))
        } else {
            None
        };
        let native = Arc::new(Native::default());
        let (kube, kube_server) = serve(
            Router::new()
                .fallback(native_api)
                .with_state(native.clone()),
        )
        .await;
        let packet = Uuid::new_v4();
        let mut job = boss_core::job::Job::new(
            "gate-run",
            boss_core::job::Subject::new("custom", "fixture"),
            "Fixture gate",
            "agent-test",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        );
        job.id = boss_core::job::JobId::from_uuid(packet);
        job.status = boss_core::job::JobStatus::Open;
        job.metadata =
            json!({"branch":"fixture","sha":FIXTURE_HEAD,"worktree":"fixture-original-checkout"});
        let registry = Arc::new(InMemoryWorkflows::for_fixture());
        let mut protocol =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .unwrap()
                .into_iter()
                .find(|row| row.kind == "gate-run")
                .unwrap();
        if let Some(schema) = schema {
            protocol.metadata_schema = schema;
        }
        registry.seed(protocol).unwrap();
        for protocol in
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .unwrap()
                .into_iter()
                .filter(|row| matches!(row.kind.as_str(), "agent-run" | "backlog-item"))
        {
            registry.seed(protocol).unwrap();
        }

        // Model an already admitted row before the current registry read. This
        // permits corrupt/read-skew protocol controls without asking admission
        // to accept a malformed declaration (which it correctly refuses).
        let repo = boss_jobs::InMemoryJobs::new();
        repo.create_job(&job).await.unwrap();
        let repo = Arc::new(repo.with_workflows(registry.clone()));
        let bus = boss_testing::RecordingEventBus::new();
        let mut state = boss_jobs::http::JobsApiState::minimal(
            repo.clone(),
            bus.clone(),
            boss_core::publisher::DomainPublisher::new(bus, "jobs"),
            Arc::new(boss_policy_client::PermissivePolicyClient),
            Arc::new(boss_clock_client::WallClockClient),
        );
        state.kind_registry = Some(registry);
        let policy = boss_jobs::seed_loader::load_delivery_policies(
            boss_jobs::delivery_policy_seed::platform_delivery_policy_path(),
        )
        .unwrap()
        .into_iter()
        .find(|row| row.row.name == "train-conductor")
        .unwrap();
        let delivery = Arc::new(boss_jobs::delivery::InMemoryDeliveryPolicy::new(vec![
            boss_jobs::delivery::StoredPolicy {
                row: policy.row,
                status: "active".into(),
            },
        ]));
        state.delivery = Some(delivery.clone());
        let jobs_app = boss_jobs::http::router(state)
            .merge(boss_jobs::delivery::http::router(
                boss_jobs::delivery::http::DeliveryPolicyApiState { repo: delivery },
            ))
            .layer(axum::middleware::from_fn_with_state(
                native.clone(),
                claim_reply_fault,
            ));
        let (jobs, jobs_server, content_listener) = if let Some((jobs, content)) = listeners {
            let base = format!("http://{}", jobs.local_addr().unwrap());
            (
                base,
                tokio::spawn(async move { axum::serve(jobs, jobs_app).await.unwrap() }),
                Some(content),
            )
        } else {
            let (base, server) = serve(jobs_app).await;
            (base, server, None)
        };
        let content_app =
            boss_content::files::http::router(boss_content::files::http::FilesApiState {
                repo: Arc::new(boss_content::files::InMemoryFileRepository::new()),
                storage: Arc::new(boss_content::files::InMemoryFileStorage::new()),
                publisher: None,
                policy: Arc::new(boss_policy_client::PermissivePolicyClient),
                bucket: "fixture".into(),
                pool: None,
                clock: Arc::new(boss_clock_client::WallClockClient),
            });
        let (content, files_server) = if let Some(listener) = content_listener {
            let base = format!("http://{}", listener.local_addr().unwrap());
            (
                base,
                tokio::spawn(async move { axum::serve(listener, content_app).await.unwrap() }),
            )
        } else {
            serve(content_app).await
        };
        let files = tempfile::tempdir().unwrap();
        let kubeconfig = files.path().join("kubeconfig.json");
        std::fs::write(&kubeconfig,json!({"apiVersion":"v1","kind":"Config","current-context":"fixture","clusters":[{"name":"fixture","cluster":{"server":kube}}],"contexts":[{"name":"fixture","context":{"cluster":"fixture","user":"fixture","namespace":"fixture"}}],"users":[{"name":"fixture","user":{}}]}).to_string()).unwrap();
        Self {
            input: Input {
                jobs,
                content,
                kubeconfig: kubeconfig.display().to_string(),
                cache: files.path().join("cache").display().to_string(),
                packet: packet.to_string(),
                create_program: None,
                tamper_bytes: false,
                operation: String::new(),
                checkout: None,
                manifest: None,
            },
            repo,
            native,
            _files: files,
            servers: vec![kube_server, jobs_server, files_server],
        }
    }
    fn ordinary_input(&self, operation: &str) -> Input {
        let checkout = self._files.path().join("ordinary-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&checkout)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--initial-branch=main"]);
        git(&["config", "user.name", "Controlled fixture"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "Original fixture source",
        ]);
        git(&["checkout", "-b", "fixture"]);
        git(&["commit", "--allow-empty", "-m", "Unlanded fixture source"]);
        git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
        git(&["fetch", "origin"]);
        let manifest = checkout.join("gate-runner.yaml");
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../infra/gate-runner/gate-runner.yaml"),
        )
        .unwrap()
        .replace("namespace: boss-dev", "namespace: fixture");
        std::fs::write(&manifest, source).unwrap();
        let mut input = self.input.clone();
        input.operation = operation.into();
        input.checkout = Some(checkout.display().to_string());
        input.manifest = Some(manifest.display().to_string());
        input
    }
    fn spawn(&self, label: &str) -> tokio::process::Child {
        self.spawn_input(label, &self.input)
    }
    fn spawn_program(&self, label: &str, program: String) -> tokio::process::Child {
        let mut input = self.input.clone();
        input.create_program = Some(program);
        self.spawn_input(label, &input)
    }
    fn spawn_input(&self, label: &str, configuration: &Input) -> tokio::process::Child {
        use std::os::unix::process::CommandExt;
        let input = self._files.path().join(format!("{label}-input.json"));
        std::fs::write(&input, serde_json::to_vec(configuration).unwrap()).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "gate::reservation::native_tests::isolated_native_worker",
                "--ignored",
                "--nocapture",
            ])
            .env("BOSS_GATE_QUALIFICATION_INPUT", input)
            .stdout(
                std::fs::File::create(self._files.path().join(format!("{label}.stdout"))).unwrap(),
            )
            .stderr(
                std::fs::File::create(self._files.path().join(format!("{label}.stderr"))).unwrap(),
            )
            .process_group(0);
        if let Some(checkout) = &configuration.checkout {
            let host = reqwest::Url::parse(&configuration.jobs)
                .unwrap()
                .host_str()
                .unwrap()
                .to_owned();
            let no_proxy = format!("localhost,127.0.0.1,{host}");
            command
                .env("NO_PROXY", &no_proxy)
                .env("no_proxy", &no_proxy);
            command
                .current_dir(checkout)
                .env("BOSS_JOBS_URL", &configuration.jobs)
                .env("BOSS_ACTOR", "agent-test")
                .env_remove("BOSS_AGENT_RUN")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("KUBECONFIG", &configuration.kubeconfig);
            let tools = self._files.path().join("cli-tools");
            std::fs::create_dir_all(&tools).unwrap();
            let real = std::process::Command::new("sh")
                .args(["-c", "command -v kubectl"])
                .output()
                .unwrap();
            assert!(real.status.success());
            let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
            let script = format!(
                "#!/bin/sh\nexec {} --kubeconfig={} --cache-dir={} \"$@\"\n",
                quote(String::from_utf8(real.stdout).unwrap().trim()),
                quote(&configuration.kubeconfig),
                quote(&configuration.cache)
            );
            let wrapper = tools.join("kubectl");
            boss_testing::write_exec(&wrapper, &script);
            command.env(
                "PATH",
                format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
            );
        }
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }
    async fn finish(&self, mut child: tokio::process::Child, label: &str) -> bool {
        let status = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
            .await
            .expect("bounded isolated worker")
            .unwrap();
        self.record_child(label, &status.to_string()).await;
        status.success()
    }
    async fn record_child(&self, label: &str, outcome: &str) {
        eprintln!(
            "child original {}",
            json!({"label":label,"outcome":outcome,
            "stdout":std::fs::read(self._files.path().join(format!("{label}.stdout"))).unwrap(),
            "stderr":std::fs::read(self._files.path().join(format!("{label}.stderr"))).unwrap(),
            "requests":*self.native.requests.lock().await,"metadata":self.metadata().await,
            "native":*self.native.jobs.lock().await})
        );
        self.record_artifacts(label).await;
    }
    async fn record_artifacts(&self, label: &str) {
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let rows: Vec<Value> = client
            .get(format!(
                "{}/api/files?target_kind=job&target_id={}",
                self.input.content, self.input.packet
            ))
            .header("x-boss-user", identity::header("agent-test"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        for row in rows {
            let id = row["id"].as_str().unwrap();
            let bytes = client
                .get(format!("{}/api/files/{id}", self.input.content))
                .header("x-boss-user", identity::header("agent-test"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            eprintln!(
                "{label} complete artifact {id}: {}",
                String::from_utf8_lossy(&bytes)
            );
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                for field in ["stdout", "stderr"] {
                    if let Some(values) = value.get(field).and_then(Value::as_array) {
                        let bytes: Vec<u8> = values
                            .iter()
                            .map(|value| u8::try_from(value.as_u64().unwrap()).unwrap())
                            .collect();
                        eprintln!(
                            "{label} decoded {id} {field}: {}",
                            String::from_utf8_lossy(&bytes)
                        );
                    }
                }
            }
        }
    }
    async fn source_claim(&self, fault: &str) -> (String, String) {
        use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let mut parent = Job::new(
            "backlog-item",
            Subject::new("custom", "fixture"),
            "Parent",
            "agent-test",
            Priority::Standard,
            date,
        );
        parent.status = if fault == "parent-closed" {
            JobStatus::Closed
        } else {
            JobStatus::Open
        };
        let mut run = Job::new(
            "agent-run",
            Subject::new("custom", "fixture"),
            "Source",
            "agent-test",
            Priority::Standard,
            date,
        );
        run.status = if fault == "run-closed" {
            JobStatus::Closed
        } else {
            JobStatus::Open
        };
        run.metadata =
            json!({"packet":parent.id,"step":"build","agent":"login-alias@example.invalid"});
        self.repo.create_job(&parent).await.unwrap();
        self.repo.create_job(&run).await.unwrap();
        let mut building = Step::new(run.id, "task", "Building", 0);
        building.spec_slug = Some("building".into());
        building.status = StepStatus::Active;
        building.assignee_id = Some("agent-test".into());
        self.repo.add_step(&building).await.unwrap();
        let mut step = Step::new(parent.id, "task", "Build", 0);
        step.spec_slug = Some("build".into());
        step.status = if fault == "step-moved" {
            StepStatus::Completed
        } else {
            StepStatus::Active
        };
        step.assignee_id = Some(
            if fault == "holder-moved" {
                "another-agent"
            } else {
                "agent-test"
            }
            .into(),
        );
        step.metadata = json!({"agent_run":if fault=="backlink-moved" {Uuid::new_v4().to_string()}else{run.id.to_string()}});
        self.repo.add_step(&step).await.unwrap();
        if fault == "wrong-claim-receipt" {
            let mut wrong = step.clone();
            wrong.id = boss_core::job::StepId::new();
            run.metadata["claim_receipt"] = serde_json::to_value(wrong).unwrap();
            self.repo.update_job(&run).await.unwrap();
        }
        (run.id.to_string(), parent.id.to_string())
    }
    async fn metadata(&self) -> Value {
        self.repo
            .get_job(&boss_core::job::JobId::from_uuid(
                Uuid::parse_str(&self.input.packet).unwrap(),
            ))
            .await
            .unwrap()
            .unwrap()
            .metadata
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_real_launchers_accept_one_native_job_and_resume_its_exact_identity() {
    let fixture = Fixture::new().await;
    let a = fixture.spawn("one");
    let b = fixture.spawn("two");
    let (a, b) = tokio::join!(fixture.finish(a, "one"), fixture.finish(b, "two"));
    assert!(a || b, "at least one guarded launcher must finish");
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert!(fixture.finish(fixture.spawn("resume"), "resume").await);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert!(
        model::read(&fixture.metadata().await)
            .unwrap()
            .unwrap()
            .consumed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prepared_full_request_cannot_attach_to_a_competing_auto_request() {
    let fixture = Fixture::new().await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-15T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (full, full_bytes) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let (auto, auto_bytes) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "--auto",
        &manifest,
    )
    .await
    .unwrap();
    create_on(&doors, &auto, &auto_bytes, now).await.unwrap();
    let result = create_on(&doors, &full, &full_bytes, now).await;
    eprintln!(
        "competing prepared requests {}",
        json!({"full":full,"auto":auto,"result":format!("{result:?}"),"metadata":fixture.metadata().await,"native":*fixture.native.jobs.lock().await})
    );
    fixture.record_artifacts("competing-modes").await;
    assert!(
        result.is_err(),
        "FULL cannot be satisfied by a different immutable AUTO request"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_packet_census_conserves_complete_json_before_any_send() {
    let fixture = Fixture::new().await;
    assert!(fixture.finish(fixture.spawn("census"), "census").await);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let rows: Vec<Value> = client
        .get(format!(
            "{}/api/files?target_kind=job&target_id={}",
            fixture.input.content, fixture.input.packet
        ))
        .header("x-boss-user", identity::header("agent-test"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut originals = 0;
    for row in rows {
        let bytes = client
            .get(format!(
                "{}/api/files/{}",
                fixture.input.content,
                row["id"].as_str().unwrap()
            ))
            .header("x-boss-user", identity::header("agent-test"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if record["operation"] != "native-packet-census" {
            continue;
        }
        let bytes: Vec<u8> = serde_json::from_value(record["stdout"].clone()).unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(list["kind"], "JobList");
        assert!(list["items"].is_array());
        assert!(record["stderr"].is_array());
        originals += 1;
    }
    assert!(
        originals > 0,
        "table-derived names cannot replace the only complete native census evidence"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_durable_submitter_cannot_bypass_its_declared_resident_owner() {
    let fixture = Fixture::new().await;
    let mut input = fixture.input.clone();
    input.operation = "submit".into();
    assert!(
        fixture
            .finish(
                fixture.spawn_input("resident-only-submit", &input),
                "resident-only-submit"
            )
            .await
    );
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let (mut submission, _) = submission::original_on(&doors, &fixture.input.packet)
        .await
        .unwrap();
    submission.intent.transition_generation =
        Some(reservation_generation(&fixture.metadata().await).unwrap());
    let bytes = request_bytes(&doors, &submission.intent).await.unwrap();
    let result = create_on(
        &doors,
        &submission.intent,
        &bytes,
        model::instant("2026-10-15T13:00:00Z").unwrap(),
    )
    .await;
    fixture.record_artifacts("resident-only-bypass").await;
    eprintln!("resident owner bypass result {result:?}");
    assert!(
        result.is_err(),
        "a direct native wrapper must not bypass the immutable supervisor owner"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_pass_launches_an_accepted_packet_after_its_submitter_has_left() {
    let fixture = Fixture::new().await;
    let checkout = fixture._files.path().join("conductor-clone");
    std::fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&checkout)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "Fixture source",
    ]);
    // A real local git remote proves the same exact-head query as the
    // conductor, with no credentials, provider, or global environment.
    git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
    let head = git(&["rev-parse", "HEAD"]);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    api(
        &doors,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{}/metadata", fixture.input.packet),
        Some(json!({"sha":head})),
    )
    .await
    .unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &head,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let receipt = submission::submit_on(
        &doors,
        &intent,
        &json!({"branch":"fixture","sha":head,"worktree":"original-builder-checkout"}),
    )
    .await
    .unwrap();
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let resident = qualification_doors(&client, &input);
    let report = supervisor::pass_on(&resident, &checkout, "fixture", 1, now)
        .await
        .unwrap();
    fixture.record_artifacts("resident-pass").await;
    assert_eq!(report.launched, vec![fixture.input.packet.clone()]);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    let (_, original) = submission::original_on(&resident, &fixture.input.packet)
        .await
        .unwrap();
    assert_eq!(
        original, receipt,
        "a cadence pass cannot reset accepted order or replace the original request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_pass_conserves_pages_and_uses_original_acceptance_order() {
    let fixture = Fixture::new().await;
    let checkout = fixture._files.path().join("ordered-resident-clone");
    std::fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&checkout)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "Fixture",
    ]);
    git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
    let head = git(&["rev-parse", "HEAD"]);
    let original = fixture
        .repo
        .get_job(&boss_core::job::JobId::from_uuid(
            Uuid::parse_str(&fixture.input.packet).unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    let mut packets = vec![fixture.input.packet.clone()];
    // More than one API page, including ordinary unsubmitted gates, must
    // not hide either accepted packet or turn a partial census into capacity.
    for index in 0..51 {
        let mut row = original.clone();
        row.id = boss_core::job::JobId::from_uuid(Uuid::new_v4());
        row.metadata =
            json!({"branch":"fixture","sha":head,"worktree":"fixture-original-checkout"});
        fixture.repo.create_job(&row).await.unwrap();
        if index == 50 {
            packets.push(row.id.to_string());
        }
    }
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    let mut receipts = Vec::new();
    for (index, packet) in packets.iter().enumerate() {
        renew_on(&doors, packet, now).await.unwrap();
        api(
            &doors,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(json!({"sha":head})),
        )
        .await
        .unwrap();
        let (intent, _) = prepare_on(
            &doors,
            packet,
            "fixture",
            &head,
            "full",
            &fixture_manifest(packet, ""),
        )
        .await
        .unwrap();
        let receipt = submission::submit_on(
            &doors,
            &intent,
            &json!({"branch":"fixture","sha":head,"worktree":"fixture-original-checkout"}),
        )
        .await
        .unwrap();
        receipts.push(receipt);
        // Deliberately reverse the mutable client queue stamps.
        api(&doors, reqwest::Method::PATCH, &format!("/api/jobs/{packet}/metadata"),
            Some(json!({"gate_queued_at":if index == 0 {"2099-01-01T00:00:00Z"} else {"2000-01-01T00:00:00Z"}}))).await.unwrap();
    }
    assert!(receipts[0].applied_at < receipts[1].applied_at);
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let resident = qualification_doors(&client, &input);
    let report = supervisor::pass_on(&resident, &checkout, "fixture", 1, now)
        .await
        .unwrap();
    fixture
        .record_artifacts("resident-original-order-pages")
        .await;
    assert_eq!(report.launched, vec![packets[0].clone()]);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    for (packet, receipt) in packets.iter().zip(receipts) {
        assert_eq!(
            submission::original_on(&resident, packet).await.unwrap().1,
            receipt
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_pod_counter_without_terminal_job_condition_still_owns_a_bay() {
    let fixture = Fixture::new().await;
    let checkout = fixture._files.path().join("conductor-clone");
    std::fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&checkout)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "Fixture source",
    ]);
    // A real local git remote proves the same exact-head query as the
    // conductor, with no credentials, provider, or global environment.
    git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
    let head = git(&["rev-parse", "HEAD"]);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    api(
        &doors,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{}/metadata", fixture.input.packet),
        Some(json!({"sha":head})),
    )
    .await
    .unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &head,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let receipt = submission::submit_on(
        &doors,
        &intent,
        &json!({"branch":"fixture","sha":head,"worktree":"original-builder-checkout"}),
    )
    .await
    .unwrap();
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let resident = qualification_doors(&client, &input);
    fixture.native.jobs.lock().await.insert(
        "still-retrying".into(),
        json!({"apiVersion":"batch/v1","kind":"Job",
        "metadata":{"name":"still-retrying","namespace":"fixture","uid":"other-native-uid",
            "labels":{"app":"gate-runner","boss.dev/packet":Uuid::new_v4().to_string()}},
        "spec":{},"status":{"failed":1}}),
    );
    let report = supervisor::pass_on(&resident, &checkout, "fixture", 1, now)
        .await
        .unwrap();
    fixture.record_artifacts("resident-pass").await;
    assert!(
        report.launched.is_empty(),
        "a failed pod counter is not a terminal Job verdict: {report:?}"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    let (_, original) = submission::original_on(&resident, &fixture.input.packet)
        .await
        .unwrap();
    assert_eq!(
        original, receipt,
        "a cadence pass cannot reset accepted order or replace the original request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_native_request_executes_its_full_mode_and_head_after_branch_advance() {
    let fixture = Fixture::new().await;
    let origin = fixture._files.path().join("execution-origin");
    std::fs::create_dir_all(origin.join("infra")).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&origin)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    let write_gate = |marker: &str| {
        let path = origin.join("infra/gate.sh");
        boss_testing::write_exec(
            &path,
            &format!(
                r#"#!/usr/bin/env bash
printf '{{"head":"%s","marker":"{marker}","args":"%s"}}\n' "$(git rev-parse HEAD)" "$*" >"$BOSS_GATE_RECEIPT"
"#
            ),
        );
    };
    write_gate("accepted");
    git(&["add", "infra/gate.sh"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "Accepted tree",
    ]);
    let head = git(&["rev-parse", "HEAD"]);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    api(
        &doors,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{}/metadata", fixture.input.packet),
        Some(json!({"sha":head})),
    )
    .await
    .unwrap();
    let template = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/gate-runner/gate-runner.yaml"),
    )
    .unwrap()
    .replace("namespace: boss-dev", "namespace: fixture");
    // The request says FULL even if a caller accidentally supplied the
    // conductor's old AUTO rendering: the frozen execution contract wins.
    let rendered = render_job(&template, "fixture", &fixture.input.packet, "--auto").unwrap();
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &head,
        "",
        &rendered,
    )
    .await
    .unwrap();
    submission::submit_on(
        &doors,
        &intent,
        &json!({"branch":"fixture","sha":head,"worktree":"original-checkout"}),
    )
    .await
    .unwrap();
    *fixture.native.execution.lock().await = Some(Execution {
        origin: origin.clone(),
        root: fixture._files.path().join("executions"),
    });
    fixture.native.behavior.store(4, Ordering::SeqCst);
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let child = fixture.spawn_input("execution-bound", &input);
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.native.reached.notified(),
    )
    .await
    .unwrap();
    write_gate("moved");
    git(&["add", "infra/gate.sh"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "Moved branch after native acceptance",
    ]);
    let moved = git(&["rev-parse", "HEAD"]);
    assert_ne!(moved, head);
    fixture.native.release.notify_waiters();
    assert!(fixture.finish(child, "execution-bound").await);
    let records = fixture.native.executions.lock().await.clone();
    eprintln!(
        "complete native current-run.sh executions {}",
        json!(records)
    );
    assert_eq!(records.len(), 1);
    let bytes: Vec<u8> = serde_json::from_value(records[0]["receipt"].clone()).unwrap();
    let receipt: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(records[0]["status"], 0);
    assert_eq!(
        receipt,
        json!({"head":head,"marker":"accepted","args":""}),
        "the actual current runner must execute the accepted tree in FULL mode after the branch moves"
    );
    assert_eq!(
        intent.mode, "full",
        "the immutable request names its execution mode explicitly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_submission_preserves_one_source_despite_audited_compatibility_drift() {
    let fixture = Fixture::new().await;
    let mut input = fixture.input.clone();
    input.operation = "submit".into();
    assert!(
        fixture
            .finish(fixture.spawn_input("seal-source", &input), "seal-source")
            .await
    );
    let before = fixture.metadata().await;
    for patch in [
        json!({"agent_run":Uuid::new_v4()}),
        json!({"hold":"different hold"}),
        json!({"park_summary":"different receipt"}),
        json!({"park_partial_item":Uuid::new_v4()}),
    ] {
        let response = reqwest::Client::new()
            .patch(format!("{}/api/jobs/{}/metadata", input.jobs, input.packet))
            .header("x-boss-user", identity::header("agent-test"))
            .json(&patch)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        eprintln!("post-acceptance source mutation {patch}: {status} {body}");
        assert_eq!(status, reqwest::StatusCode::NO_CONTENT);
        assert_eq!(
            fixture.metadata().await[model::SUBMISSION],
            before[model::SUBMISSION]
        );
    }
    input.operation = "supervise".into();
    assert!(
        !fixture
            .finish(
                fixture.spawn_input("changed-index", &input),
                "changed-index"
            )
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_submission_survives_its_observer_without_a_native_send() {
    let fixture = Fixture::new().await;
    let mut input = fixture.input.clone();
    input.operation = "submit".into();
    assert!(
        fixture
            .finish(fixture.spawn_input("submit-only", &input), "submit-only")
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    let submitted = fixture.metadata().await;
    assert_eq!(submitted["gate_submission"]["intent"]["mode"], "full");
    assert!(submitted.get(attempt::KEY).is_none());
    input.operation = "supervise".into();
    assert!(
        fixture
            .finish(
                fixture.spawn_input("resident-rejoin", &input),
                "resident-rejoin"
            )
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.metadata().await["gate_submission"],
        submitted["gate_submission"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_submission_observer_can_only_read_while_resident_launches_later() {
    use std::os::unix::process::ExitStatusExt;
    let fixture = Fixture::new().await;
    fixture.native.submission_barrier.store(1, Ordering::SeqCst);
    let mut input = fixture.input.clone();
    input.operation = "submit".into();
    let mut child = fixture.spawn_input("lost-submission-reply", &input);
    let pid = child.id().unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.native.reached.notified(),
    )
    .await
    .unwrap();
    let accepted = fixture.metadata().await;
    assert!(accepted.get("gate_submission").is_some());
    assert!(accepted.get(attempt::KEY).is_none());
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    assert!(
        tokio::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status()
            .await
            .unwrap()
            .success()
    );
    let exit = child.wait().await.unwrap();
    assert_eq!(exit.signal(), Some(9));
    fixture
        .record_child(
            "lost-submission-reply",
            &format!("{exit}; signal={:?}", exit.signal()),
        )
        .await;
    fixture.native.release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.native.finished.notified(),
    )
    .await
    .unwrap();
    let before = fixture.native.jobs_requests.lock().await.len();
    input.operation = "observe".into();
    assert!(
        fixture
            .finish(
                fixture.spawn_input("reader-rejoin", &input),
                "reader-rejoin"
            )
            .await
    );
    assert!(
        fixture.native.jobs_requests.lock().await[before..]
            .iter()
            .all(|request| request.starts_with("GET "))
    );
    assert_eq!(fixture.metadata().await, accepted);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    input.operation = "supervise".into();
    assert!(
        fixture
            .finish(
                fixture.spawn_input("later-resident", &input),
                "later-resident"
            )
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.metadata().await["gate_submission"],
        accepted["gate_submission"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uninterrupted_claim_retries_identical_command_before_any_send() {
    for fault in [1, 2] {
        let fixture = Fixture::new().await;
        fixture.native.claim_fault.store(fault, Ordering::SeqCst);
        assert!(
            fixture
                .finish(fixture.spawn("claim-reply"), "claim-reply")
                .await,
            "fault {fault}"
        );
        let commands = fixture.native.claim_commands.lock().await;
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], commands[1]);
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unexpected_native_authority_is_never_consumed() {
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(5, Ordering::SeqCst);
    assert!(
        !fixture
            .finish(fixture.spawn("injected-init"), "injected-init")
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert!(
        !model::read(&fixture.metadata().await)
            .unwrap()
            .unwrap()
            .consumed
    );
    assert!(fixture.metadata().await.get(model::OBSERVATION).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_request_bytes_never_claim_or_send() {
    let fixture = Fixture::new().await;
    let mut input = fixture.input.clone();
    input.tamper_bytes = true;
    assert!(
        !fixture
            .finish(
                fixture.spawn_input("changed-bytes", &input),
                "changed-bytes"
            )
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    assert!(fixture.metadata().await.get(attempt::KEY).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proved_os_spawn_refusal_allows_one_same_intent_retry() {
    let fixture = Fixture::new().await;
    let absent = fixture
        ._files
        .path()
        .join("absent-native-executable")
        .display()
        .to_string();
    assert!(
        !fixture
            .finish(fixture.spawn_program("no-child", absent), "no-child")
            .await
    );
    let before = fixture.metadata().await;
    assert_eq!(before[attempt::KEY]["state"], "not_started");
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    assert!(
        !fixture
            .native
            .requests
            .lock()
            .await
            .iter()
            .any(|request| request.starts_with("POST /apis/batch/v1/namespaces/fixture/jobs"))
    );
    let old_id = before[attempt::KEY]["id"].clone();
    assert!(
        fixture
            .finish(fixture.spawn("after-no-child"), "after-no-child")
            .await
    );
    let after = fixture.metadata().await;
    assert_eq!(after[model::INTENT], before[model::INTENT]);
    assert_eq!(after[attempt::KEY]["previous"], old_id);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(
        after[model::KEY]["queued_at"],
        before[model::KEY]["queued_at"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_send_failure_without_native_observation_stays_unknown() {
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(1, Ordering::SeqCst);
    assert!(!fixture.finish(fixture.spawn("failed"), "failed").await);
    let before = fixture.metadata().await;
    assert!(!model::read(&before).unwrap().unwrap().consumed);
    assert!(before.get(model::INTENT).is_some());
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    fixture.native.behavior.store(0, Ordering::SeqCst);
    assert!(!fixture.finish(fixture.spawn("retry"), "retry").await);
    let after = fixture.metadata().await;
    assert_eq!(before[model::INTENT], after[model::INTENT]);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_native_create_with_failed_reply_is_confirmed_by_exact_read() {
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(2, Ordering::SeqCst);
    assert!(
        fixture
            .finish(fixture.spawn("ambiguous"), "ambiguous")
            .await
    );
    let metadata = fixture.metadata().await;
    assert!(model::read(&metadata).unwrap().unwrap().consumed);
    assert_eq!(metadata["launched_at"], "2026-10-15T13:00:01Z");
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_native_create_with_parent_deadline_is_confirmed_without_resending() {
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(4, Ordering::SeqCst);
    let mut child = fixture.spawn("parent-deadline");
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.native.reached.notified(),
    )
    .await
    .unwrap();
    let accepted = fixture.native.jobs.lock().await.clone();
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    // Use the actual production 45-second parent bound and real client.
    // The controlled server accepted the request but never replies.
    let status = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait())
        .await
        .unwrap()
        .unwrap();
    fixture
        .record_child("parent-deadline", &status.to_string())
        .await;
    assert!(
        status.success(),
        "the exact native read confirms the accepted object after the parent deadline"
    );
    assert!(
        model::read(&fixture.metadata().await)
            .unwrap()
            .unwrap()
            .consumed
    );
    assert!(
        fixture
            .finish(
                fixture.spawn("after-parent-deadline"),
                "after-parent-deadline"
            )
            .await
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(*fixture.native.jobs.lock().await, accepted);
    let posts = fixture
        .native
        .requests
        .lock()
        .await
        .iter()
        .filter(|request| request.starts_with("POST /apis/batch/v1/namespaces/fixture/jobs"))
        .count();
    assert_eq!(
        posts, 1,
        "a timed-out possibly effective send cannot acquire a second native create"
    );
}

async fn killed_launcher(phase: u8, replacement: bool, disappear: bool) {
    use std::os::unix::process::ExitStatusExt;
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(phase, Ordering::SeqCst);
    let mut child = fixture.spawn("killed");
    let pid = child.id().unwrap();
    if tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.native.reached.notified(),
    )
    .await
    .is_err()
    {
        let _ = tokio::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status()
            .await;
        let _ = fixture.finish(child, "killed").await;
        panic!("worker did not reach the controlled native acceptance barrier");
    }
    let before = fixture.metadata().await;
    assert!(
        before.get(model::INTENT).is_some(),
        "intent precedes the controlled acceptance point"
    );
    assert!(!model::read(&before).unwrap().unwrap().consumed);
    let accepted_before = fixture.native.jobs.lock().await.clone();
    assert_eq!(
        fixture.native.accepted.load(Ordering::SeqCst),
        usize::from(phase == 4)
    );
    assert_eq!(accepted_before.len(), usize::from(phase == 4));
    eprintln!(
        "acceptance barrier original {}",
        json!({"phase":phase,"pid":pid,"accepted":accepted_before,"metadata":before})
    );
    let marker = if replacement {
        let command =
            model::renew(&before, model::instant("2026-10-15T13:06:00Z").unwrap()).unwrap();
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("fixture-replacement".into()),
        );
        fixture
            .repo
            .conditional_job_metadata_at(
                &boss_core::job::JobId::from_uuid(Uuid::parse_str(&fixture.input.packet).unwrap()),
                &command,
                None,
                &stamp,
            )
            .await
            .unwrap();
        fixture.metadata().await[model::KEY].clone()
    } else {
        before[model::KEY].clone()
    };
    assert!(
        tokio::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status()
            .await
            .unwrap()
            .success()
    );
    let exit = child.wait().await.unwrap();
    assert_eq!(exit.signal(), Some(9));
    fixture
        .record_child("killed", &format!("{exit}; signal={:?}", exit.signal()))
        .await;
    fixture.native.release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.native.finished.notified(),
    )
    .await
    .unwrap();
    let outcome = fixture.native.barrier_outcome.load(Ordering::SeqCst);
    assert!(
        matches!(outcome, 1 | 2),
        "barrier completion or cancellation is recorded"
    );
    eprintln!(
        "acceptance barrier terminal {}",
        json!({"phase":phase,"signal":exit.signal(),"outcome":if outcome==1 {"released"}else{"canceled"},"native":*fixture.native.jobs.lock().await,"metadata":fixture.metadata().await})
    );
    assert_eq!(
        fixture.native.accepted.load(Ordering::SeqCst),
        usize::from(phase == 4)
    );
    assert_eq!(
        fixture.metadata().await[model::KEY],
        marker,
        "death cannot spend either reservation"
    );
    assert!(
        model::stopped_reason(
            &fixture.metadata().await,
            Some(model::instant("2026-10-15T13:12:00Z").unwrap())
        )
        .is_some(),
        "a dead process leaves visible native reconciliation work"
    );
    fixture.native.behavior.store(0, Ordering::SeqCst);
    if disappear {
        fixture.native.jobs.lock().await.clear();
    }
    let success = fixture.finish(fixture.spawn("rejoin"), "rejoin").await;
    if phase == 3 || disappear {
        assert!(
            !success,
            "absence after an interrupted send is UNKNOWN, never proof of no effect"
        );
        assert_eq!(
            fixture.native.accepted.load(Ordering::SeqCst),
            usize::from(phase == 4)
        );
        assert!(fixture.native.jobs.lock().await.is_empty());
        assert_eq!(
            fixture.metadata().await[model::INTENT],
            before[model::INTENT]
        );
        assert!(
            !model::read(&fixture.metadata().await)
                .unwrap()
                .unwrap()
                .consumed
        );
        return;
    }
    assert!(
        success,
        "a fresh rejoin can reconcile the generation it explicitly observes"
    );
    assert_eq!(
        fixture.native.accepted.load(Ordering::SeqCst),
        1,
        "rejoin cannot create a second native identity"
    );
    let accepted_after = fixture.native.jobs.lock().await.clone();
    assert_eq!(accepted_after.len(), 1);
    if phase == 4 {
        assert_eq!(
            accepted_after, accepted_before,
            "rejoin keeps the complete accepted native identity"
        );
    }
    let after = fixture.metadata().await;
    assert_eq!(before[model::INTENT], after[model::INTENT]);
    if replacement {
        let mut consumed_marker = marker.clone();
        consumed_marker["consumed"] = json!(true);
        assert_eq!(
            after[model::KEY],
            consumed_marker,
            "a fresh transition consumes its observed generation without rewriting its identity"
        );
    } else {
        assert!(model::read(&after).unwrap().unwrap().consumed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_before_native_acceptance_retains_intent_and_reservation() {
    killed_launcher(3, false, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_after_native_acceptance_resumes_one_exact_job() {
    killed_launcher(4, false, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_rejoin_after_process_death_observes_the_replacement_generation() {
    killed_launcher(4, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_unrecorded_then_deleted_native_job_cannot_be_recreated() {
    killed_launcher(4, false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_old_live_transition_cannot_consume_a_replacement_after_native_acceptance() {
    let fixture = Fixture::new().await;
    fixture.native.behavior.store(4, Ordering::SeqCst);
    let child = fixture.spawn("old-live");
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.native.reached.notified(),
    )
    .await
    .unwrap();
    let before = fixture.metadata().await;
    let native = fixture.native.jobs.lock().await.clone();
    assert_eq!(native.len(), 1);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert!(!model::read(&before).unwrap().unwrap().consumed);
    let command = model::renew(&before, model::instant("2026-10-15T13:06:00Z").unwrap()).unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("fixture-replacement".into()),
    );
    fixture
        .repo
        .conditional_job_metadata_at(
            &boss_core::job::JobId::from_uuid(Uuid::parse_str(&fixture.input.packet).unwrap()),
            &command,
            None,
            &stamp,
        )
        .await
        .unwrap();
    let replacement = fixture.metadata().await[model::KEY].clone();
    assert_ne!(replacement, before[model::KEY]);
    eprintln!(
        "live stale transition original {}",
        json!({"before":before,"replacement":replacement,"native":native})
    );
    fixture.native.release.notify_one();
    assert!(
        !fixture.finish(child, "old-live").await,
        "the paused old transition must refuse the replacement"
    );
    let after = fixture.metadata().await;
    assert_eq!(after[model::KEY], replacement);
    assert_eq!(after[model::INTENT], before[model::INTENT]);
    assert_eq!(*fixture.native.jobs.lock().await, native);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.native.barrier_outcome.load(Ordering::SeqCst),
        1,
        "the old live response was explicitly released"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guarded_negative_claim_stops_an_already_prepared_native_request() {
    let fixture = Fixture::new().await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T06:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, bytes) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let verdict = json!({"verdict":"withdrawn","withdrawn_because":"controlled stand-down"});
    settlement::claim_on(&doors, &fixture.input.packet, "fixture", &verdict, now)
        .await
        .unwrap();
    assert!(create_on(&doors, &intent, &bytes, now).await.is_err());
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    let md = fixture.metadata().await;
    assert_eq!(md["gate_orphan_claim"]["verdict_receipt"], verdict);
    assert_eq!(
        md["gate_orphan_claim"],
        md["_immutable_metadata"]["gate_orphan_claim"]
    );
    assert!(md.get(attempt::KEY).is_none());
    settlement::claim_on(&doors, &fixture.input.packet, "fixture", &verdict, now)
        .await
        .unwrap();
    assert_eq!(
        fixture.metadata().await["gate_orphan_claim"],
        md["gate_orphan_claim"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_possibly_effective_send_cannot_acquire_a_negative_claim() {
    let fixture = Fixture::new().await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T06:00:00Z").unwrap();
    fixture.native.behavior.store(1, Ordering::SeqCst);
    let manifest = fixture_manifest(&fixture.input.packet, "");
    assert!(
        launch_legacy_on(
            &doors,
            &fixture.input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &manifest,
            now
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    let before = fixture.metadata().await;
    assert_eq!(before[attempt::KEY]["state"], "sending");
    assert!(
        settlement::claim_on(
            &doors,
            &fixture.input.packet,
            "fixture",
            &json!({"verdict":"lost"}),
            now
        )
        .await
        .is_err()
    );
    let after = fixture.metadata().await;
    assert!(after.get("gate_orphan_claim").is_none());
    assert_eq!(after[attempt::KEY], before[attempt::KEY]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_negative_no_child_decision_cannot_consume_a_later_conflicting_native_object() {
    let mut fixture = Fixture::new().await;
    fixture.input.create_program = Some(
        fixture
            ._files
            .path()
            .join("missing-create-program")
            .display()
            .to_string(),
    );
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T06:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, bytes) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    assert!(create_on(&doors, &intent, &bytes, now).await.is_err());
    assert_eq!(
        fixture.metadata().await[attempt::KEY]["state"],
        "not_started"
    );
    settlement::claim_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &json!({"verdict":"withdrawn","reason":"proved no child"}),
        now,
    )
    .await
    .unwrap();
    let mut conflicting: Value = serde_json::from_slice(&bytes).unwrap();
    conflicting["metadata"]["uid"] = json!("externally-created-conflicting-uid");
    conflicting["metadata"]["resourceVersion"] = json!("2");
    conflicting["metadata"]["creationTimestamp"] = json!("2026-10-16T06:01:00Z");
    fixture
        .native
        .jobs
        .lock()
        .await
        .insert(intent.name.clone(), conflicting);
    assert!(
        existing_on(&doors, "fixture", &fixture.input.packet)
            .await
            .is_err(),
        "a later contradicting native object requires reconciliation, never consumption over the original negative decision"
    );
    assert!(fixture.metadata().await.get(model::OBSERVATION).is_none());
    assert_eq!(fixture.metadata().await[model::KEY]["consumed"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repinned_submission_does_not_borrow_its_previous_protocol_acceptance() {
    let fixture = Fixture::new().await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T06:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    submission::submit_on(&doors, &intent, &json!({}))
        .await
        .unwrap();
    let id = boss_core::job::JobId::from_uuid(Uuid::parse_str(&fixture.input.packet).unwrap());
    let original = fixture
        .repo
        .get_job(&id)
        .await
        .unwrap()
        .unwrap()
        .workflow_version;
    fixture
        .repo
        .change_job_after_next_read(&id, |row| row.workflow_version += 1);
    fixture.repo.get_job(&id).await.unwrap();
    assert_eq!(
        fixture
            .repo
            .get_job(&id)
            .await
            .unwrap()
            .unwrap()
            .workflow_version,
        original + 1,
        "fixture must actually change the pinned row; ordinary update preserves it"
    );
    assert!(
        submission::original_on(&doors, &fixture.input.packet)
            .await
            .is_err(),
        "the original receipt qualifies its admitted protocol, not a later repin"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_conductor_cannot_demote_an_unknown_train_priority_read_to_no_trains() {
    let fixture = Fixture::configured(None, true).await;
    let input = fixture.ordinary_input("conductor");
    fixture.native.priority_fault.store(1, Ordering::SeqCst);
    let success = fixture
        .finish(
            fixture.spawn_input("conductor-priority-unknown", &input),
            "conductor-priority-unknown",
        )
        .await;
    assert!(
        !success && fixture.native.accepted.load(Ordering::SeqCst) == 0,
        "unknown train priority must refuse actual conductor native creation; success={success}, accepted={}",
        fixture.native.accepted.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_cli_commits_resident_default_and_explicit_staged_submissions_without_sending() {
    for (operation, owner, selection) in [
        ("ordinary", 7, "resident-default"),
        ("ordinary-submit", 0, "explicit-submit"),
    ] {
        let fixture = Fixture::configured(None, true).await;
        let input = fixture.ordinary_input(operation);
        fixture.native.policy_fault.store(owner, Ordering::SeqCst);
        assert!(
            fixture
                .finish(fixture.spawn_input(operation, &input), operation)
                .await
        );
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &input);
        let rows = api(
            &doors,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&status=open&full=true&limit=50",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let submitted: Vec<_> = rows["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row.pointer("/metadata/gate_submission").is_some())
            .collect();
        assert_eq!(
            submitted.len(),
            1,
            "actual ordinary handler must commit one original packet-owned request"
        );
        let packet = submitted[0]["id"].as_str().unwrap();
        let (submission, receipt) = submission::original_on(&doors, packet).await.unwrap();
        assert_eq!(submission.source["gate_queue_selection"], selection);
        assert_eq!(
            submission.source["gate_queue_policy"]["gate_queue_owner"],
            if owner == 7 { "resident" } else { "legacy" }
        );
        assert_eq!(receipt.job_id.to_string(), packet);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_cli_rollout_skew_is_visible_and_legacy_runs_only_after_complete_policy_read() {
    let fixture = Fixture::configured(Some(json!({})), true).await;
    let input = fixture.ordinary_input("ordinary");
    fixture.native.policy_fault.store(1, Ordering::SeqCst);
    let skew_ok = fixture
        .finish(
            fixture.spawn_input("ordinary-skew", &input),
            "ordinary-skew",
        )
        .await;
    let skew_diag =
        std::fs::read_to_string(fixture._files.path().join("ordinary-skew.stderr")).unwrap();
    let before = fixture.native.accepted.load(Ordering::SeqCst);
    fixture.native.policy_fault.store(0, Ordering::SeqCst);
    let legacy_ok = fixture
        .finish(
            fixture.spawn_input("ordinary-legacy", &input),
            "ordinary-legacy",
        )
        .await;
    assert!(
        !skew_ok
            && before == 0
            && skew_diag.contains("gate_queue_owner")
            && legacy_ok
            && fixture.native.accepted.load(Ordering::SeqCst) == 1,
        "actual ordinary handler must diagnose old API ownership as unknown, then select declared legacy after the new full read; skew_ok={skew_ok} before={before} legacy_ok={legacy_ok} diag={skew_diag}"
    );
    assert!(
        fixture.metadata().await.get(model::SUBMISSION).is_none(),
        "legacy is never called packet-owned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conductor_rollout_skew_is_visible_and_legacy_runs_only_after_complete_policy_read() {
    let fixture = Fixture::configured(Some(json!({})), true).await;
    let input = fixture.ordinary_input("conductor");
    fixture.native.policy_fault.store(1, Ordering::SeqCst);
    let skew_ok = fixture
        .finish(
            fixture.spawn_input("conductor-skew", &input),
            "conductor-skew",
        )
        .await;
    let skew_diag =
        std::fs::read_to_string(fixture._files.path().join("conductor-skew.stderr")).unwrap();
    let before = fixture.native.accepted.load(Ordering::SeqCst);
    fixture.native.policy_fault.store(0, Ordering::SeqCst);
    let legacy_ok = fixture
        .finish(
            fixture.spawn_input("conductor-legacy", &input),
            "conductor-legacy",
        )
        .await;
    assert!(
        !skew_ok
            && before == 0
            && skew_diag.contains("gate_queue_owner")
            && legacy_ok
            && fixture.native.accepted.load(Ordering::SeqCst) == 1,
        "actual conductor producer must diagnose old API ownership as unknown, then select declared legacy after the new full read; skew_ok={skew_ok} before={before} legacy_ok={legacy_ok} diag={skew_diag}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_writers_refuse_unknown_policy_and_legacy_submit_without_native_send() {
    for operation in ["ordinary", "conductor"] {
        let fixture = Fixture::configured(Some(json!({})), true).await;
        let mut input = fixture.ordinary_input(operation);
        for fault in [1, 2, 3, 4, 5, 6, 7] {
            fixture.native.policy_fault.store(fault, Ordering::SeqCst);
            let label = format!("{operation}-policy-{fault}");
            assert!(
                !fixture
                    .finish(fixture.spawn_input(&label, &input), &label)
                    .await,
                "{operation}: policy uncertainty/resident legacy mismatch cannot select legacy ({fault})"
            );
            assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
        }
        if operation == "ordinary" {
            fixture.native.policy_fault.store(0, Ordering::SeqCst);
            input.operation = "ordinary-submit".into();
            assert!(
                !fixture
                    .finish(
                        fixture.spawn_input("legacy-submit", &input),
                        "legacy-submit"
                    )
                    .await
            );
            assert_eq!(
                fixture.native.accepted.load(Ordering::SeqCst),
                0,
                "explicit submit never adopts an old protocol"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_protocols_and_modern_markers_never_select_legacy_transport() {
    for schema in [
        json!({"immutable_fields":null}),
        json!({"guarded_fields":[model::ATTEMPT]}),
        json!({"immutable_fields":[model::INTENT]}),
        json!({"conditional_fields":[model::KEY]}),
        json!({"immutable_fields":["other","other"]}),
    ] {
        let fixture = Fixture::configured(Some(schema), false).await;
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &fixture.input);
        let error = launch_legacy_on(
            &doors,
            &fixture.input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &fixture_manifest(&fixture.input.packet, ""),
            chrono::Utc::now(),
        )
        .await;
        assert!(error.is_err());
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    }
    for key in [
        model::KEY,
        model::INTENT,
        model::OBSERVATION,
        model::SUBMISSION,
        model::ATTEMPT,
        "gate_orphan_claim",
        "_immutable_metadata",
        "_guarded_metadata",
    ] {
        let fixture = Fixture::configured(Some(json!({})), false).await;
        let id = boss_core::job::JobId::from_uuid(Uuid::parse_str(&fixture.input.packet).unwrap());
        let mut row = fixture.repo.get_job(&id).await.unwrap().unwrap();
        row.metadata[key] = Value::Null;
        if fixture.repo.update_job(&row).await.is_err() {
            assert_eq!(
                fixture.native.accepted.load(Ordering::SeqCst),
                0,
                "storage refused forged binding {key}"
            );
            continue;
        }
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &fixture.input);
        assert!(
            launch_legacy_on(
                &doors,
                &fixture.input.packet,
                "fixture",
                FIXTURE_HEAD,
                "full",
                &fixture_manifest(&fixture.input.packet, ""),
                chrono::Utc::now()
            )
            .await
            .is_err(),
            "{key}"
        );
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0, "{key}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_or_mixed_protocol_cannot_authorize_legacy_negative_settlement() {
    for schema in [
        json!({"immutable_fields":null}),
        json!({"guarded_fields":[model::ATTEMPT]}),
        json!({"conditional_fields":[model::KEY]}),
    ] {
        let fixture = Fixture::configured(Some(schema), false).await;
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &fixture.input);
        assert!(
            settlement::guard_on(
                &doors,
                &fixture.input.packet,
                "fixture",
                &json!({"verdict":"lost"}),
                chrono::Utc::now()
            )
            .await
            .is_err(),
            "an incomplete protocol cannot authorize a negative effect"
        );
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
        assert!(fixture.metadata().await.get("gate_orphan_claim").is_none());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_pass_retries_only_verified_no_child_without_claiming_a_bay() {
    let fixture = Fixture::new().await;
    let checkout = fixture._files.path().join("conductor-clone");
    std::fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&checkout)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "Fixture source",
    ]);
    // A real local git remote proves the same exact-head query as the
    // conductor, with no credentials, provider, or global environment.
    git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
    let head = git(&["rev-parse", "HEAD"]);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    api(
        &doors,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{}/metadata", fixture.input.packet),
        Some(json!({"sha":head})),
    )
    .await
    .unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &head,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let receipt = submission::submit_on(
        &doors,
        &intent,
        &json!({"branch":"fixture","sha":head,"worktree":"original-builder-checkout"}),
    )
    .await
    .unwrap();
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let mut resident = qualification_doors(&client, &input);
    resident.create_program = Some(fixture._files.path().join("no-native-create-child"));
    assert!(
        submission::launch_on(&resident, &fixture.input.packet, now)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.metadata().await[attempt::KEY]["state"],
        "not_started"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    resident.create_program = None;
    let report = supervisor::pass_on(&resident, &checkout, "fixture", 1, now)
        .await
        .unwrap();
    fixture.record_artifacts("resident-pass").await;
    assert_eq!(report.launched, vec![fixture.input.packet.clone()]);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    let (_, original) = submission::original_on(&resident, &fixture.input.packet)
        .await
        .unwrap();
    assert_eq!(
        original, receipt,
        "a cadence pass cannot reset accepted order or replace the original request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_policy_snapshot_is_the_complete_original_resolved_policy() {
    let fixture = Fixture::new().await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = chrono::Utc::now();
    let original = staging::policy_on(&doors).await.unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &fixture_manifest(&fixture.input.packet, ""),
    )
    .await
    .unwrap();
    let receipt = submission::submit_on(&doors, &intent, &json!({}))
        .await
        .unwrap();
    let source = &receipt.job.metadata[model::SUBMISSION]["source"];
    assert_eq!(
        source["gate_queue_policy"], original,
        "source must retain the whole original resolved row, including adjacent policy data"
    );
    assert_eq!(source["gate_queue_selection"], "explicit-submit");
    fixture.native.policy_fault.store(7, Ordering::SeqCst);
    let (accepted, again) = submission::original_on(&doors, &fixture.input.packet)
        .await
        .unwrap();
    assert_eq!(
        accepted.source["gate_queue_policy"], original,
        "a later active owner cannot rewrite acceptance"
    );
    assert_eq!(again, receipt);
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resident_policy_read_failure_never_uses_compiled_capacity_or_launches() {
    let fixture = Fixture::new().await;
    let checkout = fixture._files.path().join("conductor-clone");
    std::fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&checkout)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--initial-branch=fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "Fixture source",
    ]);
    // A real local git remote proves the same exact-head query as the
    // conductor, with no credentials, provider, or global environment.
    git(&["remote", "add", "origin", checkout.to_str().unwrap()]);
    let head = git(&["rev-parse", "HEAD"]);
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let doors = qualification_doors(&client, &fixture.input);
    let now = model::instant("2026-10-16T13:00:00Z").unwrap();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    api(
        &doors,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{}/metadata", fixture.input.packet),
        Some(json!({"sha":head})),
    )
    .await
    .unwrap();
    let manifest = fixture_manifest(&fixture.input.packet, "");
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        &head,
        "full",
        &manifest,
    )
    .await
    .unwrap();
    let receipt = submission::submit_on(
        &doors,
        &intent,
        &json!({"branch":"fixture","sha":head,"worktree":"original-builder-checkout"}),
    )
    .await
    .unwrap();
    let mut input = fixture.input.clone();
    input.operation = "supervise".into();
    let resident = qualification_doors(&client, &input);
    fixture.native.policy_fault.store(3, Ordering::SeqCst);
    let observation = supervisor::observation_on(&resident, &checkout, "fixture", 1, now).await;
    assert_eq!(
        observation["state"], "unknown",
        "unreadable actual policy must not become a compiled fallback admission: {observation}"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    let (_, original) = submission::original_on(&resident, &fixture.input.packet)
        .await
        .unwrap();
    assert_eq!(original, receipt);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_source_claim_is_revalidated_without_requiring_a_live_model() {
    for fault in [
        "run-closed",
        "parent-closed",
        "step-moved",
        "backlink-moved",
        "holder-moved",
        "wrong-claim-receipt",
        "",
    ] {
        let fixture = Fixture::new().await;
        let (run, parent) = fixture.source_claim(fault).await;
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &fixture.input);
        let now = chrono::Utc::now();
        renew_on(&doors, &fixture.input.packet, now).await.unwrap();
        let (intent, _) = prepare_on(
            &doors,
            &fixture.input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &fixture_manifest(&fixture.input.packet, ""),
        )
        .await
        .unwrap();
        let result = submission::submit_on(&doors, &intent, &json!({"agent_run":run})).await;
        if fault.is_empty() {
            let receipt = result.unwrap();
            assert_eq!(
                receipt.job.metadata[model::SUBMISSION]["source"]["gate_source_claim"]["packet"],
                parent
            );
            assert_eq!(
                receipt.job.metadata[model::SUBMISSION]["source"]["gate_source_claim"]["run"],
                run
            );
        } else {
            assert!(
                result.is_err(),
                "{fault} cannot authorize immutable submission"
            );
        }
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_claim_change_after_acceptance_refuses_the_actual_guarded_sender() {
    use boss_core::job::{JobId, JobStatus, Step, StepStatus};
    for fault in [
        "run-closed",
        "parent-closed",
        "step-moved",
        "backlink-moved",
        "holder-moved",
        "source-retargeted",
    ] {
        let fixture = Fixture::new().await;
        let (run, parent) = fixture.source_claim("").await;
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = qualification_doors(&client, &fixture.input);
        let now = chrono::Utc::now();
        renew_on(&doors, &fixture.input.packet, now).await.unwrap();
        let (intent, _) = prepare_on(
            &doors,
            &fixture.input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &fixture_manifest(&fixture.input.packet, ""),
        )
        .await
        .unwrap();
        let receipt = submission::submit_on(&doors, &intent, &json!({"agent_run":run}))
            .await
            .unwrap();
        let run_id = JobId::from_uuid(Uuid::parse_str(&run).unwrap());
        let parent_id = JobId::from_uuid(Uuid::parse_str(&parent).unwrap());
        match fault {
            "run-closed" | "parent-closed" => {
                let id = if fault == "run-closed" {
                    run_id
                } else {
                    parent_id
                };
                let mut row = fixture.repo.get_job(&id).await.unwrap().unwrap();
                row.status = JobStatus::Closed;
                fixture.repo.update_job(&row).await.unwrap();
            }
            "source-retargeted" => {
                let mut step = Step::new(parent_id, "task", "Another build", 1);
                step.spec_slug = Some("another-build".into());
                step.status = StepStatus::Active;
                step.assignee_id = Some("agent-test".into());
                step.metadata = json!({"agent_run":run});
                fixture.repo.add_step(&step).await.unwrap();
                let mut row = fixture.repo.get_job(&run_id).await.unwrap().unwrap();
                row.metadata["step"] = json!("another-build");
                fixture.repo.update_job(&row).await.unwrap();
            }
            _ => {
                let (mut step, version) = fixture
                    .repo
                    .list_steps_versioned(&parent_id)
                    .await
                    .unwrap()
                    .remove(0);
                match fault {
                    "step-moved" => step.status = StepStatus::Completed,
                    "backlink-moved" => step.metadata["agent_run"] = json!(Uuid::new_v4()),
                    "holder-moved" => step.assignee_id = Some("another-agent".into()),
                    _ => unreachable!(),
                }
                fixture
                    .repo
                    .update_step_if_unchanged_at(&step, version, chrono::Utc::now(), &[])
                    .await
                    .unwrap();
            }
        }
        let mut input = fixture.input.clone();
        input.operation = "supervise".into();
        let resident = qualification_doors(&client, &input);
        assert!(
            submission::launch_on(&resident, &fixture.input.packet, now)
                .await
                .is_err(),
            "{fault} cannot borrow the former source permission at the actual actuator"
        );
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0, "{fault}");
        assert!(
            fixture.metadata().await.get(attempt::KEY).is_none(),
            "source refusal precedes the irreversible sending claim"
        );
        let (_, again) = submission::original_on(&resident, &fixture.input.packet)
            .await
            .unwrap();
        assert_eq!(again, receipt);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immutable_submission_cannot_borrow_another_workers_own_run() {
    let fixture = Fixture::new().await;
    let (run, _) = fixture.source_claim("").await;
    let client =
        crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
            .unwrap();
    let mut doors = qualification_doors(&client, &fixture.input);
    let now = chrono::Utc::now();
    renew_on(&doors, &fixture.input.packet, now).await.unwrap();
    let (intent, _) = prepare_on(
        &doors,
        &fixture.input.packet,
        "fixture",
        FIXTURE_HEAD,
        "full",
        &fixture_manifest(&fixture.input.packet, ""),
    )
    .await
    .unwrap();
    doors.caller = Some(identity::Caller {
        id: "different-worker".into(),
        source: identity::Source::Env,
    });
    assert!(
        submission::submit_on(&doors, &intent, &json!({"agent_run":run}))
            .await
            .is_err(),
        "a caller must not attribute its request to another worker's still-open claim"
    );
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 0);
    assert!(fixture.metadata().await.get(model::SUBMISSION).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_native_object_cannot_borrow_missing_protected_intent_observation_or_attempt() {
    for field in [model::INTENT, model::OBSERVATION, attempt::KEY] {
        let fixture = Fixture::new().await;
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let builder = qualification_doors(&client, &fixture.input);
        let now = chrono::Utc::now();
        renew_on(&builder, &fixture.input.packet, now)
            .await
            .unwrap();
        let (intent, _) = prepare_on(
            &builder,
            &fixture.input.packet,
            "fixture",
            FIXTURE_HEAD,
            "full",
            &fixture_manifest(&fixture.input.packet, ""),
        )
        .await
        .unwrap();
        submission::submit_on(&builder, &intent, &json!({}))
            .await
            .unwrap();
        let mut input = fixture.input.clone();
        input.operation = "supervise".into();
        let resident = qualification_doors(&client, &input);
        submission::launch_on(&resident, &fixture.input.packet, now)
            .await
            .unwrap();
        let id = boss_core::job::JobId::from_uuid(Uuid::parse_str(&fixture.input.packet).unwrap());
        fixture.repo.change_job_after_next_read(&id, move |row| {
            if field == attempt::KEY {
                row.metadata.as_object_mut().unwrap().remove(field);
            } else {
                row.metadata["_immutable_metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
        });
        fixture.repo.get_job(&id).await.unwrap();
        let current = fixture.repo.get_job(&id).await.unwrap().unwrap();
        assert!(
            if field == attempt::KEY {
                current.metadata.get(field).is_none()
            } else {
                current.metadata["_immutable_metadata"].get(field).is_none()
            },
            "fault must actually land"
        );
        assert!(
            existing_on(&resident, "fixture", &fixture.input.packet)
                .await
                .is_err(),
            "matching native bytes do not replace missing original protected {field}"
        );
        assert_eq!(
            fixture.native.accepted.load(Ordering::SeqCst),
            1,
            "no repair resends native work"
        );
    }
}

fn native_posts(requests: &[String]) -> usize {
    requests
        .iter()
        .filter(|request| request.starts_with("POST /apis/batch/v1/namespaces/fixture/jobs"))
        .count()
}

/// THE LIVE DEFECT, through the actual caller (971c9006). Train 48bbc0b3's
/// conductor created its gate Job, the API server added only its own
/// defaults, and the exact read after CREATE refused the admitted Job as
/// "does not match the complete recorded request" — so an effective launch
/// read UNKNOWN and the train filed a second gate on the same head. With
/// the server defaults applied, both a plain create and a create whose
/// acknowledgement is lost bind on ONE native POST.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_actual_conductor_binds_a_server_defaulted_gate_job_on_one_native_post() {
    for (behavior, label) in [(0, "defaulted-create"), (2, "defaulted-lost-reply")] {
        let fixture = Fixture::configured(None, true).await;
        let input = fixture.ordinary_input("conductor");
        fixture.native.defaults.store(1, Ordering::SeqCst);
        fixture.native.behavior.store(behavior, Ordering::SeqCst);
        let success = fixture
            .finish(fixture.spawn_input(label, &input), label)
            .await;
        let diag = std::fs::read_to_string(fixture._files.path().join(format!("{label}.stderr")))
            .unwrap_or_default();
        assert!(success, "{label}: a defaulted Job must bind: {diag}");
        assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1, "{label}");
        assert_eq!(
            native_posts(&fixture.native.requests.lock().await),
            1,
            "{label}: zero second native POST"
        );
    }
}

/// The same defaults on the packet-owned path consume the original intent
/// and record its observation; a value at a defaulted site that is NOT the
/// default stays a refusal, is never consumed, and a restarted launcher
/// does not send again (971c9006: do not blanket-drop admitted fields).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_exact_server_defaults_consume_the_original_intent() {
    let fixture = Fixture::new().await;
    fixture.native.defaults.store(1, Ordering::SeqCst);
    assert!(
        fixture
            .finish(fixture.spawn("defaulted"), "defaulted")
            .await
    );
    let metadata = fixture.metadata().await;
    assert!(model::read(&metadata).unwrap().unwrap().consumed);
    assert!(metadata.get(model::OBSERVATION).is_some());
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);

    let fixture = Fixture::new().await;
    fixture.native.defaults.store(2, Ordering::SeqCst);
    assert!(!fixture.finish(fixture.spawn("changed"), "changed").await);
    assert!(
        !fixture
            .finish(fixture.spawn("restarted"), "restarted")
            .await
    );
    let metadata = fixture.metadata().await;
    assert!(!model::read(&metadata).unwrap().unwrap().consumed);
    assert!(metadata.get(model::OBSERVATION).is_none());
    assert_eq!(fixture.native.accepted.load(Ordering::SeqCst), 1);
    assert_eq!(native_posts(&fixture.native.requests.lock().await), 1);
}
