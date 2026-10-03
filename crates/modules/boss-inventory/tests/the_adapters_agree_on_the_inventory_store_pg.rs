//! The parts inventory store answers the same on both
//! `InventoryRepository` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), the
//! last MODULE port of backlog be459ab9's census. Tests elsewhere (the
//! HTTP tier's `tests/common`) are answered by `InMemoryInventory`, while
//! production is answered by `PgInventory`.
//!
//! WHY IT EXISTS. The port was stated for Postgres alone (the `*_pg.rs`
//! files beside this one) and the double was stated nowhere; this file
//! is the one statement of every method of the port, each case holding
//! each adapter to the same stated answer, and every write also judged
//! by the facts it leaves — the double's `recorded_events` and
//! Postgres's `event_outbox`, staged in the write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - THE DOUBLE WAS A STUB. An item upsert stored nothing; a consume or
//!   receive moved no stock, wrote no proof of application and recorded
//!   only a snapshot; `record_inventory_je` and
//!   `record_overhead_absorbed` minted a fresh random fact id and
//!   answered `inserted` every time; a re-posted PO recorded nothing and
//!   kept its first status; a status flip on an unknown PO answered Ok;
//!   `open_po_exists_for_part` and `primary_vendor_for_part` answered
//!   false and None whatever the store held; a vendor update could
//!   rewrite the row's id; an invoice read back the lines Postgres does
//!   not keep. It now models each, calling the same payload builders and
//!   the same canonical fact id (`boss_ledger::deterministic_fact_id`).
//! - LIST ORDER. Postgres ordered `part_sku`, PO lines, vendor names and
//!   the category fallback's vendor ids in the database's locale, the
//!   double in byte order or insertion order; and ties (two POs placed
//!   the same day, two invoices received the same day, two vendors of
//!   one name) came back in whatever order the plan produced. Each now
//!   orders `COLLATE "C"` with an id tie-break (backlog 2987fb2d's
//!   class).
//! - A PO WITH NO VENDOR. `primary_vendor_for_part` decoded a vendor-less
//!   (draft) PO's NULL vendor as a String and failed `Storage` (a 500);
//!   a PO that names no vendor associates none, so it is skipped.
//! - REFUSALS THAT READ AS 500s. A non-positive JE or overhead, a NUL
//!   byte in any written text, a currency not three characters long, a
//!   negative limit: each was Postgres's own error as `Storage`. Each is
//!   now `Invalid` naming the field, a 400 at the HTTP tier. A PO naming
//!   a vendor no Subject holds and an invoice naming a PO nobody placed
//!   were a trigger's and a foreign key's error as `Storage`; both are
//!   now `NotFound` naming the reference, a 404.
//! - AN UNKNOWN GL ACCOUNT. `record_overhead_absorbed` mapped the
//!   ledger's `UnknownAccount` to `InvalidAccount` (a 422), but its fact
//!   was already posted inside `insert_fact`, which answered `Storage`
//!   first — the mapping was dead. It is made in `insert_fact` now, for
//!   every fact the adapter posts; the double keeps no chart of
//!   accounts, so that refusal is stated for Postgres alone, below the
//!   suite.
//!
//! KNOWN LIMIT. `inbound_reserved_for_part` reads the jobs store's open
//! restock steps; the double holds no jobs and answers 0, so the suite
//! states only the empty world.
//!
//! The world: the suite's rows are written THROUGH the port, so each
//! adapter seeds itself the way production does. Migrations may seed
//! rows of their own, so every read is judged over `suite-` keys. The
//! fixture holds a case pair and a punctuation pair (`suite-B`,
//! `suite-a-z`, `suite-ab`) — the only shape that catches the database's
//! locale disagreeing with the double's byte order.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_inventory::events::{
    INVENTORY_OVERHEAD_ABSORBED, INVENTORY_TRANSFERRED, ITEM_CONSUME_RECORDED, ITEM_CONSUMED,
    ITEM_RECEIVED, ITEM_UPSERTED, LEDGER_INVENTORY_TRANSFERRED, PO_STATUS_CHANGED, PO_UPSERTED,
    VENDOR_CREATED, VENDOR_DELETED, VENDOR_INVOICE_APPROVED, VENDOR_INVOICE_PAID,
    VENDOR_INVOICE_UPSERTED, VENDOR_UPDATED,
};
use boss_inventory::port::{InventoryError, InventoryRepository};
use boss_inventory::types::{
    BillLine, InventoryItem, PoStatus, PurchaseOrder, PurchaseOrderLine, Vendor, VendorInvoice,
    VendorInvoiceStatus, bill_approved_payload, bill_paid_payload,
};
use boss_inventory::{InMemoryInventory, PgInventory};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: InventoryRepository;
    fn repo(&self) -> &Self::R;
    /// Every fact about a `suite-` key, as `(kind, payload)`, in the
    /// order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(payload: &Value) -> bool {
    ["part_sku", "id", "vendor_invoice_id", "source_id"]
        .iter()
        .any(|k| {
            payload[*k]
                .as_str()
                .is_some_and(|v| v.starts_with("suite-"))
        })
}

struct InMemory(InMemoryInventory);

