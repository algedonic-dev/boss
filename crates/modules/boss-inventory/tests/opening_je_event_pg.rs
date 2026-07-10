//! One-construction contract for the atomic opening JE (the
//! 2026-07-09 regression): `record_inventory_je` returns the EXACT
//! in-tx fact payload plus whether THIS call inserted it. The HTTP
//! caller emits `ledger.inventory.transferred` from that payload
//! verbatim, only when inserted — so the fact's writer owns its
//! rebuild source, a replay emits nothing, and the fact rebuilt from
//! the event is byte-identical to the live one (payload-authoritative
//! `source_table` included).

#![cfg(feature = "postgres")]

use boss_inventory::PgInventory;
use boss_inventory::port::InventoryRepository;
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn opening_je_returns_verbatim_payload_once() {
    let db = TestDb::new().await;
    let inv = PgInventory::new(db.pool.clone());
    let happened_on: chrono::NaiveDate = "2025-04-01".parse().unwrap();

    let first = inv
        .record_inventory_je(
            490_000,
            "1300",
            "3000",
            "Opening balance — 196 × ING-MALT-2ROW-50 (raw materials ← retained earnings)",
            "brewery_seed_opening_balance",
            "opening-raw-ING-MALT-2ROW-50",
            happened_on,
        )
        .await
        .unwrap();
    assert!(first.inserted, "first write inserts the fact");
    // The returned payload IS the stored fact payload — the emit source
    // and the live fact can't drift because they are one construction.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM financial_facts WHERE id = $1")
            .bind(first.fact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(first.payload, stored);
    assert_eq!(
        stored.get("source_table").and_then(|v| v.as_str()),
        Some("brewery_seed_opening_balance"),
        "payload carries source_table so rebuild reproduces the provenance tag"
    );

    // The JE exists and is sized exactly.
    let (dr,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(l.debit_cents), 0)::bigint \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE a.code = '1300'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(dr, 490_000);

    // Replay: same natural key → conflicted, inserted=false (the
    // caller emits nothing), fact count unchanged.
    let replay = inv
        .record_inventory_je(
            490_000,
            "1300",
            "3000",
            "Opening balance — 196 × ING-MALT-2ROW-50 (raw materials ← retained earnings)",
            "brewery_seed_opening_balance",
            "opening-raw-ING-MALT-2ROW-50",
            happened_on,
        )
        .await
        .unwrap();
    assert!(!replay.inserted, "replay must not claim the insert");
    assert_eq!(replay.fact_id, first.fact_id);
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM financial_facts \
         WHERE source_table = 'brewery_seed_opening_balance'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}
