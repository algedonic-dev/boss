#![cfg(feature = "postgres")]

use boss_core::request_latency::{Class, Recorder, Sample};
use boss_jobs::sensors::request_latency::PgLatency;
use boss_testing::TestDb;
use chrono::{DateTime, Utc};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}
fn wall_store(pool: sqlx::PgPool) -> PgLatency {
    PgLatency::new(pool)
}
fn sample() -> Sample {
    Sample {
        service: "jobs".into(),
        class: Class::Write,
        build: "fixture-build".into(),
        recorder_epoch: "fixture-epoch".into(),
        recording_started_at: DateTime::<Utc>::UNIX_EPOCH,
        completed_at: Utc::now(),
        duration_ns: 61_123_456_789,
        status: 422.into(),
        method: "POST".into(),
        route_template: "/api/jobs/{id}/steps/{step_id}/metadata/records".into(),
        recorder_failures: 0,
        dropped_samples: 0,
        recording_gap_since: None,
        recording_ended_at: None,
        context: None,
    }
}

#[tokio::test]
async fn a_future_batch_cannot_delete_present_history_or_blind_later_recording() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let t: DateTime<Utc> = sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp(),'UTC') - interval '30 minutes'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let mut s = sample();
    s.recording_started_at = t;
    s.completed_at = t;
    store.record(&[s.clone()]).await.unwrap();
    let mut future = s.clone();
    future.completed_at = t + chrono::Duration::hours(100);
    assert!(
        store.record(&[future]).await.is_ok(),
        "per-sample future refusal must commit its failure debt"
    );
    s.completed_at = t + chrono::Duration::seconds(5);
    store.record(&[s]).await.unwrap();
    let v = store.read(t + chrono::Duration::minutes(89)).await.unwrap();
    assert_eq!(
        v["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["writes"]["count"].as_u64().unwrap())
            .sum::<u64>(),
        2
    );
}

#[tokio::test]
async fn a_writer_refused_future_sample_becomes_counted_recorder_failure_debt() {
    use axum::{Router, body::Body, http::Request, routing::post};
    use boss_core::request_latency::{RecorderError, Timing, mount};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    struct Observed {
        store: PgLatency,
        ready: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl Recorder for Observed {
        fn recording_now(&self) -> Option<DateTime<Utc>> {
            self.store.recording_now()
        }
        async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
            let result = self.store.record(samples).await;
            self.ready.notify_one();
            result
        }
    }
    let db = TestDb::new().await;
    let t: DateTime<Utc> = sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp(),'UTC') - interval '30 minutes'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let sink = Arc::new(Observed {
        store: wall_store(db.pool.clone()),
        ready: tokio::sync::Notify::new(),
    });
    let clock = Arc::new(Mutex::new(t));
    let (timing, worker) = Timing::start(
        sink.clone(),
        "jobs".into(),
        "fixture".into(),
        {
            let clock = clock.clone();
            Arc::new(move || *clock.lock().unwrap())
        },
        boss_jobs::sensors::request_latency::classify,
    );
    let app = mount(
        Router::new().route(
            "/api/jobs",
            post(|| async { axum::http::StatusCode::CREATED }),
        ),
        timing,
    );
    for instant in [
        t,
        t + chrono::Duration::hours(100),
        t + chrono::Duration::seconds(5),
    ] {
        *clock.lock().unwrap() = instant;
        app.clone()
            .oneshot(Request::post("/api/jobs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), sink.ready.notified())
            .await
            .unwrap();
    }
    worker.shutdown().await;
    let (read_now, read_at): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as("SELECT statement_timestamp(),date_trunc('hour',statement_timestamp(),'UTC') + interval '30 minutes'")
        .fetch_one(&db.pool).await.unwrap();
    let v = sink.store.read(read_at).await.unwrap();
    assert_eq!(v["window"]["writes"]["count"], 2);
    let h = v["hours"].as_array().unwrap().last().unwrap();
    assert_eq!(h["recorder_failures"], 1);
    assert_eq!(h["dropped_request_samples"], 1);
    assert!(
        at(h["recording_gap_since"].as_str().unwrap()) <= read_now,
        "a refused future producer timestamp cannot become the claimed failure instant"
    );
}

#[tokio::test]
async fn cancelled_elapsed_samples_count_toward_class_max_and_bulk_stays_separate() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let mut value = serde_json::to_value(sample()).unwrap();
    value["status"] = serde_json::json!("cancelled");
    value["duration_ns"] = serde_json::json!(99_000_000_000_u64);
    store
        .record(&[serde_json::from_value(value).unwrap()])
        .await
        .unwrap();
    let mut bulk = sample();
    bulk.class = Class::Bulk;
    bulk.duration_ns = 200_000_000_000;
    store.record(&[bulk]).await.unwrap();
    let v = store.read(Utc::now()).await.unwrap();
    assert_eq!(v["window"]["writes"]["count"], 1);
    assert_eq!(v["window"]["writes"]["max_ns"], 99_000_000_000_u64);
    assert_eq!(v["window"]["writes"]["max_origin"]["status"], "cancelled");
    assert_eq!(v["window"]["bulk"]["count"], 1);
    assert_eq!(v["window"]["bulk"]["max_ns"], 200_000_000_000_u64);
}

