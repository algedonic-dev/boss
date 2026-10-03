//! The shipment store answers the same on both `ShippingRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the next smallest MODULE
//! port of the census after the products store (backlog be459ab9).
//! Tests elsewhere may be answered by `InMemoryShipping`, while
//! production is answered by `PgShipping`.
//!
//! WHY IT EXISTS. The port was stated for Postgres through the HTTP
//! e2e files and for the double through its own unit tests, and the two
//! statements never met; this file is the one statement of every method
//! of the port, each case holding each adapter to the same stated
//! answer, and every write also judged by the facts it leaves — the
//! double's `recorded_events` and Postgres's `event_outbox`, staged in
//! the write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - A HELD ID. The port says a create "errors if ID already exists";
//!   Postgres upserted instead — overwriting the row (a redelivered
//!   `shipping.create` effect regressed a scanned shipment to
//!   label-created), keeping asset ids the new body dropped, and
//!   recording a second `created` fact for one shipment. It now refuses
//!   `Conflict` like the double.
//! - LIST ORDER. Postgres ordered newest `created_on` first and left
//!   ties to the planner; the double answered insertion order. Both now
//!   order newest first, then by id in byte order (`COLLATE "C"`), so a
//!   page is a stable cut.
//! - ASSET IDS. Postgres answered them sorted in the database's locale,
//!   each once; the double as sent. Both now answer byte order, each
//!   once.
//! - TRACKING SCANS. The double's `record_tracking_scan` was a stub that
//!   answered `Ok` for any shipment, rolled nothing up and recorded
//!   nothing. It now keeps the same proof of application, the same
//!   rollup and the same fact as Postgres.
//! - THE SUMMARY PREVIEW. The double capped the preview at ten whatever
//!   the caller asked for, and both left tied rows to chance; both now
//!   honour the limit and break ties by id in byte order.
//! - A RENAMING UPDATE. An update whose body names another id renamed the
//!   row in the double and wrote a second row in Postgres; both now
//!   refuse it `Invalid`.
//! - REFUSALS, NOT 500s. A negative limit or offset, a line item of no
//!   units, and a NUL byte (which Postgres cannot store in TEXT) each
//!   reached Postgres and came back as `Storage` (a 500) while the double
//!   accepted them. Both now refuse `Invalid` naming the field, and a
//!   read keyed by a NUL is the miss it is.
//!
//! The world: the suite's rows are written THROUGH the port, so each
//! adapter seeds itself the way production does. Every read of the whole
//! table is judged over `suite-` ids. The fixture holds a case pair and a
//! punctuation pair (`suite-B`, `suite-a-z`, `suite-ab`) — the only shape
//! that catches the database's locale disagreeing with the double's byte
//! order.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_shipping::events::{
    SHIPMENT_CREATED, SHIPMENT_DELETED, SHIPMENT_UPDATED, TRACKING_RECORDED,
};
use boss_shipping::types::{
    Carrier, Shipment, ShipmentDirection, ShipmentLineItem, ShipmentStatus,
};
use boss_shipping::{InMemoryShipping, PgShipping, ShippingError, ShippingRepository};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: ShippingRepository;
    fn repo(&self) -> &Self::R;
    /// Every fact about a `suite-` shipment, as `(kind, payload)`, in
    /// the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(payload: &Value) -> bool {
    ["id", "shipment_id"].iter().any(|k| {
        payload[*k]
            .as_str()
            .is_some_and(|v| v.starts_with("suite-"))
    })
}

struct InMemory(InMemoryShipping);

