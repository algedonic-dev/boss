//! The asset store answers the same on both `AssetsRepository` adapters
//! — reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), the smallest MODULE port left in
//! the census by adapter lines (backlog be459ab9). The HTTP tests here
//! are answered by `InMemoryAssets`, while production is answered by
//! `PgAssets`.
//!
//! WHY IT EXISTS. The port was stated for Postgres alone
//! (`projection_persistence.rs`) and for the double alone (its unit
//! tests); this file is the one statement of every method of the port,
//! each case holding each adapter to the same stated answer. The store
//! keeps no outbox: its write IS the fact, a row of `asset_events`, so
//! every write is judged by the log it leaves (`events_for`) and by the
//! projection every reader is answered from.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - LOG ORDER. Postgres read one day's events in the locale's order of
//!   id, the double (and the projection) in byte order; Postgres now
//!   orders `id COLLATE "C"`, and its asset-id page the same.
//! - A SAME-DAY EVENT ARRIVING LATE. Postgres re-projected only when an
//!   event's DAY was earlier than the projection's last, so a same-day
//!   event sorting before one already stored was applied out of order:
//!   `current_state` (folded from the log) and `list_assets` (read off
//!   the projection table) answered two different phases for one asset.
//!   It now re-projects whenever a stored event sorts after the new one.
//! - A BATCH CARRYING ONE ID TWICE. Postgres stored the first copy,
//!   counted the second a duplicate, and then applied BOTH to the
//!   projection. It now applies the copy it stored.
//! - LIST TIES. `list_assets` ordered by day alone, a common tie, which
//!   each adapter broke its own way (heap order, HashMap order); both
//!   now break it in byte order of asset id. `sku_counts` the same, by
//!   sku.
//! - THE SUMMARY. The double answered `open_tickets_total` and
//!   `warranty_expiring_30d` as 0 always; Postgres failed the whole
//!   summary on an unidentified asset (its NULL sku would not decode).
//! - OPEN TICKETS PER ACCOUNT. The double counted every open EVENT not
//!   closed anywhere — a decommissioned asset's and a redelivered open's
//!   included; it now counts the open tickets of the live assets held.
//! - AN UNKNOWN MODEL. Postgres refused one with its foreign-key error
//!   as `Storage` (a 500 at a batch, a 409 at a single event), the
//!   double stored it; both now refuse `UnknownModel` naming the sku,
//!   storing nothing, and a batch is all or nothing on both.
//! - A NUL BYTE and A NEGATIVE PAGE. Postgres refused each as `Storage`
//!   and failed a read keyed by a NUL; the double stored the byte and
//!   wrapped the page. Both now refuse `Invalid` naming the field, and
//!   answer a NUL-keyed read as the miss it is.
//!
//! The world: every row is written THROUGH the port. No migration seeds
//! an asset, so every read is judged whole; the catalog models are
//! declared to each adapter alike (Postgres rows, the double's
//! `with_models`). The ids hold a case pair and a punctuation pair
//! (`suite-B`, `suite-a-z`, `suite-ab`) — the only shape that catches
//! the database's locale disagreeing with byte order.

use boss_assets::port::{AssetsError, AssetsRepository, BatchAppendStats};
use boss_assets::types::{
    AssetCondition, AssetCurrentState, AssetEvent, AssetEventId, AssetEventKind, AssetId,
    AssetLifecyclePhase, AssetsSummary, IntakeSource, PhaseRollup, SkuRollup, WarrantyCoverage,
};
use boss_assets::{InMemoryAssets, PgAssets};
use boss_core::actor::ActorId;
use chrono::NaiveDate;
use sqlx::PgPool;

/// The catalog both adapters are given.
const MODELS: [&str; 2] = ["suite-model-a", "suite-model-b"];

trait World {
    type R: AssetsRepository;
    fn repo(&self) -> &Self::R;
}

struct InMemory(InMemoryAssets);

impl World for InMemory {
    type R = InMemoryAssets;
    fn repo(&self) -> &InMemoryAssets {
        &self.0
    }
}