impl World for InMemory {
    type R = InMemoryInventory;
    fn repo(&self) -> &InMemoryInventory {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| is_suite_fact(&e.payload))
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgInventory,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgInventory;
    fn repo(&self) -> &PgInventory {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> =
            sqlx::query_as("SELECT kind, payload FROM event_outbox ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .expect("read the outbox");
        rows.into_iter().filter(|(_, p)| is_suite_fact(p)).collect()
    }
}

/// TestDb turns the reference guards off database-wide; production runs
/// with them on, and the double always checks, so the suite turns them
/// back on and opens a FRESH pool (ALTER DATABASE reaches only new
/// sessions) — the shape of boss-commerce's `outbox_invoice_pg.rs`.
async fn guarded_pool(db: &boss_testing::TestDb) -> sqlx::PgPool {
    sqlx::query(&format!(
        r#"ALTER DATABASE "{}" SET audit_log.ref_check = 'on'"#,
        db.name()
    ))
    .execute(&db.pool)
    .await
    .expect("turn the reference guards on");
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .expect("a guarded pool")
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryInventory::new(vec![], vec![])), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let pool = guarded_pool(&db).await;
            let world = Postgres {
                repo: PgInventory::new(pool.clone()),
                pool,
            };
            (world, db)
        },
    }
    cases {
        items_list_in_byte_order_of_sku_and_read_back_with_a_derived_unit_cost,
        an_item_upsert_replaces_the_row_and_records_it_as_sent_each_time,
        a_costed_receive_adds_units_and_the_exact_line_total_and_records_its_receipt,
        an_uncosted_receive_moves_units_only,
        a_redelivered_receive_changes_nothing_and_records_nothing,
        a_consume_drains_value_proportionally_and_the_last_unit_takes_the_rest,
        a_consume_of_a_valueless_row_moves_units_and_records_no_transfer,
        a_consume_past_on_hand_is_refused_insufficient_stock,
        a_redelivered_consume_changes_nothing_and_records_nothing,
        a_redelivered_valueless_consume_changes_nothing_and_records_nothing,
        a_receive_past_the_column_s_range_is_refused_invalid_and_moves_nothing,
        a_count_past_its_column_s_range_is_refused_invalid_naming_it,
        a_stock_move_on_an_unknown_part_is_refused_not_found,
        a_purchase_order_reads_back_whole_with_lines_in_byte_order_of_sku,
        purchase_orders_list_latest_placed_first_unplaced_first_ties_by_id,
        a_re_posted_purchase_order_moves_its_status_and_keeps_its_placement_and_lines,
        a_purchase_order_naming_an_unregistered_vendor_is_refused_not_found,
        a_status_flip_records_the_post_flip_order_and_an_unknown_order_is_not_found,
        an_open_order_with_a_line_for_the_part_is_what_open_po_exists_answers,
        the_primary_vendor_is_the_latest_placed_order_s_then_the_category_s,
        inbound_reserved_for_a_part_nobody_is_restocking_is_zero,
        vendors_list_by_byte_order_of_name_nameless_last_and_ties_by_id,
        a_vendor_update_keeps_its_id_and_a_stranger_is_not_found,
        a_duplicate_vendor_is_a_conflict_and_records_nothing,
        an_invoice_reads_back_without_lines_and_lists_latest_first_ties_by_id,
        an_invoice_records_each_transition_once,
        an_invoice_naming_an_unplaced_order_is_refused_not_found,
        ap_aging_buckets_the_unpaid_by_days_since_received,
        an_inventory_je_answers_its_canonical_fact_once_and_records_it_once,
        an_overhead_absorption_answers_its_canonical_fact_once_and_records_it_once,
        a_non_positive_je_or_overhead_is_refused_invalid,
        a_currency_not_three_characters_long_is_refused_invalid,
        a_negative_limit_is_refused_invalid,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

/// An instant `secs` after a fixed origin, carrying nanoseconds no
/// column keeps.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_789)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new("inventory", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

fn day(secs: i64) -> NaiveDate {
    at(secs).date_naive()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("a date")
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

fn fact_of<T: serde::Serialize>(secs: i64, kind: &str, row: &T) -> (String, Value) {
    fact(
        secs,
        kind,
        serde_json::to_value(row).expect("a row serialises"),
    )
}

async fn since<W: World>(w: &W, before: usize) -> Vec<(String, Value)> {
    w.facts().await.into_iter().skip(before).collect()
}

/// An item as a caller sends it: the unit cost it carries is ignored.
fn item(sku: &str, on_hand: u32, value: i64) -> InventoryItem {
    InventoryItem {
        part_sku: sku.into(),
        bin: "A-01".into(),
        on_hand,
        allocated: 1,
        reorder_point: 2,
        reorder_qty: 3,
        trailing_90d_usage: 4,
        value_cents: value,
        avg_cost_cents: 999_999,
        vendor_price_cents: Some(55),
        vendor_category: None,
    }
}

/// The item as the store must answer it: unit cost DERIVED.
fn kept(mut i: InventoryItem) -> InventoryItem {
    i.avg_cost_cents = if i.on_hand > 0 {
        i.value_cents / i.on_hand as i64
    } else {
        0
    };
    i
}

async fn seed_item<W: World>(w: &W, sku: &str, on_hand: u32, value: i64) {
    w.repo()
        .upsert_item_at(&item(sku, on_hand, value), at(0), &stamp(0))
        .await
        .unwrap_or_else(|e| panic!("seed {sku}: {e:?}"));
}

async fn suite_items<W: World>(w: &W) -> Vec<InventoryItem> {
    w.repo()
        .all_items()
        .await
        .expect("all_items")
        .into_iter()
        .filter(|i| i.part_sku.starts_with("suite-"))
        .collect()
}

async fn receive<W: World>(
    w: &W,
    sku: &str,
    qty: u32,
    unit: Option<i64>,
    source: &str,
    secs: i64,
) -> Result<boss_inventory::ReceiveApplied, InventoryError> {
    w.repo()
        .receive_part_at(sku, qty, unit, at(secs), source, &stamp(secs))
        .await
}

async fn consume<W: World>(
    w: &W,
    sku: &str,
    qty: u32,
    source: &str,
    secs: i64,
) -> Result<boss_inventory::ConsumeApplied, InventoryError> {
    w.repo()
        .consume_part_at(sku, qty, at(secs), source, &stamp(secs))
        .await
}

fn receipt(sku: &str, qty: u32, unit: Option<i64>, source: &str, secs: i64) -> Value {
    json!({
        "part_sku": sku,
        "qty": qty,
        "unit_cost_cents": unit,
        "received_on": day(secs),
        "source_id": source,
    })
}

fn transfer(sku: &str, qty: u32, drained: i64, source: &str, secs: i64) -> Value {
    json!({
        "total_cost_cents": drained,
        "debit_account": "1310",
        "credit_account": "1300",
        "memo": format!("Production — consumed {qty} × {sku} (raw → WIP, value drain)"),
        "part_sku": sku,
        "qty": qty,
        "source_id": source,
        "consumed_on": day(secs),
    })
}

fn vendor(id: &str, name: Option<&str>, category: Option<&str>) -> Vendor {
    Vendor {
        id: id.into(),
        name: name.map(Into::into),
        contact_name: None,
        contact_email: None,
        city: None,
        state: None,
        lead_time_days: 7,
        payment_terms: None,
        category: category.map(Into::into),
        behavior: None,
    }
}

fn whole_vendor(id: &str) -> Vendor {
    Vendor {
        id: id.into(),
        name: Some("Suite Hops".into()),
        contact_name: Some("Ada".into()),
        contact_email: Some("ada@example.com".into()),
        city: Some("Yakima".into()),
        state: Some("WA".into()),
        lead_time_days: 12,
        payment_terms: Some("net-30".into()),
        category: Some("suite-hops".into()),
        behavior: Some(boss_inventory::types::VendorBehavior {
            lead_time_days: 3.5,
            lead_spread_days: 1.0,
            fulfilment_rate: 0.98,
            ap_payment_days: 30.0,
            ap_spread_days: 2.0,
            provenance: boss_inventory::types::BehaviorProvenance {
                source: boss_inventory::types::BehaviorSource::HandSet,
                template: Some("hops-supplier".into()),
            },
        }),
    }
}

async fn add_vendor<W: World>(w: &W, v: &Vendor) {
    w.repo()
        .create_vendor_at(v, at(0), &stamp(0))
        .await
        .unwrap_or_else(|e| panic!("vendor {}: {e:?}", v.id));
}

fn line(sku: &str, qty: u32, unit: i64) -> PurchaseOrderLine {
    PurchaseOrderLine {
        part_sku: sku.into(),
        qty,
        unit_cost_cents: unit,
        currency: "USD".into(),
    }
}

fn po(
    id: &str,
    vendor: Option<&str>,
    status: &str,
    placed_on: Option<NaiveDate>,
    lines: Vec<PurchaseOrderLine>,
) -> PurchaseOrder {
    PurchaseOrder {
        id: id.into(),
        vendor: vendor.map(Into::into),
        status: PoStatus::new(status),
        placed_on,
        expected_on: placed_on.map(|d| d + chrono::Days::new(7)),
        received_on: None,
        lines,
    }
}

async fn place<W: World>(w: &W, order: &PurchaseOrder, secs: i64) {
    w.repo()
        .create_purchase_order_at(order, at(secs), &stamp(secs))
        .await
        .unwrap_or_else(|e| panic!("place {}: {e:?}", order.id));
}

/// An order as the store must answer it: lines in byte order of SKU.
fn sorted(mut order: PurchaseOrder) -> PurchaseOrder {
    order.lines.sort_by(|a, b| a.part_sku.cmp(&b.part_sku));
    order
}

fn invoice(id: &str, po_id: &str, received_on: NaiveDate, amount: i64) -> VendorInvoice {
    VendorInvoice {
        id: id.into(),
        po_id: po_id.into(),
        vendor: "suite-v".into(),
        vendor_invoice_no: format!("INV-{id}"),
        amount_cents: amount,
        currency: "USD".into(),
        received_on,
        matched_on: None,
        approved_on: None,
        paid_on: None,
        status: VendorInvoiceStatus::new(VendorInvoiceStatus::RECEIVED),
        discrepancy_cents: None,
        discrepancy_kind: None,
        lines: vec![],
    }
}

/// A vendor and one placed order to bill against.
async fn billable<W: World>(w: &W) {
    add_vendor(w, &vendor("suite-v", Some("Suite Vendor"), None)).await;
    place(
        w,
        &po(
            "suite-po",
            Some("suite-v"),
            PoStatus::RECEIVED,
            Some(date(2026, 9, 1)),
            vec![line("suite-ab", 10, 250)],
        ),
        1,
    )
    .await;
}

async fn suite_invoices<W: World>(w: &W, status: Option<&str>, limit: i64) -> Vec<String> {
    w.repo()
        .all_vendor_invoices(status, limit)
        .await
        .expect("all_vendor_invoices")
        .into_iter()
        .map(|i| i.id)
        .filter(|id| id.starts_with("suite-"))
        .collect()
}

fn expect_invalid<T: std::fmt::Debug>(got: Result<T, InventoryError>, field: &str, adapter: &str) {
    match got {
        Err(InventoryError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
        other => panic!("{adapter}: {field}: want Invalid, got {other:?}"),
    }
}

fn expect_not_found<T: std::fmt::Debug>(got: Result<T, InventoryError>, key: &str, adapter: &str) {
    match got {
        Err(InventoryError::NotFound(m)) => assert!(m.contains(key), "{adapter}: {m}"),
        other => panic!("{adapter}: {key}: want NotFound, got {other:?}"),
    }
}

// ----- items ---------------------------------------------------------------

/// The item list is in BYTE order of SKU — `suite-B` < `suite-a-z` <
/// `suite-ab`, which a locale collation orders the other way round. An
/// item reads back whole, its unit cost DERIVED (`value / on_hand`,
/// zero when nothing is on hand) whatever was sent; a stranger is None.
async fn items_list_in_byte_order_of_sku_and_read_back_with_a_derived_unit_cost<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 3, 1_000).await;
    seed_item(w, "suite-B", 0, 0).await;
    seed_item(w, "suite-a-z", 7, 70).await;
    assert_eq!(
        suite_items(w).await,
        vec![
            kept(item("suite-B", 0, 0)),
            kept(item("suite-a-z", 7, 70)),
            kept(item("suite-ab", 3, 1_000)),
        ],
        "{adapter}"
    );
    assert_eq!(
        w.repo().item_by_sku("suite-ab").await.expect("by sku"),
        Some(kept(item("suite-ab", 3, 1_000))),
        "{adapter}: 1000 / 3 = 333"
    );
    assert_eq!(
        w.repo()
            .item_by_sku("suite-stranger")
            .await
            .expect("by sku"),
        None,
        "{adapter}"
    );
}

/// An upsert on a held SKU replaces every field; each upsert records the
/// item AS SENT (its unit cost included — the rebuild ignores it too).
async fn an_item_upsert_replaces_the_row_and_records_it_as_sent_each_time<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = item("suite-ab", 3, 1_000);
    let second = InventoryItem {
        bin: "Z-99".into(),
        allocated: 9,
        reorder_point: 8,
        reorder_qty: 7,
        trailing_90d_usage: 6,
        vendor_price_cents: None,
        vendor_category: Some("suite-hops".into()),
        ..item("suite-ab", 5, 51)
    };
    w.repo()
        .upsert_item_at(&first, at(1), &stamp(1))
        .await
        .expect("1");
    w.repo()
        .upsert_item_at(&second, at(2), &stamp(2))
        .await
        .expect("2");
    assert_eq!(
        w.repo().item_by_sku("suite-ab").await.expect("by sku"),
        Some(kept(second.clone())),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![
            fact_of(1, ITEM_UPSERTED, &first),
            fact_of(2, ITEM_UPSERTED, &second),
        ],
        "{adapter}"
    );
}

