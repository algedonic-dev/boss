//! Direct Jobs writers must leave the same original clock facts as
//! Clock API writers. A mutable row and ephemeral tick prove no past.
use std::sync::Arc;

use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::Value;

#[tokio::test(flavor = "multi_thread")]
async fn direct_clock_writers_conserve_snapshots_and_do_not_invent_a_mode() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO sim_clock(epoch_start_date,warp_factor) VALUES ('2026-01-01',1)")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE sim_clock SET paused=true,paused_at=clock_timestamp() WHERE id=1")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE sim_clock SET paused=false,paused_at=NULL,wall_anchor=clock_timestamp(),paused_offset_seconds=0 WHERE id=1")
        .execute(&db.pool).await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM audit_log WHERE kind='clock.mapping.recorded' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "insert, direct pause, direct reset all leave originals"
    );
    assert_eq!(rows[0]["schema_version"], 1);
    assert!(
        rows[0]["mode"].is_null(),
        "a sim_clock row alone is not mode evidence"
    );
    assert_eq!(rows[0]["params"]["paused"], false);
    assert_eq!(rows[1]["params"]["paused"], true);
    assert_eq!(rows[2]["params"]["paused"], false);
    assert_eq!(rows[2]["before"], rows[1]["params"]);
    assert!(rows[0]["captured_at"].is_string());
    assert_ne!(rows[0]["revision"], rows[1]["revision"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn truncation_records_unavailable_mapping_instead_of_silent_coverage() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO sim_clock(epoch_start_date) VALUES ('2026-01-01')")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO clock_mapping_mode(mode,epoch) VALUES ('sim',gen_random_uuid())")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("TRUNCATE sim_clock")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("TRUNCATE clock_mapping_mode")
        .execute(&db.pool)
        .await
        .unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM audit_log WHERE kind='clock.mapping.recorded' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        4,
        "both disappearance boundaries are originals too"
    );
    assert!(rows[2]["params"].is_null());
    assert_eq!(rows[2]["operation"], "TRUNCATE");
    assert!(rows[3]["mode"].is_null());
    assert_eq!(rows[3]["operation"], "TRUNCATE");
}