struct Postgres(PgAssets);

impl World for Postgres {
    type R = PgAssets;
    fn repo(&self) -> &PgAssets {
        &self.0
    }
}

async fn seed_model(pool: &PgPool, sku: &str) {
    sqlx::query(
        "INSERT INTO asset_models ( \
            sku, name, manufacturer, model_year, category, \
            regulator_device_class, \
            preventive_maintenance_interval_months, preventive_maintenance_hours, calibration_interval_months, \
            required_skill_level, depot_required, \
            list_price_new_cents, tagline, description, \
            width_cm, depth_cm, height_cm, weight_kg, power_requirements \
         ) VALUES ($1, $2, 'SuiteCo', 2024, 'suite', 2, 6, 2.0, 12, 3, false, \
                   5000000, 'Suite model', 'For the adapters-agree suite', \
                   50.0, 50.0, 100.0, 80.0, '120V')",
    )
    .bind(sku)
    .bind(format!("{sku} model"))
    .execute(pool)
    .await
    .expect("insert a suite model");
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryAssets::with_models(MODELS)), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            for sku in MODELS {
                seed_model(&db.pool, sku).await;
            }
            (Postgres(PgAssets::new(db.pool.clone())), db)
        },
    }
    cases {
        an_appended_event_reads_back_whole_and_a_stranger_is_empty,
        a_duplicate_id_is_refused_and_the_first_copy_stays,
        the_log_reads_by_day_then_byte_order_of_id,
        a_same_day_event_arriving_late_projects_where_the_log_puts_it,
        a_batch_counts_duplicates_within_itself_and_applies_each_stored_copy_once,
        asset_ids_page_in_byte_order_and_count_every_asset,
        assets_list_newest_first_ties_by_id_and_scope_to_an_account_holder,
        a_negative_page_is_refused_invalid,
        open_tickets_count_on_the_live_assets_an_account_holds,
        active_assets_of_a_model_skip_the_decommissioned_and_the_unidentified,
        the_summary_counts_phases_models_open_tickets_and_warranties,
        an_unknown_model_is_refused_and_nothing_is_stored,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
        one_job_open_on_two_assets_is_an_open_ticket_on_each,
        an_actor_whose_text_form_reads_back_as_another_is_refused,
    }
}

// ----- fixtures ------------------------------------------------------------

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, d).expect("a March day")
}

fn ev(id: &str, asset: &str, d: u32, kind: AssetEventKind) -> AssetEvent {
    AssetEvent {
        id: AssetEventId::new(id),
        asset_id: AssetId::new(asset),
        ts: day(d),
        actor_id: ActorId::Human("emp-suite".into()),
        kind,
    }
}

fn received(sku: Option<&str>) -> AssetEventKind {
    AssetEventKind::Received {
        sku: sku.map(Into::into),
        source: IntakeSource::new("oem-new"),
        oem_serial: Some("OEM-1".into()),
    }
}

fn installed(kind: &str, id: &str) -> AssetEventKind {
    AssetEventKind::Installed {
        holder_kind: kind.into(),
        holder_id: id.into(),
    }
}

fn opened(job: &str) -> AssetEventKind {
    AssetEventKind::ServiceJobOpened {
        job_id: job.into(),
        summary: format!("Summary of {job}"),
    }
}

fn closed(job: &str) -> AssetEventKind {
    AssetEventKind::ServiceJobClosed {
        job_id: job.into(),
        turnaround_days: 3,
    }
}

fn decommissioned() -> AssetEventKind {
    AssetEventKind::Decommissioned {
        reason: "end of life".into(),
    }
}

async fn append<W: World>(w: &W, adapter: &str, e: AssetEvent) {
    let id = e.id.0.clone();
    w.repo()
        .append(e)
        .await
        .unwrap_or_else(|err| panic!("{adapter}: append {id}: {err:?}"));
}

async fn log<W: World>(w: &W, asset: &str) -> Vec<AssetEvent> {
    w.repo()
        .events_for(&AssetId::new(asset))
        .await
        .expect("events_for")
}