/// A costed receive adds the units and EXACTLY qty × unit cost to the
/// value, answers the post-receive row and the receipt, and records both.
async fn a_costed_receive_adds_units_and_the_exact_line_total_and_records_its_receipt<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 3, 1_000).await;
    let before = w.facts().await.len();
    let got = receive(w, "suite-ab", 4, Some(7), "suite-r1", 10)
        .await
        .expect("r1");
    let want = kept(item("suite-ab", 7, 1_028));
    assert_eq!(got.item, want, "{adapter}");
    let payload = receipt("suite-ab", 4, Some(7), "suite-r1", 10);
    assert_eq!(got.receipt_payload, Some(payload.clone()), "{adapter}");
    assert_eq!(
        w.repo().item_by_sku("suite-ab").await.expect("by sku"),
        Some(want.clone()),
        "{adapter}"
    );
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(10, ITEM_UPSERTED, &want),
            fact(10, ITEM_RECEIVED, payload),
        ],
        "{adapter}"
    );
}

/// With no cost, or a cost of zero or less, a receive moves units only:
/// the value is untouched and the receipt carries the cost as sent.
async fn an_uncosted_receive_moves_units_only<W: World>(w: &W, adapter: &str) {
    seed_item(w, "suite-ab", 2, 100).await;
    let a = receive(w, "suite-ab", 2, None, "suite-r1", 11)
        .await
        .expect("r1");
    assert_eq!(a.item, kept(item("suite-ab", 4, 100)), "{adapter}");
    let b = receive(w, "suite-ab", 1, Some(0), "suite-r2", 12)
        .await
        .expect("r2");
    assert_eq!(b.item, kept(item("suite-ab", 5, 100)), "{adapter}");
    assert_eq!(
        b.receipt_payload,
        Some(receipt("suite-ab", 1, Some(0), "suite-r2", 12)),
        "{adapter}"
    );
}

/// A receive replayed with the same `source_id` (a redelivered step
/// effect) answers the current row, no receipt, and records nothing.
async fn a_redelivered_receive_changes_nothing_and_records_nothing<W: World>(w: &W, adapter: &str) {
    seed_item(w, "suite-ab", 1, 10).await;
    receive(w, "suite-ab", 2, Some(5), "suite-r1", 13)
        .await
        .expect("r1");
    let before = w.facts().await;
    let replay = receive(w, "suite-ab", 2, Some(5), "suite-r1", 14)
        .await
        .expect("replay");
    assert_eq!(replay.item, kept(item("suite-ab", 3, 20)), "{adapter}");
    assert_eq!(replay.receipt_payload, None, "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A consume drains round-half-up(value × qty / on_hand); the last unit
/// takes whatever value remains, so zero on hand is zero value. Each
/// drain answers and records its raw → WIP transfer beside the row.
async fn a_consume_drains_value_proportionally_and_the_last_unit_takes_the_rest<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 3, 1_000).await;
    let before = w.facts().await.len();
    // round(1000 × 1 / 3) = 333.
    let one = consume(w, "suite-ab", 1, "suite-c1", 20).await.expect("c1");
    let after_one = kept(item("suite-ab", 2, 667));
    assert_eq!(one.item, after_one, "{adapter}");
    assert_eq!(
        one.fact_payload,
        Some(transfer("suite-ab", 1, 333, "suite-c1", 20)),
        "{adapter}"
    );
    let rest = consume(w, "suite-ab", 2, "suite-c2", 21).await.expect("c2");
    let empty = kept(item("suite-ab", 0, 0));
    assert_eq!(rest.item, empty, "{adapter}");
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(20, ITEM_CONSUMED, &after_one),
            fact(
                20,
                INVENTORY_TRANSFERRED,
                transfer("suite-ab", 1, 333, "suite-c1", 20)
            ),
            fact_of(21, ITEM_CONSUMED, &empty),
            fact(
                21,
                INVENTORY_TRANSFERRED,
                transfer("suite-ab", 2, 667, "suite-c2", 21)
            ),
        ],
        "{adapter}"
    );
}

/// A consume of a row holding no value moves units and records the row,
/// but no transfer: nothing moved in the GL. It records the GL-inert
/// proof of its delivery instead (backlog 55f69172), which is what a
/// redelivery is guarded by.
async fn a_consume_of_a_valueless_row_moves_units_and_records_no_transfer<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 3, 0).await;
    let before = w.facts().await.len();
    let got = consume(w, "suite-ab", 1, "suite-c1", 22).await.expect("c1");
    let want = kept(item("suite-ab", 2, 0));
    assert_eq!(got.item, want, "{adapter}");
    assert_eq!(got.fact_payload, None, "{adapter}");
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(22, ITEM_CONSUMED, &want),
            fact(
                22,
                ITEM_CONSUME_RECORDED,
                json!({
                    "part_sku": "suite-ab",
                    "qty": 1,
                    "source_id": "suite-c1",
                    "consumed_on": day(22),
                })
            ),
        ],
        "{adapter}"
    );
}