#[tokio::test]
async fn old_hour_shapes_keep_counts_but_do_not_invent_max_origins_or_bulk_history() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    store.record(&[sample()]).await.unwrap();
    sqlx::query("UPDATE sensor_request_latency_hours SET payload=(payload #- '{writes,max_origin}' #- '{small_reads,max_origin}') - 'bulk'").execute(&db.pool).await.unwrap();
    let v = store.read(Utc::now()).await.unwrap();
    assert_eq!(v["window"]["writes"]["count"], 1);
    assert_eq!(v["window"]["writes"]["max_ns"], sample().duration_ns);
    assert!(v["window"]["writes"]["max_origin"].is_null());
    assert!(
        v["bulk_history"]
            .as_str()
            .unwrap()
            .contains("not reclassified")
    );
}

#[tokio::test]
async fn future_slack_cannot_anchor_retention_or_delete_a_future_stored_hour() {
    let db = TestDb::new().await;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let store = wall_store(db.pool.clone());
    let mut s = sample();
    s.recording_started_at = now - chrono::Duration::hours(72);
    for n in 0..72 {
        s.completed_at = now - chrono::Duration::hours(n);
        store.record(&[s.clone()]).await.unwrap();
    }
    let original: Vec<i64> =
        sqlx::query_scalar("SELECT hour FROM sensor_request_latency_hours ORDER BY hour")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(original.len(), 72);
    // Preexisting future data is conserved even when current producers recover.
    sqlx::query("INSERT INTO sensor_request_latency_hours(hour,payload) SELECT hour+360000,payload FROM sensor_request_latency_hours ORDER BY hour DESC LIMIT 1").execute(&db.pool).await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT sum((payload->'writes'->>'count')::bigint)::bigint FROM sensor_request_latency_hours").fetch_one(&db.pool).await.unwrap();
    s.completed_at = now;
    store.record(&[s]).await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT sum((payload->'writes'->>'count')::bigint)::bigint FROM sensor_request_latency_hours").fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        after, before,
        "preexisting future hour must refuse incoming request samples, not silently accept them"
    );
    let failures: i64 = sqlx::query_scalar("SELECT sum((payload->>'recorder_failures')::bigint)::bigint FROM sensor_request_latency_hours").fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        failures, 1,
        "store anomaly must retain explicit failure debt"
    );
    let retained: Vec<i64> =
        sqlx::query_scalar("SELECT hour FROM sensor_request_latency_hours ORDER BY hour")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert!(original.iter().all(|h| retained.contains(h)));
    assert_eq!(
        retained.len(),
        73,
        "future existing data is never deleted to disguise a72hour window"
    );
}

#[tokio::test]
async fn telemetry_transactions_disable_only_their_own_synchronous_commit() {
    let db = TestDb::new().await;
    // TestDb's cluster normally disables fsync-related waits. Give this one
    // isolated connection a known ON session setting so LOCAL is observable.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET synchronous_commit = on")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE observed_telemetry_commit(setting text); CREATE FUNCTION observe_telemetry_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO observed_telemetry_commit VALUES(current_setting('synchronous_commit')); RETURN NEW; END $$; CREATE TRIGGER observe_telemetry_commit BEFORE INSERT ON sensor_request_latency_hours FOR EACH ROW EXECUTE FUNCTION observe_telemetry_commit();").execute(&db.pool).await.unwrap();
    let store = wall_store(pool.clone());
    store.record(&[sample()]).await.unwrap();
    let settings: Vec<String> = sqlx::query_scalar("SELECT setting FROM observed_telemetry_commit")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(settings, ["off"]);
    let outside: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(
        outside, "off",
        "SET LOCAL must not change pooled command sessions"
    );
}

#[tokio::test]
async fn exposed_hour_buckets_merge_into_the_union_window_p99_and_keep_max_origin() {
    use boss_jobs::sensors::request_latency::Distribution;
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let mut union = Distribution::default();
    let instant: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    for (offset, ns, method, status) in [(1, 10, "PATCH", 200), (0, 3_000_000_001, "POST", 422)] {
        let mut s = serde_json::to_value(sample()).unwrap();
        s["completed_at"] = serde_json::json!(instant - chrono::Duration::hours(offset));
        s["duration_ns"] = serde_json::json!(ns);
        s["method"] = serde_json::json!(method);
        s["route_template"] = serde_json::json!("/api/jobs/{id}/metadata");
        s["status"] = serde_json::json!(status);
        store
            .record(&[serde_json::from_value(s).unwrap()])
            .await
            .unwrap();
        union.record(ns).unwrap();
    }
    let v = store.read(Utc::now()).await.unwrap();
    let mut merged = Distribution::default();
    for h in v["hours"].as_array().unwrap() {
        merged
            .merge(&serde_json::from_value(h["writes"].clone()).unwrap())
            .unwrap();
    }
    assert_eq!(merged, union);
    assert_eq!(
        v["window"]["writes"]["p99_ns"],
        serde_json::json!(union.p99_ns())
    );
    assert_eq!(v["window"]["writes"]["count"], 2);
    let origin = &v["hours"].as_array().unwrap().last().unwrap()["writes"]["max_origin"];
    assert_eq!(origin["service"], "jobs");
    assert_eq!(origin["method"], "POST");
    assert_eq!(origin["route_template"], "/api/jobs/{id}/metadata");
    assert_eq!(origin["status"], 422);
}

