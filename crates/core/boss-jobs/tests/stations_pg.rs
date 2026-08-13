//! Postgres-backed coverage for the station registry
//! (116-stations.sql):
//!
//! - the platform seed ships the two SDLC batch stations, active,
//!   with predicates the evaluator can actually parse (a seed row
//!   the Rust shape rejects would be a silent dead station);
//! - every registry write stages its event in `event_outbox` in the
//!   SAME transaction as the stations row (the workflow-registry
//!   posture; the InMemory contract is pinned in `stations::tests`);
//! - the three `jobs.station.*` kinds are registered in
//!   `event_kinds` so the audit trigger admits them.

#![cfg(feature = "postgres")]

use boss_core::actor::ActorId;
use boss_jobs::events::{STATION_DRAFT_SAVED, STATION_PUBLISHED, STATION_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::{PgStations, StationKind, StationRegistry, StationSpec};
use boss_testing::TestDb;

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

fn spec(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        format!("Test {name}"),
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            ..Default::default()
        },
    )
}

async fn outbox_kinds(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read outbox kinds")
}

#[tokio::test(flavor = "multi_thread")]
async fn platform_seed_ships_the_sdlc_batch_stations() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let active = registry.list_active().await.expect("list_active");
    let names: Vec<&str> = active.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["design-review", "loading-dock"],
        "the two platform SDLC batch stations seed active (departments \
         are tenant data; their stations spawn as per-tenant rows)"
    );

    let dock = registry.get_active("loading-dock").await.expect("dock row");
    assert_eq!(dock.kind, StationKind::Batch);
    assert_eq!(dock.status, WorkflowStatus::Active);
    // The dockRows predicate, ported to data: parked cars only.
    assert_eq!(dock.predicate.kind.as_deref(), Some("ship-a-change"));
    assert_eq!(dock.predicate.metadata_present, vec!["branch".to_string()]);
    assert_eq!(dock.predicate.metadata_absent, vec!["train".to_string()]);
    assert!(dock.predicate.step.is_some(), "review-step clause present");
    assert_eq!(
        dock.discipline,
        boss_jobs::station_queue::default_discipline(),
        "priority, then age"
    );

    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    assert_eq!(review.predicate.kind.as_deref(), Some("design-doc-review"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_writes_stage_their_events_in_the_outbox() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("night-dock"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("night-dock", &author(), now)
        .await
        .expect("publish");
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire");
    // Idempotent second retire: no row touched, nothing staged.
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire again");

    assert_eq!(
        outbox_kinds(&db).await,
        vec![
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_RETIRED.to_string(),
        ],
        "each registry write stages exactly one outbox row, in write order"
    );

    // Versioning against the seeded loading-dock row: a new draft
    // appends v2, publish demotes v1.
    let v2 = registry
        .create_draft(spec("loading-dock"), &author(), now)
        .await
        .expect("draft v2");
    assert_eq!(v2.version, 2);
    registry
        .publish("loading-dock", &author(), now)
        .await
        .expect("publish v2");
    let v1 = registry
        .get_version("loading-dock", 1)
        .await
        .expect("v1 row");
    assert_eq!(v1.status, WorkflowStatus::Retired);
    let active = registry.get_active("loading-dock").await.expect("active");
    assert_eq!(active.version, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn station_event_kinds_are_registered() {
    let db = TestDb::new().await;
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds WHERE kind_pattern LIKE 'jobs.station.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    assert_eq!(
        kinds,
        vec![
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_RETIRED.to_string(),
        ]
    );
}