fn ids(events: &[AssetEvent]) -> Vec<&str> {
    events.iter().map(|e| e.id.0.as_str()).collect()
}

async fn state<W: World>(w: &W, asset: &str) -> AssetCurrentState {
    w.repo()
        .current_state(&AssetId::new(asset))
        .await
        .expect("current_state")
        .expect("a known asset")
}

/// The whole list, unpaged.
async fn listed<W: World>(w: &W, account: Option<&str>) -> (Vec<AssetCurrentState>, i64) {
    w.repo()
        .list_assets(100, 0, account)
        .await
        .expect("list_assets")
}

/// Four assets: `suite-ab` installed at `acct-1`, out for service on one
/// open ticket (a second opened and closed); `suite-B` received, unidentified; `suite-a-z`
/// installed at a LOCATION whose id is `acct-1` with warranty; and
/// `suite-old`, held by `acct-1`, decommissioned with a ticket still
/// open.
async fn seed_fleet<W: World>(w: &W, adapter: &str) {
    for e in [
        ev("e-ab-1", "suite-ab", 1, received(Some("suite-model-a"))),
        ev("e-ab-2", "suite-ab", 2, installed("account", "acct-1")),
        ev("e-ab-3", "suite-ab", 3, opened("job-1")),
        ev("e-ab-4", "suite-ab", 4, opened("job-2")),
        ev("e-ab-5", "suite-ab", 5, closed("job-2")),
        ev("e-B-1", "suite-B", 5, received(None)),
        ev("e-az-1", "suite-a-z", 1, received(Some("suite-model-b"))),
        ev("e-az-2", "suite-a-z", 2, installed("location", "acct-1")),
        ev(
            "e-az-3",
            "suite-a-z",
            5,
            AssetEventKind::WarrantyStarted {
                through: day(20),
                coverage: WarrantyCoverage::new("standard"),
            },
        ),
        ev("e-old-1", "suite-old", 1, received(Some("suite-model-a"))),
        ev("e-old-2", "suite-old", 2, installed("account", "acct-1")),
        ev("e-old-3", "suite-old", 3, opened("job-3")),
        ev("e-old-4", "suite-old", 4, decommissioned()),
    ] {
        append(w, adapter, e).await;
    }
}

// ----- cases ---------------------------------------------------------------

/// An event reads back whole — envelope, actor, payload — and the asset
/// projects from it; an asset nobody wrote is an empty log and `None`.
async fn an_appended_event_reads_back_whole_and_a_stranger_is_empty<W: World>(
    w: &W,
    adapter: &str,
) {
    let e = ev("e-1", "suite-ab", 1, received(Some("suite-model-a")));
    append(w, adapter, e.clone()).await;
    assert_eq!(log(w, "suite-ab").await, vec![e], "{adapter}");
    assert_eq!(
        state(w, "suite-ab").await,
        AssetCurrentState {
            asset_id: AssetId::new("suite-ab"),
            sku: Some("suite-model-a".into()),
            phase: AssetLifecyclePhase::new(AssetLifecyclePhase::RECEIVED),
            holder_kind: None,
            holder_id: None,
            warranty_through: None,
            open_ticket_count: 0,
            first_seen: day(1),
            last_event_at: day(1),
            oem_serial: Some("OEM-1".into()),
        },
        "{adapter}"
    );
    let stranger = AssetId::new("suite-stranger");
    assert_eq!(
        w.repo().events_for(&stranger).await.expect("events_for"),
        vec![],
        "{adapter}"
    );
    assert_eq!(
        w.repo().current_state(&stranger).await.expect("state"),
        None,
        "{adapter}"
    );
}