#[tokio::test]
async fn an_inverted_epoch_end_refuses_atomically_without_poisoning_retained_samples() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    store.record(&[sample()]).await.unwrap();
    let mut end = sample();
    end.class = Class::Recording;
    end.status = 0.into();
    end.duration_ns = 0;
    end.completed_at -= chrono::Duration::seconds(1);
    end.recording_ended_at = Some(end.completed_at);
    assert!(
        store.record(&[end]).await.is_err(),
        "an end earlier than the last sample must refuse before overwriting a valid hour"
    );
    let view = store.read(sample().completed_at).await.unwrap();
    assert_eq!(
        view["hours"].as_array().unwrap().last().unwrap()["writes"]["count"],
        1
    );
    assert!(view["recording_epoch_ends"]["fixture-epoch"].is_null());
}

#[tokio::test]
async fn closed_epochs_survive_recreation_without_hiding_unclean_epochs_or_historical_gaps() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    store.record(&[sample()]).await.unwrap();
    let mut close = serde_json::to_value(sample()).unwrap();
    close["class"] = serde_json::json!("recording");
    close["duration_ns"] = serde_json::json!(0);
    close["status"] = serde_json::json!(0);
    close["recording_ended_at"] = close["completed_at"].clone();
    close["recorder_failures"] = serde_json::json!(1);
    close["dropped_samples"] = serde_json::json!(1);
    close["recording_gap_since"] = close["recording_started_at"].clone();
    store
        .record(&[serde_json::from_value(close).unwrap()])
        .await
        .unwrap();
    let mut unclean = sample();
    unclean.recorder_epoch = "unclean-epoch".into();
    store.record(&[unclean.clone()]).await.unwrap();
    let view = wall_store(db.pool.clone())
        .read(unclean.completed_at)
        .await
        .unwrap();
    assert!(
        view["recording_epoch_ends"]["fixture-epoch"].is_string(),
        "persisted normal epoch end survives restart"
    );
    assert!(
        view["recording_epoch_ends"]["unclean-epoch"].is_null(),
        "absence of an end is not a graceful close"
    );
    let hour = view["hours"].as_array().unwrap().last().unwrap();
    assert_eq!(
        hour["writes"]["count"], 2,
        "close/checkpoint cannot invent requests"
    );
    assert_eq!(
        hour["recorder_failures"], 1,
        "normal ending cannot erase historical recording debt"
    );
    assert!(hour["recording_gap_since"].is_string());
}

