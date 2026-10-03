//! The Equipment knowledge base answers the same on both `KbRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the next MODULE port of the
//! census after the finished-products store (backlog be459ab9). Tests
//! elsewhere (`http_writes.rs`, the HTTP unit tests) are answered by
//! `InMemoryKb`, while production is answered by `PgKb`.
//!
//! WHY IT EXISTS. The double was stated by its own unit tests and the
//! Postgres adapter by the audit-log and rebuild tests, never against
//! each other; this file is the one statement of every method of the
//! port, each case holding each adapter to the same stated answer, and
//! every write also judged by the facts it leaves — the double's
//! `recorded_events` and Postgres's `event_outbox`, staged in the
//! write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - LIST ORDER. The double listed models in insertion order and
//!   Postgres in the database's locale; both now list in BYTE order of
//!   SKU (`COLLATE "C"`, backlog 2987fb2d's class). A model's use cases,
//!   failure modes, spare parts and consumables read back the same way:
//!   Postgres sorted them in its locale, the double kept them as sent.
//! - A PART IS ONE ROW. Postgres keeps a part in the shared `parts`
//!   table, so the last model to write a part SKU names it for every
//!   model, and the part outlives a deleted model (the port: `all_parts`
//!   is "independent of any linkage"). The double kept a private copy
//!   per model and derived `all_parts` from the live models, so a part
//!   vanished with its model and two models disagreed on its name. It
//!   now keeps the same one row.
//! - WHAT THE COLUMNS REFUSE. A model year, device class, interval,
//!   skill level or failure frequency outside the table's CHECK, a
//!   currency not three characters long, a use case / failure-mode code
//!   / part named twice, and a NUL byte anywhere, were refused by
//!   Postgres with its constraint or encoding error as `Storage` (a 500)
//!   and STORED by the double. Both now refuse them `BadRequest` naming
//!   the field, before writing (`validate::refuse_unstorable`). A read
//!   keyed by a NUL is the miss it is, not a 500.
//! - AN UPDATE NAMING ANOTHER SKU. `update_model(sku, model)` with
//!   `model.sku != sku` made Postgres upsert the BODY's SKU (creating
//!   or overwriting a second model) while deleting the PATH's
//!   satellites, and made the double hold two rows under one SKU. Both
//!   now refuse it `BadRequest`.
//!
//! NOT STATED HERE, because the port cannot write it: the extras JSON
//! schema (Postgres reads `asset_model_extras_schema`, which no port
//! method writes and the double has no copy of — the suite's category
//! has no schema, the pass-through both answer), the `documents` table
//! behind `documents_for` (no port method writes it; an undocumented
//! entity is stated), and the stock-only stub rows `all_parts` unions in
//! from `inventory_items` (boss-inventory's table).
//!
//! The world: the suite's rows are written THROUGH the port, so each
//! adapter seeds itself the way production does. The migrations may
//! seed rows of their own, so every read is judged over `suite-` SKUs.
//! The fixtures hold case and punctuation pairs (`suite-B`, `suite-a-z`,
//! `suite-ab`) — the only shape that catches the database's locale
//! disagreeing with the double's byte order.

use boss_catalog::events::{MODEL_CREATED, MODEL_DELETED, MODEL_UPDATED};
use boss_catalog::types::*;
use boss_catalog::{InMemoryKb, KbError, KbRepository, PgKb};
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: KbRepository;
    fn repo(&self) -> &Self::R;
    /// Every fact about a `suite-` SKU, as `(kind, payload)`, in the
    /// order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(payload: &Value) -> bool {
    payload["sku"]
        .as_str()
        .is_some_and(|v| v.starts_with("suite-"))
}

struct InMemory(InMemoryKb);

