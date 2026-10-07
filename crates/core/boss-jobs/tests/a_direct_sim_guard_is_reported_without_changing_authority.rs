use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::role_guard::RoleGuardReporter;
use boss_policy_client::role_reader::{RegistryRoles, RoleSnapshotClock, SnapshotRoleReader};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{AccessTier, FakePolicyClient, User};
use boss_testing::RecordingEventBus;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

#[tokio::test]
async fn role_claimable_packet_visibility_is_reported_under_the_original_grant() {
    exercise_claimable_role_guard("read").await;
}

#[tokio::test]
async fn role_claimable_step_write_is_reported_under_the_original_grant() {
    exercise_claimable_role_guard("write").await;
}

#[tokio::test]
async fn the_claims_direct_role_match_is_reported_without_judging_a_missing_policy_answer() {
    exercise_claimable_role_guard("claim").await;
}

#[tokio::test]
async fn assignment_visibility_observes_each_captured_row_once() {
    exercise_claimable_role_guard("assignments").await;
}

#[tokio::test]
async fn complete_assignment_scope_compares_without_an_extra_packet_read() {
    exercise_claimable_role_guard("assignments-all").await;
}

async fn exercise_claimable_role_guard(operation: &str) {
    let assignments = operation.starts_with("assignments");
    let write = operation == "write";
    use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
    use boss_jobs::JobsRepository;
    use boss_policy_client::{Action, Resource, Scope};
    let snapshot = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(Clock),
    ));
    let ticket = snapshot.begin_refresh();
    assert!(snapshot.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":"agent-control", "aliases":[], "role":"other-role"}],"total":1}),
        json!({"data":[],"total":0}), json!([]),
    ).unwrap())));
    let tally = Arc::new(ReportTally::new(10));
    let jobs = Arc::new(InMemoryJobs::new());
    let mut job = Job::new(
        "work",
        Subject::new("asset", "fixture"),
        "Other owner",
        "other-owner",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Claimable", 0);
    step.status = StepStatus::Ready;
    step.metadata = json!({"authority_role":"brewer"});
    jobs.add_step(&step).await.unwrap();
    let bus = RecordingEventBus::new();
    let bus_port: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        role_guards: Some(Arc::new(RoleGuardReporter::new(
            snapshot,
            tally.clone(),
            Arc::new(ReportMode::Report),
        ))),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_port, "jobs"),
            Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "brewer",
                        Action::Read,
                        Resource::job(),
                        if operation == "assignments-all" {
                            Scope::All
                        } else {
                            Scope::Self_
                        },
                    )
                    .allow("brewer", Action::Update, Resource::step(), Scope::Self_)
                    .build(),
            ),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let user = User {
        id: "agent-control".into(),
        role: "brewer".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let request = if operation == "claim" {
        Request::post(format!("/api/jobs/{}/steps/{}/claim", job.id, step.id))
            .header("x-boss-user", serde_json::to_string(&user).unwrap())
            .body(Body::empty())
            .unwrap()
    } else if write {
        Request::put(format!("/api/jobs/{}/steps/{}", job.id, step.id))
            .header("content-type", "application/json")
            .header("x-boss-user", serde_json::to_string(&user).unwrap())
            .body(Body::from(json!({"title":"Updated"}).to_string()))
            .unwrap()
    } else {
        Request::get(if assignments {
            "/api/jobs/assignments?roles=brewer".to_owned()
        } else {
            format!("/api/jobs/{}", job.id)
        })
        .header("x-boss-user", serde_json::to_string(&user).unwrap())
        .body(Body::empty())
        .unwrap()
    };
    let response = router(state).oneshot(request).await.unwrap();
    if operation == "claim" {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().status,
            StepStatus::Active
        );
        let report = tally.snapshot();
        let observations: Vec<_> = report
            .rows
            .iter()
            .filter(|row| row.observation.resource == "claim-authority-role-match")
            .collect();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observation.would_change_scope, Some(true));
        assert_eq!(observations[0].observation.would_deny, None);
        return;
    }
    if assignments {
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows["total"], 1, "original role queue stays visible");
        assert_eq!(rows["data"][0]["step"]["id"], step.id.to_string());
        let report = tally.snapshot();
        assert_eq!(
            report.rows.len(),
            1,
            "one final observation per captured row"
        );
        let observation = &report.rows[0].observation;
        assert_eq!(observation.resource, "assignment-row-under-granted-scope");
        assert_eq!(observation.asserted_allowed, Some(true));
        assert_eq!(
            observation.recorded_allowed,
            if operation == "assignments-all" {
                Some(true)
            } else {
                None
            }
        );
        assert_eq!(
            observation.lookup_status,
            if operation == "assignments-all" {
                "registered"
            } else {
                "context-unavailable"
            }
        );
        assert_eq!(observation.would_deny, None);
        return;
    }
    if write {
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().title,
            "Updated"
        );
    }
    assert_eq!(
        response.status(),
        if write {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::OK
        },
        "original role claim preserves the native response"
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), if write { 2 } else { 1 });
    let resource = if write {
        "step-write-under-granted-scope"
    } else {
        "packet-read-under-granted-scope"
    };
    let admission: Vec<_> = report
        .rows
        .iter()
        .filter(|row| row.observation.resource == resource)
        .collect();
    assert_eq!(
        admission.len(),
        1,
        "each actual admission boundary is observed once"
    );
    if write {
        let attribution: Vec<_> = report
            .rows
            .iter()
            .filter(|row| row.observation.resource == "step-writer-proxy")
            .collect();
        assert_eq!(attribution.len(), 1);
        assert_eq!(attribution[0].observation.action, "attribution");
        assert_eq!(attribution[0].observation.would_deny, None);
    }
    assert_eq!(admission[0].observation.asserted_allowed, Some(true));
    assert_eq!(admission[0].observation.recorded_allowed, Some(false));
    assert_eq!(
        admission[0].observation.would_deny,
        if write { Some(true) } else { None },
        "visibility is not a mutation denial"
    );
}

