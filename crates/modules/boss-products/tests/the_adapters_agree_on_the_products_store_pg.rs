//! The finished-products store answers the same on both
//! `ProductsRepository` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), the next
//! smallest MODULE port of the census after the campaign store (backlog
//! be459ab9). Tests elsewhere may be answered by `InMemoryProducts`,
//! while production is answered by `PgProducts`.
//!
//! WHY IT EXISTS. The port was stated for Postgres alone
//! (`idempotency_pg.rs`, `value_conservation_pg.rs`) and the double was
//! stated nowhere; this file is the one statement of every method of the
//! port, each case holding each adapter to the same stated answer, and
//! every write also judged by the facts it leaves — the double's
//! `recorded_events` and Postgres's `event_outbox`, staged in the
//! write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - LIST ORDER. Postgres ordered `sku` and `location_id` in the
//!   database's locale, the double in byte order; Postgres now orders
//!   both `COLLATE "C"` (backlog 2987fb2d's class).
//! - THE FACTS. The double recorded no event at all, answered no GL move
//!   from a costed produce or consume, and its `record_inventory_je` was a
//!   stub that minted a fresh random fact id and answered `inserted`
//!   every time. It now records the same facts, answers the same GL move
//!   and the same canonical fact id, built by the functions both adapters
//!   call (`delta.rs`).
//! - REDELIVERY. A produce or consume replayed with the same `source_id`
//!   is a no-op on Postgres (the fact is the proof of application); the
//!   double applied it twice. It now keeps the same proof.
//! - THE ROW. The double stored `updated_at` as sent (or none) and the
//!   per-unit cost as sent; the column is the write's instant to the
//!   microsecond and the cost is derived (`value / on_hand`). The double
//!   now does both.
//! - AN UNREGISTERED SKU. Postgres refused an inventory write for one
//!   with its foreign-key error as `Storage` (a 500), the double stored
//!   it; both now refuse `NotFound` naming the SKU.
//! - A NON-POSITIVE JE. Postgres refused it `Invalid`, the double
//!   accepted it.
//! - A NUL BYTE. Postgres cannot store one in TEXT or JSONB and refused
//!   the write with its encoding error as `Storage` (a 500), and a read
//!   keyed by one the same way; the double stored it. Both now refuse a
//!   write carrying one with `Invalid` naming the field, and answer a
//!   read keyed by one as the miss it is.
//!
//! The world: the suite's rows are written THROUGH the port, so each
//! adapter seeds itself the way production does. The migrations may
//! seed rows of their own, so every read is judged over `suite-` SKUs.
//! The fixture holds a case pair and a punctuation pair (`suite-B`,
//! `suite-a-z`, `suite-ab`) — the only shape that catches the database's
//! locale disagreeing with the double's byte order.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_products::events::{
    LEDGER_INVENTORY_TRANSFERRED, PRODUCT_CONSUMED, PRODUCT_INVENTORY_UPSERTED, PRODUCT_PRODUCED,
    PRODUCT_UPSERTED,
};
use boss_products::port::{ProductsError, ProductsRepository};
use boss_products::types::{Product, ProductInventory};
use boss_products::{InMemoryProducts, PgProducts};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: ProductsRepository;
    fn repo(&self) -> &Self::R;
    /// Every fact about a `suite-` SKU or source, as `(kind, payload)`,
    /// in the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(payload: &Value) -> bool {
    ["sku", "product_sku", "source_id"].iter().any(|k| {
        payload[*k]
            .as_str()
            .is_some_and(|v| v.starts_with("suite-"))
    })
}

struct InMemory(InMemoryProducts);