/// Raw stock never goes negative: a consume past what is on hand is
/// refused `InsufficientStock` with both counts, and changes nothing.
async fn a_consume_past_on_hand_is_refused_insufficient_stock<W: World>(w: &W, adapter: &str) {
    seed_item(w, "suite-ab", 2, 20).await;
    let before = w.facts().await;
    match consume(w, "suite-ab", 3, "suite-c1", 23).await {
        Err(InventoryError::InsufficientStock(sku, have, need)) => {
            assert_eq!((sku.as_str(), have, need), ("suite-ab", 2, 3), "{adapter}")
        }
        other => panic!("{adapter}: {other:?}"),
    }
    assert_eq!(
        suite_items(w).await,
        vec![kept(item("suite-ab", 2, 20))],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A valued consume replayed with the same `source_id` answers the
/// current row, no transfer, and records nothing — even once stock has
/// fallen below the replayed quantity.
async fn a_redelivered_consume_changes_nothing_and_records_nothing<W: World>(w: &W, adapter: &str) {
    seed_item(w, "suite-ab", 4, 400).await;
    consume(w, "suite-ab", 3, "suite-c1", 24).await.expect("c1");
    let before = w.facts().await;
    let replay = consume(w, "suite-ab", 3, "suite-c1", 25)
        .await
        .expect("replay");
    assert_eq!(replay.item, kept(item("suite-ab", 1, 100)), "{adapter}");
    assert_eq!(replay.fact_payload, None, "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// The same for a consume that drained NO value (backlog 55f69172): it
/// writes no transfer for a guard to find, so until then a redelivery
/// took the units a second time on both adapters. The guard is the
/// delivery's own `source_id`, whatever the consume drained.
async fn a_redelivered_valueless_consume_changes_nothing_and_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 4, 0).await;
    consume(w, "suite-ab", 3, "suite-c1", 24).await.expect("c1");
    let before = w.facts().await;
    let replay = consume(w, "suite-ab", 3, "suite-c1", 25)
        .await
        .expect("replay");
    assert_eq!(replay.item, kept(item("suite-ab", 1, 0)), "{adapter}");
    assert_eq!(replay.fact_payload, None, "{adapter}");
    assert_eq!(
        suite_items(w).await,
        vec![kept(item("suite-ab", 1, 0))],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// The stock columns are 32-bit signed integers. A receive of more than
/// `i32::MAX` units was bound as a NEGATIVE i32 on Postgres and so
/// LOWERED stock (backlog 55f69172); one that would carry `on_hand` past
/// the column was Postgres's overflow error as `Storage` (a 500) while
/// the double, counting in u32, accepted it. Each is now refused
/// `Invalid` naming the field, before any write: the row, its value and
/// the facts are unchanged.
async fn a_receive_past_the_column_s_range_is_refused_invalid_and_moves_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_item(w, "suite-ab", 1, 10).await;
    let before = w.facts().await;
    let past = i32::MAX as u32 + 1;
    expect_invalid(
        receive(w, "suite-ab", past, Some(1), "suite-r1", 15).await,
        "qty",
        adapter,
    );
    expect_invalid(
        receive(w, "suite-ab", u32::MAX, None, "suite-r2", 15).await,
        "qty",
        adapter,
    );
    // In range alone, past the column once added to what is on hand.
    expect_invalid(
        receive(w, "suite-ab", i32::MAX as u32, None, "suite-r3", 15).await,
        "on_hand",
        adapter,
    );
    assert_eq!(
        suite_items(w).await,
        vec![kept(item("suite-ab", 1, 10))],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
    // The largest count the column holds is still received whole.
    let top = receive(w, "suite-ab", i32::MAX as u32 - 1, None, "suite-r4", 16)
        .await
        .expect("the column's top");
    assert_eq!(top.item.on_hand, i32::MAX as u32, "{adapter}");
}

/// Every other count bound into a 32-bit column — an item's five counts,
/// a PO line's quantity — and a vendor's 16-bit lead time is refused
/// `Invalid` naming the field past its range, and nothing is recorded
/// (backlog 55f69172: Postgres stored each wrapped negative).
async fn a_count_past_its_column_s_range_is_refused_invalid_naming_it<W: World>(
    w: &W,
    adapter: &str,
) {
    let past = i32::MAX as u32 + 1;
    for (field, bad) in [
        (
            "on_hand",
            InventoryItem {
                on_hand: past,
                ..item("suite-ab", 0, 0)
            },
        ),
        (
            "allocated",
            InventoryItem {
                allocated: past,
                ..item("suite-ab", 0, 0)
            },
        ),
        (
            "reorder_point",
            InventoryItem {
                reorder_point: past,
                ..item("suite-ab", 0, 0)
            },
        ),
        (
            "reorder_qty",
            InventoryItem {
                reorder_qty: past,
                ..item("suite-ab", 0, 0)
            },
        ),
        (
            "trailing_90d_usage",
            InventoryItem {
                trailing_90d_usage: past,
                ..item("suite-ab", 0, 0)
            },
        ),
    ] {
        expect_invalid(
            w.repo().upsert_item_at(&bad, at(17), &stamp(17)).await,
            field,
            adapter,
        );
    }
    assert_eq!(suite_items(w).await, vec![], "{adapter}");
    add_vendor(w, &vendor("suite-v", Some("V"), None)).await;
    let before = w.facts().await;
    expect_invalid(
        w.repo()
            .create_purchase_order_at(
                &po(
                    "suite-po",
                    Some("suite-v"),
                    PoStatus::SUBMITTED,
                    Some(date(2026, 9, 1)),
                    vec![line("suite-ab", past, 1)],
                ),
                at(18),
                &stamp(18),
            )
            .await,
        "qty",
        adapter,
    );
    let slow = Vendor {
        lead_time_days: i16::MAX as u16 + 1,
        ..vendor("suite-w", Some("W"), None)
    };
    expect_invalid(
        w.repo().create_vendor_at(&slow, at(18), &stamp(18)).await,
        "lead_time_days",
        adapter,
    );
    expect_invalid(
        w.repo().update_vendor("suite-v", &slow, &stamp(18)).await,
        "lead_time_days",
        adapter,
    );
    assert_eq!(w.facts().await, before, "{adapter}");
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po")
            .await
            .expect("by id"),
        None,
        "{adapter}"
    );
}

/// A receive or consume naming a part nobody stocked is refused
/// `NotFound` naming it and records nothing.
async fn a_stock_move_on_an_unknown_part_is_refused_not_found<W: World>(w: &W, adapter: &str) {
    expect_not_found(
        receive(w, "suite-stranger", 1, Some(1), "suite-r1", 26).await,
        "suite-stranger",
        adapter,
    );
    expect_not_found(
        consume(w, "suite-stranger", 1, "suite-c1", 26).await,
        "suite-stranger",
        adapter,
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

// ----- purchase orders -----------------------------------------------------

/// A placed order reads back whole, its lines in BYTE order of SKU
/// whatever order they were sent in; the fact is the order as sent. A
/// stranger is None.
async fn a_purchase_order_reads_back_whole_with_lines_in_byte_order_of_sku<W: World>(
    w: &W,
    adapter: &str,
) {
    add_vendor(w, &vendor("suite-v", Some("V"), None)).await;
    let sent = po(
        "suite-po",
        Some("suite-v"),
        PoStatus::SUBMITTED,
        Some(date(2026, 9, 1)),
        vec![
            line("suite-ab", 1, 100),
            line("suite-B", 2, 200),
            line("suite-a-z", 3, 300),
        ],
    );
    let before = w.facts().await.len();
    place(w, &sent, 30).await;
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po")
            .await
            .expect("by id"),
        Some(sorted(sent.clone())),
        "{adapter}"
    );
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-stranger")
            .await
            .expect("by id"),
        None,
        "{adapter}"
    );
    assert_eq!(
        since(w, before).await,
        vec![fact_of(30, PO_UPSERTED, &sent)],
        "{adapter}"
    );
}

/// The order list is latest placement first; an order not yet placed
/// (no date) leads, and orders placed the same day are in BYTE order of
/// id.
async fn purchase_orders_list_latest_placed_first_unplaced_first_ties_by_id<W: World>(
    w: &W,
    adapter: &str,
) {
    let d = |n| Some(date(2026, 9, n));
    for (id, placed) in [
        ("suite-ab", d(2)),
        ("suite-old", d(1)),
        ("suite-draft", None),
        ("suite-B", d(2)),
        ("suite-a-z", d(2)),
    ] {
        place(w, &po(id, None, PoStatus::DRAFT, placed, vec![]), 31).await;
    }
    let ids: Vec<String> = w
        .repo()
        .all_purchase_orders()
        .await
        .expect("all")
        .into_iter()
        .map(|p| p.id)
        .filter(|id| id.starts_with("suite-"))
        .collect();
    assert_eq!(
        ids,
        [
            "suite-draft",
            "suite-B",
            "suite-a-z",
            "suite-ab",
            "suite-old"
        ],
        "{adapter}"
    );
}

/// The sim re-posts an order on every transition. A re-post moves its
/// vendor, status, expected and received dates; it keeps the FIRST
/// placement date and every line it already held (a line keyed by a
/// held SKU keeps its first quantity, a new SKU is added). Each post
/// records the order as sent.
async fn a_re_posted_purchase_order_moves_its_status_and_keeps_its_placement_and_lines<W: World>(
    w: &W,
    adapter: &str,
) {
    add_vendor(w, &vendor("suite-v", Some("V"), None)).await;
    add_vendor(w, &vendor("suite-w", Some("W"), None)).await;
    let first = po(
        "suite-po",
        Some("suite-v"),
        PoStatus::SUBMITTED,
        Some(date(2026, 9, 1)),
        vec![line("suite-ab", 1, 100)],
    );
    let mut again = po(
        "suite-po",
        Some("suite-w"),
        PoStatus::RECEIVED,
        Some(date(2026, 9, 5)),
        vec![line("suite-ab", 9, 999), line("suite-B", 2, 200)],
    );
    again.received_on = Some(date(2026, 9, 9));
    let before = w.facts().await.len();
    place(w, &first, 32).await;
    place(w, &again, 33).await;
    let want = PurchaseOrder {
        placed_on: first.placed_on,
        lines: vec![line("suite-B", 2, 200), line("suite-ab", 1, 100)],
        ..again.clone()
    };
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po")
            .await
            .expect("by id"),
        Some(want),
        "{adapter}"
    );
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(32, PO_UPSERTED, &first),
            fact_of(33, PO_UPSERTED, &again),
        ],
        "{adapter}"
    );
}

/// An order naming a vendor no Subject holds is refused `NotFound`
/// naming it, writes no order and records nothing. An order naming no
/// vendor (a draft) needs none.
async fn a_purchase_order_naming_an_unregistered_vendor_is_refused_not_found<W: World>(
    w: &W,
    adapter: &str,
) {
    let order = po(
        "suite-po",
        Some("suite-nobody"),
        PoStatus::SUBMITTED,
        Some(date(2026, 9, 1)),
        vec![line("suite-ab", 1, 100)],
    );
    expect_not_found(
        w.repo()
            .create_purchase_order_at(&order, at(34), &stamp(34))
            .await,
        "suite-nobody",
        adapter,
    );
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po")
            .await
            .expect("by id"),
        None,
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
    place(w, &po("suite-po", None, PoStatus::DRAFT, None, vec![]), 34).await;
}

