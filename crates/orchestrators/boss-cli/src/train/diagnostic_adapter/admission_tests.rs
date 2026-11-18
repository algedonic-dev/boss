//! Admission and the conductor's queue read ONE definition of a
//! recovery assessment's visibility (backlog 97170186).
//!
//! 28e96fbc was filed through `boss job file`, admitted, its execute
//! step READY on the conductor — and invisible, because the drain selects
//! on job markers v1 of the protocol only described in workflow-level
//! `metadata`. These walk the supported verb through the REAL jobs router
//! and the drain's own read, so the contract and the queue cannot drift.
use super::*;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobFilter, JobsRepository, WorkflowRegistry};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use std::sync::Arc;

const KIND: &str = "diagnostic-recovery-assessment";
const LOGIN: &str = "claude@algedonic.dev";
const AGENT: &str = "agent-claude";

fn contract() -> boss_jobs::registry::WorkflowSpec {
    boss_jobs::seed_loader::parse_workflows(
        include_str!(
            "../../../../../../infra/platform/workflows/diagnostic-recovery-assessment.toml"
        ),
        "platform",
        "diagnostic-recovery-assessment.toml",
    )
    .unwrap()
    .remove(0)
}

fn decided_original(spec: &boss_jobs::registry::WorkflowSpec) -> Value {
    spec.steps
        .iter()
        .find(|s| s.title == "execute")
        .unwrap()
        .metadata_defaults["recovery_operation"]["original_request"]
        .clone()
}

/// The pin (CLAUDE.md §9a): the queue's markers are the conductor's own
/// identity in code, the contract's are registry data, so they are held
/// equal here, naming the key that drifted.
#[test]
fn the_published_contract_requires_exactly_the_markers_the_queue_reads() {
    let spec = contract();
    let required = spec.metadata_schema["required_values"]
        .as_object()
        .expect("the contract declares required_values");
    let markers = queue_markers(true);
    for (key, value) in markers.as_object().unwrap() {
        assert_eq!(
            required.get(key),
            Some(value),
            "queue marker {key} drifted from the contract"
        );
    }
    assert_eq!(
        required["request"],
        json!({"original_request": decided_original(&spec)}),
        "the admitted request is the decided original, nothing else"
    );
    for key in markers.as_object().unwrap().keys() {
        assert!(
            spec.metadata.get(key).is_none(),
            "{key} is declared twice: workflow metadata describes, required_values admits"
        );
    }
}

struct Served {
    wire: crate::steps::Wire,
    jobs: Arc<InMemoryJobs>,
    bus: Arc<boss_testing::RecordingEventBus>,
}

async fn serve() -> Served {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> =
        Arc::new(FakePolicyClient::builder().with_default_rules().build());
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
    let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in boss_jobs::registry::seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let state = boss_jobs::http::JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            publisher.clone(),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new().with_agent(AGENT, [LOGIN]));
    let door = Arc::new(boss_jobs::agents::LoginDoor::new(agents, publisher));
    let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn_with_state(
        door,
        boss_jobs::agents::resolve_login,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let wire = crate::steps::Wire::at(
        format!("http://{addr}"),
        Some(crate::identity::Caller {
            id: LOGIN.into(),
            source: crate::identity::Source::Env,
        }),
    )
    .unwrap();
    Served { wire, jobs, bus }
}

async fn file(wire: &crate::steps::Wire, metadata: Value) -> Result<String> {
    crate::job::file_on(
        wire,
        crate::job::Filing {
            kind: KIND,
            title: "Assess the retained original preparation",
            priority: None,
            subject_id: None,
            channel: None,
            metadata: Some(metadata),
        },
        crate::job::Origin {
            source: None,
            area: None,
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_assessment_is_admitted_only_where_the_conductor_queue_sees_it() {
    let served = serve().await;
    let request = json!({"original_request": decided_original(&contract())});
    let routed = {
        let mut metadata = queue_markers(true);
        metadata["request"] = request.clone();
        metadata
    };
    let without = |key: &str| {
        let mut metadata = routed.clone();
        metadata.as_object_mut().unwrap().remove(key);
        metadata
    };
    let with = |key: &str, value: Value| {
        let mut metadata = routed.clone();
        metadata[key] = value;
        metadata
    };
    // The controls. The first is 28e96fbc's own admitted shape.
    for (why, metadata, names) in [
        (
            "both markers absent",
            json!({"request": request}),
            "diagnostic_",
        ),
        (
            "executor absent",
            without("diagnostic_executor"),
            "diagnostic_executor",
        ),
        (
            "executor wrong",
            with("diagnostic_executor", json!("automation:someone-else")),
            "diagnostic_executor",
        ),
        (
            "assessment marker absent",
            without("diagnostic_recovery_assessment"),
            "diagnostic_recovery_assessment",
        ),
        (
            "assessment marker wrong",
            with("diagnostic_recovery_assessment", json!("v2")),
            "diagnostic_recovery_assessment",
        ),
        ("request absent", without("request"), "request"),
        (
            "request wrong",
            with("request", json!({"original_request": uuid::Uuid::new_v4()})),
            "request",
        ),
    ] {
        let refusal = file(&served.wire, metadata).await.expect_err(why);
        assert!(format!("{refusal:#}").contains(names), "{why}: {refusal:#}");
        let (stored, _) = served
            .jobs
            .list_jobs(&JobFilter::default(), 10, 0)
            .await
            .unwrap();
        assert!(
            stored.is_empty(),
            "{why}: a refused admission left a packet"
        );
        assert!(
            served.jobs.recorded_events().is_empty(),
            "{why}: a refused admission left events"
        );
        assert!(
            served
                .bus
                .events()
                .iter()
                .all(|e| !e.kind.starts_with("jobs.") && !e.kind.starts_with("step.")),
            "{why}: a refused admission published"
        );
    }

    // The supported filing: admitted, and visible to the drain's own read.
    let report = file(&served.wire, routed.clone())
        .await
        .expect("the routed filing");
    let (stored, _) = served
        .jobs
        .list_jobs(&JobFilter::default(), 10, 0)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "{report}");
    let id = stored[0].id.to_string();
    let page = served
        .wire
        .call(
            reqwest::Method::GET,
            &queue_path(&queue_markers(false), "open", 0),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let ours: Vec<&Value> = page["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| {
            carries_markers(job, &queue_markers(false))
                && find_step(job, "execute").is_some_and(|step| !step_done(Some(step)))
        })
        .collect();
    assert_eq!(ours.len(), 1, "the drain's read does not see it: {page}");
    assert_eq!(ours[0]["id"], id.as_str());
    assert!(
        carries_markers(ours[0], &queue_markers(true)),
        "the recovery scope drops it"
    );

    // The original request is retained at admission exactly as sent —
    // on the packet, on its execute step, and in the created event that
    // recovery::advance binds the step's request to.
    let job = served.wire.packet(&id).await.unwrap();
    assert_eq!(job["metadata"]["request"], request);
    let execute = find_step(&job, "execute").unwrap();
    assert_eq!(execute["metadata"]["request"], request);
    assert_eq!(execute["assignee_id"], ACTOR);
    let created: Vec<_> = served
        .jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::events::JOB_CREATED)
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].payload["metadata"]["request"], request);

    // And no later write can make it unroutable again.
    let erased = served
        .wire
        .patch_job_metadata(&id, json!({"diagnostic_recovery_assessment": null}))
        .await;
    assert!(erased.is_err(), "a PATCH erased the queue marker");
    let job = served.wire.packet(&id).await.unwrap();
    assert!(carries_markers(&job, &queue_markers(true)));
}
