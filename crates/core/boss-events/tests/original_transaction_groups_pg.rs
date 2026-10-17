//! Membership replay needs the producer's transaction, including a
//! completeness witness that survives a relay splitting its siblings.
use std::sync::Arc;

use boss_core::{event::Event, port::EventBus};
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{TimeZone, Utc};
use serde_json::Value;

fn event() -> Event {
    Event::new(
        "original-group-test",
        "original.group.test",
        serde_json::json!({}),
        Utc.with_ymd_and_hms(2026, 10, 16, 1, 0, 0).unwrap(),
    )
}

async fn roster(pool: &sqlx::PgPool, group: &Value) -> Vec<uuid::Uuid> {
    sqlx::query_scalar(
        "SELECT event_id FROM original_event_group_members WHERE group_id=$1 ORDER BY member_seq",
    )
    .bind(uuid::Uuid::parse_str(group["id"].as_str().unwrap()).unwrap())
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn producer_siblings_keep_their_complete_group_across_partial_relay() {
    let db = TestDb::new().await;
    let a = event();
    let b = event();
    let c = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    record_event_in_tx(&mut tx, &b).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &c).await.unwrap();
    tx.commit().await.unwrap();

    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 1).await.unwrap();
    let group: Value = sqlx::query_scalar("SELECT original_group FROM audit_log WHERE event_id=$1")
        .bind(a.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(group["schema_version"], 1);
    assert_eq!(group["event_count"], 2);
    assert_eq!(roster(&db.pool, &group).await, vec![a.id, b.id]);
    assert!(group["member_hash"].is_string());
    let reference = serde_json::from_value(group.clone()).unwrap();
    let observed = boss_events::original_groups::read_group(&db.pool, &reference, i64::MAX, 100)
        .await
        .unwrap();
    assert!(!observed.complete);
    assert!(
        !observed.validate(i64::MAX).unwrap(),
        "pure roster hash agrees with original SQL seal"
    );
    assert_eq!(observed.event_ids, vec![a.id, b.id]);
    let delivered: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(delivered, 1, "a prefix is not a complete original group");

    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let siblings: Vec<Value> = sqlx::query_scalar(
        "SELECT original_group FROM audit_log WHERE event_id=ANY($1) ORDER BY id",
    )
    .bind(vec![a.id, b.id])
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(siblings, vec![group.clone(), group.clone()]);
    let observed = boss_events::original_groups::read_group(&db.pool, &reference, i64::MAX, 100)
        .await
        .unwrap();
    assert!(observed.complete);
    assert!(observed.validate(i64::MAX).unwrap());
    let old_cut = boss_events::original_groups::read_group(&db.pool, &reference, 1, 100)
        .await
        .unwrap();
    assert!(
        !old_cut.complete,
        "a later relay cannot strengthen an older frozen audit cutoff"
    );
    let other: Value = sqlx::query_scalar("SELECT original_group FROM audit_log WHERE event_id=$1")
        .bind(c.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_ne!(
        other["id"], group["id"],
        "equal event time is not atomicity"
    );
    assert_eq!(roster(&db.pool, &other).await, vec![c.id]);
}

#[tokio::test(flavor = "multi_thread")]
async fn roster_tampering_is_refused_against_the_audit_anchored_reference() {
    let db = TestDb::new().await;
    let a = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let reference: Value =
        sqlx::query_scalar("SELECT original_group FROM audit_log WHERE event_id=$1")
            .bind(a.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let reference = serde_json::from_value(reference).unwrap();
    sqlx::query("ALTER TABLE original_event_group_members DISABLE TRIGGER original_event_group_members_guard")
        .execute(&db.pool).await.unwrap();
    sqlx::query("UPDATE original_event_group_members SET event_id=gen_random_uuid()")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE original_event_group_members ENABLE TRIGGER original_event_group_members_guard")
        .execute(&db.pool).await.unwrap();
    assert!(
        boss_events::original_groups::read_group(&db.pool, &reference, i64::MAX, 100)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rolled_back_sibling_and_later_transactions_cannot_change_a_sealed_group() {
    let db = TestDb::new().await;
    let a = event();
    let rolled_back = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    sqlx::query("SAVEPOINT sibling")
        .execute(&mut *tx)
        .await
        .unwrap();
    record_event_in_tx(&mut tx, &rolled_back).await.unwrap();
    sqlx::query("ROLLBACK TO SAVEPOINT sibling")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let members: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT event_id FROM original_event_group_members ORDER BY member_seq")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(members, vec![a.id]);
    assert!(
        sqlx::query("UPDATE original_event_groups SET member_hash='forged'")
            .execute(&db.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM original_event_groups")
            .execute(&db.pool)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_audit_and_uncommitted_producer_do_not_gain_a_group_witness() {
    let db = TestDb::new().await;
    let a = event();
    let legacy = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    let group: Option<Value> = sqlx::query_scalar(
        "INSERT INTO audit_log(event_id,timestamp,source,kind,payload) \
         VALUES ($1,$2,$3,$4,$5) RETURNING original_group",
    )
    .bind(a.id)
    .bind(a.timestamp)
    .bind(&a.source)
    .bind(&a.kind)
    .bind(&a.payload)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        group, None,
        "a producer that can still add siblings is not sealed"
    );
    tx.rollback().await.unwrap();

    let group: Option<Value> = sqlx::query_scalar(
        "INSERT INTO audit_log(event_id,timestamp,source,kind,payload) \
         VALUES ($1,$2,$3,$4,$5) RETURNING original_group",
    )
    .bind(legacy.id)
    .bind(legacy.timestamp)
    .bind(&legacy.source)
    .bind(&legacy.kind)
    .bind(&legacy.payload)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        group, None,
        "legacy history is not backfilled from a relay transaction"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_survives_outbox_retention_and_caller_witnesses_are_refused() {
    let db = TestDb::new().await;
    let a = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    sqlx::query("DELETE FROM event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let members: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT event_id FROM original_event_group_members ORDER BY member_seq")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(members, vec![a.id]);
    assert!(
        sqlx::query(
            "INSERT INTO audit_log(event_id,timestamp,source,kind,payload,original_group) \
         VALUES (gen_random_uuid(),NOW(),'forged','original.group.test','{}','{}')",
        )
        .execute(&db.pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("TRUNCATE original_event_groups CASCADE")
            .execute(&db.pool)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interleaved_producers_keep_nonadjacent_atomic_siblings() {
    let db = TestDb::new().await;
    let a = event();
    let sibling = event();
    let unrelated = event();
    let mut first = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut first, &a).await.unwrap();
    let mut second = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut second, &unrelated).await.unwrap();
    second.commit().await.unwrap();
    record_event_in_tx(&mut first, &sibling).await.unwrap();
    first.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 2).await.unwrap();
    let rows: Vec<(uuid::Uuid, Value)> =
        sqlx::query_as("SELECT event_id,original_group FROM audit_log ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, a.id);
    assert_eq!(rows[1].0, unrelated.id);
    assert_eq!(roster(&db.pool, &rows[0].1).await, vec![a.id, sibling.id]);
    assert_eq!(roster(&db.pool, &rows[1].1).await, vec![unrelated.id]);
    assert_ne!(rows[0].1["id"], rows[1].1["id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_existing_chain_verifier_detects_original_group_tampering() {
    let db = TestDb::new().await;
    let a = event();
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &a).await.unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    assert!(
        boss_events::integrity::check_audit_log_integrity(&db.pool)
            .await
            .unwrap()
            .chain_breaks
            .is_empty()
    );
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET original_group=jsonb_set(original_group,'{member_hash}','\"forged\"')")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        boss_events::integrity::check_audit_log_integrity(&db.pool)
            .await
            .unwrap()
            .chain_breaks
            .len(),
        1,
        "group completeness is original evidence covered by the audit chain"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn per_event_group_evidence_does_not_repeat_a_bulk_transactions_whole_roster() {
    let db = TestDb::new().await;
    let mut first_ids = Vec::new();
    for count in [2, 64] {
        let mut tx = db.pool.begin().await.unwrap();
        for index in 0..count {
            let e = event();
            if index == 0 {
                first_ids.push(e.id);
            }
            record_event_in_tx(&mut tx, &e).await.unwrap();
        }
        tx.commit().await.unwrap();
    }
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    let mut sizes = Vec::new();
    for id in first_ids {
        let size: i32 = sqlx::query_scalar(
            "SELECT length(original_group::text) FROM audit_log WHERE event_id=$1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        sizes.push(size);
    }
    assert!(
        sizes[1] <= sizes[0] * 2,
        "each sibling needs a tamper-bound roster reference, not quadratic copies: sizes={sizes:?}"
    );
}
