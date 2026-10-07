//! Generic inclusion/complement and server ordering preserve complete paged job scope.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day(2026, 9, 14)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    let state = JobsApiState::minimal(jobs.clone(), bus, publisher, policy, clock);
    (router(state), jobs)
}

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn packet(n: u8, kind: &str, title: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        status: JobStatus::Open,
        metadata,
        ..Job::new(
            kind,
            Subject::new("asset", "BOSSNET"),
            title,
            "emp-david",
            Priority::Standard,
            day(2026, 9, 1),
        )
    }
}

async fn get(app: &Router, query: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
    let (status, body) = get(app, query).await;
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

fn titles(body: &serde_json::Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|j| j["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn kind_sets_and_their_complement_filter_before_count_and_page() {
    let (app, jobs) = app();
    for (n, kind) in [(1, "machine-a"), (2, "machine-b"), (3, "historic-unknown")] {
        jobs.create_job(&packet(n, kind, kind, serde_json::json!({})))
            .await
            .unwrap();
    }
    let included = list(
        &app,
        "kinds=%5B%22machine-a%22%2C%22machine-b%22%5D&limit=1",
    )
    .await;
    assert_eq!(included["total"], 2);
    assert_eq!(titles(&included).len(), 1);
    let other = list(
        &app,
        "exclude_kinds=%5B%22machine-a%22%2C%22machine-b%22%5D&limit=1",
    )
    .await;
    assert_eq!(other["total"], 1);
    assert_eq!(titles(&other), ["historic-unknown"]);
    assert_eq!(list(&app, "kinds=%5B%5D").await["total"], 0);
    assert_eq!(list(&app, "exclude_kinds=%5B%5D").await["total"], 3);
    let intersection = list(
        &app,
        "kinds=%5B%22machine-a%22%2C%22machine-b%22%5D&exclude_kinds=%5B%22machine-b%22%5D",
    )
    .await;
    assert_eq!(titles(&intersection), ["machine-a"]);
    assert_eq!(intersection["total"], 1);
}

#[tokio::test]
async fn malformed_kind_sets_and_unknown_order_are_refused() {
    let (app, _) = app();
    for query in [
        "kinds=machine-a",
        "kinds=%5B1%5D",
        "kinds=%5B%22%22%5D",
        "exclude_kinds=null",
        "exclude_kinds=%5B%22%20%22%5D",
        "order=sideways",
    ] {
        assert_eq!(get(&app, query).await.0, StatusCode::BAD_REQUEST, "{query}");
    }
}

#[tokio::test]
async fn oldest_order_is_applied_before_pagination() {
    let (app, jobs) = app();
    for n in 1..=3 {
        let mut job = packet(
            n,
            "arbitrary",
            &format!("packet-{n}"),
            serde_json::json!({}),
        );
        job.opened_at = Some(
            day(2026, 9, 1)
                .and_hms_opt(10, n.into(), 0)
                .unwrap()
                .and_utc(),
        );
        jobs.create_job_at(&job, job.opened_at.unwrap(), &[])
            .await
            .unwrap();
    }
    let oldest = list(&app, "order=oldest&limit=1").await;
    assert_eq!(titles(&oldest), ["packet-1"]);
    assert_eq!(oldest["total"], 3);
    assert_eq!(
        titles(&list(&app, "order=oldest&limit=1&offset=2").await),
        ["packet-3"]
    );
    assert_eq!(titles(&list(&app, "limit=1").await), ["packet-3"]);
}
