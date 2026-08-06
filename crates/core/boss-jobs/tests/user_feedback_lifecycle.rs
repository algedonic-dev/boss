//! Layer 4: the feedback flow can actually be driven from open to
//! closed through the real HTTP surface.
//!
//! `boss-jobs` had fifteen integration tests and not one of them
//! opened a Job and closed it. That hole shipped a `user-feedback`
//! JobKind whose triage step could never complete: it used the
//! `acknowledgment` kind, whose schema requires `document_title`, and
//! metadata validators run at `completed` rather than at create. So
//! the Job materialized cleanly, sat in the triage board's waiting
//! column looking healthy, and returned
//! `400 … required field 'document_title' is missing` the first time
//! a human tried to act on it. The bug reached an operator because
//! every layer below this one was satisfied.
//!
//! The lib test `user_feedback_steps_close_without_operator_supplied_fields`
//! covers the same defect at the spec, which is cheaper and names the
//! offending kind directly. This one is deliberately not redundant
//! with it: it drives the real router, so it also covers a wrong
//! `ready_when`, a blocker gate that never opens, and a terminal that
//! never fires — none of which the spec test can see.
//!
//! Scoped to `user-feedback` on purpose. The invariant worth having
//! is "every platform JobKind can be driven from open to closed using
//! only what its own surfaces supply", but the other two kinds are
//! driven by authoring UIs that DO supply fields (`job-kind-design`'s
//! publish step takes a `job_kind_spec`), so generalizing needs a
//! per-kind fixture describing what each surface posts. Feedback is
//! the one flow where the answer is "nothing", which is exactly why
//! it is the one that broke.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::platform_kinds;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobKinds, InMemoryJobs, JobKindRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The triage step's `authority_role` is `platform-admin`, and the
/// JobKind's `owner_role` is too — so the roster must hold one or the
/// create handler rejects the Job for having no human owner.
struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryJobKinds::new());
    // Seeded from the real platform registry, not a hand-built spec —
    // a fixture copy would have kept passing while the shipped kind
    // was broken.
    for spec in platform_kinds() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
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
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn JobKindRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// Exactly the body `FeedbackControl.svelte` posts from the chrome bar.
fn submit_feedback_body() -> String {
    serde_json::json!({
        "kind": "user-feedback",
        "subject": { "subject_kind": "custom", "id": "/ux/jobs" },
        "title": "Feedback on /ux/jobs",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": {
            "message": "The column picker forgets my choice.",
            "route": "/ux/jobs",
            "submitted_by": "emp-bootstrap-admin",
        },
        "tags": ["feedback"],
    })
    .to_string()
}

#[tokio::test]
async fn feedback_submitted_from_the_chrome_bar_can_be_triaged_to_closed() {
    let app = app();

    // 1. Someone sends feedback.
    let (status, job) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(submit_feedback_body()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    // 2. Drive every actionable step the way the triage board does —
    //    a bare status flip carrying no operator-supplied fields,
    //    because no surface in this flow collects any. Looping until
    //    nothing is actionable covers the steps that only become ready
    //    once triage completes.
    for round in 0..8 {
        let (status, current) = send(
            &app,
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs/{job_id}"))
                .header("x-boss-user", admin_header())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "read failed: {current}");

        if current["status"] == "closed" {
            return;
        }

        let steps = current["steps"].as_array().cloned().unwrap_or_default();
        let actionable: Vec<&serde_json::Value> = steps
            .iter()
            .filter(|s| s["status"] == "ready" || s["status"] == "active")
            .collect();
        assert!(
            !actionable.is_empty(),
            "round {round}: Job is neither closed nor actionable — no step is \
             ready and none can be, so this feedback would sit on the board \
             forever. Steps: {steps:#?}"
        );

        for step in actionable {
            let step_id = step["id"].as_str().expect("step id");
            let (status, body) = send(
                &app,
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
                    .header("content-type", "application/json")
                    .header("x-boss-user", admin_header())
                    .body(Body::from(
                        serde_json::json!({ "status": "completed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
            assert!(
                status.is_success(),
                "completing step `{}` (kind `{}`) failed with {status} — the \
                 feedback flow supplies no metadata beyond what the JobKind \
                 sets, so a step needing more can never be closed by an \
                 operator: {body}",
                step["title"].as_str().unwrap_or("?"),
                step["kind"].as_str().unwrap_or("?"),
            );
        }
    }

    panic!("feedback Job did not reach `closed` after 8 rounds of completing every ready step");
}
