//! `infra/platform/workflows/release-milestone.toml` is the protocol that
//! puts a dated release milestone on a person's calendar (backlog
//! fe199985, design e1dba350, folded into docs/architecture-decisions.md
//! §Calendar).
//!
//! The gap it closes, measured 2026-11-15 at origin/main 8350553c:
//! /ux/calendar/me renders reservations, the step reservation hook is
//! wired (`calendar_api_url`, aa6b4b5c) and reads the StepType's
//! `duration_minutes` (a0b8e5bd), and still no live protocol could
//! produce a reservation — the registry held no `release-milestone`
//! row (GET /api/workflows/release-milestone/versions answered `[]`,
//! control `cut-a-release` answered its v1) and emp-david's calendar
//! for 2026-2027 answered `[]`.
//!
//! So the second test drives the bundled row through the create
//! handler, the step metadata merge door and the claim door, and reads
//! the reservation the calendar was asked for: the protocol, not a
//! fixture spec, is what reserves.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_calendar_client::{CalendarClient, FakeCalendarClient, FakeCall};
use boss_core::job::StepStatus;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::audience::Audience;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{WorkflowRegistry, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::json;
use tower::ServiceExt;

const KIND: &str = "release-milestone";

fn bundled() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == KIND)
        .unwrap_or_else(|| panic!("{KIND} ships in the platform bundle"))
}

/// One packet per milestone, one dated step, one way to finish. The
/// date is written by whoever schedules it, never preset by the row
/// (`metadata_defaults` cannot template one), and the step names no
/// individual: an `individual` audience in the platform bundle is
/// claim-for authority and names an automation (S3 of design 611fbffd),
/// so whose calendar carries the date is the claim's `claimed_for`.
#[test]
fn a_release_milestone_is_one_dated_scheduling_step() {
    let wf = bundled();
    let required: Vec<&str> = wf
        .metadata_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|k| k.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, ["version", "milestone"]);
    let titles: Vec<&str> = wf.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["set", "schedule", "met"]);

    let schedule = &wf.steps[1];
    assert_eq!(schedule.kind, "scheduling");
    assert_eq!(schedule.ready_when, "steps.set.done");
    assert!(
        !matches!(schedule.audience, Some(Audience::Individual(_))),
        "the schedule step names no individual: {:?}",
        schedule.audience
    );
    assert_eq!(
        schedule.metadata_defaults.get("location"),
        Some(&json!("online")),
        "the scheduling StepType requires a location at done"
    );
    for preset in ["scheduled_at", "duration_minutes"] {
        assert!(
            schedule.metadata_defaults.get(preset).is_none(),
            "the row presets no {preset}: the scheduler writes the date"
        );
    }

    let met = &wf.steps[2];
    assert_eq!(met.kind, "outcome");
    assert_eq!(met.ready_when, "steps.schedule.done");
    assert_eq!(
        met.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("met")
    );
}

fn header(id: &str) -> String {
    json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", header("emp-david"))
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Filing a milestone, writing its date and claiming the `schedule`
/// step reserves exactly that window on the claimant's calendar, keyed
/// on the step — the row `GET /api/calendar/reservations` answers and
/// /ux/calendar/me renders.
#[tokio::test]
async fn scheduling_a_milestone_reserves_its_date_on_the_claimants_calendar() {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(bundled()).expect("the bundled row seeds");
    let kind_registry: Arc<dyn WorkflowRegistry> = kinds;
    let jobs = Arc::new(InMemoryJobs::new());
    let calendar = Arc::new(FakeCalendarClient::new());
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
    let app = router(JobsApiState {
        kind_registry: Some(kind_registry),
        calendar: Some(calendar.clone() as Arc<dyn CalendarClient>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });

    let (status, body) = send(
        &app,
        "POST",
        "/api/jobs",
        json!({
            "kind": KIND,
            "status": "open",
            "title": "0.2.0 — first tagged release",
            "owner_id": "emp-david",
            "priority": "standard",
            "opened_on": "2026-11-15",
            "tags": [],
            "subject": { "subject_kind": "custom", "id": "boss" },
            "metadata": { "version": "0.2.0", "milestone": "first tagged release" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    let job = stored.first().expect("the milestone packet is stored");
    let steps = jobs.list_steps(&job.id).await.unwrap();
    let schedule = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("schedule"))
        .expect("the schedule step materialized");
    assert_eq!(
        schedule.status,
        StepStatus::Ready,
        "the set trigger is born completed"
    );

    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{}/steps/{}/metadata", job.id, schedule.id),
        json!({ "scheduled_at": "2026-12-01T17:00:00Z", "duration_minutes": 60 }),
    )
    .await;
    assert!(status.is_success(), "the date merges: {status} {body}");
    assert!(
        calendar.calls().is_empty(),
        "writing a date reserves nothing until the step starts"
    );

    let (status, body) = send(
        &app,
        "POST",
        &format!("/api/jobs/{}/steps/{}/claim", job.id, schedule.id),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let reserved: Vec<_> = calendar
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::Reserve(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(reserved.len(), 1, "one milestone, one hold: {reserved:?}");
    let r = &reserved[0];
    assert_eq!(r.subject.kind, "employee");
    assert_eq!(r.subject.id, "emp-david");
    assert_eq!(r.reason_ref_id, schedule.id.to_string());
    assert_eq!(r.window.start.to_rfc3339(), "2026-12-01T17:00:00+00:00");
    assert_eq!(r.window.end.to_rfc3339(), "2026-12-01T18:00:00+00:00");
}
