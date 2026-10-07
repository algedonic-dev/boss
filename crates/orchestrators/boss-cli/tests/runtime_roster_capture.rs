use boss_core::{job::JobId, port::EventBus, publisher::DomainPublisher};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use serde_json::{Value, json};
use std::sync::Arc;

const CALL: &str = "call_zAWnX5kBA1bfzOlhUgdi36cU";
const NATIVE: &str = include_str!("fixtures/native-roster.jsonl");

async fn inspect(text: &str) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("receipt.jsonl");
    std::fs::write(&input, text).unwrap();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_boss"))
        .args(["roster", "inspect", "--call-id", CALL, "--transcript"])
        .arg(input)
        .env(
            boss_core::machine_token::TOKEN_DIR_ENV,
            root.path().join("machine-token"),
        )
        .env("BOSS_ACTOR", "agent-codex")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

fn rows() -> Vec<Value> {
    NATIVE
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn encode(rows: &[Value]) -> String {
    rows.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[tokio::test]
async fn copied_native_pair_retains_provenance_and_unavailable_facts() {
    let output = inspect(NATIVE).await;
    assert!(output.status.success(), "{:?}", output);
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["source"], "codex.collaboration.list_agents");
    assert_eq!(receipt["namespace"], "01a0f9e6-4d1c-7c81-859d-5033e0b4418a");
    assert_eq!(receipt["call_id"], CALL);
    assert_eq!(receipt["complete"], true);
    assert_eq!(receipt["rows"].as_array().unwrap().len(), 4);
    assert_eq!(receipt["native_receipt"]["call"], rows()[1]);
    assert_eq!(receipt["native_receipt"]["response"], rows()[2]);
    assert!(
        receipt["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["current_task"].is_null()
                && r["native_id"].is_null()
                && r["observed_model"].is_null())
    );
    let again: Value = serde_json::from_slice(&inspect(NATIVE).await.stdout).unwrap();
    assert_eq!(receipt["capture_id"], again["capture_id"]);
    assert_eq!(receipt["payload_sha256"], again["payload_sha256"]);
}

#[tokio::test]
async fn future_status_is_unknown_and_completed_object_is_observed_finished() {
    let mut r = rows();
    r[2]["payload"]["output"] = json!(json!({"agents":[
        {"agent_name":"/root","agent_status":"future-state"},
        {"agent_name":"/root/worker","agent_status":{"completed":"Native outcome prose is not task identity"}}
    ]}).to_string());
    let out = inspect(&encode(&r)).await;
    assert!(out.status.success(), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["rows"][0]["status"], "unknown");
    assert_eq!(v["rows"][1]["status"], "finished");
    assert!(v["rows"][1]["current_task"].is_null());
}

#[tokio::test]
async fn failed_collection_is_distinct_from_a_complete_empty_roster() {
    let mut r = rows();
    r[2]["payload"]["output"] = json!("native tool unavailable");
    let out = inspect(&encode(&r)).await;
    assert!(out.status.success(), "{out:?}");
    let failed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(failed["complete"], false);
    assert_eq!(failed["result"], "failed");
    r[2]["payload"]["output"] = json!("{\"agents\":[]}");
    let empty: Value = serde_json::from_slice(&inspect(&encode(&r)).await.stdout).unwrap();
    assert_eq!(empty["complete"], true);
    assert_eq!(empty["rows"], json!([]));
}

#[tokio::test]
async fn duplicate_paths_are_a_failed_capture_and_invalid_provenance_refuses() {
    let mut r = rows();
    r[2]["payload"]["output"] = json!(
        "{\"agents\":[{\"agent_name\":\"/root\",\"agent_status\":\"running\"},{\"agent_name\":\"/root\",\"agent_status\":\"running\"}]}"
    );
    let out = inspect(&encode(&r)).await;
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["complete"], false);
    for mode in [
        "namespace",
        "future",
        "inverted",
        "partial",
        "filtered",
        "duplicate-call",
    ] {
        let mut r = rows();
        match mode {
            "namespace" => r[0]["payload"]["id"] = json!("manual-tree-name"),
            "future" => r[2]["timestamp"] = json!("2999-10-02T19:22:41.570Z"),
            "inverted" => r[1]["timestamp"] = json!("2026-10-03T19:22:41.407Z"),
            "partial" => {
                r.pop();
            }
            "filtered" => r[1]["payload"]["arguments"] = json!("{\"path_prefix\":\"/root/one\"}"),
            _ => r.push(r[1].clone()),
        }
        assert!(!inspect(&encode(&r)).await.status.success(), "{mode}");
    }
}

