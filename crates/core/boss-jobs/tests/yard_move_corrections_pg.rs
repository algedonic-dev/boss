//! Real database conservation, native history and exact projection replay.
#![cfg(feature = "postgres")]
use boss_core::{event::Event, job::Job, port::EventBus};
use boss_jobs::{
    InMemoryJobs,
    moves::{InMemoryMoves, MovesStore},
    port::JobsRepository,
};
use boss_testing::RecordingEventBus;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
#[path = "yard_move_corrections/fixture.rs"]
mod fixture;
use fixture::{fixture, post};

#[tokio::test]
async fn legacy_diagnosis_replay_conserves_absent_audit_instants() {
    use boss_jobs::move_diagnosis;
    let (_, mut receipt) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    for key in ["run", "parent", "station_history"] {
        let rows = receipt["inputs"][key].as_array_mut().unwrap();
        assert!(
            !rows.is_empty(),
            "legacy {key} fixture must contain originals"
        );
        for row in rows {
            row.as_object_mut().unwrap().remove("created_at");
        }
    }
    let packet = receipt["original"]["packet"].clone();
    let at = serde_json::from_value(receipt["original"]["at"].clone()).unwrap();
    let entry = json!({"kind":"diagnosis","receipt":receipt});
    let event = Event::new(
        "jobs",
        move_diagnosis::EVENT_KIND,
        json!({"entry":entry,"ledger_seq":1,"job_id":packet}),
        at,
    );
    let replayed = move_diagnosis::reconstruct(&[event]).unwrap();
    assert_eq!(
        serde_json::to_value(&replayed[0].1).unwrap(),
        entry,
        "legacy receipts keep their complete original shape"
    );
}

#[tokio::test]
async fn memory_without_an_original_audit_instant_reports_unknown() {
    let native: Value = serde_json::from_str(include_str!(
        "../src/moves/dispatch/native-registered-review.json"
    ))
    .unwrap();
    let job: Job = serde_json::from_value(native["run"].clone()).unwrap();
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-14T17:02:53Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let event = Event::new("jobs", boss_jobs::events::JOB_CREATED, json!(job), at);
    let memory = InMemoryJobs::new();
    memory
        .record_events(std::slice::from_ref(&event))
        .await
        .unwrap();
    let history = memory.historical_events(&job.id, 1, 1).await.unwrap();
    assert_eq!(
        history[0].created_at, None,
        "event time cannot become audit time"
    );
    let serialized = serde_json::to_value(history).unwrap();
    assert!(
        serialized[0].get("created_at").is_none(),
        "unknown original retains the legacy shape"
    );
    assert!(
        memory.record_audit_event(event.clone(), at).is_err(),
        "missing originals cannot be backfilled"
    );
    let legacy: boss_jobs::move_diagnosis::HistoricalEvent =
        serde_json::from_value(json!({"seq": 1, "event": event})).unwrap();
    assert_eq!(
        legacy.created_at, None,
        "legacy event time cannot fill the audit clock"
    );
}

