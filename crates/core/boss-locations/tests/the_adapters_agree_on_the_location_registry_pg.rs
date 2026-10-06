//! The Locations registry answers the same on both `LocationRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the first port outside
//! boss-jobs after the registries the census orders first (backlog
//! be459ab9). `exists_active` is the hot-path validator every write
//! that sets a `*_location_id` calls, and every door test of
//! `/api/locations` asks `InMemoryLocations`, while production is
//! answered by `PgLocations`.
//!
//! WHY IT EXISTS. The reads were stated for the double alone
//! (`in_memory::tests`) and the one write for Postgres alone
//! (`locations_pg.rs`), each in its own words; this file is the one
//! statement of every method of the port, each case holding each
//! adapter to its own stated answer, and every write also judged by the
//! `location.declared` facts it leaves — the double's
//! `recorded_events` and Postgres's `event_outbox`, staged in the
//! batch's own transaction.
//!
//! The world: the same `suite-` rows, INSERTed into Postgres the way
//! the schema seeds its own (a Location is born in a migration or a
//! tenant's batch) and handed to the double's constructor. The
//! migrations seed the platform rows (`loc-hq`, the brewery sites) into
//! every Postgres database and the double holds only what it is handed,
//! so every read that could reach them — `children_of(None)`, the
//! outbox — is judged over `suite-` ids. Instants are whole seconds.
//!
//! ORDER IS COMPARED, because the port promises it: `list_for_kind`
//! and `children_of` are "ordered by `name` ascending". The fixture
//! holds a case pair and a punctuation pair (`Suite B`, `Suite a-z`,
//! `Suite ab`) — the only shape that catches the database's locale
//! disagreeing with the double's byte order (backlog 2987fb2d).

use boss_core::actor::ActorId;
use boss_core::primitives::Location;
use boss_core::publisher::EventStamp;
use boss_locations::port::{LOCATION_DECLARED, LocationError};
use boss_locations::{InMemoryLocations, LocationRepository, PgLocations};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: LocationRepository;
    fn repo(&self) -> &Self::R;
    /// Every `location.declared` payload about a `suite-` row, in the
    /// order recorded.
    async fn facts(&self) -> Vec<Value>;
}

fn is_suite_fact(payload: &Value) -> bool {
    payload["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("suite-"))
}

struct InMemory(InMemoryLocations);

impl World for InMemory {
    type R = InMemoryLocations;
    fn repo(&self) -> &InMemoryLocations {
        &self.0
    }
    async fn facts(&self) -> Vec<Value> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == LOCATION_DECLARED && is_suite_fact(&e.payload))
            .map(|e| e.payload)
            .collect()
    }
}

struct Postgres {
    repo: PgLocations,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgLocations;
    fn repo(&self) -> &PgLocations {
        &self.repo
    }
    async fn facts(&self) -> Vec<Value> {
        let rows: Vec<Value> =
            sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
                .bind(LOCATION_DECLARED)
                .fetch_all(&self.pool)
                .await
                .expect("read the outbox");
        rows.into_iter().filter(is_suite_fact).collect()
    }
}