/// A status flip records the order as it stands AFTER the flip, then the
/// status-changed marker. An unknown order is `NotFound` and records
/// nothing.
async fn a_status_flip_records_the_post_flip_order_and_an_unknown_order_is_not_found<W: World>(
    w: &W,
    adapter: &str,
) {
    let order = po(
        "suite-po",
        None,
        PoStatus::DRAFT,
        None,
        vec![line("suite-ab", 1, 100), line("suite-B", 2, 200)],
    );
    place(w, &order, 35).await;
    let before = w.facts().await.len();
    w.repo()
        .update_po_status("suite-po", PoStatus::CLOSED, &stamp(36))
        .await
        .expect("flip");
    let flipped = PurchaseOrder {
        status: PoStatus::new(PoStatus::CLOSED),
        ..sorted(order)
    };
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po")
            .await
            .expect("by id"),
        Some(flipped.clone()),
        "{adapter}"
    );
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(36, PO_UPSERTED, &flipped),
            fact(
                36,
                PO_STATUS_CHANGED,
                json!({"id": "suite-po", "new_status": PoStatus::CLOSED})
            ),
        ],
        "{adapter}"
    );
    let before = w.facts().await;
    expect_not_found(
        w.repo()
            .update_po_status("suite-stranger", PoStatus::CLOSED, &stamp(37))
            .await,
        "suite-stranger",
        adapter,
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A part has an open order when any order not received, closed or
/// cancelled holds a line for it.
async fn an_open_order_with_a_line_for_the_part_is_what_open_po_exists_answers<W: World>(
    w: &W,
    adapter: &str,
) {
    for (id, status, sku) in [
        ("suite-1", PoStatus::RECEIVED, "suite-done"),
        ("suite-2", PoStatus::CLOSED, "suite-done"),
        ("suite-3", "cancelled", "suite-done"),
        ("suite-4", PoStatus::DRAFT, "suite-open"),
    ] {
        place(w, &po(id, None, status, None, vec![line(sku, 1, 1)]), 38).await;
    }
    let open = |sku: &'static str| async move {
        w.repo()
            .open_po_exists_for_part(sku)
            .await
            .expect("open_po_exists")
    };
    assert!(open("suite-open").await, "{adapter}");
    assert!(!open("suite-done").await, "{adapter}");
    assert!(!open("suite-never").await, "{adapter}");
}

/// The primary vendor is the vendor of the latest-placed order holding a
/// line for the part (an order naming no vendor associates none; a tie
/// on the day goes to the lowest order id in byte order). With no such
/// order it is the vendor serving the part's category — the lowest id in
/// byte order — and otherwise None.
async fn the_primary_vendor_is_the_latest_placed_order_s_then_the_category_s<W: World>(
    w: &W,
    adapter: &str,
) {
    for (id, cat) in [
        ("suite-v-old", None),
        ("suite-v-B", None),
        ("suite-v-a", None),
        ("suite-v-ab", Some("suite-cat")),
        ("suite-v-B2", Some("suite-cat")),
    ] {
        add_vendor(w, &vendor(id, Some(id), cat)).await;
    }
    let d = |n| Some(date(2026, 9, n));
    for (id, v, placed) in [
        ("suite-po-old", Some("suite-v-old"), d(1)),
        ("suite-po-ab", Some("suite-v-a"), d(5)),
        ("suite-po-B", Some("suite-v-B"), d(5)),
        ("suite-po-none", None, d(9)),
    ] {
        place(
            w,
            &po(
                id,
                v,
                PoStatus::SUBMITTED,
                placed,
                vec![line("suite-x", 1, 1)],
            ),
            39,
        )
        .await;
    }
    let primary = |sku: &'static str| async move {
        w.repo()
            .primary_vendor_for_part(sku)
            .await
            .unwrap_or_else(|e| panic!("{adapter}: primary {sku}: {e:?}"))
    };
    assert_eq!(
        primary("suite-x").await.as_deref(),
        Some("suite-v-B"),
        "{adapter}"
    );
    let mut categorised = item("suite-y", 1, 1);
    categorised.vendor_category = Some("suite-cat".into());
    w.repo()
        .upsert_item_at(&categorised, at(39), &stamp(39))
        .await
        .expect("item");
    assert_eq!(
        primary("suite-y").await.as_deref(),
        Some("suite-v-B2"),
        "{adapter}"
    );
    seed_item(w, "suite-z", 1, 1).await;
    assert_eq!(primary("suite-z").await, None, "{adapter}");
    assert_eq!(primary("suite-never").await, None, "{adapter}");
}

/// No open restock names the part, so nothing is inbound.
async fn inbound_reserved_for_a_part_nobody_is_restocking_is_zero<W: World>(w: &W, adapter: &str) {
    seed_item(w, "suite-ab", 1, 1).await;
    assert_eq!(
        w.repo()
            .inbound_reserved_for_part("suite-ab")
            .await
            .expect("inbound"),
        0,
        "{adapter}"
    );
}

// ----- vendors -------------------------------------------------------------

/// Vendors list in BYTE order of name, nameless vendors last, a tie on
/// name in byte order of id. A vendor reads back whole, its behaviour
/// profile included, and its creation records it as sent.
async fn vendors_list_by_byte_order_of_name_nameless_last_and_ties_by_id<W: World>(
    w: &W,
    adapter: &str,
) {
    let vs = [
        whole_vendor("suite-1"),
        vendor("suite-2", Some("suite-B"), None),
        vendor("suite-3", None, None),
        vendor("suite-4", Some("suite-ab"), None),
        vendor("suite-5", Some("suite-a-z"), None),
        vendor("suite-6", Some("suite-B"), None),
    ];
    for v in &vs {
        add_vendor(w, v).await;
    }
    let listed: Vec<Vendor> = w
        .repo()
        .all_vendors()
        .await
        .expect("all")
        .into_iter()
        .filter(|v| v.id.starts_with("suite-"))
        .collect();
    let ids: Vec<&str> = listed.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "suite-1", "suite-2", "suite-6", "suite-5", "suite-4", "suite-3"
        ],
        "{adapter}: Suite Hops, suite-B ×2, suite-a-z, suite-ab, nameless"
    );
    assert_eq!(listed[0], whole_vendor("suite-1"), "{adapter}");
    let want: Vec<_> = vs.iter().map(|v| fact_of(0, VENDOR_CREATED, v)).collect();
    assert_eq!(w.facts().await, want, "{adapter}");
}

/// An update replaces every field but the id — the row is the one the
/// path names, whatever id the body carries — and records the body as
/// sent. Updating or deleting a stranger is `NotFound` and records
/// nothing; a delete records the deletion.
async fn a_vendor_update_keeps_its_id_and_a_stranger_is_not_found<W: World>(w: &W, adapter: &str) {
    add_vendor(w, &vendor("suite-v", Some("Old"), None)).await;
    let body = whole_vendor("suite-other");
    let before = w.facts().await.len();
    w.repo()
        .update_vendor("suite-v", &body, &stamp(40))
        .await
        .expect("update");
    let listed: Vec<Vendor> = w
        .repo()
        .all_vendors()
        .await
        .expect("all")
        .into_iter()
        .filter(|v| v.id.starts_with("suite-"))
        .collect();
    assert_eq!(
        listed,
        vec![Vendor {
            id: "suite-v".into(),
            ..body.clone()
        }],
        "{adapter}"
    );
    expect_not_found(
        w.repo()
            .update_vendor("suite-stranger", &body, &stamp(41))
            .await,
        "suite-stranger",
        adapter,
    );
    expect_not_found(
        w.repo().delete_vendor("suite-stranger", &stamp(41)).await,
        "suite-stranger",
        adapter,
    );
    w.repo()
        .delete_vendor("suite-v", &stamp(42))
        .await
        .expect("delete");
    assert!(
        !w.repo()
            .all_vendors()
            .await
            .expect("all")
            .iter()
            .any(|v| v.id == "suite-v"),
        "{adapter}"
    );
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(40, VENDOR_UPDATED, &body),
            fact(
                42,
                VENDOR_DELETED,
                json!({"id": "suite-v", "deleted_at": at(42)})
            ),
        ],
        "{adapter}"
    );
}

/// A second vendor with a held id is a `Conflict` naming it; the first
/// stands and only its creation is recorded.
async fn a_duplicate_vendor_is_a_conflict_and_records_nothing<W: World>(w: &W, adapter: &str) {
    let first = vendor("suite-v", Some("First"), None);
    add_vendor(w, &first).await;
    match w
        .repo()
        .create_vendor_at(&vendor("suite-v", Some("Second"), None), at(43), &stamp(43))
        .await
    {
        Err(InventoryError::Conflict(m)) => assert!(m.contains("suite-v"), "{adapter}: {m}"),
        other => panic!("{adapter}: {other:?}"),
    }
    let names: Vec<Option<String>> = w
        .repo()
        .all_vendors()
        .await
        .expect("all")
        .into_iter()
        .filter(|v| v.id == "suite-v")
        .map(|v| v.name)
        .collect();
    assert_eq!(names, vec![Some("First".to_string())], "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![fact_of(0, VENDOR_CREATED, &first)],
        "{adapter}"
    );
}