impl World for InMemory {
    type R = InMemoryShipping;
    fn repo(&self) -> &InMemoryShipping {
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
    repo: PgShipping,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgShipping;
    fn repo(&self) -> &PgShipping {
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
        in_memory => (InMemory(InMemoryShipping::new(vec![])), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgShipping::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        every_shipment_lists_newest_first_then_by_byte_order_of_id,
        a_page_is_a_stable_cut_of_that_order_and_counts_the_accounts_total,
        a_shipment_reads_back_whole_with_its_asset_ids_in_byte_order_each_once,
        a_create_records_its_state_and_a_held_id_is_refused_conflict,
        an_update_replaces_the_row_and_records_its_state,
        an_update_of_a_stranger_or_naming_another_id_is_refused_and_records_nothing,
        a_delete_removes_the_row_and_records_it_once,
        a_scan_rolls_up_in_transit_and_delivered_and_records_once,
        a_redelivered_scan_changes_nothing_and_records_nothing,
        a_scan_for_a_stranger_is_refused_not_found,
        the_summary_counts_in_flight_and_the_week_and_previews_in_order,
        a_negative_limit_or_offset_is_refused_invalid,
        a_line_item_of_no_units_is_refused_invalid,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

/// An instant `secs` after a fixed origin, carrying nanoseconds.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_789)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new("shipping", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

fn state(s: &Shipment) -> Value {
    serde_json::to_value(s).expect("a shipment serialises")
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("a September day")
}

/// A label-created outbound shipment, created on day `created`.
fn shipment(id: &str, created: u32) -> Shipment {
    Shipment {
        id: id.into(),
        direction: ShipmentDirection::Outbound,
        status: ShipmentStatus::new(ShipmentStatus::LABEL_CREATED),
        carrier: None,
        tracking_number: None,
        origin: "Brewhouse".into(),
        destination: format!("Dock of {id}"),
        asset_ids: vec![],
        line_items: vec![],
        po_id: None,
        order_id: None,
        account_id: None,
        created_on: day(created),
        shipped_on: None,
        estimated_delivery: None,
        delivered_on: None,
    }
}

/// Every optional field filled.
fn whole(id: &str) -> Shipment {
    Shipment {
        carrier: Some(Carrier::new("keg-courier")),
        tracking_number: Some("TRK-1".into()),
        asset_ids: vec!["asset-b".into(), "asset-B".into(), "asset-a-z".into()],
        line_items: vec![
            ShipmentLineItem {
                sku: "FP-PALE".into(),
                qty: 12,
                unit_price_cents: Some(13_500),
                description: Some("Pale half-barrel".into()),
            },
            ShipmentLineItem {
                sku: "FP-IPA".into(),
                qty: 3,
                unit_price_cents: None,
                description: None,
            },
        ],
        po_id: Some("po-1".into()),
        order_id: Some("ord-1".into()),
        account_id: Some("suite-acct".into()),
        shipped_on: Some(day(3)),
        estimated_delivery: Some(day(6)),
        ..shipment(id, 2)
    }
}

/// The suite's shipments: three created the same day (a case pair and a
/// punctuation pair, so only the id can order them), one older, one
/// newer.
fn fleet() -> Vec<Shipment> {
    vec![
        shipment("suite-ab", 10),
        shipment("suite-old", 5),
        shipment("suite-B", 10),
        shipment("suite-new", 12),
        shipment("suite-a-z", 10),
    ]
}

const FLEET_ORDER: [&str; 5] = ["suite-new", "suite-B", "suite-a-z", "suite-ab", "suite-old"];

async fn create<W: World>(w: &W, s: &Shipment, secs: i64) -> Result<String, ShippingError> {
    w.repo().create_shipment_at(s, at(secs), &stamp(secs)).await
}

async fn seed<W: World>(w: &W, adapter: &str, shipments: &[Shipment]) {
    for s in shipments {
        create(w, s, 0)
            .await
            .unwrap_or_else(|e| panic!("{adapter}: seed {}: {e:?}", s.id));
    }
}

fn ids(shipments: &[Shipment]) -> Vec<String> {
    shipments
        .iter()
        .map(|s| s.id.clone())
        .filter(|id| id.starts_with("suite-"))
        .collect()
}

async fn get<W: World>(w: &W, adapter: &str, id: &str) -> Option<Shipment> {
    w.repo()
        .shipment_by_id(id)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: get {id}: {e:?}"))
}

async fn scan<W: World>(
    w: &W,
    id: &str,
    status: &str,
    on: NaiveDate,
    stage: Option<i16>,
    secs: i64,
) -> Result<(), ShippingError> {
    w.repo()
        .record_tracking_scan(id, status, on, stage, &stamp(secs))
        .await
}

fn scanned(
    id: &str,
    status: &str,
    on: NaiveDate,
    stage: Option<i16>,
    secs: i64,
) -> (String, Value) {
    fact(
        secs,
        TRACKING_RECORDED,
        json!({
            "shipment_id": id,
            "status": status,
            "occurred_on": on,
            "stage_index": stage,
        }),
    )
}

// ----- cases ---------------------------------------------------------------

/// Every shipment, and the unfiltered page, is newest `created_on` first
/// and, within a day, by id in BYTE order — `suite-B` < `suite-a-z` <
/// `suite-ab`, which a locale collation orders the other way round.
async fn every_shipment_lists_newest_first_then_by_byte_order_of_id<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter, &fleet()).await;
    let all = w.repo().all_shipments().await.expect("all");
    assert_eq!(ids(&all), FLEET_ORDER, "{adapter}: all_shipments");
    let (page, _) = w
        .repo()
        .list_shipments(10_000, 0, None)
        .await
        .expect("list");
    assert_eq!(ids(&page), FLEET_ORDER, "{adapter}: list_shipments");
}

/// A page filtered to one account is cut from that same order, and its
/// total counts every shipment of the account, not the page. A limit of
/// zero is an empty page with the total; an offset past the end is an
/// empty page; an account nobody ships to is empty with a total of 0.
async fn a_page_is_a_stable_cut_of_that_order_and_counts_the_accounts_total<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut fleet = fleet();
    for s in fleet.iter_mut() {
        s.account_id = Some("suite-acct".into());
    }
    let mut other = shipment("suite-other", 11);
    other.account_id = Some("suite-elsewhere".into());
    fleet.push(other);
    seed(w, adapter, &fleet).await;
    let page = |limit: i64, offset: i64| async move {
        let (rows, total) = w
            .repo()
            .list_shipments(limit, offset, Some("suite-acct"))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: list {limit}/{offset}: {e:?}"));
        (ids(&rows), total)
    };
    assert_eq!(
        page(2, 0).await,
        (vec!["suite-new".into(), "suite-B".into()], 5),
        "{adapter}"
    );
    assert_eq!(
        page(2, 2).await,
        (vec!["suite-a-z".into(), "suite-ab".into()], 5),
        "{adapter}"
    );
    assert_eq!(page(2, 4).await, (vec!["suite-old".into()], 5), "{adapter}");
    assert_eq!(page(0, 0).await, (vec![], 5), "{adapter}");
    assert_eq!(page(2, 99).await, (vec![], 5), "{adapter}");
    assert_eq!(page(i64::MAX, 1).await.1, 5, "{adapter}: a huge limit");
    let (none, total) = w
        .repo()
        .list_shipments(10, 0, Some("suite-nobody"))
        .await
        .expect("list");
    assert_eq!((none.len(), total), (0, 0), "{adapter}");
}

/// A shipment reads back whole — every field as sent, its line items in
/// authoring order — except its asset ids, which answer in BYTE order,
/// each once. A stranger is `None`, not an error.
async fn a_shipment_reads_back_whole_with_its_asset_ids_in_byte_order_each_once<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut sent = whole("suite-ab");
    sent.asset_ids.push("asset-b".into());
    seed(w, adapter, std::slice::from_ref(&sent)).await;
    let want = Shipment {
        asset_ids: vec!["asset-B".into(), "asset-a-z".into(), "asset-b".into()],
        ..sent
    };
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(want.clone()),
        "{adapter}"
    );
    let all = w.repo().all_shipments().await.expect("all");
    assert!(all.contains(&want), "{adapter}: {all:?}");
    assert_eq!(get(w, adapter, "suite-stranger").await, None, "{adapter}");
}

