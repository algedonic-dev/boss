//! Layer 1 of the immutable-audit-log story: schema-level append-only
//! enforcement plus the integrity scan that surfaces tampering when an
//! operator drops the trigger.
//!
//! See `docs/architecture-decisions.md` §Correctness protocol &
//! the audit log.

#![cfg(feature = "postgres")]

use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use boss_events::{PgAuditWriter, check_audit_log_integrity};
use boss_testing::TestDb;
use chrono::{Duration, Utc};

/// Helper: insert N events through the writer so the BIGSERIAL `id`
/// column gets populated in monotonic order.
async fn seed_events(writer: &PgAuditWriter, count: usize) {
    for i in 0..count {
        let event = Event::new(
            "test",
            "test.event",
            serde_json::json!({"i": i}),
            chrono::Utc::now(),
        );
        writer.write(&event).await.unwrap();
    }
}

// =========================================================================
// Trigger: UPDATE / DELETE / TRUNCATE all fail.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn update_on_audit_log_is_rejected() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 1).await;

    let err = sqlx::query("UPDATE audit_log SET kind = 'tampered' WHERE TRUE")
        .execute(&db.pool)
        .await
        .expect_err("UPDATE on audit_log must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("audit_log is append-only"),
        "expected append-only rejection, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_on_audit_log_is_rejected() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 1).await;

    let err = sqlx::query("DELETE FROM audit_log WHERE TRUE")
        .execute(&db.pool)
        .await
        .expect_err("DELETE on audit_log must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("audit_log is append-only"),
        "expected append-only rejection, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn truncate_on_audit_log_is_rejected() {
    let db = TestDb::new().await;
    let err = sqlx::query("TRUNCATE audit_log")
        .execute(&db.pool)
        .await
        .expect_err("TRUNCATE on audit_log must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("audit_log is append-only"),
        "expected append-only rejection, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn insert_still_works_under_the_trigger() {
    // Sanity: blocking UPDATE/DELETE must not collateral-damage the
    // INSERT path that the writer relies on.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 5);
}

// =========================================================================
// Integrity scan.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn integrity_scan_is_clean_on_a_fresh_log() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 10).await;

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert_eq!(report.total_rows, 10);
    assert!(report.is_clean(), "fresh log should be clean: {report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn integrity_scan_is_clean_on_an_empty_log() {
    let db = TestDb::new().await;
    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert_eq!(report.total_rows, 0);
    assert!(report.is_clean());
}

#[tokio::test(flavor = "multi_thread")]
async fn epoch_trim_gap_at_baseline_is_sanctioned_not_an_anomaly() {
    // The demo's restart_epoch trims audit_log back to
    // sim_clock.epoch_baseline_audit_id every rollover, so exactly ONE
    // id gap — the one starting at the baseline row — is by-design.
    // Pre-fix the nightly timer went red on every epoch for it
    // (playground, 2026-07-13/14: gap 2405 → first post-trim id).
    // The scan must report it separately, not as an anomaly, and not
    // silently drop it either.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    // The trim, exactly as restart_epoch performs it.
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id > 2")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sim_clock (id, epoch_baseline_audit_id, epoch_start_date, wall_anchor) \
         VALUES (1, 2, '2025-03-31', NOW())",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // The new epoch writes on; the sequence continues past the trim.
    seed_events(&writer, 2).await;

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(
        report.gaps.is_empty(),
        "the baseline gap must not be an anomaly: {report:?}"
    );
    let sanctioned = report
        .sanctioned_trim_gap
        .as_ref()
        .expect("the trim gap must be reported, not dropped");
    assert_eq!(sanctioned.prev_id, 2);
    assert_eq!(sanctioned.id, 6);
    assert!(report.is_clean(), "a by-design trim is clean: {report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn gap_elsewhere_stays_an_anomaly_even_with_a_baseline() {
    // A baseline sanctions exactly the gap that STARTS at it. Any
    // other gap is still a trigger-bypass signal.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    sqlx::query(
        "INSERT INTO sim_clock (id, epoch_baseline_audit_id, epoch_start_date, wall_anchor) \
         VALUES (1, 2, '2025-03-31', NOW())",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id = 4")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert_eq!(report.gaps.len(), 1, "{report:?}");
    assert_eq!(report.gaps[0].prev_id, 3);
    assert!(report.sanctioned_trim_gap.is_none());
    assert!(!report.is_clean());
}

#[tokio::test(flavor = "multi_thread")]
async fn integrity_scan_detects_an_id_gap() {
    // Simulate the trigger-bypass attack: disable the trigger,
    // delete a row, re-enable. The scan must surface the gap.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id = 3")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.total_rows, 4);
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].prev_id, 2);
    assert_eq!(report.gaps[0].id, 4);
    assert_eq!(report.gaps[0].missing_count(), 1);
    assert!(report.regressions.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn integrity_scan_detects_a_multi_row_gap() {
    // A bigger hole — three consecutive rows wiped — collapses to a
    // single IdGap with missing_count = 3.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 8).await;

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id BETWEEN 4 AND 6")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].prev_id, 3);
    assert_eq!(report.gaps[0].id, 7);
    assert_eq!(report.gaps[0].missing_count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn integrity_scan_detects_created_at_regression() {
    // Simulate a row being rewritten with a backdated `created_at`
    // (the kind of tampering that would otherwise be invisible in
    // BIGSERIAL order). UPDATE is normally blocked, so the test
    // disables the trigger, mutates, re-enables.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    // Postgres TIMESTAMPTZ stores microseconds; chrono::Utc::now()
    // carries nanos. Round to microseconds before binding so the
    // round-trip equality assertion below is exact.
    let backdate_raw = Utc::now() - Duration::days(30);
    let backdate_us = backdate_raw.timestamp_micros();
    let backdate = chrono::DateTime::<Utc>::from_timestamp_micros(backdate_us)
        .expect("microsecond timestamp roundtrip");

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET created_at = $1 WHERE id = 4")
        .bind(backdate)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean());
    // Row 4 went earlier than row 3 (regression at id=4) AND row 5
    // is later than row 4 — but row 5 is *later* than row 3 so the
    // only regression is at id=4 vs id=3.
    assert_eq!(report.regressions.len(), 1);
    assert_eq!(report.regressions[0].prev_id, 3);
    assert_eq!(report.regressions[0].id, 4);
    assert_eq!(report.regressions[0].created_at, backdate);
}

// =========================================================================
// Layer 2: hash chain.
// =========================================================================

const ZERO_HASH: [u8; 32] = [0u8; 32];

#[tokio::test(flavor = "multi_thread")]
async fn genesis_row_uses_zero_seed_for_prev_hash() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 1).await;

    let (prev_hash, row_hash): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT prev_hash, row_hash FROM audit_log ORDER BY id LIMIT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();

    assert_eq!(prev_hash, ZERO_HASH.to_vec());
    assert_eq!(row_hash.len(), 32);
    assert_ne!(row_hash, prev_hash, "row_hash must differ from prev_hash");
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_inserts_form_a_valid_chain() {
    // Each row's prev_hash equals the previous row's row_hash.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    let rows: Vec<(i64, Vec<u8>, Vec<u8>)> =
        sqlx::query_as("SELECT id, prev_hash, row_hash FROM audit_log ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();

    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].1, ZERO_HASH.to_vec(), "genesis prev_hash is zero");
    for window in rows.windows(2) {
        assert_eq!(
            window[1].1, window[0].2,
            "row {}'s prev_hash must equal row {}'s row_hash",
            window[1].0, window[0].0
        );
    }

    // And the integrity scan reports clean.
    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(report.is_clean(), "valid chain should be clean: {report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chain_verifier_detects_tampered_payload() {
    // Tamper with the payload but leave the hash columns alone — the
    // recomputed hash differs from the stored hash, so the chain
    // breaks at the tampered row AND every subsequent row inherits a
    // wrong prev_hash, but our verifier reports each row whose
    // stored != recomputed independently. Since later rows' stored
    // hashes were computed from the *original* row 3's row_hash, and
    // tampering didn't change row 3's row_hash (only the payload),
    // those later rows still verify clean. Only row 3 breaks.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET payload = '{\"tampered\": true}'::jsonb WHERE id = 3")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.chain_breaks.len(), 1);
    assert_eq!(report.chain_breaks[0].id, 3);
    assert_eq!(report.chain_breaks[0].stored_hash.len(), 32);
    assert_eq!(report.chain_breaks[0].computed_hash.len(), 32);
    assert_ne!(
        report.chain_breaks[0].stored_hash,
        report.chain_breaks[0].computed_hash
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chain_verifier_detects_deleted_row() {
    // Delete row 3. The verifier walks id-order: row 4's stored_hash
    // was computed against row 3's row_hash, but the recomputation
    // (LAG(row_hash) over the surviving rows) sees row 2 as row 4's
    // predecessor. Mismatch at id=4.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE id = 3")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean());
    // Both signals fire: id gap at 2→4, AND chain break at id=4.
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].id, 4);
    assert!(
        report.chain_breaks.iter().any(|b| b.id == 4),
        "expected chain break at id=4, got: {:?}",
        report.chain_breaks
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chain_verifier_detects_inserted_row() {
    // Insert a row mid-chain with a hand-rolled (incorrect) row_hash.
    // The verifier must catch it, because recomputing the chain from
    // `payload::text` won't reproduce the attacker's hash.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed_events(&writer, 5).await;

    // Disable the BEFORE INSERT trigger so we can supply our own
    // (wrong) prev_hash + row_hash. This simulates an attacker who
    // bypasses the writer-side trigger entirely.
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_compute_row_hash_trg")
        .execute(&db.pool)
        .await
        .unwrap();
    // The trigger normally allocates id + created_at post-lock
    // (`b550a03`), so disabling it means the attacker has to
    // supply both columns themselves. nextval(...) keeps the id
    // contiguous so the gap-detector doesn't false-positive on
    // top of the chain break we're actually testing for.
    sqlx::query(
        "INSERT INTO audit_log \
             (id, event_id, timestamp, source, kind, payload, created_at, prev_hash, row_hash) \
         VALUES (nextval('audit_log_id_seq'), gen_random_uuid(), NOW(), 'attacker', \
                 'forged.event', '{}'::jsonb, clock_timestamp(), \
                 decode(repeat('aa', 32), 'hex'), decode(repeat('bb', 32), 'hex'))",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_compute_row_hash_trg")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean());
    assert!(
        !report.chain_breaks.is_empty(),
        "expected at least one chain break for the forged row"
    );
    // The forged row has id=6 (BIGSERIAL after 5 seeded rows).
    assert!(
        report.chain_breaks.iter().any(|b| b.id == 6),
        "expected chain break at the forged row id=6, got: {:?}",
        report.chain_breaks
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chain_verifier_clean_on_empty_log() {
    let db = TestDb::new().await;
    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(report.chain_breaks.is_empty());
    assert!(report.is_clean());
}

// =========================================================================
// Soft FKs.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn dangling_invoice_account_ref_is_caught() {
    // The classic brewery seed-pipeline regression: invoice points
    // at an account_id that was never emitted as
    // accounts.account.created. The integrity scan must surface it.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    // Invoice references acc-bigseed-0099 — never seeded as an
    // account.created event.
    let inv = Event::new(
        "commerce",
        "commerce.invoice.created",
        serde_json::json!({
            "id": "inv-test-001",
            "account_id": "acc-bigseed-0099",
            "amount_cents": 1000,
        }),
        chrono::Utc::now(),
    );
    writer.write(&inv).await.unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean(), "expected dangling-ref anomaly");
    assert_eq!(report.dangling_refs.len(), 1);
    assert_eq!(report.dangling_refs[0].kind, "commerce.invoice.created");
    assert_eq!(report.dangling_refs[0].field, "account_id");
    assert_eq!(report.dangling_refs[0].foreign_id, "acc-bigseed-0099");
    assert_eq!(
        report.dangling_refs[0].expected_parent_kind,
        "accounts.account.created"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invoice_with_seeded_account_is_clean() {
    // Same shape, but the parent account.created event lands first.
    // Scan must come up clean.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let acct = Event::new(
        "accounts",
        "accounts.account.created",
        serde_json::json!({
            "id": "acc-bigseed-0001",
            "name": "Test Account",
        }),
        chrono::Utc::now(),
    );
    writer.write(&acct).await.unwrap();

    let inv = Event::new(
        "commerce",
        "commerce.invoice.created",
        serde_json::json!({
            "id": "inv-test-001",
            "account_id": "acc-bigseed-0001",
        }),
        chrono::Utc::now(),
    );
    writer.write(&inv).await.unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(
        report.dangling_refs.is_empty(),
        "expected clean log: {:?}",
        report.dangling_refs
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dangling_refs_distinct_per_invoice() {
    // Two invoices both pointing at the same missing account each
    // contribute their own DanglingForeignRef row — operators see
    // every audit_log id involved, not just the first.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    for i in 0..3 {
        let inv = Event::new(
            "commerce",
            "commerce.invoice.created",
            serde_json::json!({
                "id": format!("inv-test-{i:03}"),
                "account_id": "acc-bigseed-9999",
            }),
            chrono::Utc::now(),
        );
        writer.write(&inv).await.unwrap();
    }

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert_eq!(report.dangling_refs.len(), 3);
    for r in &report.dangling_refs {
        assert_eq!(r.foreign_id, "acc-bigseed-9999");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dangling_vendor_invoice_vendor_ref_is_caught() {
    // Sibling of the invoice→account rule: a vendor_invoice event
    // referencing a vendor with no matching vendor.created anywhere
    // in the log must be caught. Note the field name is `vendor` (not
    // `vendor_id` like POs use) — an asymmetry to keep in mind.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let vi = Event::new(
        "inventory",
        "inventory.vendor_invoice.upserted",
        serde_json::json!({
            "id": "vi-test-001",
            "vendor": "vendor-00001",
            "amount_cents": 50000,
        }),
        chrono::Utc::now(),
    );
    writer.write(&vi).await.unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(!report.is_clean(), "expected dangling-ref anomaly");
    assert_eq!(report.dangling_refs.len(), 1);
    let r = &report.dangling_refs[0];
    assert_eq!(r.kind, "inventory.vendor_invoice.upserted");
    assert_eq!(r.field, "vendor");
    assert_eq!(r.foreign_id, "vendor-00001");
    assert_eq!(r.expected_parent_kind, "inventory.vendor.created");
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_invoice_with_seeded_vendor_is_clean() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let vendor = Event::new(
        "inventory",
        "inventory.vendor.created",
        serde_json::json!({
            "id": "vnd-bigseed-001",
            "name": "Test Vendor",
        }),
        chrono::Utc::now(),
    );
    writer.write(&vendor).await.unwrap();

    let vi = Event::new(
        "inventory",
        "inventory.vendor_invoice.upserted",
        serde_json::json!({
            "id": "vi-test-001",
            "vendor": "vnd-bigseed-001",
            "amount_cents": 50000,
        }),
        chrono::Utc::now(),
    );
    writer.write(&vi).await.unwrap();

    let report = check_audit_log_integrity(&db.pool).await.unwrap();
    assert!(
        report.dangling_refs.is_empty(),
        "expected clean log: {:?}",
        report.dangling_refs
    );
}