impl World for InMemory {
    type R = InMemoryProducts;
    fn repo(&self) -> &InMemoryProducts {
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
    repo: PgProducts,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgProducts;
    fn repo(&self) -> &PgProducts {
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

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryProducts::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgProducts::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        the_catalog_lists_in_byte_order_of_sku_and_active_only_drops_the_retired,
        a_product_reads_back_whole_and_a_stranger_is_none,
        an_upsert_replaces_the_row_and_records_its_state_each_time,
        an_inventory_row_lists_by_byte_order_of_location_with_derived_cost_and_its_instant,
        the_recorded_inventory_facts_replay_to_the_table,
        an_inventory_write_on_an_unregistered_sku_is_refused_not_found,
        a_costed_produce_adds_units_and_value_and_answers_its_gl_move,
        an_uncosted_produce_moves_units_only,
        a_redelivered_costed_produce_changes_nothing_and_records_nothing,
        a_consume_drains_value_proportionally_and_the_last_unit_takes_the_rest,
        a_consume_past_on_hand_or_on_a_missing_row_is_refused_invalid,
        a_redelivered_consume_changes_nothing_and_records_nothing,
        a_non_positive_quantity_is_refused_invalid,
        an_inventory_je_answers_its_canonical_fact_once_and_records_it_once,
        a_non_positive_je_is_refused_invalid,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

/// An instant `secs` after a fixed origin, carrying nanoseconds the
/// columns cannot keep.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_789)
        .single()
        .expect("a representable instant")
}

/// `at(secs)` as the columns keep it: to the microsecond.
fn kept(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_000)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new("products", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

fn product(sku: &str) -> Product {
    Product {
        sku: sku.into(),
        name: format!("Name of {sku}"),
        product_kind: "beer".into(),
        package_unit: "1/2-bbl-keg".into(),
        description: None,
        metadata: json!({}),
        active: true,
    }
}

fn whole(sku: &str) -> Product {
    let mut p = product(sku);
    p.description = Some("A described keg".into());
    p.metadata = json!({"abv": 6.5, "style": "ipa", "tags": ["hoppy", "pale"]});
    p
}

/// The suite's catalog: a case pair and a punctuation pair, and a
/// retired SKU sorting last.
fn catalog() -> Vec<Product> {
    let mut retired = product("suite-retired");
    retired.active = false;
    vec![
        whole("suite-ab"),
        product("suite-B"),
        product("suite-a-z"),
        retired,
    ]
}

async fn seed<W: World>(w: &W, adapter: &str) {
    for p in catalog() {
        w.repo()
            .upsert_product(&p, &stamp(0))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: seed {}: {e:?}", p.sku));
    }
}

async fn suite_skus<W: World>(w: &W, adapter: &str, active_only: bool) -> Vec<String> {
    w.repo()
        .list_products(active_only)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: list: {e:?}"))
        .into_iter()
        .map(|p| p.sku)
        .filter(|s| s.starts_with("suite-"))
        .collect()
}

async fn rows<W: World>(w: &W, adapter: &str, sku: &str) -> Vec<ProductInventory> {
    w.repo()
        .inventory_for(sku)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: inventory_for {sku}: {e:?}"))
}

/// The row as the store must answer it.
fn row(loc: &str, on_hand: i32, value: i64, secs: i64) -> ProductInventory {
    ProductInventory {
        product_sku: "suite-ab".into(),
        location_id: loc.into(),
        on_hand,
        reserved: 0,
        value_cents: value,
        production_cost_cents: if on_hand > 0 {
            value / on_hand as i64
        } else {
            0
        },
        updated_at: Some(kept(secs)),
    }
}

fn day(secs: i64) -> NaiveDate {
    at(secs).date_naive()
}

/// The GL payload a costed produce answers and records.
fn produced(qty: i32, total: i64, source: &str, secs: i64) -> Value {
    json!({
        "total_cost_cents": total,
        "debit_account": "1320",
        "credit_account": "1310",
        "memo": format!("Production — produced {qty} × suite-ab (WIP → FG, exact line total)"),
        "sku": "suite-ab",
        "location_id": "loc-a",
        "qty": qty,
        "source_id": source,
        "happened_on": day(secs).to_string(),
    })
}

/// The GL payload a costed consume answers and records.
fn consumed(qty: i32, total: i64, source: &str, secs: i64, category: Option<&str>) -> Value {
    let mut p = json!({
        "total_cost_cents": total,
        "cogs_account": "5100",
        "inventory_account": "1320",
        "memo": format!("COGS — sold {qty} × suite-ab (value drain)"),
        "sku": "suite-ab",
        "location_id": "loc-a",
        "qty": qty,
        "source_id": source,
        "happened_on": day(secs).to_string(),
    });
    if let Some(c) = category {
        p["revenue_category"] = json!(c);
    }
    p
}

fn inv_fact(r: &ProductInventory, secs: i64) -> (String, Value) {
    fact(
        secs,
        PRODUCT_INVENTORY_UPSERTED,
        serde_json::to_value(r).expect("a row serialises"),
    )
}

async fn produce<W: World>(
    w: &W,
    qty: i32,
    total: Option<i64>,
    source: &str,
    secs: i64,
) -> Result<boss_products::InventoryDeltaResult, ProductsError> {
    w.repo()
        .produce(
            "suite-ab",
            "loc-a",
            qty,
            total,
            at(secs),
            source.into(),
            &stamp(secs),
        )
        .await
}

async fn consume<W: World>(
    w: &W,
    qty: i32,
    category: Option<&str>,
    source: &str,
    secs: i64,
) -> Result<boss_products::InventoryDeltaResult, ProductsError> {
    w.repo()
        .consume(
            "suite-ab",
            "loc-a",
            qty,
            category,
            at(secs),
            source.into(),
            &stamp(secs),
        )
        .await
}

// ----- cases ---------------------------------------------------------------

/// The catalog is in BYTE order of SKU — `suite-B` < `suite-a-z` <
/// `suite-ab`, which a locale collation orders the other way round —
/// and `active_only` drops the retired SKU.
async fn the_catalog_lists_in_byte_order_of_sku_and_active_only_drops_the_retired<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    assert_eq!(
        suite_skus(w, adapter, false).await,
        ["suite-B", "suite-a-z", "suite-ab", "suite-retired"],
        "{adapter}"
    );
    assert_eq!(
        suite_skus(w, adapter, true).await,
        ["suite-B", "suite-a-z", "suite-ab"],
        "{adapter}"
    );
}

/// A product reads back whole by `get` and inside the list; a SKU
/// nobody registered is `None`, not an error.
async fn a_product_reads_back_whole_and_a_stranger_is_none<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let want = whole("suite-ab");
    assert_eq!(
        w.repo().get_product("suite-ab").await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );
    let listed = w.repo().list_products(false).await.expect("list");
    assert!(listed.contains(&want), "{adapter}: {listed:?}");
    assert_eq!(
        w.repo().get_product("suite-stranger").await.expect("get"),
        None,
        "{adapter}"
    );
}

/// An upsert on a held SKU replaces every field, and each upsert records
/// the row's state as sent.
async fn an_upsert_replaces_the_row_and_records_its_state_each_time<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = whole("suite-ab");
    let mut second = product("suite-ab");
    second.name = "Renamed".into();
    second.product_kind = "cider".into();
    second.package_unit = "12oz-case".into();
    second.active = false;
    w.repo().upsert_product(&first, &stamp(1)).await.expect("1");
    w.repo()
        .upsert_product(&second, &stamp(2))
        .await
        .expect("2");
    assert_eq!(
        w.repo().get_product("suite-ab").await.expect("get"),
        Some(second.clone()),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![
            fact(1, PRODUCT_UPSERTED, serde_json::to_value(&first).unwrap()),
            fact(2, PRODUCT_UPSERTED, serde_json::to_value(&second).unwrap()),
        ],
        "{adapter}"
    );
}

