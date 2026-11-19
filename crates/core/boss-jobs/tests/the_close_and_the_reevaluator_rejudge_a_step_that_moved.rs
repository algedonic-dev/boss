//! The two writers that retry a refused step write — the readiness
//! re-evaluator and the terminal close's skip — re-judge the step as it
//! NOW stands, and never record what the row refused (backlog 6ec22d71).
//!
//! Both write a step they read through a LIST read, whole-row, through
//! the judged door (car 88123ae0). Neither path had a test of its
//! refusal: the in-memory hook the first tests used fired only on
//! `get_step`, which neither reads through. Here another writer lands
//! inside each one's window through `change_before_next_judged_write`,
//! which does not care which read was taken:
//!
//! - an assignment of a pending step, just before the re-evaluator
//!   promotes it: the promotion must keep the holder;
//! - a skip of that step by another writer: the promotion must not
//!   record a `ready` the row refused (the rebuild would replay it);
//! - an assignment of an open step, just before the terminal close
//!   skips it: the skip must keep the holder;
//! - a completion of that step: the close must not record a `skipped`
//!   the row refused.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, StepId, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn step_spec(title: &str, ready_when: &str, terminal: Option<&str>) -> StepSpec {
    StepSpec {
        title: title.into(),
        kind: "task".into(),
        ready_when: ready_when.into(),
        title_template: title.into(),
        authority_role: Some("platform-admin".into()),
        terminal: terminal.map(|o| Terminal { outcome: o.into() }),
        ..Default::default()
    }
}

fn spec(kind: &str, steps: Vec<StepSpec>) -> WorkflowSpec {
    let mut spec =
        WorkflowSpec::platform_seed(kind, kind, "platform", vec!["custom".into()], steps);
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

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

/// `promotes`: work → review, where review waits on work.
/// `closes`: finish (a declared terminal) beside other, both open at once.
fn app_with_registry() -> (axum::Router, Arc<InMemoryJobs>, Arc<InMemoryWorkflows>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds
        .seed(spec(
            "promotes",
            vec![
                step_spec("work", "true", None),
                step_spec("review", "steps.work.done", None),
            ],
        ))
        .expect("seed the promoting kind");
    kinds
        .seed(spec(
            "closes",
            vec![
                step_spec("finish", "true", Some("done")),
                step_spec("other", "true", None),
            ],
        ))
        .expect("seed the closing kind");
    kinds
        .seed(spec(
            "promotes-too",
            vec![
                step_spec("work", "true", None),
                step_spec("review", "steps.work.done", None),
            ],
        ))
        .unwrap();
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
            .allow("audit-readonly", Action::Read, Resource::job(), Scope::All)
            .allow("own-reader", Action::Read, Resource::job(), Scope::Self_)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
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
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, kinds)
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let (app, jobs, _) = app_with_registry();
    (app, jobs)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn open_job(app: &axum::Router, kind: &str) -> JobId {
    let (status, body) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": kind,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": format!("A {kind} packet"),
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {body}");
    let job: serde_json::Value = serde_json::from_str(&body).expect("job json");
    JobId::from_uuid(uuid::Uuid::parse_str(job["id"].as_str().expect("job id")).expect("uuid"))
}

async fn step_titled(jobs: &InMemoryJobs, job_id: &JobId, title: &str) -> boss_core::job::Step {
    jobs.list_steps(job_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("no step titled {title}"))
}

/// The status alone: the stored metadata it used to send back wrote
/// nothing, and the step PUT refuses any body carrying metadata since
/// e39a9d2a.
async fn complete(app: &axum::Router, jobs: &InMemoryJobs, job_id: &JobId, title: &str) {
    let step = step_titled(jobs, job_id, title).await;
    let (status, body) = send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{}", step.id))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "completion refused: {body}");
}

/// Every `jobs.step.updated` recorded for `step`, as the status it carried.
fn recorded_statuses(jobs: &InMemoryJobs, step: &StepId) -> Vec<String> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::events::STEP_UPDATED)
        .filter(|e| e.payload["step_id"] == step.to_string())
        .map(|e| e.payload["status"].as_str().unwrap_or("?").to_string())
        .collect()
}