struct World {
    root: tempfile::TempDir,
    jobs: Arc<InMemoryJobs>,
    base: String,
    server: tokio::task::JoinHandle<()>,
}

// Exercise Q7 admission through its real port: the responsible human
// owns the packet, while the signed agent performs the capture.
struct HumanRoster;

#[async_trait::async_trait]
impl boss_jobs::owner_resolution::RosterLookup for HumanRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(if role == "platform-admin" {
            vec!["emp-david".to_string()]
        } else {
            vec![]
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-david")
    }
}
impl Drop for World {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl World {
    async fn new(allow: bool, mode: &'static str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("native.jsonl"), NATIVE).unwrap();
        let jobs = Arc::new(InMemoryJobs::new());
        let workflows = Arc::new(InMemoryWorkflows::for_fixture());
        let spec = boss_jobs::seed_loader::load_workflows(
            boss_testing::repo_root().join("infra/platform/workflows"),
        )
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "runtime-roster-capture")
        .unwrap();
        workflows.seed(spec).unwrap();
        let policy: Arc<dyn PolicyClient> = if allow {
            Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "platform-admin",
                        Action::Create,
                        Resource::job(),
                        Scope::All,
                    )
                    .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                    .allow(
                        "platform-admin",
                        Action::Update,
                        Resource::step(),
                        Scope::All,
                    )
                    .build(),
            )
        } else {
            Arc::new(FakePolicyClient::builder().build())
        };
        let bus = boss_testing::RecordingEventBus::new();
        let dynbus: Arc<dyn EventBus> = bus.clone();
        let state = JobsApiState {
            kind_registry: Some(workflows as Arc<dyn WorkflowRegistry>),
            roster: (mode == "human-owner").then(|| {
                Arc::new(HumanRoster) as Arc<dyn boss_jobs::owner_resolution::RosterLookup>
            }),
            ..JobsApiState::minimal(
                jobs.clone(),
                bus,
                DomainPublisher::new(dynbus, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = router(state).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let patch = req.method() == axum::http::Method::PATCH;
                let read = req.method() == axum::http::Method::GET;
                let mut response = next.run(req).await;
                if mode == "silent-write" && patch {
                    *response.body_mut() = axum::body::Body::empty();
                    *response.status_mut() = axum::http::StatusCode::OK;
                }
                if mode == "wrong-readback" && read {
                    *response.body_mut() = axum::body::Body::from("{\"id\":\"other\"}");
                }
                response
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            root,
            jobs,
            base,
            server,
        }
    }
    async fn record(&self, text: &str) -> std::process::Output {
        self.record_as(text, "agent-codex").await
    }

    async fn record_as(&self, text: &str, actor: &str) -> std::process::Output {
        let path = self.root.path().join("native.jsonl");
        std::fs::write(&path, text).unwrap();
        tokio::process::Command::new(env!("CARGO_BIN_EXE_boss"))
            .args(["roster", "record", "--call-id", CALL, "--transcript"])
            .arg(path)
            .env("BOSS_JOBS_URL", &self.base)
            .env("BOSS_ACTOR", actor)
            .env(
                "BOSS_MACHINE_TOKEN_DIR",
                self.root.path().join("empty-token"),
            )
            .kill_on_drop(true)
            .output()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn recording_accepts_the_signers_canonical_event_spelling() {
    let w = World::new(true, "human-owner").await;
    let out = w.record_as(NATIVE, "rule:roster-capture").await;
    assert!(out.status.success(), "{out:?}");
    let replay = w.record_as(NATIVE, "automation:rule:roster-capture").await;
    assert!(replay.status.success(), "{replay:?}");
    let foreign = w.record_as(NATIVE, "agent-other").await;
    assert!(!foreign.status.success(), "foreign recorder accepted");
}

#[tokio::test]
async fn recording_preserves_human_owner_and_credits_the_agent_executor() {
    let w = World::new(true, "human-owner").await;
    let out = w.record(NATIVE).await;
    assert!(out.status.success(), "{out:?}");
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id =
        JobId::from_uuid(uuid::Uuid::parse_str(receipt["capture_id"].as_str().unwrap()).unwrap());
    let job = w.jobs.get_job(&id).await.unwrap().unwrap();
    assert_eq!(job.owner_id.to_string(), "emp-david");
    assert_eq!(job.metadata["opened_by"], "agent-codex");
    let steps = w.jobs.list_steps(&id).await.unwrap();
    let capture = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("capture"))
        .unwrap();
    assert_eq!(
        capture.completed_by.as_ref().map(ToString::to_string),
        Some("agent-codex".into())
    );
    let again = w.record(NATIVE).await;
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        serde_json::to_value(w.jobs.list_steps(&id).await.unwrap()).unwrap(),
        serde_json::to_value(steps).unwrap()
    );
}