/// A second event under a held id is refused `DuplicateEvent` — even one
/// naming a model the catalog lacks, since a redelivery is a duplicate
/// whatever it names — and the first copy stays.
async fn a_duplicate_id_is_refused_and_the_first_copy_stays<W: World>(w: &W, adapter: &str) {
    let first = ev("e-1", "suite-ab", 1, received(Some("suite-model-a")));
    append(w, adapter, first.clone()).await;
    for again in [
        ev("e-1", "suite-B", 2, received(Some("suite-model-b"))),
        ev("e-1", "suite-ab", 2, received(Some("suite-no-such-model"))),
    ] {
        let err = w.repo().append(again).await.expect_err("a duplicate");
        assert!(
            matches!(&err, AssetsError::DuplicateEvent(id) if id == "e-1"),
            "{adapter}: {err:?}"
        );
    }
    assert_eq!(log(w, "suite-ab").await, vec![first], "{adapter}");
    assert_eq!(log(w, "suite-B").await, vec![], "{adapter}");
}

/// The log reads by day, one day's events in BYTE order of id —
/// `suite-B` < `suite-a-z` < `suite-ab`, which a locale collation orders
/// the other way round — whatever order they were appended in.
async fn the_log_reads_by_day_then_byte_order_of_id<W: World>(w: &W, adapter: &str) {
    for (id, d) in [
        ("suite-ab", 2),
        ("suite-B", 2),
        ("suite-z", 1),
        ("suite-a-z", 2),
    ] {
        append(
            w,
            adapter,
            ev(
                id,
                "suite-asset",
                d,
                AssetEventKind::PutAway { bin: id.into() },
            ),
        )
        .await;
    }
    assert_eq!(
        ids(&log(w, "suite-asset").await),
        ["suite-z", "suite-B", "suite-a-z", "suite-ab"],
        "{adapter}"
    );
}

/// A same-day event that sorts BEFORE one already stored is folded where
/// the log puts it: the stored `Installed` (`e-z`) follows the late
/// `Shipped` (`e-m`), so the asset is installed at `acct-1` — and the
/// list, read off the projection, answers what `current_state` folds.
async fn a_same_day_event_arriving_late_projects_where_the_log_puts_it<W: World>(
    w: &W,
    adapter: &str,
) {
    append(
        w,
        adapter,
        ev("e-a", "suite-ab", 1, received(Some("suite-model-a"))),
    )
    .await;
    append(
        w,
        adapter,
        ev("e-z", "suite-ab", 5, installed("account", "acct-1")),
    )
    .await;
    append(
        w,
        adapter,
        ev(
            "e-m",
            "suite-ab",
            5,
            AssetEventKind::Shipped {
                holder_kind: "account".into(),
                holder_id: "acct-2".into(),
            },
        ),
    )
    .await;
    let folded = state(w, "suite-ab").await;
    assert_eq!(
        folded.phase.as_str(),
        AssetLifecyclePhase::INSTALLED,
        "{adapter}"
    );
    assert_eq!(folded.holder_id.as_deref(), Some("acct-1"), "{adapter}");
    assert_eq!(listed(w, None).await, (vec![folded], 1), "{adapter}");
}

/// A batch counts a held id AND a second copy of an id within itself as
/// duplicates; the first copy is the one stored and the one the
/// projection folds.
async fn a_batch_counts_duplicates_within_itself_and_applies_each_stored_copy_once<W: World>(
    w: &W,
    adapter: &str,
) {
    let held = ev("e-1", "suite-ab", 1, received(Some("suite-model-a")));
    append(w, adapter, held.clone()).await;
    let ship = ev("e-2", "suite-ab", 2, installed("account", "acct-1"));
    let stats = w
        .repo()
        .batch_append(vec![
            ev("e-1", "suite-ab", 1, received(Some("suite-model-b"))),
            ship.clone(),
            ev("e-2", "suite-ab", 3, installed("account", "acct-2")),
        ])
        .await
        .expect("batch");
    assert_eq!(
        stats,
        BatchAppendStats {
            inserted: 1,
            duplicates: 2
        },
        "{adapter}"
    );
    assert_eq!(log(w, "suite-ab").await, vec![held, ship], "{adapter}");
    let folded = state(w, "suite-ab").await;
    assert_eq!(folded.holder_id.as_deref(), Some("acct-1"), "{adapter}");
    assert_eq!(folded.last_event_at, day(2), "{adapter}");
    assert_eq!(listed(w, None).await, (vec![folded], 1), "{adapter}");
}

