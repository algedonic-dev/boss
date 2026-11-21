//! A dark message bus degrades the jobs API; it never exits it
//! (backlog 2e45840a, design ad9519bc car 1).
//!
//! Outage 05fd165a, 2026-11-09 23:01Z: the boss pod crash-looped on
//! `connecting to NATS: No route to host` while Postgres, the system of
//! record, stayed up — because `NatsEventBus::connect` failed on a dark
//! bus and the binary `?`-ed it. The write path stopped needing the bus
//! when the transactional outbox landed, so the crash protected nothing.
//!
//! This builds the jobs API the way the binary does — over a bus from
//! `NatsEventBus::connect` — against an address nothing listens on, and
//! holds the design's three claims: the bus constructs, a write commits
//! with its event held in `event_outbox`, a read is served, and a relay
//! pass over the dark bus leaves the event undelivered rather than
//! stamping a publish that never left the process. The second test holds
//! the same for the login door's count (part d of the design): it is
//! staged on the outbox, not published past it and dropped.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::PgJobs;
use boss_jobs::agents::{AgentsRegistry, LoginDoor, PgAgents, UNRESOLVED_LOGIN, resolve_login};
use boss_jobs::http::{JobsApiState, router};
use boss_nats::NatsEventBus;
use boss_policy_client::User;
use boss_policy_client::{AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::TestDb;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const PACKET: &str = "2e45840a-0000-4000-8000-000000000001";

fn ceo() -> String {
    signed_as("emp-cto")
}

fn signed_as(id: &str) -> String {
    serde_json::to_string(&User {
        id: id.into(),
        role: "ceo".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .unwrap()
}

fn app(pool: sqlx::PgPool, bus: Arc<NatsEventBus>) -> Router {
    let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    router(JobsApiState::minimal(
        Arc::new(PgJobs::new(pool)),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dark_bus_serves_reads_and_commits_writes_into_the_outbox() {
    let db = TestDb::new().await;
    // Held for the whole test: the client keeps reconnecting to it, and
    // a released port could be handed to a server that answers.
    let dark = boss_testing::dark_port();
    let bus = Arc::new(
        NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a boot failure"),
    );
    let app = app(db.pool.clone(), bus.clone());

    let packet = Job {
        id: JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()),
        ..Job::new(
            "ad-hoc",
            Subject::new("custom", "bosspipeline"),
            "a write while the bus is dark",
            "emp-cto",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
        )
    };
    let (status, body) = send(
        &app,
        Request::post("/api/jobs")
            .header("x-boss-user", ceo())
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&packet).unwrap()))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "the write commits: {status} {body}");

    let (status, body) = send(
        &app,
        Request::get(format!("/api/jobs/{PACKET}"))
            .header("x-boss-user", ceo())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the read is served: {body}");
    assert!(body.contains(PACKET), "the read returns the packet: {body}");

    // A relay pass over the dark bus: the publish cannot leave the
    // process, so the row must stay owed rather than be stamped
    // delivered off a client-side buffer.
    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("a relay pass over a dark bus is not an error");

    let held: Vec<(String, bool)> = sqlx::query_as(
        "SELECT kind, delivered_at IS NULL FROM event_outbox \
         WHERE payload::text LIKE '%' || $1 || '%' ORDER BY id",
    )
    .bind(PACKET)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        held.iter().any(|(k, _)| k == "jobs.job.created"),
        "the write's event is held in the outbox: {held:?}"
    );
    assert!(
        held.iter().all(|(_, undelivered)| *undelivered),
        "nothing reached a bus that is dark, so nothing is stamped delivered: {held:?}"
    );

    // Part (c): the degraded service looks degraded. The relay's report
    // dates what the outbox holds, so the outage reads as an owed,
    // ageing outbox rather than silence...
    let owed = boss_events::outbox::undelivered(&db.pool).await.unwrap();
    assert!(owed.count > 0, "the outbox owes the bus: {owed:?}");
    assert!(
        owed.oldest_age_seconds.is_some_and(|s| s > 0.0),
        "the oldest owed event has an age: {owed:?}"
    );
    // ...and the API's own health says the bus is down while still
    // answering 200: liveness and readiness never fail on the bus.
    let (status, body) = send(
        &app,
        Request::get("/api/jobs/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health stays 200: {body}");
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["capabilities"]["bus"], "disconnected", "{body}");
}

/// The login door's count rides the outbox too (design ad9519bc part
/// d, first half). Until this car the door published
/// `actor.login.unresolved` straight to the bus, so with the bus dark
/// car 1's guard refused the publish, the door logged ERROR, and the
/// count was gone: the write it counted committed and the fact that it
/// was admitted under an unmapped address did not. Here the same write
/// goes through the door the binary mounts, over the same dark bus, and
/// the count must be held in `event_outbox` for the relay to deliver.
#[tokio::test(flavor = "multi_thread")]
async fn a_dark_bus_still_counts_a_write_under_an_unresolved_login() {
    let db = TestDb::new().await;
    let dark = boss_testing::dark_port();
    let bus = Arc::new(
        NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a boot failure"),
    );
    let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
    let door = Arc::new(LoginDoor::new(
        Arc::new(PgAgents::new(db.pool.clone())) as Arc<dyn AgentsRegistry>,
        publisher,
    ));
    // The binary's order: the door OUTSIDE the request-context layer.
    let app = app(db.pool.clone(), bus.clone())
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login));

    let packet = Job {
        id: JobId::from_uuid(Uuid::parse_str(COUNTED).unwrap()),
        ..Job::new(
            "ad-hoc",
            Subject::new("custom", "bosspipeline"),
            "a write under an unmapped address while the bus is dark",
            "emp-cto",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
        )
    };
    let (status, body) = send(
        &app,
        Request::post("/api/jobs")
            .header("x-boss-user", signed_as(STRANGER))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&packet).unwrap()))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "the write commits: {status} {body}");

    boss_events::outbox::drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("a relay pass over a dark bus is not an error");

    let counted: Vec<(serde_json::Value, bool)> = sqlx::query_as(
        "SELECT payload, delivered_at IS NULL FROM event_outbox WHERE kind = $1 ORDER BY id",
    )
    .bind(UNRESOLVED_LOGIN)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        counted.len(),
        1,
        "one admitted write, one count held in the outbox, none dropped: {counted:?}"
    );
    let (payload, undelivered) = &counted[0];
    assert_eq!(payload["login"], STRANGER, "{payload}");
    assert_eq!(payload["method"], "POST", "{payload}");
    assert_eq!(payload["path"], "/api/jobs", "{payload}");
    assert!(
        *undelivered,
        "the bus is dark, so the count is owed, not stamped delivered"
    );
}

const COUNTED: &str = "2e45840a-0000-4000-8000-000000000002";
const STRANGER: &str = "nobody@example.test";
