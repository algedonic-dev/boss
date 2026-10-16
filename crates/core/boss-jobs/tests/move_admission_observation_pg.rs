//! Real audit insertion clocks, atomic observation storage and complete replay.
#![cfg(feature = "postgres")]
use boss_core::{event::Event, partition::Partition, port::EventBus};
use boss_jobs::move_admission::Observation;
use boss_jobs::move_diagnosis::{Diagnosis, Entry, Inputs};
use boss_jobs::{
    PgJobs, move_admission, move_diagnosis,
    moves::{MovesStore, PgMoves},
    port::JobsRepository,
};
use serde_json::json;
use uuid::Uuid;
#[path = "yard_move_corrections/fixture.rs"]
#[allow(dead_code)]
mod fixture;

async fn native() -> (boss_testing::TestDb, Diagnosis) {
    let (app, memory) = fixture::fixture_with_parent_cause().await;
    let (_, body) = fixture::post(
        app,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let mut d: Diagnosis = serde_json::from_value(body).unwrap();
    let db = boss_testing::TestDb::new().await;
    for event in memory.recorded_events() {
        let (seq,created_at):(i64,chrono::DateTime<chrono::Utc>)=sqlx::query_as("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,$3,$4,$5) RETURNING id,created_at")
            .bind(event.id).bind(event.timestamp).bind(&event.source).bind(&event.kind).bind(&event.payload).fetch_one(&db.pool).await.unwrap();
        if event.id == d.original.r#move.cause_event_id {
            d.original.r#move.cause_seq = seq;
            d.original.r#move.at = created_at;
        }
    }
    let pg = PgJobs::new(db.pool.clone());
    let (run, parent) = move_admission::identities(&Diagnosis {
        original_hash: move_diagnosis::hash(&d.original).unwrap(),
        ..d.clone()
    })
    .unwrap();
    let inputs = Inputs {
        run: pg
            .historical_events(
                &boss_core::job::JobId::from_uuid(run),
                d.original.r#move.cause_seq,
                4096,
            )
            .await
            .unwrap(),
        parent: pg
            .historical_events(
                &boss_core::job::JobId::from_uuid(parent),
                d.original.r#move.cause_seq,
                4096,
            )
            .await
            .unwrap(),
        workflows: vec![],
        stations: vec![],
        station_history: vec![],
    };
    d = move_diagnosis::diagnose(d.id, d.original, inputs).unwrap();
    assert_eq!(d.status, move_diagnosis::Status::Unknown);
    assert_eq!(d.why, "admission missing");
    (db, d)
}

