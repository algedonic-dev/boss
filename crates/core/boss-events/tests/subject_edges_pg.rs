//! The `subject_edges` referential guard (R2 — the one relationship
//! registry). The BEFORE-INSERT trigger on `event_outbox` (and
//! `audit_log`) reads `subject_edges` and resolves every declared
//! edge against the `subjects` identity table, aborting the write
//! when the referenced subject is missing — inside the domain
//! transaction, so a dangling reference never becomes state (the
//! 2026-07-13 phantom-account incident class, now closed for every
//! subject-referential edge, not just the hand-picked ones).

use boss_core::event::Event;
use boss_events::outbox::record_event_in_tx;
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};
use uuid::Uuid;

async fn declare_edge(db: &TestDb, source_kind: &str, field: &str, target: &str, on_missing: &str) {
    sqlx::query(
        "INSERT INTO subject_edges (source_kind, field_path, target_kind, on_missing) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(source_kind)
    .bind(field)
    .bind(target)
    .bind(on_missing)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// Declare a dynamic-kind edge: the target kind is read from the
/// event payload at `target_kind_path` (R2 PR2 — the typed-pair
/// edges: job.subject, asset custody holder).
async fn declare_dynamic_edge(db: &TestDb, source_kind: &str, field: &str, kind_path: &str) {
    sqlx::query(
        "INSERT INTO subject_edges (source_kind, field_path, target_kind_path) \
         VALUES ($1, $2, $3)",
    )
    .bind(source_kind)
    .bind(field)
    .bind(kind_path)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn seed_subject(db: &TestDb, kind: &str, id: &str) {
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ($1, $2)")
        .bind(kind)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
}

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 7, 17, 12, 0, 0).unwrap(),
        source: "subject-edges-test".to_string(),
        kind: kind.to_string(),
        payload,
    }
}

// TestDb disables ref-checks database-wide (test sessions write
// events without the full projection pipeline that populates parent
// tables); re-enable for exactly the transaction under test.
// SET LOCAL cannot leak through the pool.
async fn enable_ref_check(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) {
    sqlx::query("SET LOCAL audit_log.ref_check = 'on'")
        .execute(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn edge_resolves_present_subject_and_aborts_a_missing_one() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.thing.happened", "account_id", "account", "abort").await;
    seed_subject(&db, "account", "acc-real").await;

    // Present reference → the write lands.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.thing.happened",
            serde_json::json!({"account_id": "acc-real"}),
        ),
    )
    .await
    .expect("present subject must pass the edge guard");
    tx.commit().await.unwrap();

    // Missing reference → the guard aborts the domain write.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    let err = record_event_in_tx(
        &mut tx,
        &event(
            "test.thing.happened",
            serde_json::json!({"account_id": "acc-ghost"}),
        ),
    )
    .await;
    assert!(err.is_err(), "missing subject must abort the write");
    drop(tx);

    // Only the committed (valid) row survives.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'test.thing.happened'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "the aborted write must leave no row");
}

#[tokio::test(flavor = "multi_thread")]
async fn null_or_empty_reference_is_skipped() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.optional.ref", "account_id", "account", "abort").await;

    // Field absent entirely → no check.
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event("test.optional.ref", serde_json::json!({"other": 1})),
    )
    .await
    .expect("absent optional ref must skip the check");
    // Field present but empty string → no check.
    record_event_in_tx(
        &mut tx,
        &event("test.optional.ref", serde_json::json!({"account_id": ""})),
    )
    .await
    .expect("empty optional ref must skip the check");
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn on_missing_warn_allows_the_write() {
    let db = TestDb::new().await;
    declare_edge(&db, "test.soft.ref", "account_id", "account", "warn").await;

    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.soft.ref",
            serde_json::json!({"account_id": "acc-ghost"}),
        ),
    )
    .await
    .expect("on_missing=warn must allow a missing reference");
    tx.commit().await.unwrap();

    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'test.soft.ref'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_module_edges_are_present() {
    // The migration moved the invoice→account and products→product
    // rows off audit_log_ref_checks and onto subject_edges; the
    // part_sku non-subject residual stays behind. Pin both halves so
    // a botched migration fails here.
    let db = TestDb::new().await;
    let subject_edges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges WHERE target_kind = 'account' \
         AND source_kind LIKE 'commerce.invoice.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        subject_edges, 4,
        "invoice→account edges must be in subject_edges"
    );

    let residual: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log_ref_checks WHERE ref_table = 'inventory_items'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        residual, 2,
        "part_sku checks stay on the non-subject residual"
    );

    // And nothing subject-referential was left behind on the old
    // mechanism.
    let stragglers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log_ref_checks WHERE ref_table IN ('accounts', 'products')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        stragglers, 0,
        "no subject-referential rows remain on audit_log_ref_checks"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_target_kind_resolves_from_the_payload() {
    // The typed-pair shape (R2 PR2): the event names its own target
    // kind. One edge rule covers "the brewery installs at locations;
    // the device shop ships to accounts".
    let db = TestDb::new().await;
    declare_dynamic_edge(&db, "test.custody.moved", "holder_id", "holder_kind").await;
    seed_subject(&db, "location", "loc-real").await;
    seed_subject(&db, "account", "acc-real").await;

    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    // Same rule resolves against either kind, per event.
    record_event_in_tx(
        &mut tx,
        &event(
            "test.custody.moved",
            serde_json::json!({"holder_kind": "location", "holder_id": "loc-real"}),
        ),
    )
    .await
    .expect("location holder must resolve");
    record_event_in_tx(
        &mut tx,
        &event(
            "test.custody.moved",
            serde_json::json!({"holder_kind": "account", "holder_id": "acc-real"}),
        ),
    )
    .await
    .expect("account holder must resolve");
    tx.commit().await.unwrap();

    // Wrong kind for a real id → abort (the pair is checked as a
    // pair, not id-existence-anywhere).
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    let err = record_event_in_tx(
        &mut tx,
        &event(
            "test.custody.moved",
            serde_json::json!({"holder_kind": "account", "holder_id": "loc-real"}),
        ),
    )
    .await;
    assert!(err.is_err(), "kind-mismatched pair must abort");
    drop(tx);

    // Id present but kind absent → skipped like an absent ref
    // (identity-first events may carry neither half yet).
    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.custody.moved",
            serde_json::json!({"holder_id": "loc-real"}),
        ),
    )
    .await
    .expect("kindless ref must skip the check");
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_field_paths_resolve_with_dotted_syntax() {
    // The job.subject shape: {"subject": {"subject_kind": ..., "id":
    // ...}} — dotted paths on both halves (the #140 nesting lesson,
    // now enforceable at the write).
    let db = TestDb::new().await;
    declare_dynamic_edge(&db, "test.job.opened", "subject.id", "subject.subject_kind").await;
    seed_subject(&db, "account", "acc-nested").await;

    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    record_event_in_tx(
        &mut tx,
        &event(
            "test.job.opened",
            serde_json::json!({"subject": {"subject_kind": "account", "id": "acc-nested"}}),
        ),
    )
    .await
    .expect("nested subject pair must resolve");
    tx.commit().await.unwrap();

    let mut tx = db.pool.begin().await.unwrap();
    enable_ref_check(&mut tx).await;
    let err = record_event_in_tx(
        &mut tx,
        &event(
            "test.job.opened",
            serde_json::json!({"subject": {"subject_kind": "account", "id": "acc-phantom"}}),
        ),
    )
    .await;
    assert!(err.is_err(), "nested phantom subject must abort");
    drop(tx);
}