struct Clock;
impl RoleSnapshotClock for Clock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[tokio::test]
async fn the_actual_sim_guard_keeps_its_answer_and_reports_the_recorded_role() {
    for (asserted, recorded, expected) in [
        ("platform-admin", "visitor", StatusCode::OK),
        ("visitor", "platform-admin", StatusCode::FORBIDDEN),
    ] {
        let snapshot = Arc::new(SnapshotRoleReader::new(
            Duration::from_secs(30),
            Arc::new(Clock),
        ));
        let ticket = snapshot.begin_refresh();
        assert!(snapshot.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
            json!({"data":[{"id":"agent-control", "aliases":[], "role":recorded}],"total":1}),
            json!({"data":[],"total":0}), json!([]),
        ).unwrap())));
        let tally = Arc::new(ReportTally::new(10));
        let bus = RecordingEventBus::new();
        let bus_port: Arc<dyn EventBus> = bus.clone();
        let state = JobsApiState {
            role_guards: Some(Arc::new(RoleGuardReporter::new(
                snapshot,
                tally.clone(),
                Arc::new(ReportMode::Report),
            ))),
            ..JobsApiState::minimal(
                Arc::new(InMemoryJobs::new()),
                bus,
                DomainPublisher::new(bus_port, "jobs"),
                Arc::new(FakePolicyClient::builder().build()),
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let user = User {
            id: "agent-control".into(),
            role: asserted.into(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        };
        let response = router(state)
            .oneshot(
                Request::post("/api/jobs/sim-clock/pause")
                    .header("x-boss-user", serde_json::to_string(&user).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let report = tally.snapshot();
        assert_eq!(
            report.rows.len(),
            1,
            "the actual authority reader must report"
        );
        assert_eq!(report.rows[0].observation.resource, "sim-clock-control");
        assert_eq!(
            report.rows[0].observation.asserted_allowed,
            Some(asserted == "platform-admin")
        );
        assert_eq!(
            report.rows[0].observation.recorded_allowed,
            Some(recorded == "platform-admin")
        );
    }
}