impl World for InMemory {
    type R = InMemoryKb;
    fn repo(&self) -> &InMemoryKb {
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
    repo: PgKb,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgKb;
    fn repo(&self) -> &PgKb {
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
        in_memory => (InMemory(InMemoryKb::new(vec![])), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgKb::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        the_models_list_in_byte_order_of_sku,
        a_model_reads_back_whole_its_satellites_in_byte_order_and_a_stranger_is_none,
        a_create_records_the_model_as_sent_and_a_held_sku_is_refused_conflict,
        an_update_replaces_the_model_and_its_satellites_and_records_its_state,
        an_update_naming_another_sku_is_refused_and_changes_nothing,
        a_delete_removes_the_model_and_records_its_sku_and_instant,
        an_update_or_delete_of_a_stranger_is_refused_not_found,
        a_part_is_one_row_every_model_shares_and_it_outlives_its_models,
        an_undocumented_entity_has_no_documents,
        a_value_the_columns_refuse_is_refused_bad_request_naming_its_field,
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
    EventStamp::new("kb", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("a date")
}

/// A bare model: every satellite empty, every value the columns keep.
/// Its category carries no extras schema, so both adapters pass its
/// extras through.
fn model(sku: &str) -> AssetModel {
    AssetModel {
        sku: sku.into(),
        name: format!("Name of {sku}"),
        manufacturer: "SuiteCo".into(),
        model_year: 2024,
        category: DeviceCategory::new("suite-category"),
        extras: json!({}),
        physical: Physical {
            width_cm: 50.5,
            depth_cm: 40.25,
            height_cm: 100.0,
            weight_kg: 80.125,
            power_requirements: "120V".into(),
        },
        regulatory: Regulatory {
            clearance_id: None,
            clearance_date: None,
            regulator_device_class: 2,
        },
        commerce: Commerce {
            list_price_new_cents: 5_000_000,
            typical_refurb_price_cents: None,
            currency: "USD".into(),
            lead_time_days: None,
            tagline: "A suite device".into(),
            description: "For the suite only".into(),
            use_cases: vec![],
            hero_image: None,
        },
        service: ServiceProfile {
            preventive_maintenance_hours: 2.5,
            preventive_maintenance_interval_months: 6,
            calibration_interval_months: 12,
            required_skill_level: 3,
            depot_required: false,
            common_failure_modes: vec![],
            pm_checklist: vec![],
        },
        spare_parts: vec![],
        consumables: vec![],
        documents: vec![],
        end_of_support: None,
        current_firmware: None,
    }
}

fn failure(code: &str) -> FailureMode {
    FailureMode {
        code: code.into(),
        name: format!("Failure {code}"),
        frequency: 0.25,
        typical_fix: "Replace it".into(),
    }
}

fn spare(part: &str, name: &str) -> SparePart {
    SparePart {
        part_sku: part.into(),
        name: name.into(),
        description: format!("Spare {part}"),
        unit_price_cents: 1_200,
        currency: "USD".into(),
        lead_time_days: 9,
        high_usage: true,
    }
}

fn consumable(part: &str, name: &str) -> Consumable {
    Consumable {
        part_sku: part.into(),
        name: name.into(),
        description: format!("Consumable {part}"),
        unit_price_cents: 300,
        currency: "EUR".into(),
        treatments_per_unit: Some(40),
    }
}

fn document(title: &str) -> Document {
    Document {
        kind: DocumentKind::new("operator-manual"),
        title: title.into(),
        url: format!("https://docs.example/{title}"),
        version: Some("v2".into()),
        published: Some(date(2025, 3, 4)),
        audience: DocumentAudience::new("internal"),
    }
}

/// A model with every field set and every satellite holding a case
/// pair and a punctuation pair, each sent OUT of byte order.
fn whole(sku: &str) -> AssetModel {
    let mut m = model(sku);
    m.extras = json!({"port_count": 24, "bands": ["2.4", "5"], "poe": {"watts": 30.5}});
    m.regulatory.clearance_id = Some("K123456".into());
    m.regulatory.clearance_date = Some(date(2023, 7, 1));
    m.commerce.typical_refurb_price_cents = Some(2_500_000);
    m.commerce.lead_time_days = Some(14);
    m.commerce.hero_image = Some("hero.png".into());
    m.commerce.use_cases = vec![
        "zeta".into(),
        "alphab".into(),
        "Alpha".into(),
        "alpha-z".into(),
    ];
    m.service.depot_required = true;
    m.service.common_failure_modes = vec![failure("fm-b"), failure("fm-a-z"), failure("FM-A")];
    // Kept in the order sent, a repeated item included.
    m.service.pm_checklist = vec!["Wipe".into(), "Calibrate".into(), "Wipe".into()];
    m.spare_parts = vec![
        spare("suite-part-ab", "Spare AB"),
        spare("suite-part-B", "Spare B"),
        spare("suite-part-a-z", "Spare A-Z"),
    ];
    m.consumables = vec![
        consumable("suite-cons-b", "Cons b"),
        consumable("suite-cons-A", "Cons A"),
    ];
    // Kept in the order sent.
    m.documents = vec![document("zz-manual"), document("aa-sheet")];
    m.end_of_support = Some(date(2031, 12, 31));
    m.current_firmware = Some("4.2.1".into());
    m
}

/// `whole(sku)` as the store answers it: use cases, failure modes,
/// spare parts and consumables in BYTE order, the rest as sent.
fn whole_read(sku: &str) -> AssetModel {
    let mut m = whole(sku);
    m.commerce.use_cases = vec![
        "Alpha".into(),
        "alpha-z".into(),
        "alphab".into(),
        "zeta".into(),
    ];
    m.service.common_failure_modes = vec![failure("FM-A"), failure("fm-a-z"), failure("fm-b")];
    m.spare_parts = vec![
        spare("suite-part-B", "Spare B"),
        spare("suite-part-a-z", "Spare A-Z"),
        spare("suite-part-ab", "Spare AB"),
    ];
    m.consumables = vec![
        consumable("suite-cons-A", "Cons A"),
        consumable("suite-cons-b", "Cons b"),
    ];
    m
}

async fn create<W: World>(w: &W, adapter: &str, m: &AssetModel, secs: i64) {
    let sku = w
        .repo()
        .create_model_at(m, at(secs), &stamp(secs))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: create {}: {e:?}", m.sku));
    assert_eq!(sku, m.sku, "{adapter}: create answers the SKU");
}

async fn suite_models<W: World>(w: &W, adapter: &str) -> Vec<AssetModel> {
    w.repo()
        .all_models()
        .await
        .unwrap_or_else(|e| panic!("{adapter}: all_models: {e:?}"))
        .into_iter()
        .filter(|m| m.sku.starts_with("suite-"))
        .collect()
}

async fn suite_skus<W: World>(w: &W, adapter: &str) -> Vec<String> {
    suite_models(w, adapter)
        .await
        .into_iter()
        .map(|m| m.sku)
        .collect()
}

async fn suite_parts<W: World>(w: &W, adapter: &str) -> Vec<PartCatalogRow> {
    w.repo()
        .all_parts()
        .await
        .unwrap_or_else(|e| panic!("{adapter}: all_parts: {e:?}"))
        .into_iter()
        .filter(|p| p.part_sku.starts_with("suite-"))
        .collect()
}

async fn get<W: World>(w: &W, adapter: &str, sku: &str) -> Option<AssetModel> {
    w.repo()
        .model_by_sku(sku)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: model_by_sku {sku:?}: {e:?}"))
}

fn part_row(
    part: &str,
    name: &str,
    desc: &str,
    cents: i64,
    cur: &str,
    lead: u16,
) -> PartCatalogRow {
    PartCatalogRow {
        part_sku: part.into(),
        name: name.into(),
        description: desc.into(),
        unit_price_cents: cents,
        currency: cur.into(),
        lead_time_days: lead,
    }
}

// ----- cases ---------------------------------------------------------------

/// The models list in BYTE order of SKU — `suite-B` < `suite-a-z` <
/// `suite-ab`, which a locale collation orders the other way round —
/// whatever order they were created in.
async fn the_models_list_in_byte_order_of_sku<W: World>(w: &W, adapter: &str) {
    for (i, sku) in ["suite-ab", "suite-B", "suite-a-z"].iter().enumerate() {
        create(w, adapter, &model(sku), i as i64).await;
    }
    assert_eq!(
        suite_skus(w, adapter).await,
        ["suite-B", "suite-a-z", "suite-ab"],
        "{adapter}"
    );
}

/// A model reads back whole by SKU and inside the list: its use cases,
/// failure modes, spare parts and consumables in BYTE order, its
/// checklist and documents in the order sent. A SKU nobody created is
/// `None`, not an error.
async fn a_model_reads_back_whole_its_satellites_in_byte_order_and_a_stranger_is_none<W: World>(
    w: &W,
    adapter: &str,
) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    create(w, adapter, &model("suite-B"), 2).await;
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(whole_read("suite-ab")),
        "{adapter}"
    );
    assert_eq!(
        suite_models(w, adapter).await,
        vec![model("suite-B"), whole_read("suite-ab")],
        "{adapter}"
    );
    assert_eq!(get(w, adapter, "suite-stranger").await, None, "{adapter}");
}

/// A create records `kb.model.created` carrying the model AS SENT. A
/// second create on a held SKU is refused `Conflict`, changes nothing
/// and records nothing.
async fn a_create_records_the_model_as_sent_and_a_held_sku_is_refused_conflict<W: World>(
    w: &W,
    adapter: &str,
) {
    let sent = whole("suite-ab");
    create(w, adapter, &sent, 1).await;
    let mut again = model("suite-ab");
    again.name = "Second".into();
    match w.repo().create_model_at(&again, at(2), &stamp(2)).await {
        Err(KbError::Conflict(m)) => assert!(m.contains("suite-ab"), "{adapter}: {m}"),
        other => panic!("{adapter}: a held SKU: {other:?}"),
    }
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(whole_read("suite-ab")),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![fact(
            1,
            MODEL_CREATED,
            serde_json::to_value(&sent).expect("a model serialises")
        )],
        "{adapter}"
    );
}

/// An update replaces every field and every satellite list, and
/// records `kb.model.updated` carrying the model as sent.
async fn an_update_replaces_the_model_and_its_satellites_and_records_its_state<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = whole("suite-ab");
    create(w, adapter, &first, 1).await;
    let mut second = model("suite-ab");
    second.name = "Renamed".into();
    second.model_year = 2030;
    second.extras = json!({"port_count": 48});
    second.commerce.use_cases = vec!["only".into()];
    second.service.pm_checklist = vec!["Inspect".into()];
    second.spare_parts = vec![spare("suite-part-new", "New")];
    second.documents = vec![document("mm-guide")];
    w.repo()
        .update_model_at("suite-ab", &second, at(2), &stamp(2))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: update: {e:?}"));
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(second.clone()),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![
            fact(1, MODEL_CREATED, serde_json::to_value(&first).unwrap()),
            fact(2, MODEL_UPDATED, serde_json::to_value(&second).unwrap()),
        ],
        "{adapter}"
    );
}