#[tokio::test]
async fn native_snapshot_and_both_stores_preserve_evidence_without_clearing() {
    let (db, d) = native().await;
    let jobs = PgJobs::new(db.pool.clone());
    let snapshot = jobs.move_admission_snapshot(&d).await.unwrap();
    assert!(snapshot.native_admissions[0].seq > d.original.r#move.cause_seq);
    assert_ne!(
        snapshot.native_admissions[0].created_at.unwrap(),
        snapshot.native_admissions[0].event.timestamp
    );
    let o = move_admission::observe(Uuid::new_v4(), d.clone(), snapshot, Partition::Real).unwrap();
    let pg = PgMoves::new(db.pool.clone());
    let memory = boss_jobs::moves::InMemoryMoves::new();
    for store in [&pg as &dyn MovesStore, &memory] {
        store
            .record(std::slice::from_ref(&d.original.r#move))
            .await
            .unwrap();
        let entry = Entry::Diagnosis {
            receipt: Box::new(d.clone()),
        };
        let event = Event::new(
            "jobs",
            entry.event_kind(),
            json!({"job_id":d.original.r#move.packet,"_actor":"operator","_partition":"real","entry":entry}),
            d.original.r#move.at,
        );
        store.append_correction(&entry, &event).await.unwrap();
        let e = Event::new(
            "jobs",
            move_admission::EVENT_KIND,
            json!({"job_id":d.original.r#move.packet,"_actor":"operator","_partition":"real","observation":o}),
            d.original.r#move.at,
        );
        let before = store
            .correction_conservation(d.original.r#move.at - chrono::Duration::seconds(1))
            .await
            .unwrap();
        assert!(store.append_admission_observation(&o, &e).await.unwrap());
        assert!(!store.append_admission_observation(&o, &e).await.unwrap());
        assert_eq!(
            serde_json::to_value(store.admission_observation(o.id).await.unwrap().unwrap())
                .unwrap(),
            serde_json::to_value(&o).unwrap()
        );
        assert_eq!(
            store
                .correction_conservation(d.original.r#move.at - chrono::Duration::seconds(1))
                .await
                .unwrap(),
            before
        );
        assert_eq!(store.since(0, 10).await.unwrap(), vec![d.original.clone()]);
        let mut conflicting = o.clone();
        conflicting.snapshot.acquired_at += chrono::Duration::microseconds(1);
        let conflict_event = Event::new(
            "jobs",
            move_admission::EVENT_KIND,
            json!({"job_id":d.original.r#move.packet,"_actor":"operator","_partition":"real","observation":conflicting}),
            d.original.r#move.at,
        );
        assert!(
            !store
                .append_admission_observation(&conflicting, &conflict_event)
                .await
                .unwrap(),
            "a concurrent same-command retry returns the winner without replacing its snapshot"
        );
        assert_eq!(
            serde_json::to_value(store.admission_observation(o.id).await.unwrap().unwrap())
                .unwrap(),
            serde_json::to_value(&o).unwrap()
        );
    }
    let before: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(o) FROM yard_move_admission_observations o ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let bus: std::sync::Arc<dyn EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 100)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    let after: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(o) FROM yard_move_admission_observations o ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        before, after,
        "every observation projection column must replay identically"
    );
    let stored: Observation = pg.admission_observation(o.id).await.unwrap().unwrap();
    assert_eq!(
        move_diagnosis::hash(&stored).unwrap(),
        move_diagnosis::hash(&o).unwrap()
    );
}

#[tokio::test]
async fn oversized_observation_event_refuses_atomically_on_both_stores() {
    let (db, d) = native().await;
    let jobs = PgJobs::new(db.pool.clone());
    let mut s = jobs.move_admission_snapshot(&d).await.unwrap();
    s.native_admissions[0].event.payload["metadata"]["large_diagnostic_control"] =
        json!("x".repeat(boss_events::outbox::MAX_STAGED_EVENT_BYTES));
    let o = move_admission::observe(Uuid::new_v4(), d.clone(), s, Partition::Real).unwrap();
    let pg = PgMoves::new(db.pool.clone());
    let memory = boss_jobs::moves::InMemoryMoves::new();
    for store in [&pg as &dyn MovesStore, &memory] {
        store
            .record(std::slice::from_ref(&d.original.r#move))
            .await
            .unwrap();
        let entry = Entry::Diagnosis {
            receipt: Box::new(d.clone()),
        };
        let e = Event::new(
            "jobs",
            entry.event_kind(),
            json!({"job_id":d.original.r#move.packet,"_actor":"operator","entry":entry}),
            d.original.r#move.at,
        );
        store.append_correction(&entry, &e).await.unwrap();
        let e = Event::new(
            "jobs",
            move_admission::EVENT_KIND,
            json!({"job_id":d.original.r#move.packet,"_actor":"operator","_partition":"real","observation":o}),
            d.original.r#move.at,
        );
        assert!(store.append_admission_observation(&o, &e).await.is_err());
        assert!(store.admission_observation(o.id).await.unwrap().is_none());
        assert_eq!(store.correction_entries().await.unwrap().len(), 1);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind=$1")
        .bind(move_admission::EVENT_KIND)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