/// A create answers the id and records the shipment's state as sent. A
/// second create on a held id is refused `Conflict` naming it — the row
/// keeps what the first create wrote and nothing more is recorded.
async fn a_create_records_its_state_and_a_held_id_is_refused_conflict<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = whole("suite-ab");
    assert_eq!(
        create(w, &first, 1).await.expect("create"),
        "suite-ab",
        "{adapter}"
    );
    let mut again = shipment("suite-ab", 9);
    again.status = ShipmentStatus::new(ShipmentStatus::EXCEPTION);
    match create(w, &again, 2).await {
        Err(ShippingError::Conflict(m)) => assert!(m.contains("suite-ab"), "{adapter}: {m}"),
        other => panic!("{adapter}: a held id: {other:?}"),
    }
    let kept = get(w, adapter, "suite-ab").await.expect("held");
    assert_eq!(kept.status, first.status, "{adapter}");
    assert_eq!(kept.line_items, first.line_items, "{adapter}");
    assert_eq!(kept.asset_ids.len(), 3, "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![fact(1, SHIPMENT_CREATED, state(&first))],
        "{adapter}"
    );
}

/// An update replaces every field — asset ids and line items included,
/// so one the new body drops is gone — and records the new state as
/// sent.
async fn an_update_replaces_the_row_and_records_its_state<W: World>(w: &W, adapter: &str) {
    let first = whole("suite-ab");
    create(w, &first, 1).await.expect("create");
    let second = Shipment {
        direction: ShipmentDirection::Inbound,
        status: ShipmentStatus::new(ShipmentStatus::DELIVERED),
        carrier: None,
        tracking_number: None,
        origin: "Vendor".into(),
        destination: "Brewhouse".into(),
        asset_ids: vec!["asset-c".into()],
        line_items: vec![ShipmentLineItem {
            sku: "FP-STOUT".into(),
            qty: 1,
            unit_price_cents: None,
            description: None,
        }],
        po_id: None,
        order_id: None,
        account_id: None,
        created_on: day(4),
        shipped_on: None,
        estimated_delivery: None,
        delivered_on: Some(day(8)),
        id: "suite-ab".into(),
    };
    w.repo()
        .update_shipment_at("suite-ab", &second, at(2), &stamp(2))
        .await
        .expect("update");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(second.clone()),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![
            fact(1, SHIPMENT_CREATED, state(&first)),
            fact(2, SHIPMENT_UPDATED, state(&second)),
        ],
        "{adapter}"
    );
}

