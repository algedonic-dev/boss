//! The campaign store answers the same on both `CampaignsRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the next smallest MODULE
//! port of the census after the employee roster and the customer
//! registry (backlog be459ab9). Tests elsewhere may be answered by
//! `InMemoryCampaigns`, while production is answered by `PgCampaigns`.
//!
//! WHY IT EXISTS. The port was stated for Postgres alone
//! (`campaigns_pg.rs`) and the double was stated nowhere; this file is
//! the one statement of every method of the port, each case holding each
//! adapter to the same stated answer, and every write also judged by the
//! `campaigns.campaign.created` fact it leaves — the double's
//! `recorded_events` and Postgres's `event_outbox`, staged in the write's
//! own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - LIST ORDER. Postgres broke a `created_at` tie by `id` in the
//!   database's locale, the double in byte order. Postgres now orders
//!   `id COLLATE "C"` (backlog 2987fb2d's class). Case
//!   `the_list_is_newest_first_and_a_tie_is_byte_order_of_id`.
//! - THE BIRTH FACT. The double recorded no event at all, so no test
//!   answered by it could see what a create puts on the log. It now
//!   records the same `campaigns.campaign.created` fact, built by the one
//!   function both adapters call (`events::campaign_created`).
//! - PRECISION. The double kept nanoseconds; the column keeps
//!   microseconds, so one write read back unequal across adapters. The
//!   double now truncates as the column does. Case
//!   `an_instant_reads_back_at_microsecond_precision`.
//! - A NUL BYTE. Postgres cannot store one in TEXT or JSONB and refused
//!   the write with its encoding error as `Storage` (a 500), and a read
//!   keyed by one the same way; the double stored it. Both now refuse a
//!   write carrying one with `Invalid` naming the field, and answer a
//!   read keyed by one as the miss it is. Case
//!   `a_nul_byte_is_refused_naming_its_field_and_misses_on_read`.
//!
//! The world: the suite's rows are created THROUGH the port, so each
//! adapter seeds itself the way production does. The migrations may seed
//! rows of their own, so every read is judged over `suite-` ids. The
//! fixture holds a case pair and a punctuation pair at one instant
//! (`suite-B`, `suite-a-z`, `suite-ab`) — the only shape that catches the
//! database's locale disagreeing with the double's byte order.

use boss_campaigns::events::CAMPAIGN_CREATED;
use boss_campaigns::port::{CampaignsError, CampaignsRepository};
use boss_campaigns::types::Campaign;
use boss_campaigns::{InMemoryCampaigns, PgCampaigns};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: CampaignsRepository;
    fn repo(&self) -> &Self::R;
    /// Every `campaigns.campaign.*` fact about a `suite-` row, as
    /// `(kind, payload)`, in the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("campaigns.campaign.")
        && payload["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("suite-"))
}

struct InMemory(InMemoryCampaigns);