/// An inventory row is absolute state, last write wins. It reads back
/// listed in BYTE order of location, its per-unit cost DERIVED (what was
/// sent is ignored), its `updated_at` the write's instant to the
/// microsecond; the fact carries the row AS STORED — the derived cost and
/// the write's instant, never the caller's (backlog 797b6168: it carried
/// the row as sent, a cost and a time the table never held).
async fn an_inventory_row_lists_by_byte_order_of_location_with_derived_cost_and_its_instant<
    W: World,
>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    let sent = |loc: &str, on_hand: i32, value: i64| ProductInventory {
        product_sku: "suite-ab".into(),
        location_id: loc.into(),
        on_hand,
        reserved: 2,
        value_cents: value,
        production_cost_cents: 999_999,
        updated_at: None,
    };
    let writes = [
        sent("loc-ab", 3, 1000),
        sent("loc-B", 0, 0),
        sent("loc-a-z", 7, 70),
        sent("loc-ab", 4, 1001),
    ];
    let before = w.facts().await;
    for (i, r) in writes.iter().enumerate() {
        w.repo()
            .upsert_inventory(r, &stamp(10 + i as i64))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: upsert {i}: {e:?}"));
    }
    let want = |loc: &str, on_hand: i32, value: i64, secs: i64| ProductInventory {
        reserved: 2,
        ..row(loc, on_hand, value, secs)
    };
    assert_eq!(
        rows(w, adapter, "suite-ab").await,
        vec![
            want("loc-B", 0, 0, 11),
            want("loc-a-z", 7, 70, 12),
            want("loc-ab", 4, 1001, 13),
        ],
        "{adapter}"
    );
    assert_eq!(rows(w, adapter, "suite-B").await, vec![], "{adapter}");
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    let want_facts: Vec<_> = writes
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let secs = 10 + i as i64;
            inv_fact(&want(&r.location_id, r.on_hand, r.value_cents, secs), secs)
        })
        .collect();
    assert_eq!(recorded, want_facts, "{adapter}");
}