/// An update of an id nobody holds is refused `NotFound`; an update whose
/// body names another id than the one it updates is refused `Invalid`
/// naming both — it neither renames the row nor writes a second one.
/// Neither records anything.
async fn an_update_of_a_stranger_or_naming_another_id_is_refused_and_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter, &[shipment("suite-ab", 1)]).await;
    let before = w.facts().await;
    match w
        .repo()
        .update_shipment_at(
            "suite-stranger",
            &shipment("suite-stranger", 1),
            at(2),
            &stamp(2),
        )
        .await
    {
        Err(ShippingError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: a stranger: {other:?}"),
    }
    match w
        .repo()
        .update_shipment_at("suite-ab", &shipment("suite-other", 1), at(3), &stamp(3))
        .await
    {
        Err(ShippingError::Invalid(m)) => {
            assert!(
                m.contains("suite-ab") && m.contains("suite-other"),
                "{adapter}: {m}"
            )
        }
        other => panic!("{adapter}: another id: {other:?}"),
    }
    let all = w.repo().all_shipments().await.expect("all");
    assert_eq!(ids(&all), ["suite-ab"], "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(shipment("suite-ab", 1)),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A delete removes the shipment and records `{id, deleted_at}` once;
/// deleting it again is refused `NotFound` and records nothing.
async fn a_delete_removes_the_row_and_records_it_once<W: World>(w: &W, adapter: &str) {
    seed(w, adapter, &[whole("suite-ab"), shipment("suite-B", 1)]).await;
    let before = w.facts().await;
    w.repo()
        .delete_shipment_at("suite-ab", at(5), &stamp(5))
        .await
        .expect("delete");
    assert_eq!(get(w, adapter, "suite-ab").await, None, "{adapter}");
    match w
        .repo()
        .delete_shipment_at("suite-ab", at(6), &stamp(6))
        .await
    {
        Err(ShippingError::NotFound(m)) => assert!(m.contains("suite-ab"), "{adapter}: {m}"),
        other => panic!("{adapter}: a second delete: {other:?}"),
    }
    let all = w.repo().all_shipments().await.expect("all");
    assert_eq!(ids(&all), ["suite-B"], "{adapter}");
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![fact(
            5,
            SHIPMENT_DELETED,
            json!({"id": "suite-ab", "deleted_at": at(5)})
        )],
        "{adapter}"
    );
}

/// A scan records `shipping.tracking.recorded`. An in-transit scan rolls
/// the shipment's status up and stamps `shipped_on` if it had none; a
/// delivered scan rolls it up and stamps `delivered_on` (and `shipped_on`
/// for a shipment that never saw an in-transit scan). Any other status —
/// out-for-delivery, a carrier's own edge — is recorded and moves
/// nothing on the row.
async fn a_scan_rolls_up_in_transit_and_delivered_and_records_once<W: World>(w: &W, adapter: &str) {
    let mut held = shipment("suite-ab", 1);
    held.shipped_on = Some(day(2));
    seed(w, adapter, &[held.clone(), shipment("suite-B", 1)]).await;
    let before = w.facts().await;

    scan(w, "suite-ab", "in-transit", day(3), Some(1), 10)
        .await
        .expect("in-transit");
    let row = get(w, adapter, "suite-ab").await.expect("held");
    assert_eq!(row.status.as_str(), "in-transit", "{adapter}");
    assert_eq!(
        row.shipped_on,
        Some(day(2)),
        "{adapter}: kept, not restamped"
    );
    assert_eq!(row.delivered_on, None, "{adapter}");

    scan(w, "suite-ab", "out-for-delivery", day(4), Some(2), 11)
        .await
        .expect("ofd");
    let row = get(w, adapter, "suite-ab").await.expect("held");
    assert_eq!(row.status.as_str(), "in-transit", "{adapter}: no rollup");

    scan(w, "suite-ab", "delivered", day(5), None, 12)
        .await
        .expect("delivered");
    let row = get(w, adapter, "suite-ab").await.expect("held");
    assert_eq!(row.status.as_str(), "delivered", "{adapter}");
    assert_eq!(row.shipped_on, Some(day(2)), "{adapter}");
    assert_eq!(row.delivered_on, Some(day(5)), "{adapter}");

    // A shipment whose only scan is the delivery.
    scan(w, "suite-B", "delivered", day(6), None, 13)
        .await
        .expect("only delivery");
    let row = get(w, adapter, "suite-B").await.expect("held");
    assert_eq!(row.status.as_str(), "delivered", "{adapter}");
    assert_eq!(row.shipped_on, Some(day(6)), "{adapter}");
    assert_eq!(row.delivered_on, Some(day(6)), "{adapter}");

    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![
            scanned("suite-ab", "in-transit", day(3), Some(1), 10),
            scanned("suite-ab", "out-for-delivery", day(4), Some(2), 11),
            scanned("suite-ab", "delivered", day(5), None, 12),
            scanned("suite-B", "delivered", day(6), None, 13),
        ],
        "{adapter}"
    );
}