#[tokio::test]
async fn sensor_series_survives_recreation_and_concurrent_hour_merges_without_audit_events() {
    let db = TestDb::new().await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let store = wall_store(db.pool.clone());
        tasks.push(tokio::spawn(async move {
            store.record(&vec![sample(); 6]).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let restarted = wall_store(db.pool.clone());
    let response = restarted.read(Utc::now()).await.unwrap();
    let hours = response["hours"].as_array().unwrap();
    assert_eq!(
        hours.len(),
        24,
        "default read returns24 UTC hours, both classes in each row"
    );
    let current = hours.last().unwrap();
    assert_eq!(current["writes"]["count"], 12);
    assert_eq!(current["writes"]["max_ns"], 61_123_456_789_u64);
    assert_eq!(current["small_reads"]["max_ns"], serde_json::Value::Null);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before, after, "sensor telemetry is outside the audit log");
}

#[tokio::test]
async fn rolling_retention_is_72_rows_and_checkpoints_do_not_invent_samples() {
    let db = TestDb::new().await;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let start = now - chrono::Duration::hours(74);
    let store = PgLatency::new(db.pool.clone());
    let mut checkpoint = sample();
    checkpoint.class = Class::Recording;
    checkpoint.duration_ns = 0;
    checkpoint.status = 0.into();
    checkpoint.recording_started_at = start;
    for n in 0..75 {
        checkpoint.completed_at = start + chrono::Duration::hours(n);
        store.record(&[checkpoint.clone()]).await.unwrap();
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_request_latency_hours")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 72);
    let debt: (i64, i64) = sqlx::query_as("SELECT sum((payload->>'recorder_failures')::bigint)::bigint,sum((payload->>'dropped_samples')::bigint)::bigint FROM sensor_request_latency_hours")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        debt,
        (3, 0),
        "expired checkpoints retain failure debt without inventing request samples"
    );
    let view = store.read(checkpoint.completed_at).await.unwrap();
    assert!(
        view["hours"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["writes"]["count"] == 0
                && h["writes"]["max_ns"].is_null()
                && h["small_reads"]["max_ns"].is_null())
    );
    checkpoint.completed_at = start;
    store.record(&[checkpoint]).await.unwrap();
    let late_debt: i64 = sqlx::query_scalar("SELECT sum((payload->>'recorder_failures')::bigint)::bigint FROM sensor_request_latency_hours")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        late_debt, 4,
        "late old checkpoints add debt, never a73rd hour"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_request_latency_hours")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 72);
}

#[tokio::test]
async fn recovered_failure_counts_and_unavailable_persisted_data_are_not_healthy_empty() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let mut recovered = sample();
    recovered.class = Class::Recording;
    recovered.duration_ns = 0;
    recovered.status = 0.into();
    recovered.recorder_failures = 2;
    recovered.dropped_samples = 17;
    recovered.recording_gap_since = Some(recovered.recording_started_at);
    store.record(&[recovered.clone()]).await.unwrap();
    let view = store.read(recovered.completed_at).await.unwrap();
    let hour = view["hours"].as_array().unwrap().last().unwrap();
    assert_eq!(hour["recorder_failures"], 2);
    assert_eq!(hour["dropped_request_samples"], 17);
    assert!(hour["writes"]["max_ns"].is_null());
    assert!(hour["recording_gap_since"].is_string());
    sqlx::query("UPDATE sensor_request_latency_hours SET payload='{}'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        store.read(recovered.completed_at).await.is_err(),
        "malformed present data is unavailable, not zero samples"
    );
}

#[tokio::test]
async fn series_is_read_only_and_preserves_sensor_reader_authority() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let app = boss_jobs::sensors::request_latency::router(std::sync::Arc::new(PgLatency::new(
        db.pool.clone(),
    )));
    for (tier, expected) in [
        ("user", StatusCode::FORBIDDEN),
        ("operator", StatusCode::OK),
        ("auditor", StatusCode::OK),
    ] {
        let identity = serde_json::json!({"id":"fixture-reader","role":"engineer","access_tier":tier,"territory_account_ids":[],"direct_report_ids":[],"department":"it"});
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/sensors/request-latency")
                    .header("x-boss-user", identity.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{tier}");
    }
    let response = app
        .oneshot(
            Request::post("/api/sensors/request-latency")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_request_latency_hours")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[path = "support/first_record_fixture.rs"]
mod first_record_fixture;

#[tokio::test]
async fn operational_timing_is_separate_from_a_refused_first_records_command_state() {
    let db = TestDb::new().await;
    let instant: DateTime<Utc> = sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp(),'UTC') - interval '1 second'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let sink = std::sync::Arc::new(PgLatency::with_clock(
        db.pool.clone(),
        std::sync::Arc::new(move || instant),
    ));
    first_record_fixture::assert_refusal_is_unchanged(
        sink.clone(),
        std::sync::Arc::new(move || instant),
    )
    .await;
    let view = sink.read(instant).await.unwrap();
    assert_eq!(
        view["hours"].as_array().unwrap().last().unwrap()["writes"]["count"],
        1,
        "the refused route's sample actually lands in its independent durable sensor"
    );
    let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(audit_count, 0, "operational telemetry emits no audit event");
}

#[tokio::test]
async fn shared_pod_clock_steps_cannot_prune_database_hours() {
    use std::sync::{Arc, Mutex};
    let db = TestDb::new().await;
    let t: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let clock = Arc::new(Mutex::new(t));
    let writer_clock = clock.clone();
    let store = PgLatency::with_clock(
        db.pool.clone(),
        Arc::new(move || *writer_clock.lock().unwrap()),
    );
    let mut s = sample();
    s.recording_started_at = t - chrono::Duration::hours(4);
    for offset in -3..=0 {
        *clock.lock().unwrap() = t + chrono::Duration::hours(offset);
        s.completed_at = *clock.lock().unwrap();
        store.record(&[s.clone()]).await.unwrap();
    }
    let original: Vec<i64> =
        sqlx::query_scalar("SELECT hour FROM sensor_request_latency_hours ORDER BY hour")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(original.len(), 4);
    *clock.lock().unwrap() = t + chrono::Duration::hours(100);
    s.completed_at = *clock.lock().unwrap();
    let _ = store.record(&[s.clone()]).await;
    *clock.lock().unwrap() = t + chrono::Duration::seconds(5);
    s.completed_at = *clock.lock().unwrap();
    store.record(&[s.clone()]).await.unwrap();
    *clock.lock().unwrap() = t - chrono::Duration::hours(2);
    s.completed_at = *clock.lock().unwrap();
    store.record(&[s]).await.unwrap();
    let retained: Vec<i64> =
        sqlx::query_scalar("SELECT hour FROM sensor_request_latency_hours ORDER BY hour")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert!(
        original.iter().all(|h| retained.contains(h)),
        "shared pod clock must not delete any real database hour: {original:?} -> {retained:?}"
    );
}

#[tokio::test]
async fn mixed_future_sample_preserves_good_samples_and_counts_its_debt() {
    let db = TestDb::new().await;
    let t: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let store = PgLatency::new(db.pool.clone());
    let mut good = sample();
    good.recording_started_at = t - chrono::Duration::hours(1);
    good.completed_at = t;
    let mut future = good.clone();
    future.completed_at = t + chrono::Duration::hours(100);
    future.recorder_failures = 3;
    future.dropped_samples = 4;
    future.recording_gap_since = Some(t - chrono::Duration::hours(2));
    store.record(&[good.clone(), future, good]).await.unwrap();
    let v = store.read(t).await.unwrap();
    assert_eq!(
        v["window"]["writes"]["count"], 2,
        "good batch members must land"
    );
    assert_eq!(
        v["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["recorder_failures"].as_u64().unwrap())
            .sum::<u64>(),
        4
    );
    assert_eq!(
        v["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["dropped_request_samples"].as_u64().unwrap())
            .sum::<u64>(),
        5
    );
}

#[tokio::test]
async fn series_query_bounds_the_actual_hour_roster_and_window() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let app = boss_jobs::sensors::request_latency::router(std::sync::Arc::new(PgLatency::new(
        db.pool.clone(),
    )));
    let identity = serde_json::json!({"id":"fixture-reader","role":"engineer","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"it"});
    for (query, expected) in [("", 24), ("?hours=1", 1), ("?hours=72", 72)] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/sensors/request-latency{query}"))
                    .header("x-boss-user", identity.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap())
                .unwrap();
        assert_eq!(
            body["hours"].as_array().unwrap().len(),
            expected,
            "actual returned roster for{query}"
        );
        assert_eq!(body["window"]["hour_count"], expected);
    }
    for query in ["?hours=0", "?hours=73", "?hours=-1", "?hours=bad"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/sensors/request-latency{query}"))
                    .header("x-boss-user", identity.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
}

#[tokio::test]
async fn bulk_classifier_reaches_the_actual_pg_recording_path() {
    use axum::{Router, body::Body, http::Request, routing::post};
    use boss_core::request_latency::{Timing, mount};
    use std::sync::Arc;
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let store = Arc::new(PgLatency::new(db.pool.clone()));
    let (timing, worker) = Timing::start(
        store.clone(),
        "jobs".into(),
        "fixture".into(),
        Arc::new(Utc::now),
        boss_jobs::sensors::request_latency::classify,
    );
    let app = mount(
        Router::new()
            .route(
                "/api/jobs/readiness",
                post(|| async { axum::http::StatusCode::OK }),
            )
            .route(
                "/api/jobs/repairs/step-plugin-version",
                post(|| async { axum::http::StatusCode::OK }),
            ),
        timing,
    );
    for path in [
        "/api/jobs/readiness",
        "/api/jobs/repairs/step-plugin-version",
    ] {
        app.clone()
            .oneshot(Request::post(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
    }
    worker.shutdown().await;
    let v = store.read(Utc::now()).await.unwrap();
    assert_eq!(v["window"]["bulk"]["count"], 2);
    assert_eq!(v["window"]["writes"]["count"], 0);
}

#[tokio::test]
async fn two_pod_writers_seven_minutes_apart_cannot_delete_each_others_hour() {
    use std::sync::{Arc, Mutex};
    let db = TestDb::new().await;
    let t: DateTime<Utc> = sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp()) - interval '1 hour' + interval '1 minute'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    for (service, instant) in [("jobs", t), ("messages", t - chrono::Duration::minutes(7))] {
        let clock = Arc::new(Mutex::new(instant));
        let writer_clock = clock.clone();
        let store = PgLatency::with_clock(
            db.pool.clone(),
            Arc::new(move || *writer_clock.lock().unwrap()),
        );
        let mut s = sample();
        s.service = service.into();
        s.recorder_epoch = service.into();
        s.recording_started_at = t - chrono::Duration::hours(1);
        s.completed_at = *clock.lock().unwrap();
        store.record(&[s]).await.unwrap();
    }
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_request_latency_hours")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 2,
        "a slower pod's clock must not delete the other pod's newer hour"
    );
}

#[tokio::test]
async fn accepted_next_hour_sample_cannot_black_out_either_service() {
    let db = TestDb::new().await;
    sqlx::raw_sql("CREATE SCHEMA review_clock; CREATE TABLE review_clock.clock(at timestamptz NOT NULL); INSERT INTO review_clock.clock VALUES ('2026-10-28T02:58:00Z'); CREATE FUNCTION review_clock.statement_timestamp() RETURNS timestamptz LANGUAGE sql STABLE AS 'SELECT at FROM review_clock.clock'")
        .execute(&db.pool).await.unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET search_path=review_clock,pg_catalog,public")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    let t = at("2026-10-28T02:58:00Z");
    let observed: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        observed, t,
        "the actual writer database clock must be controlled"
    );
    let jobs = PgLatency::new(pool.clone());
    let messages = PgLatency::new(pool.clone());
    let mut s = sample();
    s.recording_started_at = t - chrono::Duration::hours(1);
    s.completed_at = t;
    jobs.record(&[s.clone()]).await.unwrap();
    let mut ahead = s.clone();
    ahead.completed_at = at("2026-10-28T03:01:00Z");
    jobs.record(&[ahead]).await.unwrap();
    s.service = "messages".into();
    s.recorder_epoch = "messages-epoch".into();
    s.completed_at = at("2026-10-28T02:58:10Z");
    messages.record(&[s.clone(), s.clone()]).await.unwrap();
    s.service = "jobs".into();
    s.recorder_epoch = "fixture-epoch".into();
    s.completed_at = at("2026-10-28T02:59:20Z");
    jobs.record(&[s]).await.unwrap();
    let v = jobs.read(at("2026-10-28T03:05:00Z")).await.unwrap();
    // Use the actual hour identity rather than depending on output roster order.
    let rows: Vec<(i64, serde_json::Value)> =
        sqlx::query_as("SELECT hour,payload FROM sensor_request_latency_hours ORDER BY hour")
            .fetch_all(&pool)
            .await
            .unwrap();
    let current = rows
        .iter()
        .find(|(hour, _)| *hour == t.timestamp().div_euclid(3600) * 3600)
        .unwrap();
    assert_eq!(
        current.1["writes"]["count"], 4,
        "accepted next-hour evidence must not refuse honest later samples"
    );
    assert_eq!(v["window"]["writes"]["count"], 5);
    assert_eq!(current.1["recorder_failures"], 0);
    assert_eq!(current.1["dropped_samples"], 0);
    pool.close().await;
}

#[tokio::test]
async fn old_sample_refusal_keeps_good_members_and_all_carried_debt() {
    let db = TestDb::new().await;
    let t: DateTime<Utc> = sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp(),'UTC') - interval '30 minutes'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let store = PgLatency::new(db.pool.clone());
    let mut good = sample();
    good.recording_started_at = t - chrono::Duration::hours(100);
    good.completed_at = t;
    good.duration_ns = 10_000_000;
    let mut old = good.clone();
    old.completed_at = t - chrono::Duration::hours(75);
    old.duration_ns = 999_000_000_000;
    old.recorder_failures = 3;
    old.dropped_samples = 4;
    old.recording_gap_since = Some(t - chrono::Duration::hours(76));
    store
        .record(&[good, old])
        .await
        .expect("old members refuse per sample, while good samples and debt commit");
    let v = store.read(t + chrono::Duration::hours(1)).await.unwrap();
    assert_eq!(v["window"]["writes"]["count"], 1);
    assert_eq!(v["window"]["writes"]["max_ns"], 10_000_000);
    let rows: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM sensor_request_latency_hours")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        rows.iter()
            .map(|h| h["recorder_failures"].as_u64().unwrap())
            .sum::<u64>(),
        4
    );
    assert_eq!(
        rows.iter()
            .map(|h| h["dropped_samples"].as_u64().unwrap())
            .sum::<u64>(),
        5
    );
    assert!(rows.iter().any(|h| h["recording_gap_since"] == serde_json::json!(t - chrono::Duration::hours(76))));
}

#[tokio::test]
async fn reader_authority_precedes_malformed_hours() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let app = boss_jobs::sensors::request_latency::router(std::sync::Arc::new(PgLatency::new(
        db.pool.clone(),
    )));
    let identity = serde_json::json!({"id":"fixture-reader","role":"engineer","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"it"});
    for query in ["", "?hours=bad", "?hours=300", "?hours=0"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/sensors/request-latency{query}"))
                    .header("x-boss-user", identity.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            403,
            "authority must be judged before query {query}"
        );
        let anonymous = app
            .clone()
            .oneshot(
                Request::get(format!("/api/sensors/request-latency{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            anonymous.status(),
            403,
            "anonymous authority must precede query {query}"
        );
    }
}

// d9b39685, 2026-10-29: a request past the 2 s line keeps the moment it
// stalled in, and the recorder can tell a refusal it never committed from
// one it may have.
async fn previous_half_hour(db: &TestDb) -> DateTime<Utc> {
    sqlx::query_scalar(
        "SELECT date_trunc('hour',statement_timestamp(),'UTC') - interval '30 minutes'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

fn slow_sample(at: DateTime<Utc>, duration_ns: u64, marker: &str) -> Sample {
    Sample {
        completed_at: at,
        duration_ns,
        status: 204.into(),
        method: "PATCH".into(),
        route_template: "/api/jobs/{id}/metadata".into(),
        context: Some(serde_json::json!({"shared": false, "observation": {"marker": marker}})),
        ..sample()
    }
}

#[tokio::test]
async fn a_slow_sample_keeps_its_moment_and_a_fast_one_leaves_no_moment_row() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let t = previous_half_hour(&db).await;
    let fast = Sample {
        completed_at: t,
        duration_ns: 40_000_000,
        ..sample()
    };
    store
        .record(&[slow_sample(t, 3_500_000_000, "stalled"), fast])
        .await
        .unwrap();
    let moments = store.read_slow(t, 24).await.unwrap();
    let rows = moments["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "only the request past 2 s keeps a moment");
    assert_eq!(rows[0]["duration_ns"], 3_500_000_000_u64);
    assert_eq!(rows[0]["method"], "PATCH");
    assert_eq!(rows[0]["route_template"], "/api/jobs/{id}/metadata");
    assert_eq!(rows[0]["status"], 204);
    assert_eq!(rows[0]["service"], "jobs");
    assert_eq!(rows[0]["class"], "write");
    assert_eq!(rows[0]["context"]["observation"]["marker"], "stalled");
    assert_eq!(moments["retained_per_hour"], 32);
    let series = store.read(t).await.unwrap();
    assert_eq!(
        series["window"]["writes"]["count"], 2,
        "the hourly distribution still counts both"
    );
}

#[tokio::test]
async fn each_hour_keeps_only_its_slowest_32_moments() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let t = previous_half_hour(&db).await;
    let batch = (0..40_u64)
        .map(|n| slow_sample(t, 2_100_000_000 + n * 100_000_000, &n.to_string()))
        .collect::<Vec<_>>();
    store.record(&batch).await.unwrap();
    let moments = store.read_slow(t, 24).await.unwrap();
    let rows = moments["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 32);
    let kept = rows
        .iter()
        .map(|row| row["duration_ns"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert!(
        kept.iter().all(|&ns| ns >= 2_900_000_000),
        "the eight fastest of the hour give way: {kept:?}"
    );
    assert!(kept.contains(&6_000_000_000));
}

#[tokio::test]
async fn an_oversized_moment_is_kept_as_unavailable_never_refusing_its_sample() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let t = previous_half_hour(&db).await;
    let mut huge = slow_sample(t, 2_500_000_000, "huge");
    huge.context = Some(serde_json::json!({"observation": "x".repeat(20_000)}));
    store.record(&[huge]).await.unwrap();
    let moments = store.read_slow(t, 24).await.unwrap();
    let rows = moments["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["context"]["state"], "unavailable");
}

#[tokio::test]
async fn a_refusal_before_commit_is_retryable_and_a_refusal_of_the_data_is_not() {
    let db = TestDb::new().await;
    let store = wall_store(db.pool.clone());
    let bad = Sample {
        method: "M".repeat(17),
        ..sample()
    };
    assert!(store.record(&[bad]).await.is_err());
    assert!(
        !store.last_attempt_retryable(),
        "a sample refused for its own shape would fail every retry"
    );
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('sensor_request_latency_hours',0))")
        .execute(&mut *holder)
        .await
        .unwrap();
    assert!(store.record(&[sample()]).await.is_err());
    assert!(
        store.last_attempt_retryable(),
        "a lock timeout before COMMIT committed nothing"
    );
    holder.rollback().await.unwrap();
    store.record(&[sample()]).await.unwrap();
    assert!(
        !store.last_attempt_retryable(),
        "an attempt that sent its COMMIT is never retryable"
    );
}

#[tokio::test]
async fn a_slow_moment_names_the_lock_wait_and_the_backend_holding_it() {
    use boss_core::request_latency::SlowObserver;
    let db = TestDb::new().await;
    sqlx::raw_sql("CREATE TABLE moment_fixture(id int PRIMARY KEY, v int); INSERT INTO moment_fixture VALUES (1, 0);")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut holder = db.pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("UPDATE moment_fixture SET v = 1 WHERE id = 1")
        .execute(&mut *holder)
        .await
        .unwrap();
    let waiter_options = (*db.pool.connect_options())
        .clone()
        .application_name("moment-waiter");
    let waiter = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(waiter_options)
        .await
        .unwrap();
    let waiting = tokio::spawn(async move {
        sqlx::query("UPDATE moment_fixture SET v = 2 WHERE id = 1")
            .execute(&waiter)
            .await
    });
    let mut seen = false;
    for _ in 0..100 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name = 'moment-waiter' AND wait_event_type = 'Lock'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if n > 0 {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(seen, "fixture: the waiter must be waiting on the row lock");
    let observer =
        boss_jobs::sensors::request_latency::PgSlowObserver::new(db.pool.clone(), db.pool.clone());
    let moment = observer.observe().await;
    assert_eq!(moment["state"], "observed", "{moment}");
    let database = &moment["database"];
    let waits = database["lock_waits"].as_array().unwrap();
    let waiter_row = waits
        .iter()
        .find(|w| w["application_name"] == "moment-waiter")
        .unwrap_or_else(|| panic!("the waiting backend is named: {moment}"));
    assert!(
        waiter_row["blocking_pids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pid| pid == holder_pid),
        "{moment}"
    );
    assert!(
        database["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["pid"] == holder_pid && b["state"] == "idle in transaction"),
        "the backend holding the lock is named with its state: {moment}"
    );
    assert!(database["lock_wait_count"].as_i64().unwrap() >= 1);
    assert!(
        database["wait_groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["wait_event_type"] == "Lock" && g["application_name"] == "moment-waiter"),
        "{moment}"
    );
    assert!(
        !moment.to_string().contains("UPDATE moment_fixture"),
        "no query text, the precedent of the forge's observation"
    );
    assert!(moment["serving_pool"]["max_connections"].as_u64().unwrap() > 0);
    holder.rollback().await.unwrap();
    waiting.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_moment_whose_database_cannot_answer_still_keeps_the_serving_pool() {
    use boss_core::request_latency::SlowObserver;
    let db = TestDb::new().await;
    let closed = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    closed.close().await;
    let observer =
        boss_jobs::sensors::request_latency::PgSlowObserver::new(db.pool.clone(), closed);
    let moment = observer.observe().await;
    assert_eq!(moment["database"]["state"], "unavailable", "{moment}");
    assert!(moment["database"]["reason"].is_string());
    assert!(moment["serving_pool"]["size"].is_u64());
    assert!(moment["observed_at"].is_string());
}

/// d9b39685 / 923d75bd: the live moments named LWLock:WALWrite in every
/// stall and no lock wait. A moment now times the WAL writes that happened
/// between it and the moment before it, so a stall reads as milliseconds of
/// write and sync, not only as a wait name.
#[tokio::test]
async fn a_slow_moment_times_the_wal_writes_since_the_moment_before_it() {
    use boss_core::request_latency::SlowObserver;
    let db = TestDb::new().await;
    // The deployed server turns timing on for every session from its pod
    // arguments; this shared test server does not, so each connection asks.
    let timed = |name: &str| {
        (*db.pool.connect_options())
            .clone()
            .application_name(name)
            .options([("track_wal_io_timing", "on"), ("track_io_timing", "on")])
    };
    let observer_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(timed("moment-observer"))
        .await
        .unwrap();
    let writer = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(timed("moment-wal-writer").options([("synchronous_commit", "on")]))
        .await
        .unwrap();
    let observer =
        boss_jobs::sensors::request_latency::PgSlowObserver::new(db.pool.clone(), observer_pool);
    let first = observer.observe().await;
    assert_eq!(first["database"]["state"], "observed", "{first}");
    assert_eq!(
        first["database"]["wal_since_previous"]["state"], "no earlier sample",
        "the first moment of a process has nothing to subtract: {first}"
    );
    assert_eq!(
        first["database"]["wal"]["track_wal_io_timing"], "on",
        "{first}"
    );
    sqlx::query("CREATE TABLE wal_moment_fixture(id int PRIMARY KEY, v text)")
        .execute(&writer)
        .await
        .unwrap();
    // Each autocommit flushes its own commit record under
    // synchronous_commit=on, so this backend writes WAL itself, timed.
    for id in 0..50 {
        sqlx::query("INSERT INTO wal_moment_fixture VALUES ($1, repeat('w', 4000))")
            .bind(id)
            .execute(&writer)
            .await
            .unwrap();
    }
    // Pending backend statistics reach the shared store when the backend
    // goes idle after this call, not on a 1 s cadence.
    sqlx::query("SELECT pg_stat_force_next_flush()")
        .execute(&writer)
        .await
        .unwrap();
    let second = observer.observe().await;
    let delta = &second["database"]["wal_since_previous"];
    assert_eq!(delta["state"], "observed", "{second}");
    assert!(delta["seconds"].as_f64().unwrap() > 0.0, "{second}");
    assert!(
        delta["deltas"]["wal_write"].as_f64().unwrap() >= 1.0,
        "the fixture's commits are counted: {second}"
    );
    assert!(
        delta["deltas"]["wal_write_time_ms"].as_f64().unwrap() > 0.0,
        "and timed: {second}"
    );
    assert!(
        second["database"]["io"].is_array(),
        "pg_stat_io's write and fsync times ride beside the WAL's: {second}"
    );
    assert!(
        second.to_string().len() < 16_384,
        "the moment stays inside the store's byte bound: {}",
        second.to_string().len()
    );
    assert!(!second.to_string().contains("wal_moment_fixture"));
}

#[tokio::test]
async fn the_slow_moments_read_follows_the_series_authority_and_window() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let store = std::sync::Arc::new(PgLatency::new(db.pool.clone()));
    let t: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp() - interval '1 minute'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    store
        .record(&[slow_sample(t, 2_200_000_000, "routed")])
        .await
        .unwrap();
    let app = boss_jobs::sensors::request_latency::router(store);
    let reader = serde_json::json!({"id":"fixture-reader","role":"engineer","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"it"});
    let user = serde_json::json!({"id":"fixture-reader","role":"engineer","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"it"});
    let get = |identity: &serde_json::Value, query: &str| {
        Request::get(format!("/api/sensors/request-latency{query}"))
            .header("x-boss-user", identity.to_string())
            .body(Body::empty())
            .unwrap()
    };
    let refused = app.clone().oneshot(get(&user, "?slow=true")).await.unwrap();
    assert_eq!(refused.status(), 403);
    let response = app
        .clone()
        .oneshot(get(&reader, "?slow=true&hours=2"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        body["rows"][0]["context"]["observation"]["marker"],
        "routed"
    );
    assert_eq!(body["window"]["hour_count"], 2);
    let series = app.oneshot(get(&reader, "")).await.unwrap();
    let series: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(series.into_body(), 1 << 22)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        series.get("rows").is_none(),
        "the default series read is unchanged"
    );
}