#[tokio::test]
async fn a_periodic_readiness_sweep_recovers_a_committed_write_without_its_followup() {
    let (app, jobs) = app();
    let id = open_job(&app, "promotes").await;
    let mut work = step_titled(&jobs, &id, "work").await;
    let review = step_titled(&jobs, &id, "review").await;
    // The writer committed the row, then died before its readiness pass.
    // No idempotent resend is made: this is the generic recovery gap.
    work.status = StepStatus::Completed;
    jobs.update_step(&work).await.unwrap();
    assert_eq!(
        jobs.get_step(&review.id).await.unwrap().unwrap().status,
        StepStatus::Pending
    );
    let request = |method: &str| {
        Request::builder()
            .method(method)
            .uri("/api/jobs/readiness")
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap()
    };
    let (status, body) = send(&app, request("GET")).await;
    assert_eq!(status, StatusCode::OK, "readiness audit: {body}");
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["complete"], true);
    assert_eq!(audit["satisfied_pending"], 1);
    assert_eq!(
        jobs.get_step(&review.id).await.unwrap().unwrap().status,
        StepStatus::Pending,
        "audit never writes"
    );
    let (status, body) = send(&app, request("POST")).await;
    assert_eq!(status, StatusCode::OK, "readiness repair: {body}");
    assert_eq!(
        jobs.get_step(&review.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    assert_eq!(recorded_statuses(&jobs, &review.id), vec!["ready"]);
    let (status, body) = send(&app, request("POST")).await;
    assert_eq!(status, StatusCode::OK, "idempotent second pass: {body}");
    assert_eq!(
        recorded_statuses(&jobs, &review.id),
        vec!["ready"],
        "retry records no duplicate promotion"
    );
    let (_, body) = send(&app, request("GET")).await;
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["satisfied_pending"], 0);
    assert_eq!(audit["complete"], true);
}

#[tokio::test]
async fn the_sweep_recovers_multiple_protocols_and_leaves_an_unsatisfied_guard_alone() {
    let (app, jobs) = app();
    let mut reviews = Vec::new();
    for kind in ["promotes", "promotes-too"] {
        let id = open_job(&app, kind).await;
        let mut work = step_titled(&jobs, &id, "work").await;
        reviews.push(step_titled(&jobs, &id, "review").await);
        work.status = StepStatus::Completed;
        jobs.update_step(&work).await.unwrap();
    }
    let waiting = open_job(&app, "promotes").await;
    let waiting = step_titled(&jobs, &waiting, "review").await;
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs/readiness")
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for review in reviews {
        assert_eq!(
            jobs.get_step(&review.id).await.unwrap().unwrap().status,
            StepStatus::Ready
        );
        assert_eq!(recorded_statuses(&jobs, &review.id), vec!["ready"]);
    }
    assert_eq!(
        jobs.get_step(&waiting.id).await.unwrap().unwrap().status,
        StepStatus::Pending
    );
}

#[tokio::test]
async fn unreadable_steps_are_an_incomplete_audit_not_a_clean_zero() {
    let (app, jobs) = app();
    let id = open_job(&app, "promotes").await;
    jobs.fail_steps_read(&id);
    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/api/jobs/readiness")
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["complete"], false);
    assert!(
        report["errors"][0]
            .as_str()
            .unwrap()
            .contains(&id.to_string()),
        "{body}"
    );
    assert!(
        report["errors"][0].as_str().unwrap().contains("unreadable"),
        "{body}"
    );
}