/// A scan replayed with the same `(shipment, status, day)` is a full
/// no-op: a stale in-transit redelivered after the delivery does not
/// regress the row, and nothing is recorded.
async fn a_redelivered_scan_changes_nothing_and_records_nothing<W: World>(w: &W, adapter: &str) {
    seed(w, adapter, &[shipment("suite-ab", 1)]).await;
    scan(w, "suite-ab", "in-transit", day(3), Some(1), 10)
        .await
        .expect("in-transit");
    scan(w, "suite-ab", "delivered", day(5), None, 11)
        .await
        .expect("delivered");
    let row = get(w, adapter, "suite-ab").await;
    let before = w.facts().await;
    scan(w, "suite-ab", "in-transit", day(3), Some(9), 12)
        .await
        .expect("replay");
    assert_eq!(get(w, adapter, "suite-ab").await, row, "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A scan for a shipment nobody holds is refused `NotFound` naming it
/// (the HTTP layer skips an out-of-order scan on exactly this), writes
/// nothing and records nothing.
async fn a_scan_for_a_stranger_is_refused_not_found<W: World>(w: &W, adapter: &str) {
    match scan(w, "suite-stranger", "in-transit", day(3), None, 10).await {
        Err(ShippingError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: a stranger: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-stranger").await, None, "{adapter}");
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// The summary for one direction counts each in-flight platform status,
/// the deliveries of the trailing seven days (inclusive) and nothing of
/// the other direction; its preview is in-flight first, newest shipped
/// (or created) first, then delivered, newest delivered first — ties by
/// id in byte order — cut at the caller's limit, which may exceed ten.
async fn the_summary_counts_in_flight_and_the_week_and_previews_in_order<W: World>(
    w: &W,
    adapter: &str,
) {
    let with = |id: &str, status: &str, shipped: Option<u32>, delivered: Option<u32>| Shipment {
        status: ShipmentStatus::new(status),
        shipped_on: shipped.map(day),
        delivered_on: delivered.map(day),
        ..shipment(id, 1)
    };
    let mut fleet = vec![
        with("suite-label", "label-created", None, None),
        with("suite-picked", "picked-up", Some(3), None),
        with("suite-B", "in-transit", Some(5), None),
        with("suite-a-z", "in-transit", Some(5), None),
        with("suite-ab", "in-transit", Some(5), None),
        with("suite-exc", "exception", Some(4), None),
        with("suite-custom", "held-at-customs", Some(2), None),
        with("suite-d-edge", "delivered", Some(2), Some(13)),
        with("suite-d-new", "delivered", Some(2), Some(19)),
        with("suite-d-old", "delivered", Some(2), Some(12)),
    ];
    let mut whole_ab = whole("suite-assets");
    whole_ab.status = ShipmentStatus::new("in-transit");
    whole_ab.shipped_on = Some(day(6));
    fleet.push(whole_ab);
    let mut inbound = with("suite-inbound", "in-transit", Some(9), None);
    inbound.direction = ShipmentDirection::Inbound;
    fleet.push(inbound);
    seed(w, adapter, &fleet).await;

    let today = day(20);
    let summary = w
        .repo()
        .status_summary(ShipmentDirection::Outbound, today, 50)
        .await
        .expect("summary");
    assert_eq!(
        (
            summary.label_created,
            summary.picked_up,
            summary.in_transit,
            summary.exception,
            summary.delivered_7d
        ),
        (1, 1, 4, 1, 2),
        "{adapter}"
    );
    let order: Vec<&str> = summary.recent.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        order,
        [
            "suite-assets",
            "suite-B",
            "suite-a-z",
            "suite-ab",
            "suite-exc",
            "suite-picked",
            "suite-custom",
            "suite-label",
            "suite-d-new",
            "suite-d-edge",
            "suite-d-old",
        ],
        "{adapter}: the limit of 50 is honoured past ten"
    );
    let assets = &summary.recent[0];
    assert_eq!(
        (
            assets.carrier.as_str(),
            assets.asset_id_count,
            assets.account_id.as_deref()
        ),
        ("keg-courier", 3, Some("suite-acct")),
        "{adapter}"
    );
    assert_eq!(
        summary.recent[7].carrier, "",
        "{adapter}: no carrier is empty"
    );

    let cut = w
        .repo()
        .status_summary(ShipmentDirection::Outbound, today, 2)
        .await
        .expect("cut");
    let order: Vec<&str> = cut.recent.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(order, ["suite-assets", "suite-B"], "{adapter}");
}

/// A negative limit or offset is the caller's mistake: refused `Invalid`
/// naming it, never a storage failure.
async fn a_negative_limit_or_offset_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    seed(w, adapter, &fleet()).await;
    for (field, limit, offset) in [("limit", -1, 0), ("offset", 10, -1)] {
        match w.repo().list_shipments(limit, offset, None).await {
            Err(ShippingError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a negative {field}: {other:?}"),
        }
    }
    match w
        .repo()
        .status_summary(ShipmentDirection::Outbound, day(20), -1)
        .await
    {
        Err(ShippingError::Invalid(m)) => assert!(m.contains("recent_limit"), "{adapter}: {m}"),
        other => panic!("{adapter}: a negative recent_limit: {other:?}"),
    }
}

/// A line item of zero or fewer units is refused `Invalid` naming `qty`,
/// on create and on update, and writes and records nothing.
async fn a_line_item_of_no_units_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    seed(w, adapter, &[shipment("suite-ab", 1)]).await;
    let before = w.facts().await;
    for qty in [0, -2] {
        let mut bad = whole("suite-new");
        bad.line_items[1].qty = qty;
        match create(w, &bad, 2).await {
            Err(ShippingError::Invalid(m)) => assert!(m.contains("qty"), "{adapter}: {m}"),
            other => panic!("{adapter}: create qty {qty}: {other:?}"),
        }
        let mut bad = whole("suite-ab");
        bad.line_items[0].qty = qty;
        match w
            .repo()
            .update_shipment_at("suite-ab", &bad, at(3), &stamp(3))
            .await
        {
            Err(ShippingError::Invalid(m)) => assert!(m.contains("qty"), "{adapter}: {m}"),
            other => panic!("{adapter}: update qty {qty}: {other:?}"),
        }
    }
    assert_eq!(get(w, adapter, "suite-new").await, None, "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(shipment("suite-ab", 1)),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A NUL byte cannot be stored in TEXT. A write carrying one — in any
/// text field of the shipment, an asset id, a line item, or a scan's
/// status — is refused `Invalid` naming the field and writes nothing. A
/// read or a delete keyed by one is the miss it is: no stored id can
/// hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    seed(w, adapter, &[shipment("suite-ab", 1)]).await;
    let before = w.facts().await;
    let nul = |f: &dyn Fn(&mut Shipment)| {
        let mut s = whole("suite-nul");
        f(&mut s);
        s
    };
    let shipments = [
        ("id", nul(&|s| s.id = "suite-n\0ul".into())),
        (
            "status",
            nul(&|s| s.status = ShipmentStatus::new("in-\0transit")),
        ),
        ("carrier", nul(&|s| s.carrier = Some(Carrier::new("k\0eg")))),
        (
            "tracking_number",
            nul(&|s| s.tracking_number = Some("T\0".into())),
        ),
        ("origin", nul(&|s| s.origin = "o\0".into())),
        ("destination", nul(&|s| s.destination = "d\0".into())),
        ("asset_ids", nul(&|s| s.asset_ids.push("as\0set".into()))),
        ("sku", nul(&|s| s.line_items[0].sku = "FP\0".into())),
        (
            "description",
            nul(&|s| s.line_items[1].description = Some("d\0".into())),
        ),
        ("po_id", nul(&|s| s.po_id = Some("p\0".into()))),
        ("order_id", nul(&|s| s.order_id = Some("o\0".into()))),
        ("account_id", nul(&|s| s.account_id = Some("a\0".into()))),
    ];
    for (field, bad) in &shipments {
        match create(w, bad, 2).await {
            Err(ShippingError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: create a NUL {field}: {other:?}"),
        }
    }
    for (field, bad) in shipments.iter().skip(1) {
        let bad = Shipment {
            id: "suite-ab".into(),
            ..bad.clone()
        };
        match w
            .repo()
            .update_shipment_at("suite-ab", &bad, at(3), &stamp(3))
            .await
        {
            Err(ShippingError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: update a NUL {field}: {other:?}"),
        }
    }
    match scan(w, "suite-ab", "in-\0transit", day(3), None, 4).await {
        Err(ShippingError::Invalid(m)) => assert!(m.contains("status"), "{adapter}: {m}"),
        other => panic!("{adapter}: scan a NUL status: {other:?}"),
    }
    match scan(w, "suite-a\0b", "in-transit", day(3), None, 4).await {
        Err(ShippingError::NotFound(_)) => {}
        other => panic!("{adapter}: scan a NUL id: {other:?}"),
    }
    match w
        .repo()
        .delete_shipment_at("suite-a\0b", at(5), &stamp(5))
        .await
    {
        Err(ShippingError::NotFound(_)) => {}
        other => panic!("{adapter}: delete a NUL id: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-a\0b").await, None, "{adapter}");
    let (rows, total) = w
        .repo()
        .list_shipments(10, 0, Some("suite-a\0cct"))
        .await
        .expect("a miss");
    assert_eq!((rows.len(), total), (0, 0), "{adapter}");
    let all = w.repo().all_shipments().await.expect("all");
    assert_eq!(ids(&all), ["suite-ab"], "{adapter}: no row");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(shipment("suite-ab", 1)),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}