/// `update_model(sku, model)` replaces the model AT `sku`; a body naming
/// another SKU is refused `BadRequest`, changes neither model and
/// records nothing.
async fn an_update_naming_another_sku_is_refused_and_changes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    create(w, adapter, &model("suite-B"), 2).await;
    let before = w.facts().await;
    for body in ["suite-B", "suite-new"] {
        let mut other = model(body);
        other.name = "Moved".into();
        match w
            .repo()
            .update_model_at("suite-ab", &other, at(3), &stamp(3))
            .await
        {
            Err(KbError::BadRequest(m)) => assert!(m.contains("sku"), "{adapter}: {m}"),
            other => panic!("{adapter}: a body naming {body}: {other:?}"),
        }
    }
    assert_eq!(
        suite_models(w, adapter).await,
        vec![model("suite-B"), whole_read("suite-ab")],
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A delete removes the model (and its satellites: the SKU may be
/// created afresh) and records `kb.model.deleted` carrying the SKU and
/// the delete's instant.
async fn a_delete_removes_the_model_and_records_its_sku_and_instant<W: World>(
    w: &W,
    adapter: &str,
) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    create(w, adapter, &model("suite-B"), 2).await;
    w.repo()
        .delete_model_at("suite-ab", at(3), &stamp(3))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: delete: {e:?}"));
    assert_eq!(get(w, adapter, "suite-ab").await, None, "{adapter}");
    assert_eq!(suite_skus(w, adapter).await, ["suite-B"], "{adapter}");
    let recorded = w.facts().await;
    assert_eq!(
        recorded.last(),
        Some(&fact(
            3,
            MODEL_DELETED,
            json!({"sku": "suite-ab", "deleted_at": at(3)})
        )),
        "{adapter}"
    );
    // Created afresh, it carries none of the deleted model's satellites.
    create(w, adapter, &model("suite-ab"), 4).await;
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(model("suite-ab")),
        "{adapter}"
    );
}