#[tokio::test]
async fn postgres_and_memory_conserve_corrected_withdrawn_and_genuine_originals() {
    use boss_jobs::{
        move_diagnosis::{self, Diagnosis, Entry},
        moves::PgMoves,
    };
    let (_, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let template: Diagnosis = serde_json::from_value(body).unwrap();
    let db = boss_testing::TestDb::new().await;
    let memory = InMemoryMoves::new();
    let pg = PgMoves::new(db.pool.clone());
    for store in [&memory as &dyn MovesStore, &pg as &dyn MovesStore] {
        store
            .record(std::slice::from_ref(&template.original.r#move))
            .await
            .unwrap();
        let original = store.since(0, 1).await.unwrap().remove(0);
        let receipt =
            move_diagnosis::diagnose(template.id, original.clone(), template.inputs.clone())
                .unwrap();
        assert!(move_diagnosis::validated(&receipt));
        let diagnosis = Entry::Diagnosis {
            receipt: Box::new(receipt.clone()),
        };
        let correction = Entry::Correction {
            id: Uuid::new_v4(),
            original_seq: original.seq,
            diagnosis_id: receipt.id,
            original_hash: receipt.original_hash.clone(),
            proof: Some(Box::new(fixture::machine_proof(&receipt))),
        };
        for entry in [&diagnosis, &correction] {
            let event = Event::new(
                "jobs",
                entry.event_kind(),
                json!({"entry":entry,"job_id":original.r#move.packet,"_actor":"agent-codex"}),
                original.r#move.at,
            );
            assert!(store.append_correction(entry, &event).await.unwrap());
            assert!(!store.append_correction(entry, &event).await.unwrap());
        }
        let since = original.r#move.at - chrono::Duration::seconds(1);
        assert_eq!(
            store.correction_conservation(since).await.unwrap(),
            move_diagnosis::Conservation {
                raw: 1,
                active: 0,
                superseded: 1,
                unknown: 0
            }
        );
        assert!(store.crossings(since).await.unwrap().is_empty());
        assert_eq!(store.since(0, 10).await.unwrap(), vec![original.clone()]);
        let withdrawal = Entry::Withdrawal {
            id: Uuid::new_v4(),
            original_seq: original.seq,
            correction_id: correction.id(),
        };
        let event = Event::new(
            "jobs",
            withdrawal.event_kind(),
            json!({"entry":withdrawal,"job_id":original.r#move.packet,"_actor":"agent-codex"}),
            original.r#move.at,
        );
        store.append_correction(&withdrawal, &event).await.unwrap();
        assert_eq!(
            store
                .crossings(since)
                .await
                .unwrap()
                .iter()
                .map(|r| r.moves)
                .sum::<i64>(),
            1
        );
        assert_eq!(
            store.correction_conservation(since).await.unwrap(),
            move_diagnosis::Conservation {
                raw: 1,
                active: 1,
                superseded: 0,
                unknown: 0
            }
        );
    }
    let outbox: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind LIKE 'jobs.yard.move.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(outbox, 3, "retries never duplicate immutable evidence");
    let before = pg.correction_entries().await.unwrap();
    let before_rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM yard_move_corrections c ORDER BY seq")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 100)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        move_diagnosis::hash(&before).unwrap(),
        move_diagnosis::hash(&pg.correction_entries().await.unwrap()).unwrap()
    );
    let after_rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM yard_move_corrections c ORDER BY seq")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        before_rows, after_rows,
        "every projection column must replay identically"
    );
    assert_eq!(memory.correction_events().unwrap().len(), 3);
}

#[tokio::test]
async fn historical_adapters_refuse_invalid_bounds_and_conserve_exact_native_cutoffs() {
    use boss_jobs::{PgJobs, move_diagnosis::HistoricalEvent};
    let native: Value = serde_json::from_str(include_str!(
        "../src/moves/dispatch/native-registered-review.json"
    ))
    .unwrap();
    let job: Job = serde_json::from_value(native["run"].clone()).unwrap();
    let db = boss_testing::TestDb::new().await;
    let pg = PgJobs::new(db.pool.clone());
    let memory = InMemoryJobs::new();
    for repository in [&memory as &dyn JobsRepository, &pg as &dyn JobsRepository] {
        for (upto, limit) in [(-1, 10), (1, -1), (1, 4097)] {
            assert!(
                repository
                    .historical_events(&job.id, upto, limit)
                    .await
                    .is_err(),
                "invalid {upto}/{limit}"
            );
        }
    }
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-14T17:02:53Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let first = Event::new(
        "jobs",
        boss_jobs::events::JOB_CREATED,
        serde_json::to_value(&job).unwrap(),
        at,
    );
    let second = Event::new(
        "jobs",
        boss_jobs::events::JOB_UPDATED,
        serde_json::to_value(&job).unwrap(),
        at,
    );
    let mut captured: Vec<HistoricalEvent> = Vec::new();
    let mut original_created_at = Vec::new();
    for event in [&first, &second] {
        let (seq, created_at): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,$3,$4,$5) RETURNING id,created_at")
            .bind(event.id).bind(event.timestamp).bind(&event.source).bind(&event.kind).bind(&event.payload).fetch_one(&db.pool).await.unwrap();
        original_created_at.push(created_at);
        captured.push(HistoricalEvent {
            seq,
            created_at: Some(created_at),
            event: event.clone(),
        });
    }
    assert!(original_created_at[0] < original_created_at[1]);
    let originals = pg
        .historical_events(&job.id, captured[1].seq, 2)
        .await
        .unwrap();
    let serialized = serde_json::to_value(&originals).unwrap();
    let low_level =
        boss_events::tail_http::history_through(&db.pool, &job.id.to_string(), captured[1].seq, 2)
            .await
            .unwrap();
    for (index, created_at) in original_created_at.iter().enumerate() {
        assert_eq!(
            serde_json::to_value(&low_level).unwrap()[index]["created_at"],
            json!(created_at)
        );
        assert_eq!(
            serialized[index].get("created_at"),
            Some(&serde_json::to_value(created_at).unwrap()),
            "original audit creation time must survive the historical packet reader"
        );
        assert_ne!(*created_at, at, "event time is a distinct original");
    }
    let memory = InMemoryJobs::new();
    for (event, created_at) in [&first, &second].into_iter().zip(&original_created_at) {
        memory
            .record_audit_event(event.clone(), *created_at)
            .unwrap();
    }
    let staged = memory.historical_events(&job.id, 2, 2).await.unwrap();
    for (row, original) in staged.iter().zip(&original_created_at) {
        assert_eq!(row.created_at, Some(*original));
    }
    assert!(
        memory
            .record_audit_event(first.clone(), original_created_at[1])
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(memory.historical_events(&job.id, 2, 2).await.unwrap()).unwrap(),
        serde_json::to_value(&staged).unwrap(),
        "an original audit instant cannot be restamped"
    );
    let cutoff = original_created_at[1];
    let in_window: Vec<_> = serialized
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| {
            serde_json::from_value::<chrono::DateTime<chrono::Utc>>(entry["created_at"].clone())
                .unwrap()
                >= cutoff
        })
        .collect();
    assert_eq!(in_window.len(), 1);
    assert_eq!(in_window[0]["event"]["id"], json!(second.id));
    let slice = pg
        .historical_events(&job.id, captured[0].seq, 1)
        .await
        .unwrap();
    assert_eq!(slice.len(), 1);
    assert_eq!(slice[0].event.id, first.id);
    assert_eq!(slice[0].seq, captured[0].seq);
    let spec =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .unwrap()
            .into_iter()
            .find(|s| s.name == "loading-dock")
            .unwrap();
    let publication = Event::new(
        "jobs",
        boss_jobs::events::STATION_PUBLISHED,
        serde_json::to_value(spec).unwrap(),
        at,
    );
    let (pub_seq, pub_created_at): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,$3,$4,$5) RETURNING id,created_at")
        .bind(publication.id).bind(publication.timestamp).bind(&publication.source).bind(&publication.kind).bind(&publication.payload).fetch_one(&db.pool).await.unwrap();
    assert!(
        pg.historical_station_events("loading-dock", captured[0].seq, 4096)
            .await
            .unwrap()
            .is_empty(),
        "later publication cannot supply an original map"
    );
    let published = pg
        .historical_station_events("loading-dock", pub_seq, 4096)
        .await
        .unwrap();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].event.id, publication.id);
    assert_eq!(published[0].seq, pub_seq);
    let low_level_registry = boss_events::tail_http::registry_history_through(
        &db.pool,
        "loading-dock",
        &[boss_jobs::events::STATION_PUBLISHED],
        pub_seq,
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(low_level_registry).unwrap()[0]["created_at"],
        json!(pub_created_at)
    );
    assert_eq!(
        serde_json::to_value(&published).unwrap()[0].get("created_at"),
        Some(&serde_json::to_value(pub_created_at).unwrap()),
        "original audit creation time must survive the historical registry reader"
    );
    memory
        .record_audit_event(publication.clone(), pub_created_at)
        .unwrap();
    let memory_publication = memory
        .historical_station_events("loading-dock", 3, 3)
        .await
        .unwrap();
    assert_eq!(memory_publication.len(), 1);
    assert_eq!(memory_publication[0].created_at, Some(pub_created_at));
    assert!(
        pg.historical_station_events("another-station", pub_seq, 4096)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        pg.historical_events(&job.id, captured[1].seq, 1)
            .await
            .is_err(),
        "a truncated slice is never proof"
    );
}