/// Determinism (correctness protocol): the recorded
/// `products.inventory.upserted` facts, replayed into an empty table in
/// the order recorded, give back exactly the table the writes left —
/// derived cost and instant included — across every write that records
/// one (an absolute upsert, a produce, a consume). Backlog 797b6168: an
/// upsert recorded the caller's row, so a reader of the log saw a cost
/// and a time the table never held.
async fn the_recorded_inventory_facts_replay_to_the_table<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let before = w.facts().await.len();
    let sent = ProductInventory {
        production_cost_cents: 999_999,
        updated_at: Some(at(-1_000)),
        ..row("loc-a", 4, 1_001, 0)
    };
    w.repo()
        .upsert_inventory(&sent, &stamp(70))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: upsert: {e:?}"));
    let other = ProductInventory {
        location_id: "loc-B".into(),
        ..sent.clone()
    };
    w.repo()
        .upsert_inventory(&other, &stamp(71))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: upsert loc-B: {e:?}"));
    produce(w, 3, Some(300), "suite-p1", 72).await.expect("p1");
    consume(w, 2, None, "suite-c1", 73).await.expect("c1");

    let replayed: Vec<ProductInventory> = w
        .facts()
        .await
        .into_iter()
        .skip(before)
        .filter(|(kind, _)| kind == PRODUCT_INVENTORY_UPSERTED)
        .map(|(_, p)| serde_json::from_value::<ProductInventory>(p).expect("a row deserialises"))
        .fold(
            std::collections::BTreeMap::new(),
            |mut table, r: ProductInventory| {
                table.insert(r.location_id.clone(), r);
                table
            },
        )
        .into_values()
        .collect();
    assert_eq!(replayed.len(), 2, "{adapter}: {replayed:?}");
    assert_eq!(replayed, rows(w, adapter, "suite-ab").await, "{adapter}");
}