/// An update or a delete of a SKU nobody created is refused `NotFound`
/// naming it, writes nothing and records nothing.
async fn an_update_or_delete_of_a_stranger_is_refused_not_found<W: World>(w: &W, adapter: &str) {
    match w
        .repo()
        .update_model_at("suite-stranger", &model("suite-stranger"), at(1), &stamp(1))
        .await
    {
        Err(KbError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: update: {other:?}"),
    }
    match w
        .repo()
        .delete_model_at("suite-stranger", at(1), &stamp(1))
        .await
    {
        Err(KbError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: delete: {other:?}"),
    }
    assert_eq!(
        suite_skus(w, adapter).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// A part is ONE row, keyed by its part SKU, that every model naming it
/// shares: the last write names it for every model. A spare part writes
/// every field; a consumable writes its name, description, price and
/// currency, takes a lead time of 7 days when it is the first to write
/// the part and leaves the lead time alone otherwise. `all_parts` lists
/// every row in BYTE order of part SKU, independent of any model, so a
/// part outlives the models that named it.
async fn a_part_is_one_row_every_model_shares_and_it_outlives_its_models<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut a = model("suite-a");
    a.spare_parts = vec![spare("suite-p-ab", "First"), spare("suite-p-B", "Bee")];
    a.consumables = vec![consumable("suite-p-a-z", "Fresh")];
    create(w, adapter, &a, 1).await;
    let mut b = model("suite-b");
    b.spare_parts = vec![SparePart {
        lead_time_days: 21,
        high_usage: false,
        ..spare("suite-p-ab", "Second")
    }];
    // A consumable on a part a spare already wrote: the name moves, the
    // lead time stays the spare's.
    b.consumables = vec![consumable("suite-p-B", "Bee as consumable")];
    create(w, adapter, &b, 2).await;

    let read_a = get(w, adapter, "suite-a").await.expect("a");
    assert_eq!(
        read_a.spare_parts,
        vec![
            SparePart {
                name: "Bee as consumable".into(),
                description: "Consumable suite-p-B".into(),
                unit_price_cents: 300,
                currency: "EUR".into(),
                ..spare("suite-p-B", "")
            },
            SparePart {
                lead_time_days: 21,
                ..spare("suite-p-ab", "Second")
            },
        ],
        "{adapter}: the last write names the part for every model"
    );
    assert_eq!(
        read_a.consumables,
        vec![consumable("suite-p-a-z", "Fresh")],
        "{adapter}"
    );
    let want = vec![
        part_row(
            "suite-p-B",
            "Bee as consumable",
            "Consumable suite-p-B",
            300,
            "EUR",
            9,
        ),
        part_row(
            "suite-p-a-z",
            "Fresh",
            "Consumable suite-p-a-z",
            300,
            "EUR",
            7,
        ),
        part_row("suite-p-ab", "Second", "Spare suite-p-ab", 1_200, "USD", 21),
    ];
    assert_eq!(suite_parts(w, adapter).await, want, "{adapter}");

    for (i, sku) in ["suite-a", "suite-b"].iter().enumerate() {
        w.repo()
            .delete_model_at(sku, at(3 + i as i64), &stamp(3 + i as i64))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: delete {sku}: {e:?}"));
    }
    assert_eq!(
        suite_parts(w, adapter).await,
        want,
        "{adapter}: parts outlive their models"
    );
}

/// An entity nobody documented answers no documents, not an error —
/// and so does a key no stored entity can hold.
async fn an_undocumented_entity_has_no_documents<W: World>(w: &W, adapter: &str) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    for (kind, id) in [("asset_model", "suite-ab"), ("part", "suite-p\0")] {
        assert_eq!(
            w.repo()
                .documents_for(kind, id)
                .await
                .unwrap_or_else(|e| panic!("{adapter}: documents_for {id:?}: {e:?}")),
            vec![],
            "{adapter}"
        );
    }
}

/// A value the table's columns cannot hold — a CHECK it fails, or a
/// satellite key named twice — is refused `BadRequest` naming the
/// field, on create and on update alike, and nothing is written or
/// recorded.
async fn a_value_the_columns_refuse_is_refused_bad_request_naming_its_field<W: World>(
    w: &W,
    adapter: &str,
) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    let before = w.facts().await;
    let bad = |f: &dyn Fn(&mut AssetModel)| {
        let mut m = model("suite-ab");
        f(&mut m);
        m
    };
    let cases = [
        ("model_year", bad(&|m| m.model_year = 1979)),
        ("model_year", bad(&|m| m.model_year = 2101)),
        ("currency", bad(&|m| m.commerce.currency = "US".into())),
        ("currency", bad(&|m| m.commerce.currency = "USDX".into())),
        (
            "regulator_device_class",
            bad(&|m| m.regulatory.regulator_device_class = 0),
        ),
        (
            "regulator_device_class",
            bad(&|m| m.regulatory.regulator_device_class = 4),
        ),
        (
            "preventive_maintenance_interval_months",
            bad(&|m| m.service.preventive_maintenance_interval_months = 0),
        ),
        (
            "calibration_interval_months",
            bad(&|m| m.service.calibration_interval_months = 0),
        ),
        (
            "required_skill_level",
            bad(&|m| m.service.required_skill_level = 0),
        ),
        (
            "required_skill_level",
            bad(&|m| m.service.required_skill_level = 6),
        ),
        (
            "frequency",
            bad(&|m| {
                m.service.common_failure_modes = vec![FailureMode {
                    frequency: 1.5,
                    ..failure("fm")
                }]
            }),
        ),
        (
            "frequency",
            bad(&|m| {
                m.service.common_failure_modes = vec![FailureMode {
                    frequency: f32::NAN,
                    ..failure("fm")
                }]
            }),
        ),
        (
            "use_cases",
            bad(&|m| m.commerce.use_cases = vec!["x".into(), "x".into()]),
        ),
        (
            "common_failure_modes",
            bad(&|m| m.service.common_failure_modes = vec![failure("fm"), failure("fm")]),
        ),
        (
            "spare_parts",
            bad(&|m| m.spare_parts = vec![spare("suite-p", "a"), spare("suite-p", "b")]),
        ),
        (
            "consumables",
            bad(&|m| m.consumables = vec![consumable("suite-p", "a"), consumable("suite-p", "b")]),
        ),
        (
            "currency",
            bad(&|m| {
                m.spare_parts = vec![SparePart {
                    currency: "EU".into(),
                    ..spare("suite-p", "a")
                }]
            }),
        ),
        (
            "currency",
            bad(&|m| {
                m.consumables = vec![Consumable {
                    currency: "EURO".into(),
                    ..consumable("suite-p", "a")
                }]
            }),
        ),
        // The column widths (backlog e9ff7ccb): SMALLINT and INTEGER
        // columns bound from u16 / u32 / a list position.
        (
            "lead_time_days",
            bad(&|m| m.commerce.lead_time_days = Some(32_768)),
        ),
        (
            "lead_time_days",
            bad(&|m| {
                m.spare_parts = vec![SparePart {
                    lead_time_days: 40_000,
                    ..spare("suite-p", "a")
                }]
            }),
        ),
        (
            "pm_checklist",
            bad(&|m| m.service.pm_checklist = vec!["step".into(); 32_768]),
        ),
        (
            "treatments_per_unit",
            bad(&|m| {
                m.consumables = vec![Consumable {
                    treatments_per_unit: Some(u32::MAX),
                    ..consumable("suite-p", "a")
                }]
            }),
        ),
    ];
    for (field, m) in &cases {
        let mut fresh = m.clone();
        fresh.sku = "suite-new".into();
        match w.repo().create_model_at(&fresh, at(5), &stamp(5)).await {
            Err(KbError::BadRequest(msg)) => assert!(msg.contains(field), "{adapter}: {msg}"),
            other => panic!("{adapter}: create with a bad {field}: {other:?}"),
        }
        match w
            .repo()
            .update_model_at("suite-ab", m, at(5), &stamp(5))
            .await
        {
            Err(KbError::BadRequest(msg)) => assert!(msg.contains(field), "{adapter}: {msg}"),
            other => panic!("{adapter}: update with a bad {field}: {other:?}"),
        }
    }
    assert_eq!(suite_skus(w, adapter).await, ["suite-ab"], "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(whole_read("suite-ab")),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}");
}

/// A NUL byte cannot be stored in TEXT or JSONB. A write carrying one —
/// in any text field, any satellite, or anywhere inside the extras — is
/// refused `BadRequest` naming the field and writes nothing. A read or
/// a delete keyed by one is the miss it is: no stored SKU can hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    create(w, adapter, &whole("suite-ab"), 1).await;
    let before = w.facts().await;
    let nul = |f: &dyn Fn(&mut AssetModel)| {
        let mut m = whole("suite-ab");
        f(&mut m);
        m
    };
    let cases = [
        ("name", nul(&|m| m.name = "N\0".into())),
        ("manufacturer", nul(&|m| m.manufacturer = "M\0".into())),
        (
            "category",
            nul(&|m| m.category = DeviceCategory::new("c\0")),
        ),
        ("extras", nul(&|m| m.extras = json!({"k": ["a\0b"]}))),
        ("extras", nul(&|m| m.extras = json!({"k": {"a\0b": 1}}))),
        (
            "power_requirements",
            nul(&|m| m.physical.power_requirements = "1\0".into()),
        ),
        (
            "clearance_id",
            nul(&|m| m.regulatory.clearance_id = Some("K\0".into())),
        ),
        ("currency", nul(&|m| m.commerce.currency = "U\0D".into())),
        ("tagline", nul(&|m| m.commerce.tagline = "t\0".into())),
        (
            "description",
            nul(&|m| m.commerce.description = "d\0".into()),
        ),
        (
            "use_cases",
            nul(&|m| m.commerce.use_cases = vec!["u\0".into()]),
        ),
        (
            "hero_image",
            nul(&|m| m.commerce.hero_image = Some("h\0".into())),
        ),
        (
            "common_failure_modes",
            nul(&|m| m.service.common_failure_modes = vec![failure("f\0")]),
        ),
        (
            "common_failure_modes",
            nul(&|m| {
                m.service.common_failure_modes = vec![FailureMode {
                    typical_fix: "x\0".into(),
                    ..failure("fm")
                }]
            }),
        ),
        (
            "pm_checklist",
            nul(&|m| m.service.pm_checklist = vec!["p\0".into()]),
        ),
        (
            "spare_parts",
            nul(&|m| m.spare_parts = vec![spare("suite-p\0", "a")]),
        ),
        (
            "spare_parts",
            nul(&|m| m.spare_parts = vec![spare("suite-p", "a\0")]),
        ),
        (
            "consumables",
            nul(&|m| {
                m.consumables = vec![Consumable {
                    description: "d\0".into(),
                    ..consumable("suite-p", "a")
                }]
            }),
        ),
        ("documents", nul(&|m| m.documents = vec![document("t\0")])),
        (
            "documents",
            nul(&|m| {
                m.documents = vec![Document {
                    audience: DocumentAudience::new("in\0"),
                    ..document("t")
                }]
            }),
        ),
        (
            "current_firmware",
            nul(&|m| m.current_firmware = Some("4\0".into())),
        ),
    ];
    for (field, m) in &cases {
        match w
            .repo()
            .update_model_at("suite-ab", m, at(9), &stamp(9))
            .await
        {
            Err(KbError::BadRequest(msg)) => assert!(msg.contains(field), "{adapter}: {msg}"),
            other => panic!("{adapter}: update with a NUL {field}: {other:?}"),
        }
        let mut fresh = m.clone();
        fresh.sku = "suite-new".into();
        match w.repo().create_model_at(&fresh, at(9), &stamp(9)).await {
            Err(KbError::BadRequest(msg)) => assert!(msg.contains(field), "{adapter}: {msg}"),
            other => panic!("{adapter}: create with a NUL {field}: {other:?}"),
        }
    }
    let mut keyed = model("suite-n\0ul");
    match w.repo().create_model_at(&keyed, at(9), &stamp(9)).await {
        Err(KbError::BadRequest(msg)) => assert!(msg.contains("sku"), "{adapter}: {msg}"),
        other => panic!("{adapter}: create with a NUL sku: {other:?}"),
    }
    keyed.sku = "suite-n\0ul".into();
    match w
        .repo()
        .update_model_at("suite-n\0ul", &keyed, at(9), &stamp(9))
        .await
    {
        Err(KbError::BadRequest(msg)) => assert!(msg.contains("sku"), "{adapter}: {msg}"),
        other => panic!("{adapter}: update keyed by a NUL: {other:?}"),
    }
    match w
        .repo()
        .delete_model_at("suite-n\0ul", at(9), &stamp(9))
        .await
    {
        Err(KbError::NotFound(_)) => {}
        other => panic!("{adapter}: delete keyed by a NUL: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-n\0ul").await, None, "{adapter}");
    assert_eq!(suite_skus(w, adapter).await, ["suite-ab"], "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-ab").await,
        Some(whole_read("suite-ab")),
        "{adapter}"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}