/// Land the suite's rows in a fresh database the way the schema seeds
/// its own — one statement each, parents first.
async fn seed(pool: &sqlx::PgPool) {
    for l in rows() {
        sqlx::query(
            "INSERT INTO locations
                (id, name, kind, parent_id, timezone, latitude, longitude,
                 address, account_id, metadata, retired_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&l.id)
        .bind(&l.name)
        .bind(&l.kind)
        .bind(&l.parent_id)
        .bind(&l.timezone)
        .bind(l.latitude)
        .bind(l.longitude)
        .bind(&l.address)
        .bind(&l.account_id)
        .bind(&l.metadata)
        .bind(l.retired_at)
        .execute(pool)
        .await
        .expect("a suite location lands");
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryLocations::new(rows())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            seed(&db.pool).await;
            let world = Postgres { repo: PgLocations::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        get_answers_every_row_whole_retired_or_not_and_none_for_a_stranger,
        exists_active_is_true_only_for_a_live_location,
        list_for_kind_is_the_live_rows_of_that_kind_in_byte_order_of_name,
        children_of_is_one_level_of_live_children_in_byte_order_of_name,
        a_batch_inserts_only_absent_ids_and_records_one_fact_per_insert,
        a_declared_row_lands_as_declared_retired_or_not,
        a_batch_naming_an_absent_parent_is_refused_whole,
    }
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn loc(id: &str, name: &str, kind: &str, parent: Option<&str>) -> Location {
    Location {
        id: id.into(),
        name: name.into(),
        kind: kind.into(),
        parent_id: parent.map(String::from),
        timezone: "America/Los_Angeles".into(),
        latitude: None,
        longitude: None,
        address: None,
        account_id: None,
        metadata: json!({}),
        retired_at: None,
    }
}

fn retired(mut l: Location) -> Location {
    l.retired_at = Some(at(0));
    l
}

/// The suite's rows, parents first: a region holding three storefronts
/// whose names are a case pair and a punctuation pair, one retired
/// storefront, a zone under a storefront (a grandchild of the region),
/// a second region, a retired region, and a fully-described HQ.
fn rows() -> Vec<Location> {
    let mut hq = loc("suite-hq", "Suite HQ", "hq", None);
    hq.timezone = "UTC".into();
    hq.latitude = Some(37.7749);
    hq.longitude = Some(-122.4194);
    hq.address = Some("1 Market St".into());
    hq.account_id = Some("acct-suite".into());
    hq.metadata = json!({"floor": 3, "badge": "blue"});
    vec![
        loc("suite-west", "Suite West", "field-region", None),
        loc("suite-east", "Suite East", "field-region", None),
        retired(loc("suite-north", "Suite North", "field-region", None)),
        loc("suite-ab", "Suite ab", "storefront", Some("suite-west")),
        loc("suite-a-z", "Suite a-z", "storefront", Some("suite-west")),
        loc("suite-b", "Suite B", "storefront", Some("suite-west")),
        retired(loc(
            "suite-closed",
            "Suite Aardvark",
            "storefront",
            Some("suite-west"),
        )),
        loc(
            "suite-zone",
            "Suite Zone",
            "warehouse-zone",
            Some("suite-ab"),
        ),
        hq,
    ]
}

fn row(id: &str) -> Location {
    rows()
        .into_iter()
        .find(|l| l.id == id)
        .expect("a suite row")
}

fn stamp() -> EventStamp {
    EventStamp::new(
        "locations",
        ActorId::Automation("rule:adapters-agree-suite".into()),
    )
}

/// The `suite-` ids among `rows`, in the order answered.
fn ids(rows: &[Location]) -> Vec<&str> {
    rows.iter()
        .map(|l| l.id.as_str())
        .filter(|id| id.starts_with("suite-"))
        .collect()
}

/// A fact with its `_`-prefixed envelope keys removed.
fn stripped(fact: &Value) -> Value {
    let mut f = fact.clone();
    if let Value::Object(m) = &mut f {
        m.retain(|k, _| !k.starts_with('_'));
    }
    f
}

/// The `location.declared` payload a row's insert must leave: the row
/// as inserted plus who declared it.
fn declared(l: &Location) -> Value {
    let mut v = serde_json::to_value(l).expect("a row serialises");
    if let Value::Object(m) = &mut v {
        m.insert(
            "declared_by".into(),
            json!("automation:rule:adapters-agree-suite"),
        );
    }
    v
}

// ----- cases -----------------------------------------------------------------

/// `get` answers a row whole — every column, live or retired, since
/// audit surfaces resolve old ids through it — and `None`, not an
/// error, for an id no row carries.
async fn get_answers_every_row_whole_retired_or_not_and_none_for_a_stranger<W: World>(
    w: &W,
    adapter: &str,
) {
    for id in ["suite-hq", "suite-ab", "suite-closed", "suite-north"] {
        assert_eq!(
            w.repo().get(id).await.expect("get answers"),
            Some(row(id)),
            "{adapter}: {id} reads back whole"
        );
    }
    assert_eq!(
        w.repo().get("suite-stranger").await.expect("get answers"),
        None,
        "{adapter}"
    );
}

/// The hot-path validator: true for a live row, false for a retired one
/// AND for one no row carries — a retired site refuses a new reference
/// exactly as a typo does.
async fn exists_active_is_true_only_for_a_live_location<W: World>(w: &W, adapter: &str) {
    for (id, live) in [
        ("suite-hq", true),
        ("suite-zone", true),
        ("suite-closed", false),
        ("suite-north", false),
        ("suite-stranger", false),
    ] {
        assert_eq!(
            w.repo().exists_active(id).await.expect("answers"),
            live,
            "{adapter}: {id}"
        );
    }
}

/// `list_for_kind` is every live row of that kind, whole, by `name` in
/// BYTE order — `Suite B` before `Suite a-z` before `Suite ab` — never a
/// retired one or one of another kind, and nothing for a kind no row
/// carries.
async fn list_for_kind_is_the_live_rows_of_that_kind_in_byte_order_of_name<W: World>(
    w: &W,
    adapter: &str,
) {
    let stores = w
        .repo()
        .list_for_kind("storefront")
        .await
        .expect("list_for_kind answers");
    assert_eq!(
        ids(&stores),
        vec!["suite-b", "suite-a-z", "suite-ab"],
        "{adapter}: live storefronts, name in byte order"
    );
    for l in stores.iter().filter(|l| l.id.starts_with("suite-")) {
        assert_eq!(*l, row(&l.id), "{adapter}: {} listed whole", l.id);
    }
    assert_eq!(
        ids(&w
            .repo()
            .list_for_kind("field-region")
            .await
            .expect("answers")),
        vec!["suite-east", "suite-west"],
        "{adapter}: the retired region is not listed"
    );
    assert!(
        w.repo()
            .list_for_kind("suite-no-such-kind")
            .await
            .expect("answers")
            .is_empty(),
        "{adapter}"
    );
}

/// `children_of` is the LIVE rows whose parent is the one named — one
/// level, so a grandchild is not a child — by `name` in byte order;
/// `None` is the live roots; a leaf or a stranger has none.
async fn children_of_is_one_level_of_live_children_in_byte_order_of_name<W: World>(
    w: &W,
    adapter: &str,
) {
    let kids = w
        .repo()
        .children_of(Some("suite-west"))
        .await
        .expect("children_of answers");
    assert_eq!(
        ids(&kids),
        vec!["suite-b", "suite-a-z", "suite-ab"],
        "{adapter}: live children, name in byte order, no grandchild"
    );
    for l in &kids {
        assert_eq!(*l, row(&l.id), "{adapter}: {} whole", l.id);
    }
    assert_eq!(
        ids(&w
            .repo()
            .children_of(Some("suite-ab"))
            .await
            .expect("answers")),
        vec!["suite-zone"],
        "{adapter}"
    );
    assert_eq!(
        ids(&w.repo().children_of(None).await.expect("answers")),
        vec!["suite-east", "suite-hq", "suite-west"],
        "{adapter}: the live roots, name in byte order"
    );
    for leaf in ["suite-zone", "suite-stranger"] {
        assert!(
            w.repo()
                .children_of(Some(leaf))
                .await
                .expect("answers")
                .is_empty(),
            "{adapter}: {leaf}"
        );
    }
}

/// A batch inserts every id not already held — a child listed before
/// its parent lands, one transaction — and leaves a held id exactly as
/// it was (a re-publish never clobbers an operator's edit); an id
/// repeated in the batch lands once, as first listed. It answers the
/// count inserted and records ONE `location.declared` per insert, the
/// row as inserted plus who declared it, and none for a kept row. The
/// same batch again inserts nothing and records nothing.
async fn a_batch_inserts_only_absent_ids_and_records_one_fact_per_insert<W: World>(
    w: &W,
    adapter: &str,
) {
    let desk = loc("suite-desk", "Suite Desk", "office", Some("suite-lab"));
    let lab = loc("suite-lab", "Suite Lab", "office", Some("suite-hq"));
    let mut renamed = row("suite-hq");
    renamed.name = "Suite HQ renamed".into();
    let mut lab_again = lab.clone();
    lab_again.name = "Suite Lab listed twice".into();
    let batch = [desk.clone(), renamed, lab.clone(), lab_again];

    for (attempt, want) in [("first", 2), ("restated", 0)] {
        assert_eq!(
            w.repo()
                .batch_upsert(&batch, &stamp())
                .await
                .expect("the batch lands"),
            want,
            "{adapter}: {attempt}"
        );
    }
    assert_eq!(
        w.repo().get("suite-hq").await.expect("get"),
        Some(row("suite-hq")),
        "{adapter}: the held row is untouched"
    );
    assert_eq!(
        w.repo().get("suite-lab").await.expect("get"),
        Some(lab.clone()),
        "{adapter}: the first listing lands"
    );
    assert_eq!(
        w.repo().get("suite-desk").await.expect("get"),
        Some(desk.clone()),
        "{adapter}: a child listed before its parent lands"
    );
    assert_eq!(
        ids(&w
            .repo()
            .children_of(Some("suite-hq"))
            .await
            .expect("answers")),
        vec!["suite-lab"],
        "{adapter}"
    );

    let facts = w.facts().await;
    assert_eq!(
        facts.iter().map(stripped).collect::<Vec<_>>(),
        vec![declared(&desk), declared(&lab)],
        "{adapter}: one fact per insert, none for a kept row or the restatement"
    );
    for f in &facts {
        assert_eq!(f["_actor"], f["declared_by"], "{adapter}: one actor");
    }
}

/// A declared row lands as declared — every column, the retirement
/// instant included, since the port inserts "the row" and its fact is
/// "the row as inserted" — so what `get` answers and what the log
/// holds are one statement.
async fn a_declared_row_lands_as_declared_retired_or_not<W: World>(w: &W, adapter: &str) {
    let mut shop = loc("suite-shop", "Suite Shop", "storefront", Some("suite-east"));
    shop.timezone = "Europe/London".into();
    shop.latitude = Some(51.5072);
    shop.longitude = Some(-0.1276);
    shop.address = Some("2 Strand".into());
    shop.account_id = Some("acct-shop".into());
    shop.metadata = json!({"tills": 2, "opens": "09:00"});
    let gone = retired(loc("suite-gone", "Suite Gone", "storefront", None));

    assert_eq!(
        w.repo()
            .batch_upsert(&[shop.clone(), gone.clone()], &stamp())
            .await
            .expect("the batch lands"),
        2,
        "{adapter}"
    );
    for l in [&shop, &gone] {
        assert_eq!(
            w.repo().get(&l.id).await.expect("get"),
            Some(l.clone()),
            "{adapter}: {} reads back as declared",
            l.id
        );
    }
    assert!(
        !w.repo().exists_active("suite-gone").await.expect("answers"),
        "{adapter}: a row declared retired is not active"
    );
    assert_eq!(
        w.facts().await.iter().map(stripped).collect::<Vec<_>>(),
        vec![declared(&shop), declared(&gone)],
        "{adapter}: each fact is the row as it now stands"
    );
}

/// A batch naming a parent that neither the registry nor the batch
/// holds is refused WHOLE as a `Conflict` naming that parent: no row of
/// it lands, not even one whose own parent resolves, and it records no
/// fact.
async fn a_batch_naming_an_absent_parent_is_refused_whole<W: World>(w: &W, adapter: &str) {
    let fine = loc("suite-fine", "Suite Fine", "office", Some("suite-hq"));
    let orphan = loc(
        "suite-orphan",
        "Suite Orphan",
        "office",
        Some("suite-nowhere"),
    );
    let got = w.repo().batch_upsert(&[fine, orphan], &stamp()).await;
    assert!(
        matches!(got, Err(LocationError::Conflict(ref m)) if m.contains("suite-nowhere")),
        "{adapter}: {got:?}"
    );
    for id in ["suite-fine", "suite-orphan"] {
        assert_eq!(
            w.repo().get(id).await.expect("get"),
            None,
            "{adapter}: {id} did not land"
        );
    }
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}