#[tokio::test(flavor = "multi_thread")]
async fn edge_rows_require_exactly_one_kind_source() {
    // The CHECK constraint: static target_kind XOR dynamic
    // target_kind_path.
    let db = TestDb::new().await;
    let both = sqlx::query(
        "INSERT INTO subject_edges (source_kind, field_path, target_kind, target_kind_path) \
         VALUES ('test.bad.both', 'x', 'account', 'y')",
    )
    .execute(&db.pool)
    .await;
    assert!(both.is_err(), "both kind sources must be rejected");
    let neither = sqlx::query(
        "INSERT INTO subject_edges (source_kind, field_path) VALUES ('test.bad.neither', 'x')",
    )
    .execute(&db.pool)
    .await;
    assert!(neither.is_err(), "neither kind source must be rejected");
}

#[tokio::test(flavor = "multi_thread")]
async fn pr2_module_edges_are_seeded() {
    // Pin the R2 PR2 edge rows: jobs' subject pair (dynamic), the
    // asset custody pair (dynamic) + commerce account refs, and the
    // PO vendor.
    let db = TestDb::new().await;
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges \
         WHERE source_kind IN ('jobs.job.created', 'jobs.job.updated') \
           AND field_path = 'subject.id' AND target_kind_path = 'subject.subject_kind'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(jobs, 2, "job subject edges (created + updated)");

    // Top-level, not `kind.holder_id`. `AssetEvent` declares
    // `#[serde(flatten)] kind: AssetEventKind`, so the variant's
    // fields land beside `asset_id` and `kind` on the wire is just
    // the tag string.
    //
    // This assertion used to name the prefixed paths, and passed —
    // the rows existed exactly as written. What it could not see was
    // that nothing resolved through them: 170 `asset.installed`
    // events sat unlinked behind a rule that looked correct in the
    // registry. Pinning the seed's spelling is not the same as
    // pinning its behavior, so keep the negative below.
    let custody: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges \
         WHERE source_kind IN ('asset.shipped', 'asset.installed') \
           AND field_path = 'holder_id' AND target_kind_path = 'holder_kind'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(custody, 2, "asset custody-pair edges");

    // No asset edge may address a flattened field through `kind.`.
    // Such a row resolves to NULL for every event it matches and
    // reports nothing — the failure is silent, which is why it needs
    // a test rather than a reader.
    let prefixed: Vec<String> = sqlx::query_scalar(
        "SELECT source_kind || ' -> ' || field_path FROM subject_edges \
         WHERE source_kind LIKE 'asset.%' \
           AND (field_path LIKE 'kind.%' OR target_kind_path LIKE 'kind.%')",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        prefixed.is_empty(),
        "asset edges must address flattened fields at the top level; \
         these resolve to NULL on every event: {prefixed:?}"
    );

    let commerce: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges \
         WHERE source_kind IN ('asset.sold', 'asset.ownership_transferred') \
           AND target_kind = 'account'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(commerce, 3, "asset commerce account refs");

    let po: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subject_edges \
         WHERE source_kind = 'inventory.purchase_order.upserted' AND target_kind = 'vendor'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(po, 1, "PO vendor edge");
}