impl World for InMemory {
    type R = InMemoryCampaigns;
    fn repo(&self) -> &InMemoryCampaigns {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| is_suite_fact(&e.kind, &e.payload))
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgCampaigns,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgCampaigns;
    fn repo(&self) -> &PgCampaigns {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> =
            sqlx::query_as("SELECT kind, payload FROM event_outbox ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .expect("read the outbox");
        rows.into_iter()
            .filter(|(k, p)| is_suite_fact(k, p))
            .collect()
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryCampaigns::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgCampaigns::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        the_list_is_newest_first_and_a_tie_is_byte_order_of_id,
        a_campaign_reads_back_whole_and_a_stranger_is_none,
        a_created_campaign_reads_back_whole_and_records_its_birth,
        a_campaign_with_no_dates_reads_back_without_them,
        a_create_on_a_held_id_answers_false_changes_nothing_records_nothing,
        an_instant_reads_back_at_microsecond_precision,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn day(m: u32, d: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(2026, m, d)
}

/// A campaign carrying only what the suite needs to tell it apart.
fn bare(id: &str) -> Campaign {
    Campaign {
        id: id.into(),
        name: format!("Name of {id}"),
        status: "active".into(),
        starts_on: None,
        ends_on: None,
        metadata: json!({}),
        created_at: None,
    }
}

/// A fully-described campaign.
fn whole(id: &str) -> Campaign {
    let mut c = bare(id);
    c.status = "ended".into();
    c.starts_on = day(7, 16);
    c.ends_on = day(8, 31);
    c.metadata = json!({"channel": "taproom", "budget_cents": 125_000, "skus": ["ipa", "stout"]});
    c
}

/// The suite's rows and the instant each is born at: an old and a new
/// row, and a case pair and a punctuation pair born at one instant
/// between them.
fn rows() -> Vec<(Campaign, DateTime<Utc>)> {
    vec![
        (bare("suite-old"), at(0)),
        (whole("suite-ab"), at(10)),
        (bare("suite-B"), at(10)),
        (bare("suite-a-z"), at(10)),
        (bare("suite-new"), at(20)),
    ]
}

/// Create the suite's rows through the port.
async fn seed<W: World>(w: &W, adapter: &str) {
    for (c, now) in rows() {
        assert!(
            w.repo()
                .create_campaign_at(&c, now)
                .await
                .unwrap_or_else(|e| panic!("{adapter}: seed {}: {e:?}", c.id)),
            "{adapter}: seed {} is a fresh birth",
            c.id
        );
    }
}

/// The row as the port must answer it: what was sent, stamped with the
/// instant it was born at.
fn born(mut c: Campaign, now: DateTime<Utc>) -> Campaign {
    c.created_at = Some(now);
    c
}

/// The `suite-` rows of a listing, in the order answered.
async fn suite_list<W: World>(w: &W, adapter: &str) -> Vec<Campaign> {
    w.repo()
        .list_campaigns()
        .await
        .unwrap_or_else(|e| panic!("{adapter}: list: {e:?}"))
        .into_iter()
        .filter(|c| c.id.starts_with("suite-"))
        .collect()
}

/// The birth fact the port promises for `c`: the row as sent — the log
/// is the system of record, and the rebuild (`rebuild.rs`) reproduces
/// the row from this payload alone.
fn birth(c: &Campaign) -> (String, Value) {
    (
        CAMPAIGN_CREATED.to_string(),
        json!({
            "id": c.id,
            "name": c.name,
            "status": c.status,
            "starts_on": c.starts_on,
            "ends_on": c.ends_on,
            "metadata": c.metadata,
        }),
    )
}

// ----- cases ---------------------------------------------------------------

/// Newest first; a tie on `created_at` is broken by `id` in BYTE order —
/// `suite-B` < `suite-a-z` < `suite-ab`, which a locale collation orders
/// the other way round.
async fn the_list_is_newest_first_and_a_tie_is_byte_order_of_id<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let ids: Vec<String> = suite_list(w, adapter)
        .await
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(
        ids,
        ["suite-new", "suite-B", "suite-a-z", "suite-ab", "suite-old"],
        "{adapter}"
    );
}

/// A row reads back whole — every field as sent, `created_at` the birth
/// instant — by `get` and inside the list alike; an id nobody holds is
/// `None`, not an error.
async fn a_campaign_reads_back_whole_and_a_stranger_is_none<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let want = born(whole("suite-ab"), at(10));
    assert_eq!(
        w.repo().get_campaign("suite-ab").await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );
    let listed = suite_list(w, adapter).await;
    assert!(listed.contains(&want), "{adapter}: {listed:?}");
    assert_eq!(
        w.repo().get_campaign("suite-stranger").await.expect("get"),
        None,
        "{adapter}"
    );
}

/// A fresh create answers `true`, reads back as sent stamped with its
/// instant, and records exactly one birth fact carrying the row.
async fn a_created_campaign_reads_back_whole_and_records_its_birth<W: World>(w: &W, adapter: &str) {
    let fresh = whole("suite-fresh");
    assert!(
        w.repo()
            .create_campaign_at(&fresh, at(30))
            .await
            .expect("create"),
        "{adapter}: a fresh id is a birth"
    );
    assert_eq!(
        w.repo().get_campaign("suite-fresh").await.expect("get"),
        Some(born(fresh.clone(), at(30))),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![birth(&fresh)], "{adapter}");
}

/// The dates are optional: a campaign born without them reads back
/// without them, and its fact carries them as null.
async fn a_campaign_with_no_dates_reads_back_without_them<W: World>(w: &W, adapter: &str) {
    let undated = bare("suite-undated");
    assert!(
        w.repo()
            .create_campaign_at(&undated, at(30))
            .await
            .expect("create"),
        "{adapter}"
    );
    assert_eq!(
        w.repo().get_campaign("suite-undated").await.expect("get"),
        Some(born(undated.clone(), at(30))),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![birth(&undated)], "{adapter}");
}

/// A create on a held id is the idempotent re-POST: it answers `false`,
/// leaves the held row exactly as it was, and records nothing.
async fn a_create_on_a_held_id_answers_false_changes_nothing_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let mut renamed = whole("suite-ab");
    renamed.name = "Someone Else".into();
    renamed.status = "active".into();
    renamed.ends_on = None;
    renamed.metadata = json!({"channel": "online"});
    assert!(
        !w.repo()
            .create_campaign_at(&renamed, at(40))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: re-create: {e:?}")),
        "{adapter}: a held id is not a birth"
    );
    assert_eq!(
        w.repo().get_campaign("suite-ab").await.expect("get"),
        Some(born(whole("suite-ab"), at(10))),
        "{adapter}: the held row is untouched"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}

/// The column keeps microseconds; an instant read back is the instant
/// written, truncated to the microsecond, on both adapters.
async fn an_instant_reads_back_at_microsecond_precision<W: World>(w: &W, adapter: &str) {
    let fine = Utc
        .timestamp_opt(1_790_000_050, 123_456_789)
        .single()
        .expect("a representable instant");
    let micro = Utc
        .timestamp_opt(1_790_000_050, 123_456_000)
        .single()
        .expect("a representable instant");
    w.repo()
        .create_campaign_at(&bare("suite-fine"), fine)
        .await
        .expect("create");
    let got = w
        .repo()
        .get_campaign("suite-fine")
        .await
        .expect("get")
        .and_then(|c| c.created_at);
    assert_eq!(got, Some(micro), "{adapter}");
}

/// A NUL byte cannot be stored in TEXT or JSONB. A write carrying one —
/// in any text field, or anywhere inside the metadata — is refused
/// `Invalid` naming the field and writes nothing. A read keyed by one is
/// the miss it is: no stored id can hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    let mut id = bare("suite-n\0ul");
    id.name = "Nul Id".into();
    let mut name = bare("suite-nul-name");
    name.name = "Nu\0l".into();
    let mut status = bare("suite-nul-status");
    status.status = "act\0ive".into();
    let mut value = bare("suite-nul-value");
    value.metadata = json!({"skus": ["ipa", "a\0b"]});
    let mut key = bare("suite-nul-key");
    key.metadata = json!({"nested": {"a\0b": 1}});
    for (field, bad) in [
        ("id", id),
        ("name", name),
        ("status", status),
        ("metadata", value),
        ("metadata", key),
    ] {
        match w.repo().create_campaign_at(&bad, at(60)).await {
            Err(CampaignsError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    assert_eq!(suite_list(w, adapter).await, vec![], "{adapter}: no row");
    assert_eq!(w.facts().await, vec![], "{adapter}: nothing recorded");
    assert_eq!(
        w.repo().get_campaign("suite-n\0ul").await.expect("a miss"),
        None,
        "{adapter}"
    );
}