/// Asset ids page in BYTE order with the count of every asset; every id
/// is in `all_asset_ids`, whose order the port leaves open.
async fn asset_ids_page_in_byte_order_and_count_every_asset<W: World>(w: &W, adapter: &str) {
    seed_fleet(w, adapter).await;
    let page = |limit, offset| async move {
        let (ids, total) = w
            .repo()
            .list_asset_ids(limit, offset)
            .await
            .expect("list_asset_ids");
        (ids.into_iter().map(|a| a.0).collect::<Vec<_>>(), total)
    };
    assert_eq!(
        page(10, 0).await,
        (
            vec![
                "suite-B".to_string(),
                "suite-a-z".into(),
                "suite-ab".into(),
                "suite-old".into()
            ],
            4
        ),
        "{adapter}"
    );
    assert_eq!(
        page(2, 1).await,
        (vec!["suite-a-z".to_string(), "suite-ab".into()], 4),
        "{adapter}"
    );
    assert_eq!(page(0, 0).await, (vec![], 4), "{adapter}");
    assert_eq!(page(10, 9).await, (vec![], 4), "{adapter}");
    let mut all: Vec<String> = w
        .repo()
        .all_asset_ids()
        .await
        .expect("all_asset_ids")
        .into_iter()
        .map(|a| a.0)
        .collect();
    all.sort();
    assert_eq!(
        all,
        ["suite-B", "suite-a-z", "suite-ab", "suite-old"],
        "{adapter}"
    );
}

/// Assets list newest `last_event_at` first, one day's assets in BYTE
/// order of id, each as `current_state` folds it; an account scope keeps
/// the assets the ACCOUNT holds — a location with the same id never
/// matches.
async fn assets_list_newest_first_ties_by_id_and_scope_to_an_account_holder<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_fleet(w, adapter).await;
    let mut want = Vec::new();
    for id in ["suite-B", "suite-a-z", "suite-ab", "suite-old"] {
        want.push(state(w, id).await);
    }
    assert_eq!(listed(w, None).await, (want.clone(), 4), "{adapter}");
    let page = w.repo().list_assets(2, 1, None).await.expect("page");
    assert_eq!(page, (want[1..3].to_vec(), 4), "{adapter}");
    assert_eq!(
        listed(w, Some("acct-1")).await,
        (vec![want[2].clone(), want[3].clone()], 2),
        "{adapter}"
    );
    assert_eq!(listed(w, Some("acct-9")).await, (vec![], 0), "{adapter}");
}

/// A negative limit or offset is refused `Invalid` by both pages.
async fn a_negative_page_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    seed_fleet(w, adapter).await;
    for (limit, offset) in [(-1, 0), (1, -1)] {
        let ids = w.repo().list_asset_ids(limit, offset).await;
        assert!(
            matches!(ids, Err(AssetsError::Invalid(_))),
            "{adapter}: ids {limit},{offset}: {ids:?}"
        );
        let assets = w.repo().list_assets(limit, offset, None).await;
        assert!(
            matches!(assets, Err(AssetsError::Invalid(_))),
            "{adapter}: assets {limit},{offset}: {assets:?}"
        );
    }
}

/// An account's open tickets are those of the live assets it holds: the
/// closed ticket, the decommissioned asset's ticket and the ticket at a
/// location sharing the account's id are not counted, and an open
/// redelivered under a new event id is one ticket.
async fn open_tickets_count_on_the_live_assets_an_account_holds<W: World>(w: &W, adapter: &str) {
    seed_fleet(w, adapter).await;
    append(w, adapter, ev("e-ab-6", "suite-ab", 6, opened("job-1"))).await;
    append(w, adapter, ev("e-az-4", "suite-a-z", 6, opened("job-4"))).await;
    let count = |a: &'static str| async move {
        w.repo()
            .open_ticket_count_for_account(a)
            .await
            .expect("count")
    };
    assert_eq!(count("acct-1").await, 1, "{adapter}");
    assert_eq!(count("acct-9").await, 0, "{adapter}");
}

