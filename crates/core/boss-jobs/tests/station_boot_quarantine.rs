//! Boot-time quarantine of unviable ACTIVE stations.
//!
//! `station_lint::gate_active` closes the publish edge, but the rows
//! that actually exist in a deployed cluster got there by SQL seed
//! (`116-stations.sql`, `118-watchlist-station.sql`) and never passed
//! through publish at all. This pass is what covers them.
//!
//! The Workflow equivalent had to grow a refuse-to-start case, because
//! auto-retiring a Workflow with open Jobs pinned to it strands live
//! work. Stations cannot reach that state: membership is derived from
//! the predicate at read time and no packet carries a station version,
//! so retiring one strands nothing. These tests pin that difference —
//! the pass retires and continues, always.

use std::sync::Arc;

use boss_core::job::JobStatus;
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_quarantine::{QUARANTINE_ACTOR, quarantine_unviable_active_stations};
use boss_jobs::station_queue::{SELF, StationPredicate};
use boss_jobs::{InMemoryJobs, InMemoryStations, StationKind, StationRegistry, StationSpec};
use std::collections::BTreeMap;

fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
}

fn actor() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Automation(QUARANTINE_ACTOR.into())
}

/// Seeded straight to ACTIVE, exactly as the SQL migrations do —
/// bypassing publish, which is the whole point of this pass.
fn seed_active(stations: &InMemoryStations, mut spec: StationSpec) {
    spec.status = WorkflowStatus::Active;
    stations.seed(spec).unwrap();
}

fn viable(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "A real queue",
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        },
        now(),
    )
}

/// A contradiction: the same key demanded present and absent.
fn always_empty(name: &str) -> StationSpec {
    let mut s = viable(name);
    s.predicate.metadata_present = vec!["train".into()];
    s.predicate.metadata_absent = vec!["train".into()];
    s
}

#[tokio::test]
async fn a_clean_registry_is_left_alone() {
    let stations = Arc::new(InMemoryStations::new());
    let jobs = InMemoryJobs::new();
    seed_active(&stations, viable("dock"));
    seed_active(&stations, viable("review"));

    let report = quarantine_unviable_active_stations(
        stations.as_ref() as &dyn StationRegistry,
        &jobs,
        &actor(),
        now(),
    )
    .await
    .unwrap();

    assert_eq!(report.checked, 2);
    assert!(report.quarantined.is_empty());
    assert_eq!(stations.list_active().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_seeded_row_that_can_never_match_is_retired_and_the_rest_keep_serving() {
    let stations = Arc::new(InMemoryStations::new());
    let jobs = InMemoryJobs::new();
    seed_active(&stations, viable("dock"));
    seed_active(&stations, always_empty("broken"));

    let report = quarantine_unviable_active_stations(
        stations.as_ref() as &dyn StationRegistry,
        &jobs,
        &actor(),
        now(),
    )
    .await
    .unwrap();

    assert_eq!(report.checked, 2);
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.quarantined[0].name, "broken");
    assert!(!report.quarantined[0].problems.is_empty());

    // The blast radius is the one row, not the registry.
    let live = stations.list_active().await.unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].name, "dock");
}

#[tokio::test]
async fn the_retirement_and_the_loud_marker_are_both_recorded() {
    let stations = Arc::new(InMemoryStations::new());
    let jobs = InMemoryJobs::new();
    seed_active(&stations, always_empty("broken"));

    quarantine_unviable_active_stations(
        stations.as_ref() as &dyn StationRegistry,
        &jobs,
        &actor(),
        now(),
    )
    .await
    .unwrap();

    // The registry's own state event, through its normal path.
    let kinds: Vec<String> = stations
        .recorded_events()
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert!(
        kinds.contains(&"jobs.station.retired".to_string()),
        "the retirement must witness itself like any other: {kinds:?}"
    );

    // And the loud marker, carrying why.
    let recorded = jobs.recorded_events();
    let marker = recorded
        .iter()
        .find(|e| e.kind == "jobs.station.quarantined")
        .expect("the quarantine marker must be recorded");
    assert_eq!(marker.payload["name"], "broken");
    let problems = marker.payload["problems"].as_array().unwrap();
    assert!(!problems.is_empty(), "the marker must carry the problems");
    assert!(
        problems[0]["message"].as_str().unwrap().contains("train"),
        "the log must answer 'why did this queue disappear' without a re-lint: {problems:?}"
    );
    // `_actor` is a flat string — ActorId serializes as one, not as an
    // object with an `id` field. Worth an explicit assertion: reading
    // an actor out of the wrong shape is exactly how I built a wrong
    // finding on 2026-08-13 (d53374cc, withdrawn).
    assert_eq!(
        marker.payload["_actor"], "automation:station-quarantine",
        "the log must say who retired it"
    );
}

#[tokio::test]
async fn a_station_that_lies_about_being_personal_is_caught() {
    // The shape that made the census misreport orphaned packets: an
    // `actor` row every executor sees identically.
    let stations = Arc::new(InMemoryStations::new());
    let jobs = InMemoryJobs::new();
    let mut impostor = viable("my-queue");
    impostor.kind = StationKind::Actor;
    seed_active(&stations, impostor);

    let report = quarantine_unviable_active_stations(
        stations.as_ref() as &dyn StationRegistry,
        &jobs,
        &actor(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(report.quarantined.len(), 1);
}

#[tokio::test]
async fn the_real_watchlist_row_survives_the_pass() {
    // 118-watchlist-station.sql, as deployed. A regression here means
    // the pass would delete a live queue on the next pod roll — so this
    // test is the one standing between this file and an outage.
    let stations = Arc::new(InMemoryStations::new());
    let jobs = InMemoryJobs::new();
    seed_active(
        &stations,
        StationSpec::draft(
            "my-watchlist",
            "My watchlist",
            StationKind::Actor,
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
                ..Default::default()
            },
            now(),
        ),
    );

    let report = quarantine_unviable_active_stations(
        stations.as_ref() as &dyn StationRegistry,
        &jobs,
        &actor(),
        now(),
    )
    .await
    .unwrap();
    assert!(
        report.quarantined.is_empty(),
        "the deployed watchlist row must survive: {:?}",
        report.quarantined
    );
    assert_eq!(stations.list_active().await.unwrap().len(), 1);
}