/// Inventory is kept only for a registered product: a write naming a SKU
/// nobody registered is refused `NotFound` naming it, writes no row and
/// records nothing.
async fn an_inventory_write_on_an_unregistered_sku_is_refused_not_found<W: World>(
    w: &W,
    adapter: &str,
) {
    let stranger = ProductInventory {
        product_sku: "suite-stranger".into(),
        ..row("loc-a", 1, 1, 0)
    };
    match w.repo().upsert_inventory(&stranger, &stamp(1)).await {
        Err(ProductsError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: upsert_inventory: {other:?}"),
    }
    match w
        .repo()
        .produce(
            "suite-stranger",
            "loc-a",
            1,
            Some(10),
            at(1),
            "suite-p".into(),
            &stamp(1),
        )
        .await
    {
        Err(ProductsError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: produce: {other:?}"),
    }
    assert_eq!(
        rows(w, adapter, "suite-stranger").await,
        vec![],
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// A costed produce adds the units and the EXACT line total, answers the
/// WIP→FG move, and records the post-delta row and the move.
async fn a_costed_produce_adds_units_and_value_and_answers_its_gl_move<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let first = produce(w, 3, Some(1_000), "suite-p1", 20)
        .await
        .expect("p1");
    assert_eq!(first.inventory, row("loc-a", 3, 1_000, 20), "{adapter}");
    let gl = first.gl_move.expect("a costed produce moves the GL");
    assert_eq!(gl.source_id, "suite-p1", "{adapter}");
    assert_eq!(gl.happened_on, day(20), "{adapter}");
    assert_eq!(gl.payload, produced(3, 1_000, "suite-p1", 20), "{adapter}");

    let second = produce(w, 4, Some(7), "suite-p2", 21).await.expect("p2");
    assert_eq!(second.inventory, row("loc-a", 7, 1_007, 21), "{adapter}");
    assert_eq!(
        rows(w, adapter, "suite-ab").await,
        vec![second.inventory.clone()]
    );

    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![
            inv_fact(&row("loc-a", 3, 1_000, 20), 20),
            fact(20, PRODUCT_PRODUCED, produced(3, 1_000, "suite-p1", 20)),
            inv_fact(&row("loc-a", 7, 1_007, 21), 21),
            fact(21, PRODUCT_PRODUCED, produced(4, 7, "suite-p2", 21)),
        ],
        "{adapter}"
    );
}

/// With no cost, or a cost of zero, a produce moves units only: value
/// unchanged, no GL move, only the row recorded.
async fn an_uncosted_produce_moves_units_only<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let a = produce(w, 2, None, "suite-u1", 30).await.expect("u1");
    assert_eq!(a.inventory, row("loc-a", 2, 0, 30), "{adapter}");
    assert!(a.gl_move.is_none(), "{adapter}");
    let b = produce(w, 3, Some(0), "suite-u2", 31).await.expect("u2");
    assert_eq!(b.inventory, row("loc-a", 5, 0, 31), "{adapter}");
    assert!(b.gl_move.is_none(), "{adapter}");
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![
            inv_fact(&row("loc-a", 2, 0, 30), 30),
            inv_fact(&row("loc-a", 5, 0, 31), 31)
        ],
        "{adapter}"
    );
}

/// A costed produce replayed with the same `source_id` (a redelivered
/// step effect) answers the current row unchanged, no GL move, and
/// records nothing.
async fn a_redelivered_costed_produce_changes_nothing_and_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    produce(w, 3, Some(300), "suite-p1", 40).await.expect("p1");
    let before = w.facts().await;
    let replay = produce(w, 3, Some(300), "suite-p1", 41)
        .await
        .expect("replay");
    assert_eq!(replay.inventory, row("loc-a", 3, 300, 40), "{adapter}");
    assert!(replay.gl_move.is_none(), "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A consume drains round(value × qty / on_hand); the last unit takes
/// whatever value remains, so zero on hand is zero value. Each costed
/// drain answers its COGS move, tagged with the revenue category when
/// one is given, and records the row and the move.
async fn a_consume_drains_value_proportionally_and_the_last_unit_takes_the_rest<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    produce(w, 3, Some(1_000), "suite-p1", 50)
        .await
        .expect("p1");
    let before = w.facts().await;
    // round(1000 × 1 / 3) = 333.
    let one = consume(w, 1, Some("draft"), "suite-c1", 51)
        .await
        .expect("c1");
    assert_eq!(one.inventory, row("loc-a", 2, 667, 51), "{adapter}");
    let gl = one.gl_move.expect("a costed consume moves the GL");
    assert_eq!(
        gl.payload,
        consumed(1, 333, "suite-c1", 51, Some("draft")),
        "{adapter}"
    );
    let rest = consume(w, 2, None, "suite-c2", 52).await.expect("c2");
    assert_eq!(rest.inventory, row("loc-a", 0, 0, 52), "{adapter}");
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![
            inv_fact(&row("loc-a", 2, 667, 51), 51),
            fact(
                51,
                PRODUCT_CONSUMED,
                consumed(1, 333, "suite-c1", 51, Some("draft"))
            ),
            inv_fact(&row("loc-a", 0, 0, 52), 52),
            fact(52, PRODUCT_CONSUMED, consumed(2, 667, "suite-c2", 52, None)),
        ],
        "{adapter}"
    );
}