#[tokio::test]
async fn signed_real_router_capture_is_read_back_immutable_and_equal_replay_idempotent() {
    let w = World::new(true, "normal").await;
    let out = w.record(NATIVE).await;
    assert!(out.status.success(), "{out:?}");
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id =
        JobId::from_uuid(uuid::Uuid::parse_str(receipt["capture_id"].as_str().unwrap()).unwrap());
    let job = w.jobs.get_job(&id).await.unwrap().unwrap();
    let first_steps = w.jobs.list_steps(&id).await.unwrap();
    assert_eq!(job.kind, "runtime-roster-capture");
    let step = first_steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("capture"))
        .unwrap();
    assert_eq!(
        step.completed_by.as_ref().map(ToString::to_string),
        Some("agent-codex".into())
    );
    assert_eq!(
        step.metadata["snapshot"]["native_receipt"]["response"],
        rows()[2]
    );
    let events = w.jobs.events_for_job(&id, 1000).await.unwrap();
    assert!(!events.is_empty());
    let replayed = events
        .iter()
        .filter_map(|e| e.payload.get("metadata").and_then(|md| md.get("snapshot")))
        .next_back()
        .unwrap();
    assert_eq!(replayed, &step.metadata["snapshot"]);
    let again = w.record(NATIVE).await;
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        serde_json::to_value(w.jobs.list_steps(&id).await.unwrap()).unwrap(),
        serde_json::to_value(&first_steps).unwrap()
    );
    assert_eq!(
        w.jobs.events_for_job(&id, 1000).await.unwrap().len(),
        events.len()
    );
    let client = reqwest::Client::new();
    let denied = client
        .patch(format!(
            "{}/api/jobs/{id}/steps/{}/metadata",
            w.base, step.id
        ))
        .header(
            "x-boss-user",
            "{\"id\":\"agent-codex\",\"role\":\"platform-admin\"}",
        )
        .json(&json!({"snapshot":{"complete":false}}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::CONFLICT);
    let mut conflicting = rows();
    conflicting[2]["payload"]["output"] = json!("{\"agents\":[]}");
    assert!(!w.record(&encode(&conflicting)).await.status.success());
    assert_eq!(
        serde_json::to_value(w.jobs.list_steps(&id).await.unwrap()).unwrap(),
        serde_json::to_value(&first_steps).unwrap()
    );
}

#[tokio::test]
async fn authority_and_unverified_write_or_readback_never_claim_success() {
    for (allow, mode) in [
        (false, "normal"),
        (true, "silent-write"),
        (true, "wrong-readback"),
    ] {
        let w = World::new(allow, mode).await;
        assert!(!w.record(NATIVE).await.status.success(), "{mode}");
    }
}

#[tokio::test]
async fn failed_native_collection_is_recorded_without_a_healthy_empty_claim() {
    let w = World::new(true, "normal").await;
    let mut input = rows();
    input[2]["payload"]["output"] = json!("Tool failed");
    let out = w.record(&encode(&input)).await;
    assert!(out.status.success(), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["complete"], false);
    assert_eq!(v["result"], "failed");
}