#[tokio::test]
async fn a_denied_audit_does_not_report_a_clean_empty_estate() {
    let (app, _) = app();
    for method in ["GET", "POST"] {
        let (status, body) = send(
            &app,
            Request::builder()
                .method(method)
                .uri("/api/jobs/readiness")
                .header(
                    "x-boss-user",
                    r#"{"id":"emp-untrusted","role":"guest","access_tier":"user"}"#,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test]
async fn recovery_reads_the_packets_pinned_version_not_the_latest_protocol() {
    let (app, jobs, kinds) = app_with_registry();
    let id = open_job(&app, "promotes").await;
    let review = step_titled(&jobs, &id, "review").await;
    let mut work = step_titled(&jobs, &id, "work").await;
    work.status = StepStatus::Completed;
    jobs.update_step(&work).await.unwrap();
    let mut replacement = spec(
        "promotes",
        vec![
            step_spec("work", "true", None),
            step_spec("review", "false", None),
        ],
    );
    replacement.version = 2;
    kinds.seed(replacement).unwrap();
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs/readiness")
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        jobs.get_step(&review.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    assert_eq!(
        jobs.get_job(&id).await.unwrap().unwrap().workflow_version,
        1
    );
}

#[tokio::test]
async fn audit_authority_does_not_grant_repair_and_a_scoped_zero_is_not_global() {
    let (app, _) = app();
    for (role, global) in [("audit-readonly", true), ("own-reader", false)] {
        let caller =
            serde_json::json!({"id":"emp-auditor","role":role,"access_tier":"user"}).to_string();
        let request = |method: &str| {
            Request::builder()
                .method(method)
                .uri("/api/jobs/readiness")
                .header("x-boss-user", &caller)
                .body(Body::empty())
                .unwrap()
        };
        let (status, body) = send(&app, request("GET")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let report: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(report["global"], global);
        let (status, body) = send(&app, request("POST")).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "read authority is not write authority: {body}"
        );
    }
}

#[tokio::test]
async fn a_promotion_computed_before_an_assignment_is_re_judged_and_keeps_the_holder() {
    let (app, jobs) = app();
    let job_id = open_job(&app, "promotes").await;
    let review = step_titled(&jobs, &job_id, "review").await;
    assert_eq!(review.status, StepStatus::Pending, "precondition");

    jobs.change_before_next_judged_write(&review.id, |row| {
        row.assignee_id = Some("emp-assigned-meanwhile".into());
    });
    complete(&app, &jobs, &job_id, "work").await;

    let stored = jobs.get_step(&review.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Ready,
        "the refused promotion was re-judged over the row as it now stands, and landed"
    );
    assert_eq!(
        stored.assignee_id.as_deref(),
        Some("emp-assigned-meanwhile"),
        "and the promotion did not erase the holder another writer set"
    );
}

#[tokio::test]
async fn a_promotion_over_a_step_another_writer_finished_records_no_ready() {
    let (app, jobs) = app();
    let job_id = open_job(&app, "promotes").await;
    let review = step_titled(&jobs, &job_id, "review").await;
    let before = recorded_statuses(&jobs, &review.id);

    jobs.change_before_next_judged_write(&review.id, |row| {
        row.status = StepStatus::Skipped;
    });
    complete(&app, &jobs, &job_id, "work").await;

    let stored = jobs.get_step(&review.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Skipped, "the row keeps its skip");
    let after = recorded_statuses(&jobs, &review.id);
    assert_eq!(
        after[before.len()..]
            .iter()
            .filter(|s| *s == "ready")
            .count(),
        0,
        "no jobs.step.updated says `ready` for a row that stayed skipped — \
         a rebuild would replay it and demote the step: {after:?}"
    );
}

#[tokio::test]
async fn a_terminal_close_skips_the_step_as_it_now_stands() {
    let (app, jobs) = app();
    let job_id = open_job(&app, "closes").await;
    let other = step_titled(&jobs, &job_id, "other").await;

    jobs.change_before_next_judged_write(&other.id, |row| {
        row.assignee_id = Some("emp-assigned-meanwhile".into());
    });
    complete(&app, &jobs, &job_id, "finish").await;

    let stored = jobs.get_step(&other.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Skipped,
        "the close still skipped it"
    );
    assert_eq!(
        stored.assignee_id.as_deref(),
        Some("emp-assigned-meanwhile"),
        "from the row as it now stands, erasing nothing"
    );
}

#[tokio::test]
async fn a_terminal_close_records_no_skip_of_a_step_that_finished_in_its_window() {
    let (app, jobs) = app();
    let job_id = open_job(&app, "closes").await;
    let other = step_titled(&jobs, &job_id, "other").await;
    let before = recorded_statuses(&jobs, &other.id);

    jobs.change_before_next_judged_write(&other.id, |row| {
        row.status = StepStatus::Completed;
    });
    complete(&app, &jobs, &job_id, "finish").await;

    let stored = jobs.get_step(&other.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        StepStatus::Completed,
        "the completion stands"
    );
    let after = recorded_statuses(&jobs, &other.id);
    assert_eq!(
        after[before.len()..]
            .iter()
            .filter(|s| *s == "skipped")
            .count(),
        0,
        "no jobs.step.updated says `skipped` for a row that stayed completed: {after:?}"
    );
}
