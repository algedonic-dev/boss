//! Exact original diagnosis through native inputs, never a supplied verdict.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::{
    event::Event,
    job::{Job, Step},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    InMemoryJobs,
    http::{JobsApiState, router},
    moves::{InMemoryMoves, Move, MovesFeed, MovesStore},
    port::JobsRepository,
    registry::{InMemoryWorkflows, WorkflowRegistry},
    stations::InMemoryStations,
};
use boss_policy_client::{
    Action, FakePolicyClient, PolicyClient, Resource, Scope,
    types::{AccessTier, User},
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

pub(super) async fn fixture() -> axum::Router {
    fixture_and_jobs().await.0
}
pub(super) async fn fixture_and_jobs() -> (axum::Router, Arc<InMemoryJobs>) {
    fixture_at_cause(false).await
}
#[allow(
    dead_code,
    reason = "The HTTP diagnostic test owns this partial-history case; PostgreSQL shares the complete fixture."
)]
pub(super) async fn fixture_with_parent_cause() -> (axum::Router, Arc<InMemoryJobs>) {
    fixture_at_cause(true).await
}
async fn fixture_at_cause(parent_cause: bool) -> (axum::Router, Arc<InMemoryJobs>) {
    let original: Value = serde_json::from_str(include_str!(
        "../../src/moves/dispatch/native-registered-review.json"
    ))
    .unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-14T17:02:53Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for key in ["parent", "run"] {
        let job: Job = serde_json::from_value(original[key].clone()).unwrap();
        let steps: Vec<Step> = serde_json::from_value(original[key]["steps"].clone()).unwrap();
        let created = Event::new(
            "jobs",
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&job).unwrap(),
            now,
        );
        let step_events: Vec<_> = steps
            .iter()
            .map(|step| {
                Event::new(
                    "jobs",
                    boss_jobs::events::STEP_CREATED,
                    boss_jobs::events::step_state_payload(step),
                    now,
                )
            })
            .collect();
        let mut admission = vec![created];
        if key == "parent" {
            let spec = boss_jobs::seed_loader::load_stations(
                boss_jobs::station_seed::platform_stations_path(),
            )
            .unwrap()
            .into_iter()
            .find(|s| s.name == "loading-dock")
            .unwrap();
            admission.insert(
                0,
                Event::new(
                    "jobs",
                    boss_jobs::events::STATION_PUBLISHED,
                    serde_json::to_value(spec).unwrap(),
                    now,
                ),
            );
        }
        // Build synthetic rows separately from their owned audit envelopes. The
        // ordinary in-memory outbox has no insertion clock; it is not evidence.
        let _ = jobs
            .create_job_with_steps_at(&job, &[], now, &[], &[])
            .await
            .unwrap();
        for step in &steps {
            jobs.add_step_at(step, now, &[]).await.unwrap();
        }
        for mut event in admission.into_iter().chain(step_events) {
            event.payload["_partition"] = json!(job.partition.as_str());
            jobs.record_audit_event(event, now).unwrap();
        }
    }
    let history = jobs.recorded_events();
    let cause_index = if parent_cause {
        history
            .iter()
            .rposition(|event| {
                event.payload.get("job_id").and_then(Value::as_str)
                    == original["parent"]["id"].as_str()
            })
            .unwrap()
    } else {
        history.len() - 1
    };
    let cause = &history[cause_index];
    let store = Arc::new(InMemoryMoves::new());
    store
        .record(&[Move {
            at: now,
            packet: original["run"]["id"].as_str().unwrap().into(),
            kind: "agent-run".into(),
            label: "native review handoff".into(),
            from: Some("shed".into()),
            to: Some("shop-floor".into()),
            declared: Some(false),
            cause_event_id: cause.id,
            cause_seq: cause_index as i64 + 1,
            cause_kind: cause.kind.clone(),
            handoff_from: Some(original["parent"]["id"].as_str().unwrap().into()),
            lineage: Some("packet".into()),
            aboard: vec![],
            terminal: None,
            dispatch: None,
        }])
        .await
        .unwrap();
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    let native: boss_jobs::registry::WorkflowSpec = serde_json::from_str(include_str!(
        "../../src/moves/dispatch/native-workflow-routing.json"
    ))
    .unwrap();
    for spec in boss_jobs::registry::seedable_platform_workflows() {
        if spec.kind != native.kind {
            kinds.seed(spec).unwrap();
        }
    }
    kinds.seed(native).unwrap();
    kinds
        .seed(
            serde_json::from_str(include_str!(
                "../../src/moves/dispatch/native-agent-run-v8.json"
            ))
            .unwrap(),
        )
        .unwrap();
    let stations = Arc::new(InMemoryStations::new());
    for spec in
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .unwrap()
    {
        stations.seed(spec).unwrap();
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("operator", Action::Update, Resource::job(), Scope::All)
            .allow("evidence-reader", Action::Read, Resource::job(), Scope::All)
            .allow(
                "scoped-evidence-reader",
                Action::Read,
                Resource::job(),
                Scope::Self_,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        yard_moves: Some(Arc::new(MovesFeed::new(store))),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        stations: Some(stations),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::FixedClockClient::new(
                boss_clock_client::ClockNow {
                    now,
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                    warp_factor: None,
                },
            )),
        )
    });
    (app, jobs)
}