// ----- vendor invoices -----------------------------------------------------

/// An invoice reads back as sent but for its lines, which the table does
/// not keep (the bill's breakdown lives on its approval fact). The list
/// is latest received first, a tie on the day in BYTE order of id;
/// `status` narrows it and `limit` caps it.
async fn an_invoice_reads_back_without_lines_and_lists_latest_first_ties_by_id<W: World>(
    w: &W,
    adapter: &str,
) {
    billable(w).await;
    let mut lined = invoice("suite-ab", "suite-po", date(2026, 9, 2), 2_500);
    lined.lines = vec![BillLine {
        part_sku: "suite-ab".into(),
        qty: 10,
        unit_cost_cents: 250,
    }];
    lined.matched_on = Some(date(2026, 9, 3));
    lined.status = VendorInvoiceStatus::new(VendorInvoiceStatus::MATCHED);
    lined.discrepancy_cents = Some(-5);
    lined.discrepancy_kind = Some(boss_inventory::types::DiscrepancyKind::new("shorted"));
    let rows = [
        lined.clone(),
        invoice("suite-B", "suite-po", date(2026, 9, 2), 1),
        invoice("suite-a-z", "suite-po", date(2026, 9, 2), 1),
        invoice("suite-old", "suite-po", date(2026, 8, 1), 1),
    ];
    for r in &rows {
        w.repo()
            .upsert_vendor_invoice_at(r, at(50), &stamp(50))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: {}: {e:?}", r.id));
    }
    assert_eq!(
        w.repo()
            .vendor_invoice_by_id("suite-ab")
            .await
            .expect("by id"),
        Some(VendorInvoice {
            lines: vec![],
            ..lined
        }),
        "{adapter}"
    );
    assert_eq!(
        w.repo()
            .vendor_invoice_by_id("suite-stranger")
            .await
            .expect("by id"),
        None,
        "{adapter}"
    );
    assert_eq!(
        suite_invoices(w, None, 100).await,
        ["suite-B", "suite-a-z", "suite-ab", "suite-old"],
        "{adapter}"
    );
    assert_eq!(
        suite_invoices(w, Some(VendorInvoiceStatus::RECEIVED), 100).await,
        ["suite-B", "suite-a-z", "suite-old"],
        "{adapter}"
    );
    assert_eq!(
        suite_invoices(w, Some(VendorInvoiceStatus::RECEIVED), 2).await,
        ["suite-B", "suite-a-z"],
        "{adapter}"
    );
}

/// Every upsert records the invoice as sent. Approval and payment each
/// record their transition the FIRST time the invoice carries the date —
/// a re-upsert of an approved or paid invoice records only its state.
async fn an_invoice_records_each_transition_once<W: World>(w: &W, adapter: &str) {
    billable(w).await;
    // Paying a bill draws on 1000 Cash, which the ledger will not drive
    // negative: fund it first, through the port, under a key the suite's
    // facts do not read.
    w.repo()
        .record_inventory_je(
            1_000_000,
            "1000",
            "3000",
            "Opening cash",
            "suite_opening",
            "opening-cash",
            date(2026, 9, 1),
            &stamp(51),
        )
        .await
        .expect("fund cash");
    let received = invoice("suite-i", "suite-po", date(2026, 9, 2), 2_500);
    let approved = VendorInvoice {
        approved_on: Some(date(2026, 9, 4)),
        status: VendorInvoiceStatus::new(VendorInvoiceStatus::APPROVED),
        lines: vec![BillLine {
            part_sku: "suite-ab".into(),
            qty: 10,
            unit_cost_cents: 250,
        }],
        ..received.clone()
    };
    let paid = VendorInvoice {
        paid_on: Some(date(2026, 9, 20)),
        status: VendorInvoiceStatus::new(VendorInvoiceStatus::PAID),
        ..approved.clone()
    };
    let before = w.facts().await.len();
    for (secs, r) in [
        (51, &received),
        (52, &approved),
        (53, &approved),
        (54, &paid),
        (55, &paid),
    ] {
        w.repo()
            .upsert_vendor_invoice_at(r, at(secs), &stamp(secs))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: {secs}: {e:?}"));
    }
    assert_eq!(
        since(w, before).await,
        vec![
            fact_of(51, VENDOR_INVOICE_UPSERTED, &received),
            fact_of(52, VENDOR_INVOICE_UPSERTED, &approved),
            fact(
                52,
                VENDOR_INVOICE_APPROVED,
                bill_approved_payload(&approved, date(2026, 9, 4))
            ),
            fact_of(53, VENDOR_INVOICE_UPSERTED, &approved),
            fact_of(54, VENDOR_INVOICE_UPSERTED, &paid),
            fact(
                54,
                VENDOR_INVOICE_PAID,
                bill_paid_payload(&paid, date(2026, 9, 20))
            ),
            fact_of(55, VENDOR_INVOICE_UPSERTED, &paid),
        ],
        "{adapter}"
    );
}

/// An invoice billing an order nobody placed is refused `NotFound`
/// naming the order, writes nothing and records nothing.
async fn an_invoice_naming_an_unplaced_order_is_refused_not_found<W: World>(w: &W, adapter: &str) {
    let orphan = invoice("suite-i", "suite-nopo", date(2026, 9, 2), 1);
    expect_not_found(
        w.repo()
            .upsert_vendor_invoice_at(&orphan, at(56), &stamp(56))
            .await,
        "suite-nopo",
        adapter,
    );
    assert_eq!(
        suite_invoices(w, None, 100).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// AP aging counts and sums every UNPAID invoice by days since it was
/// received: on or before today is current, then 1-30, 31-60, 61-90 and
/// 90+; all five buckets always present, in that order.
async fn ap_aging_buckets_the_unpaid_by_days_since_received<W: World>(w: &W, adapter: &str) {
    billable(w).await;
    let today = date(2026, 9, 30);
    let ago = |n: u64| today - chrono::Days::new(n);
    let mut paid = invoice("suite-paid", "suite-po", ago(200), 999_999);
    paid.status = VendorInvoiceStatus::new(VendorInvoiceStatus::PAID);
    for r in [
        invoice("suite-0", "suite-po", today, 1),
        invoice("suite-1", "suite-po", ago(1), 10),
        invoice("suite-30", "suite-po", ago(30), 100),
        invoice("suite-31", "suite-po", ago(31), 1_000),
        invoice("suite-90", "suite-po", ago(90), 10_000),
        invoice("suite-91", "suite-po", ago(91), 100_000),
        invoice("suite-200", "suite-po", ago(200), 1_000_000),
        paid,
    ] {
        w.repo()
            .upsert_vendor_invoice_at(&r, at(57), &stamp(57))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: {}: {e:?}", r.id));
    }
    let aging = w.repo().ap_aging(today).await.expect("aging");
    let buckets: Vec<(&str, i64, i64)> = aging
        .buckets
        .iter()
        .map(|b| (b.label.as_str(), b.count, b.total_cents))
        .collect();
    assert_eq!(
        buckets,
        [
            ("current", 1, 1),
            ("1-30", 2, 110),
            ("31-60", 1, 1_000),
            ("61-90", 1, 10_000),
            ("90+", 2, 1_100_000),
        ],
        "{adapter}"
    );
    assert_eq!(aging.total_outstanding_cents, 1_111_111, "{adapter}");
    assert_eq!(aging.total_invoice_count, 7, "{adapter}");
    assert_eq!(aging.currency, "USD", "{adapter}");
}

// ----- GL facts ------------------------------------------------------------

/// An inventory JE answers the canonical fact — its id derived from
/// `(kind, source_table, source_id)` — and `inserted` on the call that
/// wrote it, recording `ledger.inventory.transferred` once. The same key
/// again answers the same fact, `inserted: false`, and records nothing.
async fn an_inventory_je_answers_its_canonical_fact_once_and_records_it_once<W: World>(
    w: &W,
    adapter: &str,
) {
    let je = |secs: i64| async move {
        w.repo()
            .record_inventory_je(
                12_345,
                "1300",
                "3000",
                "Opening raw — suite",
                "brewery_seed_opening_balance",
                "suite-open-1",
                day(60),
                &stamp(secs),
            )
            .await
    };
    let payload = json!({
        "total_cost_cents": 12_345,
        "debit_account": "1300",
        "credit_account": "3000",
        "memo": "Opening raw — suite",
        "happened_on": day(60).to_string(),
        "source_table": "brewery_seed_opening_balance",
        "source_id": "suite-open-1",
    });
    let id = boss_ledger::deterministic_fact_id(
        "finance.inventory.transferred",
        "brewery_seed_opening_balance",
        "suite-open-1",
    );
    let first = je(60).await.expect("first");
    assert_eq!(first.fact_id, id, "{adapter}");
    assert!(first.inserted, "{adapter}");
    assert_eq!(first.payload, payload, "{adapter}");
    let again = je(61).await.expect("again");
    assert_eq!(again.fact_id, id, "{adapter}");
    assert!(!again.inserted, "{adapter}: the same key is not a new fact");
    assert_eq!(
        w.facts().await,
        vec![fact(60, LEDGER_INVENTORY_TRANSFERRED, payload)],
        "{adapter}"
    );
}

/// An overhead absorption answers the canonical fact id and `inserted`
/// once, recording `inventory.overhead.absorbed` once; the same source
/// again answers the same id, `false`, and records nothing.
async fn an_overhead_absorption_answers_its_canonical_fact_once_and_records_it_once<W: World>(
    w: &W,
    adapter: &str,
) {
    let absorb = |secs: i64| async move {
        w.repo()
            .record_overhead_absorbed(
                777,
                "1310",
                "5100",
                "Overhead — suite",
                "suite-step-1",
                day(62),
                &stamp(secs),
            )
            .await
    };
    let id = boss_ledger::deterministic_fact_id(
        "finance.inventory.transferred",
        "ledger_overhead_absorbed",
        "suite-step-1",
    );
    assert_eq!(absorb(62).await.expect("first"), (id, true), "{adapter}");
    assert_eq!(absorb(63).await.expect("again"), (id, false), "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![fact(
            62,
            INVENTORY_OVERHEAD_ABSORBED,
            json!({
                "total_cost_cents": 777,
                "debit_account": "1310",
                "credit_account": "5100",
                "memo": "Overhead — suite",
                "happened_on": day(62),
                "source_id": "suite-step-1",
            })
        )],
        "{adapter}"
    );
}