/// Finished goods never go negative: a consume past what is on hand, or
/// against a location holding no row, is refused `Invalid`, changes
/// nothing and records nothing.
async fn a_consume_past_on_hand_or_on_a_missing_row_is_refused_invalid<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    produce(w, 2, Some(20), "suite-p1", 60).await.expect("p1");
    let before = w.facts().await;
    match consume(w, 3, None, "suite-c1", 61).await {
        Err(ProductsError::Invalid(_)) => {}
        other => panic!("{adapter}: past on hand: {other:?}"),
    }
    match w
        .repo()
        .consume(
            "suite-ab",
            "loc-empty",
            1,
            None,
            at(61),
            "suite-c2".into(),
            &stamp(61),
        )
        .await
    {
        Err(ProductsError::Invalid(_)) => {}
        other => panic!("{adapter}: missing row: {other:?}"),
    }
    assert_eq!(
        rows(w, adapter, "suite-ab").await,
        vec![row("loc-a", 2, 20, 60)],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A costed consume replayed with the same `source_id` answers the
/// current row unchanged, no GL move, and records nothing.
async fn a_redelivered_consume_changes_nothing_and_records_nothing<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    produce(w, 4, Some(400), "suite-p1", 70).await.expect("p1");
    consume(w, 1, None, "suite-c1", 71).await.expect("c1");
    let before = w.facts().await;
    let replay = consume(w, 1, None, "suite-c1", 72).await.expect("replay");
    assert_eq!(replay.inventory, row("loc-a", 3, 300, 71), "{adapter}");
    assert!(replay.gl_move.is_none(), "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A produce or consume of zero or fewer units is refused `Invalid` and
/// records nothing.
async fn a_non_positive_quantity_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    produce(w, 1, Some(10), "suite-p1", 80).await.expect("p1");
    let before = w.facts().await;
    for qty in [0, -1] {
        match produce(w, qty, Some(10), "suite-p2", 81).await {
            Err(ProductsError::Invalid(m)) => assert!(m.contains("qty"), "{adapter}: {m}"),
            other => panic!("{adapter}: produce {qty}: {other:?}"),
        }
        match consume(w, qty, None, "suite-c1", 81).await {
            Err(ProductsError::Invalid(m)) => assert!(m.contains("qty"), "{adapter}: {m}"),
            other => panic!("{adapter}: consume {qty}: {other:?}"),
        }
    }
    assert_eq!(
        rows(w, adapter, "suite-ab").await,
        vec![row("loc-a", 1, 10, 80)],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

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
                "1320",
                "3000",
                "Opening FG — suite",
                "products_opening",
                "suite-open-1",
                day(90),
                &stamp(secs),
            )
            .await
    };
    let payload = json!({
        "total_cost_cents": 12_345,
        "debit_account": "1320",
        "credit_account": "3000",
        "memo": "Opening FG — suite",
        "happened_on": day(90).to_string(),
        "source_table": "products_opening",
        "source_id": "suite-open-1",
    });
    let id = boss_ledger::deterministic_fact_id(
        "finance.inventory.transferred",
        "products_opening",
        "suite-open-1",
    );
    let first = je(90).await.expect("first");
    assert_eq!(first.fact_id, id, "{adapter}");
    assert!(first.inserted, "{adapter}");
    assert_eq!(first.payload, payload, "{adapter}");
    let again = je(91).await.expect("again");
    assert_eq!(again.fact_id, id, "{adapter}");
    assert!(!again.inserted, "{adapter}: the same key is not a new fact");
    assert_eq!(
        w.facts().await,
        vec![fact(90, LEDGER_INVENTORY_TRANSFERRED, payload)],
        "{adapter}"
    );
}