pub(super) fn machine_proof(
    receipt: &boss_jobs::move_diagnosis::Diagnosis,
) -> boss_jobs::move_diagnosis::proof::MachineProof {
    use boss_core::job::{JobId, JobStatus, StepStatus};
    let native: Value = serde_json::from_str(include_str!(
        "../../src/moves/dispatch/native-registered-review.json"
    ))
    .unwrap();
    let mut artifact: Job = serde_json::from_value(native["parent"].clone()).unwrap();
    artifact.id = JobId::new();
    let head = "9c75e5e36068cea899fdb4b6bbbe3a25c2a17c24";
    artifact.metadata = json!({"merged":true,"merge_ref":head});
    let mut request = artifact.clone();
    request.id = JobId::new();
    request.kind = "ops-request".into();
    request.status = JobStatus::Closed;
    let at = receipt.original.r#move.at + chrono::Duration::hours(1);
    request.opened_at = Some(at - chrono::Duration::seconds(10));
    request.metadata =
        json!({"host":"forge","verb":"run-car-probe","args":[artifact.id.to_string()]});
    let protocol = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "ops-request")
        .unwrap();
    request.workflow_version = protocol.version;
    let spec = protocol
        .steps
        .iter()
        .find(|s| s.title == "execute")
        .unwrap();
    let observation = boss_jobs::move_diagnosis::proof::Observation {
        schema: 1,
        diagnosis_id: receipt.id,
        original_seq: receipt.original.seq,
        original_hash: receipt.original_hash.clone(),
        packet: receipt.original.r#move.packet.clone(),
        cause_event_id: receipt.original.r#move.cause_event_id,
        cause_seq: receipt.original.r#move.cause_seq,
        model: receipt.model.clone(),
        artifact_head: head.into(),
        result: "false-interpretation".into(),
    };
    let stdout = serde_json::to_string(&observation).unwrap();
    let proof = json!({"probe":"read original and corrective artifact","expect":"false-interpretation","exit":0,"stdout":stdout,"stderr":"","host":"forge","cwd":"/var/lib/boss/probe-view","user":"boss-probe","uid":"997","at":at.to_rfc3339(),"tree_head":head,"tree_standing":"unreadable"});
    let mut execute = Step::new(request.id, "task", "Execute", 0);
    execute.spec_slug = Some("execute".into());
    execute.status = StepStatus::Completed;
    execute.completed_by = Some(boss_core::actor::ActorId::automation("ops-runner"));
    execute.completed_at = Some(at + chrono::Duration::seconds(2));
    execute.fields = spec.fields.clone();
    execute.metadata = json!({"credential_executor":"runner:ops","disposition":"answered","exit_code":"0","runner_host":"forge","output":proof_output(&artifact,&proof)});
    let mut proven = Step::new(artifact.id, "task", "Proven in prod", 0);
    proven.spec_slug = Some("proven".into());
    proven.status = StepStatus::Completed;
    proven.completed_at = Some(at + chrono::Duration::seconds(1));
    proven.metadata = json!({"proven_by":"run-car-probe","proof":proof});
    boss_jobs::move_diagnosis::proof::MachineProof {
        request,
        execute,
        protocol,
        artifact,
        proven,
    }
}

pub(super) fn proof_output(artifact: &Job, proof: &Value) -> String {
    let envelope = boss_jobs::move_diagnosis::proof::Envelope {
        schema: 1,
        artifact: artifact.id.to_string().parse().unwrap(),
        proof_json: proof.to_string(),
    };
    format!(
        "boss prove: PROVEN\n{}{}\n",
        boss_jobs::move_diagnosis::proof::FRAME,
        serde_json::to_string(&envelope).unwrap()
    )
}

#[allow(
    dead_code,
    reason = "Shared fixture: the PostgreSQL test uses the domain proof directly, while HTTP tests resolve stored packets."
)]
pub(super) async fn install_proof(
    jobs: &InMemoryJobs,
    receipt: &boss_jobs::move_diagnosis::Diagnosis,
) -> Uuid {
    let proof = machine_proof(receipt);
    for (job, step) in [
        (&proof.request, &proof.execute),
        (&proof.artifact, &proof.proven),
    ] {
        let at = receipt.original.r#move.at;
        let admission = Event::new(
            "jobs",
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(job).unwrap(),
            at,
        );
        let step_event = Event::new(
            "jobs",
            boss_jobs::events::STEP_CREATED,
            boss_jobs::events::step_state_payload(step),
            at,
        );
        let _ = jobs
            .create_job_with_steps_at(
                job,
                std::slice::from_ref(step),
                at,
                &[admission],
                &[step_event],
            )
            .await
            .unwrap();
    }
    proof.request.id.to_string().parse().unwrap()
}

pub(super) async fn post(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let user = User {
        id: "agent-codex".into(),
        role: "operator".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}