/// A JE or overhead of zero or fewer cents is refused `Invalid` naming
/// the amount and records nothing.
async fn a_non_positive_je_or_overhead_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    for cents in [0, -5] {
        expect_invalid(
            w.repo()
                .record_inventory_je(
                    cents,
                    "1300",
                    "3000",
                    "m",
                    "brewery_seed_opening_balance",
                    "suite-o",
                    day(64),
                    &stamp(64),
                )
                .await,
            "total_cost_cents",
            adapter,
        );
        expect_invalid(
            w.repo()
                .record_overhead_absorbed(
                    cents,
                    "1310",
                    "5100",
                    "m",
                    "suite-o",
                    day(64),
                    &stamp(64),
                )
                .await,
            "total_cost_cents",
            adapter,
        );
    }
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

// ----- refusals ------------------------------------------------------------

/// A currency is three characters (the table's CHECK). An order line or
/// an invoice carrying any other length is refused `Invalid` naming the
/// field and writes nothing.
async fn a_currency_not_three_characters_long_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    billable(w).await;
    let before = w.facts().await;
    let mut bad_line = line("suite-ab", 1, 1);
    bad_line.currency = "US".into();
    expect_invalid(
        w.repo()
            .create_purchase_order_at(
                &po("suite-po2", None, PoStatus::DRAFT, None, vec![bad_line]),
                at(65),
                &stamp(65),
            )
            .await,
        "currency",
        adapter,
    );
    let mut bad_bill = invoice("suite-i", "suite-po", date(2026, 9, 2), 1);
    bad_bill.currency = "USDX".into();
    expect_invalid(
        w.repo()
            .upsert_vendor_invoice_at(&bad_bill, at(65), &stamp(65))
            .await,
        "currency",
        adapter,
    );
    assert_eq!(
        w.repo()
            .purchase_order_by_id("suite-po2")
            .await
            .expect("by id"),
        None,
        "{adapter}"
    );
    assert_eq!(
        suite_invoices(w, None, 100).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A negative limit asks for nothing a list can answer: `Invalid`
/// naming it. Zero is an empty page.
async fn a_negative_limit_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    expect_invalid(
        w.repo().all_vendor_invoices(None, -1).await,
        "limit",
        adapter,
    );
    assert_eq!(
        suite_invoices(w, None, 0).await,
        Vec::<String>::new(),
        "{adapter}"
    );
}

/// A NUL byte cannot be stored in TEXT or JSONB. A write carrying one in
/// any text argument or field is refused `Invalid` naming the field and
/// writes nothing. A read keyed by one is the miss it is.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    billable(w).await;
    seed_item(w, "suite-ab", 5, 50).await;
    let before = w.facts().await;
    let r = w.repo();

    // Items.
    for (field, bad) in [
        ("part_sku", item("suite-a\0b", 1, 1)),
        (
            "bin",
            InventoryItem {
                bin: "A\0".into(),
                ..item("suite-ab", 1, 1)
            },
        ),
        (
            "vendor_category",
            InventoryItem {
                vendor_category: Some("c\0".into()),
                ..item("suite-ab", 1, 1)
            },
        ),
    ] {
        expect_invalid(
            r.upsert_item_at(&bad, at(70), &stamp(70)).await,
            field,
            adapter,
        );
    }
    // Stock moves.
    expect_invalid(
        receive(w, "suite-a\0b", 1, None, "suite-r", 70).await,
        "part_sku",
        adapter,
    );
    expect_invalid(
        receive(w, "suite-ab", 1, None, "suite-\0r", 70).await,
        "source_id",
        adapter,
    );
    expect_invalid(
        consume(w, "suite-a\0b", 1, "suite-c", 70).await,
        "part_sku",
        adapter,
    );
    expect_invalid(
        consume(w, "suite-ab", 1, "suite-\0c", 70).await,
        "source_id",
        adapter,
    );
    // GL facts.
    for (field, args) in [
        ("debit_account", ["dr\0", "3000", "m", "t", "suite-o"]),
        ("credit_account", ["1300", "3\0", "m", "t", "suite-o"]),
        ("memo", ["1300", "3000", "m\0", "t", "suite-o"]),
        ("source_table", ["1300", "3000", "m", "t\0", "suite-o"]),
        ("source_id", ["1300", "3000", "m", "t", "suite-\0o"]),
    ] {
        let [dr, cr, memo, table, source] = args;
        expect_invalid(
            r.record_inventory_je(1, dr, cr, memo, table, source, day(70), &stamp(70))
                .await,
            field,
            adapter,
        );
        if field != "source_table" {
            expect_invalid(
                r.record_overhead_absorbed(1, dr, cr, memo, source, day(70), &stamp(70))
                    .await,
                field,
                adapter,
            );
        }
    }
    // Purchase orders.
    let order = || {
        po(
            "suite-po2",
            None,
            PoStatus::DRAFT,
            None,
            vec![line("suite-ab", 1, 1)],
        )
    };
    let mut bad_orders = vec![
        (
            "id",
            PurchaseOrder {
                id: "suite-p\0".into(),
                ..order()
            },
        ),
        (
            "vendor",
            PurchaseOrder {
                vendor: Some("v\0".into()),
                ..order()
            },
        ),
        (
            "status",
            PurchaseOrder {
                status: PoStatus::new("dr\0aft"),
                ..order()
            },
        ),
    ];
    bad_orders.push((
        "part_sku",
        po(
            "suite-po2",
            None,
            PoStatus::DRAFT,
            None,
            vec![line("s\0", 1, 1)],
        ),
    ));
    let mut nul_currency = line("suite-ab", 1, 1);
    nul_currency.currency = "U\0D".into();
    bad_orders.push((
        "currency",
        po("suite-po2", None, PoStatus::DRAFT, None, vec![nul_currency]),
    ));
    for (field, bad) in bad_orders {
        expect_invalid(
            r.create_purchase_order_at(&bad, at(70), &stamp(70)).await,
            field,
            adapter,
        );
    }
    expect_invalid(
        r.update_po_status("suite-p\0", "closed", &stamp(70)).await,
        "id",
        adapter,
    );
    expect_invalid(
        r.update_po_status("suite-po", "cl\0", &stamp(70)).await,
        "status",
        adapter,
    );
    // Vendors.
    let v = || vendor("suite-v2", Some("V2"), None);
    let mut nul_behavior = whole_vendor("suite-v2");
    if let Some(b) = nul_behavior.behavior.as_mut() {
        b.provenance.template = Some("t\0".into());
    }
    for (field, bad) in [
        (
            "id",
            Vendor {
                id: "suite-\0".into(),
                ..v()
            },
        ),
        (
            "name",
            Vendor {
                name: Some("n\0".into()),
                ..v()
            },
        ),
        (
            "contact_name",
            Vendor {
                contact_name: Some("c\0".into()),
                ..v()
            },
        ),
        (
            "contact_email",
            Vendor {
                contact_email: Some("e\0".into()),
                ..v()
            },
        ),
        (
            "city",
            Vendor {
                city: Some("c\0".into()),
                ..v()
            },
        ),
        (
            "state",
            Vendor {
                state: Some("s\0".into()),
                ..v()
            },
        ),
        (
            "payment_terms",
            Vendor {
                payment_terms: Some("p\0".into()),
                ..v()
            },
        ),
        (
            "category",
            Vendor {
                category: Some("c\0".into()),
                ..v()
            },
        ),
        ("behavior", nul_behavior),
    ] {
        expect_invalid(
            r.create_vendor_at(&bad, at(70), &stamp(70)).await,
            field,
            adapter,
        );
        if field != "id" {
            expect_invalid(
                r.update_vendor("suite-v", &bad, &stamp(70)).await,
                field,
                adapter,
            );
        }
    }
    expect_invalid(
        r.update_vendor("suite-\0", &v(), &stamp(70)).await,
        "id",
        adapter,
    );
    expect_invalid(r.delete_vendor("suite-\0", &stamp(70)).await, "id", adapter);
    // Invoices.
    let i = || invoice("suite-i", "suite-po", date(2026, 9, 2), 1);
    let mut nul_line = i();
    nul_line.lines = vec![BillLine {
        part_sku: "s\0".into(),
        qty: 1,
        unit_cost_cents: 1,
    }];
    for (field, bad) in [
        (
            "id",
            VendorInvoice {
                id: "suite-\0".into(),
                ..i()
            },
        ),
        (
            "po_id",
            VendorInvoice {
                po_id: "suite-p\0".into(),
                ..i()
            },
        ),
        (
            "vendor",
            VendorInvoice {
                vendor: "v\0".into(),
                ..i()
            },
        ),
        (
            "vendor_invoice_no",
            VendorInvoice {
                vendor_invoice_no: "n\0".into(),
                ..i()
            },
        ),
        (
            "currency",
            VendorInvoice {
                currency: "U\0D".into(),
                ..i()
            },
        ),
        (
            "status",
            VendorInvoice {
                status: VendorInvoiceStatus::new("r\0"),
                ..i()
            },
        ),
        (
            "discrepancy_kind",
            VendorInvoice {
                discrepancy_kind: Some(boss_inventory::types::DiscrepancyKind::new("k\0")),
                ..i()
            },
        ),
        ("part_sku", nul_line),
    ] {
        expect_invalid(
            r.upsert_vendor_invoice_at(&bad, at(70), &stamp(70)).await,
            field,
            adapter,
        );
    }

    // Nothing written, nothing recorded.
    assert_eq!(
        suite_items(w).await,
        vec![kept(item("suite-ab", 5, 50))],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
    assert_eq!(
        suite_invoices(w, None, 100).await,
        Vec::<String>::new(),
        "{adapter}"
    );

    // Reads keyed by a NUL miss.
    assert_eq!(
        r.item_by_sku("suite-a\0b").await.expect("a miss"),
        None,
        "{adapter}"
    );
    assert_eq!(
        r.purchase_order_by_id("suite-p\0").await.expect("a miss"),
        None,
        "{adapter}"
    );
    assert!(
        !r.open_po_exists_for_part("s\0").await.expect("a miss"),
        "{adapter}"
    );
    assert_eq!(
        r.primary_vendor_for_part("s\0").await.expect("a miss"),
        None,
        "{adapter}"
    );
    assert_eq!(
        r.inbound_reserved_for_part("s\0").await.expect("a miss"),
        0,
        "{adapter}"
    );
    assert_eq!(
        r.vendor_invoice_by_id("suite-\0").await.expect("a miss"),
        None,
        "{adapter}"
    );
    assert_eq!(
        r.all_vendor_invoices(Some("r\0"), 100)
            .await
            .expect("a miss"),
        vec![],
        "{adapter}"
    );
}