/// A model's active assets are those identified as it and not
/// decommissioned.
async fn active_assets_of_a_model_skip_the_decommissioned_and_the_unidentified<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_fleet(w, adapter).await;
    append(
        w,
        adapter,
        ev(
            "e-B-2",
            "suite-B",
            6,
            AssetEventKind::Identified {
                sku: "suite-model-b".into(),
            },
        ),
    )
    .await;
    let count = |s: &'static str| async move {
        w.repo().active_asset_count_for_sku(s).await.expect("count")
    };
    assert_eq!(count("suite-model-a").await, 1, "{adapter}");
    assert_eq!(count("suite-model-b").await, 2, "{adapter}");
    assert_eq!(count("suite-no-such-model").await, 0, "{adapter}");
}

/// The summary over the fleet: every phase in pipeline order, the live
/// assets per model (ties in byte order of sku, the unidentified under
/// none), the live open tickets, and the warranties ending within thirty
/// days of `today`.
async fn the_summary_counts_phases_models_open_tickets_and_warranties<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_fleet(w, adapter).await;
    let phases = |counts: [i64; 6]| -> Vec<PhaseRollup> {
        AssetLifecyclePhase::ORDER
            .iter()
            .zip(counts)
            .map(|(p, count)| PhaseRollup {
                phase: p.to_string(),
                count,
            })
            .collect()
    };
    let summary = |today| async move { w.repo().assets_summary(today).await.expect("summary") };
    assert_eq!(
        summary(day(1)).await,
        AssetsSummary {
            phase_counts: phases([0, 1, 0, 1, 1, 1]),
            total_systems: 4,
            in_field_count: 3,
            open_tickets_total: 1,
            sku_counts: vec![
                SkuRollup {
                    sku: "suite-model-a".into(),
                    count: 1
                },
                SkuRollup {
                    sku: "suite-model-b".into(),
                    count: 1
                },
            ],
            warranty_expiring_30d: 1,
        },
        "{adapter}"
    );
    // The warranty runs through day 20: counted from day 20, not from
    // the day after, nor from more than thirty days before.
    assert_eq!(summary(day(21)).await.warranty_expiring_30d, 0, "{adapter}");
    let early = NaiveDate::from_ymd_opt(2026, 2, 18).expect("a day");
    assert_eq!(summary(early).await.warranty_expiring_30d, 0, "{adapter}");
}

/// An event naming a model the catalog lacks — at receipt or at
/// identification — is refused `UnknownModel` naming it, and stored
/// nowhere; a batch carrying one stores none of the batch.
async fn an_unknown_model_is_refused_and_nothing_is_stored<W: World>(w: &W, adapter: &str) {
    for e in [
        ev("e-1", "suite-ab", 1, received(Some("suite-no-such-model"))),
        ev(
            "e-2",
            "suite-ab",
            1,
            AssetEventKind::Identified {
                sku: "suite-no-such-model".into(),
            },
        ),
    ] {
        let err = w.repo().append(e).await.expect_err("an unknown model");
        assert!(
            matches!(&err, AssetsError::UnknownModel(s) if s == "suite-no-such-model"),
            "{adapter}: {err:?}"
        );
    }
    let err = w
        .repo()
        .batch_append(vec![
            ev("e-3", "suite-B", 1, received(Some("suite-model-a"))),
            ev(
                "e-4",
                "suite-a-z",
                1,
                AssetEventKind::Sold {
                    account_id: "acct-1".into(),
                    price_cents: 100,
                    currency: "USD".into(),
                    order_id: None,
                    condition: AssetCondition::new("new"),
                },
            ),
            ev("e-5", "suite-ab", 2, received(Some("suite-no-such-model"))),
        ])
        .await
        .expect_err("an unknown model in a batch");
    assert!(
        matches!(&err, AssetsError::UnknownModel(s) if s == "suite-no-such-model"),
        "{adapter}: {err:?}"
    );
    assert_eq!(
        w.repo().list_asset_ids(10, 0).await.expect("ids"),
        (vec![], 0),
        "{adapter}"
    );
}