/// A JE of zero or fewer cents is refused `Invalid` and records nothing.
async fn a_non_positive_je_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    for cents in [0, -5] {
        match w
            .repo()
            .record_inventory_je(
                cents,
                "1320",
                "3000",
                "memo",
                "products_opening",
                "suite-open-2",
                day(95),
                &stamp(95),
            )
            .await
        {
            Err(ProductsError::Invalid(m)) => {
                assert!(m.contains("total_cost_cents"), "{adapter}: {m}")
            }
            other => panic!("{adapter}: {cents}: {other:?}"),
        }
    }
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// A NUL byte cannot be stored in TEXT or JSONB. A write carrying one —
/// in any text argument or field, or anywhere inside the metadata — is
/// refused `Invalid` naming the field and writes nothing. A read keyed
/// by one is the miss it is: no stored SKU can hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let nul = |f: &dyn Fn(&mut Product)| {
        let mut p = product("suite-nul");
        f(&mut p);
        p
    };
    let products = [
        ("sku", nul(&|p| p.sku = "suite-n\0ul".into())),
        ("name", nul(&|p| p.name = "Nu\0l".into())),
        ("product_kind", nul(&|p| p.product_kind = "be\0er".into())),
        ("package_unit", nul(&|p| p.package_unit = "k\0eg".into())),
        ("description", nul(&|p| p.description = Some("d\0".into()))),
        ("metadata", nul(&|p| p.metadata = json!({"tags": ["a\0b"]}))),
        ("metadata", nul(&|p| p.metadata = json!({"n": {"a\0b": 1}}))),
    ];
    for (field, bad) in products {
        match w.repo().upsert_product(&bad, &stamp(99)).await {
            Err(ProductsError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    let inv = |sku: &str, loc: &str| ProductInventory {
        product_sku: sku.into(),
        location_id: loc.into(),
        ..row("loc-a", 1, 1, 0)
    };
    for (field, bad) in [
        ("product_sku", inv("suite-a\0b", "loc-a")),
        ("location_id", inv("suite-ab", "lo\0c")),
    ] {
        match w.repo().upsert_inventory(&bad, &stamp(99)).await {
            Err(ProductsError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    let r = w.repo();
    for (field, got) in [
        (
            "sku",
            r.produce(
                "suite-a\0b",
                "loc-a",
                1,
                Some(1),
                at(99),
                "suite-p".into(),
                &stamp(99),
            )
            .await,
        ),
        (
            "location_id",
            r.produce(
                "suite-ab",
                "lo\0c",
                1,
                Some(1),
                at(99),
                "suite-p".into(),
                &stamp(99),
            )
            .await,
        ),
        (
            "source_id",
            r.produce(
                "suite-ab",
                "loc-a",
                1,
                Some(1),
                at(99),
                "suite-\0p".into(),
                &stamp(99),
            )
            .await,
        ),
        (
            "revenue_category",
            r.consume(
                "suite-ab",
                "loc-a",
                1,
                Some("d\0"),
                at(99),
                "suite-c".into(),
                &stamp(99),
            )
            .await,
        ),
    ] {
        match got {
            Err(ProductsError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    match r
        .record_inventory_je(
            1,
            "1320",
            "3000",
            "m\0emo",
            "products_opening",
            "suite-o",
            day(99),
            &stamp(99),
        )
        .await
    {
        Err(ProductsError::Invalid(m)) => assert!(m.contains("memo"), "{adapter}: {m}"),
        other => panic!("{adapter}: a NUL memo: {other:?}"),
    }
    assert_eq!(
        suite_skus(w, adapter, false).await,
        ["suite-B", "suite-a-z", "suite-ab", "suite-retired"],
        "{adapter}: no row"
    );
    assert_eq!(rows(w, adapter, "suite-ab").await, vec![], "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
    assert_eq!(
        r.get_product("suite-n\0ul").await.expect("a miss"),
        None,
        "{adapter}"
    );
    assert_eq!(rows(w, adapter, "suite-n\0ul").await, vec![], "{adapter}");
}