// ----- Postgres alone ------------------------------------------------------

/// A GL account the chart does not hold is request data, not storage:
/// `InvalidAccount` naming the code (a 422), for a JE and an overhead
/// alike, and nothing is recorded. Stated for Postgres alone — the
/// double keeps no chart of accounts.
#[tokio::test]
async fn an_unknown_gl_account_is_refused_invalid_account_on_postgres() {
    let db = boss_testing::TestDb::new().await;
    let w = Postgres {
        repo: PgInventory::new(db.pool.clone()),
        pool: db.pool.clone(),
    };
    match w
        .repo()
        .record_inventory_je(
            1,
            "9999-nope",
            "3000",
            "m",
            "brewery_seed_opening_balance",
            "suite-o",
            day(80),
            &stamp(80),
        )
        .await
    {
        Err(InventoryError::InvalidAccount(m)) => assert!(m.contains("9999-nope"), "{m}"),
        other => panic!("je: {other:?}"),
    }
    match w
        .repo()
        .record_overhead_absorbed(1, "9999-nope", "5100", "m", "suite-o", day(80), &stamp(80))
        .await
    {
        Err(InventoryError::InvalidAccount(m)) => assert!(m.contains("9999-nope"), "{m}"),
        other => panic!("overhead: {other:?}"),
    }
    assert_eq!(w.facts().await, vec![]);
}

/// Paying a bill the ledger refuses — it would drive 1000 Cash below
/// zero — is a refusal the caller can act on, not a storage failure:
/// `Conflict` naming the ledger's reason (a 409), and the payment does
/// not land (backlog 55f69172: it answered `Storage`, a 500). The row
/// stays approved and nothing is recorded. Stated for Postgres alone —
/// the double keeps no journal, so holds no cash to refuse.
#[tokio::test]
async fn paying_a_bill_the_ledger_refuses_for_negative_cash_is_a_conflict_on_postgres() {
    let db = boss_testing::TestDb::new().await;
    let pool = guarded_pool(&db).await;
    let w = Postgres {
        repo: PgInventory::new(pool.clone()),
        pool,
    };
    billable(&w).await;
    let approved = VendorInvoice {
        approved_on: Some(date(2026, 9, 4)),
        status: VendorInvoiceStatus::new(VendorInvoiceStatus::APPROVED),
        ..invoice("suite-i", "suite-po", date(2026, 9, 2), 2_500)
    };
    w.repo()
        .upsert_vendor_invoice_at(&approved, at(60), &stamp(60))
        .await
        .expect("approve");
    let before = w.facts().await;
    let paid = VendorInvoice {
        paid_on: Some(date(2026, 9, 20)),
        status: VendorInvoiceStatus::new(VendorInvoiceStatus::PAID),
        ..approved.clone()
    };
    match w
        .repo()
        .upsert_vendor_invoice_at(&paid, at(61), &stamp(61))
        .await
    {
        Err(InventoryError::Conflict(m)) => assert!(m.contains("1000 Cash"), "{m}"),
        other => panic!("want Conflict naming 1000 Cash, got {other:?}"),
    }
    assert_eq!(
        w.repo()
            .vendor_invoice_by_id("suite-i")
            .await
            .expect("by id")
            .map(|i| i.status),
        Some(VendorInvoiceStatus::new(VendorInvoiceStatus::APPROVED))
    );
    assert_eq!(w.facts().await, before);
}

/// Conservation across the dropped second post (backlog be459ab9): a JE
/// and an overhead each post ONE journal entry whose debits and credits
/// both equal the fact's amount, and a replay of either posts nothing
/// more. Stated for Postgres alone — the double keeps no journal.
#[tokio::test]
async fn a_je_and_an_overhead_each_post_one_balanced_entry_and_a_replay_posts_none() {
    let db = boss_testing::TestDb::new().await;
    let repo = PgInventory::new(db.pool.clone());
    for _ in 0..2 {
        repo.record_inventory_je(
            12_345,
            "1300",
            "3000",
            "Opening raw — suite",
            "brewery_seed_opening_balance",
            "suite-open-1",
            day(90),
            &stamp(90),
        )
        .await
        .expect("je");
        repo.record_overhead_absorbed(
            777,
            "1310",
            "5100",
            "Overhead",
            "suite-step-1",
            day(90),
            &stamp(90),
        )
        .await
        .expect("overhead");
    }
    for (table, source, want) in [
        ("brewery_seed_opening_balance", "suite-open-1", 12_345_i64),
        ("ledger_overhead_absorbed", "suite-step-1", 777),
    ] {
        let (entries, dr, cr): (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(DISTINCT e.id)::bigint, \
                    COALESCE(SUM(l.debit_cents), 0)::bigint, \
                    COALESCE(SUM(l.credit_cents), 0)::bigint \
             FROM financial_facts f \
             JOIN gl_journal_entries e ON e.fact_id = f.id \
             JOIN gl_journal_lines l ON l.journal_entry_id = e.id \
             WHERE f.source_table = $1 AND f.source_id = $2",
        )
        .bind(table)
        .bind(source)
        .fetch_one(&db.pool)
        .await
        .expect("journal");
        assert_eq!((entries, dr, cr), (1, want, want), "{table}");
    }
}