/// A NUL byte cannot be stored in TEXT or JSONB. An event carrying one
/// anywhere is refused `Invalid` naming the field — alone or in a batch,
/// which then stores nothing — and a read keyed by one is a miss.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    let fine = || ev("e-1", "suite-ab", 1, received(Some("suite-model-a")));
    let mut cases: Vec<(&str, AssetEvent)> = Vec::new();
    let mut e = fine();
    e.id = AssetEventId::new("e-\0-1");
    cases.push(("id", e));
    let mut e = fine();
    e.asset_id = AssetId::new("suite-\0ab");
    cases.push(("asset_id", e));
    let mut e = fine();
    e.actor_id = ActorId::Human("emp-\0suite".into());
    cases.push(("actor_id", e));
    let mut e = fine();
    e.kind = AssetEventKind::Received {
        sku: Some("suite-model-a".into()),
        source: IntakeSource::new("oem-\0new"),
        oem_serial: None,
    };
    cases.push(("source", e));
    let mut e = fine();
    e.kind = opened("job-\0-1");
    cases.push(("job_id", e));
    for (field, e) in cases {
        let err = w.repo().append(e.clone()).await.expect_err("a NUL");
        assert!(
            matches!(&err, AssetsError::Invalid(m) if m.starts_with(&format!("{field} "))),
            "{adapter}: {field}: {err:?}"
        );
        let err = w
            .repo()
            .batch_append(vec![
                ev("e-ok", "suite-B", 1, received(Some("suite-model-a"))),
                e,
            ])
            .await
            .expect_err("a NUL in a batch");
        assert!(
            matches!(&err, AssetsError::Invalid(m) if m.starts_with(&format!("{field} "))),
            "{adapter}: batch {field}: {err:?}"
        );
    }
    assert_eq!(
        w.repo().list_asset_ids(10, 0).await.expect("ids"),
        (vec![], 0),
        "{adapter}"
    );
    seed_fleet(w, adapter).await;
    let nul = AssetId::new("suite-\0ab");
    assert_eq!(
        w.repo().events_for(&nul).await.expect("events_for"),
        vec![],
        "{adapter}"
    );
    assert_eq!(
        w.repo().current_state(&nul).await.expect("state"),
        None,
        "{adapter}"
    );
    assert_eq!(listed(w, Some("acct-\0-1")).await, (vec![], 0), "{adapter}");
    assert_eq!(
        w.repo()
            .open_ticket_count_for_account("acct-\0-1")
            .await
            .expect("count"),
        0,
        "{adapter}"
    );
    assert_eq!(
        w.repo()
            .active_asset_count_for_sku("suite-model-\0a")
            .await
            .expect("count"),
        0,
        "{adapter}"
    );
}

/// One service job open on TWO assets is an open ticket on each: the
/// ticket id is the caller's (`POST /api/assets/events` takes it from the
/// body; nothing mints it or refuses its reuse), and the projection
/// counts tickets per asset. Postgres keyed `asset_open_tickets` by the
/// ticket id alone until backlog b8099caf, so the second asset's open
/// was dropped on conflict, a close on one asset deleted the other's row
/// too, and the account's count drifted from the per-asset counts — on
/// the fast path and on the re-projection a late event forces.
async fn one_job_open_on_two_assets_is_an_open_ticket_on_each<W: World>(w: &W, adapter: &str) {
    for e in [
        ev("e-ab-1", "suite-ab", 1, received(Some("suite-model-a"))),
        ev("e-ab-2", "suite-ab", 2, installed("account", "acct-1")),
        ev("e-B-1", "suite-B", 1, received(Some("suite-model-b"))),
        ev("e-B-2", "suite-B", 2, installed("account", "acct-1")),
        ev("e-ab-3", "suite-ab", 3, opened("job-shared")),
        ev("e-B-3", "suite-B", 3, opened("job-shared")),
        // Sorts before e-B-3 on its day, so Postgres re-projects suite-B
        // whole while suite-ab still holds the ticket open.
        ev(
            "e-B-0",
            "suite-B",
            3,
            AssetEventKind::WarrantyStarted {
                through: day(20),
                coverage: WarrantyCoverage::new("standard"),
            },
        ),
    ] {
        append(w, adapter, e).await;
    }
    let count = || async {
        w.repo()
            .open_ticket_count_for_account("acct-1")
            .await
            .expect("count")
    };
    let open_on = |a: &'static str| async move {
        listed(w, Some("acct-1"))
            .await
            .0
            .into_iter()
            .find(|s| s.asset_id.0 == a)
            .map(|s| s.open_ticket_count)
    };
    assert_eq!(count().await, 2, "{adapter}: open on both");
    append(
        w,
        adapter,
        ev("e-ab-4", "suite-ab", 4, closed("job-shared")),
    )
    .await;
    // A later event on suite-B projects from the open set the store kept.
    append(
        w,
        adapter,
        ev("e-B-4", "suite-B", 4, AssetEventKind::WarrantyExpired),
    )
    .await;
    assert_eq!(count().await, 1, "{adapter}: closed on one");
    assert_eq!(open_on("suite-ab").await, Some(0), "{adapter}");
    assert_eq!(open_on("suite-B").await, Some(1), "{adapter}");
    assert_eq!(state(w, "suite-B").await.open_ticket_count, 1, "{adapter}");
}

/// An actor whose text form parses back as ANOTHER actor is refused
/// `Invalid` naming `actor_id`, alone or in a batch, storing nothing:
/// Postgres keeps the text form, the double the value, so until backlog
/// b8099caf `Human("system")` read back from Postgres as the `platform`
/// automation and from the double as a human. Each class reads back
/// whole from its well-formed spelling.
async fn an_actor_whose_text_form_reads_back_as_another_is_refused<W: World>(w: &W, adapter: &str) {
    for actor in [
        ActorId::Human("system".into()),
        ActorId::Human("emp:suite".into()),
        ActorId::Human("agent-suite".into()),
        ActorId::RegisteredAgent("emp-suite".into()),
        ActorId::agent("automation", "suite"),
    ] {
        let mut e = ev("e-1", "suite-ab", 1, received(Some("suite-model-a")));
        e.actor_id = actor.clone();
        let err = w
            .repo()
            .append(e.clone())
            .await
            .expect_err("a drifting actor");
        assert!(
            matches!(&err, AssetsError::Invalid(m) if m.starts_with("actor_id ")),
            "{adapter}: {actor:?}: {err:?}"
        );
        let err = w
            .repo()
            .batch_append(vec![
                ev("e-ok", "suite-B", 1, received(Some("suite-model-a"))),
                e,
            ])
            .await
            .expect_err("a drifting actor in a batch");
        assert!(
            matches!(&err, AssetsError::Invalid(m) if m.starts_with("actor_id ")),
            "{adapter}: batch {actor:?}: {err:?}"
        );
    }
    assert_eq!(
        w.repo().list_asset_ids(10, 0).await.expect("ids"),
        (vec![], 0),
        "{adapter}"
    );
    let actors = [
        ActorId::human("emp-suite"),
        ActorId::automation("rule:suite"),
        ActorId::agent("claude", "opus-5"),
        ActorId::RegisteredAgent("agent-suite".into()),
    ];
    let mut written = Vec::new();
    for (i, actor) in actors.into_iter().enumerate() {
        let mut e = ev(
            &format!("e-{i}"),
            "suite-ab",
            1,
            received(Some("suite-model-a")),
        );
        e.actor_id = actor;
        append(w, adapter, e.clone()).await;
        written.push(e);
    }
    assert_eq!(log(w, "suite-ab").await, written, "{adapter}");
}
