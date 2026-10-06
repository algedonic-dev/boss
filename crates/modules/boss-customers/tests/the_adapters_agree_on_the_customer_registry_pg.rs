//! The customer registry answers the same on both `CustomersRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the second MODULE port of
//! the census after the employee roster (backlog be459ab9). The door's
//! own tests (`http.rs`) are answered by `InMemoryCustomers`, while
//! production is answered by `PgCustomers`.
//!
//! WHY IT EXISTS. The port was stated for Postgres alone
//! (`customers_pg.rs`) and the double was stated nowhere; this file is
//! the one statement of every method of the port, each case holding each
//! adapter to the same stated answer, and every write also judged by the
//! `customers.customer.created` fact it leaves — the double's
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
//!   records the same `customers.customer.created` fact, built by the one
//!   function both adapters call (`events::customer_created`).
//! - ONE ADDRESS. The double refused a second id whose email differed
//!   from a held one only by surrounding whitespace; the port says the
//!   unique key is `lower(email)`, which is what Postgres's partial index
//!   holds. The double now compares `lower(email)` and nothing more.
//!   That aligned the double to the index; the index itself disagreed
//!   with the id mint, which trims, and Postgres's `lower()` follows the
//!   database's locale while the double folded full Unicode. Backlog
//!   e1b08aaa made ONE rule, `types::email_key` — trim ASCII whitespace,
//!   lower ASCII letters — and held the mint, both adapters and the index
//!   (migration 20261001070511) to it. Case
//!   `an_email_is_one_address_padded_or_in_ascii_case_but_not_in_other_case`.
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

use boss_customers::events::CUSTOMER_CREATED;
use boss_customers::port::{CustomersError, CustomersRepository};
use boss_customers::types::Customer;
use boss_customers::{InMemoryCustomers, PgCustomers};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: CustomersRepository;
    fn repo(&self) -> &Self::R;
    /// Every `customers.customer.*` fact about a `suite-` row, as
    /// `(kind, payload)`, in the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("customers.customer.")
        && payload["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("suite-"))
}

struct InMemory(InMemoryCustomers);

impl World for InMemory {
    type R = InMemoryCustomers;
    fn repo(&self) -> &InMemoryCustomers {
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
    repo: PgCustomers,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgCustomers;
    fn repo(&self) -> &PgCustomers {
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
        in_memory => (InMemory(InMemoryCustomers::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgCustomers::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        the_list_is_newest_first_and_a_tie_is_byte_order_of_id,
        a_customer_reads_back_whole_and_a_stranger_is_none,
        a_created_customer_reads_back_whole_and_records_its_birth,
        a_create_on_a_held_id_answers_false_changes_nothing_records_nothing,
        a_registered_email_under_a_new_id_is_invalid_and_lands_nothing,
        an_email_is_one_address_padded_or_in_ascii_case_but_not_in_other_case,
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

/// A customer carrying only what the suite needs to tell it apart.
fn cust(id: &str, email: Option<&str>) -> Customer {
    Customer {
        id: id.into(),
        name: format!("Name of {id}"),
        email: email.map(String::from),
        phone: None,
        metadata: json!({}),
        created_at: None,
    }
}

/// A fully-described customer.
fn whole(id: &str) -> Customer {
    let mut c = cust(id, Some(&format!("{id}@Suite.Test")));
    c.phone = Some("555-0100".into());
    c.metadata = json!({"source": "shop", "consent": {"email": true, "sms": false}});
    c
}

/// The suite's rows and the instant each is born at: an old and a new
/// row without an email (two absent emails never clash), and a case
/// pair and a punctuation pair born at one instant between them.
fn rows() -> Vec<(Customer, DateTime<Utc>)> {
    vec![
        (cust("suite-old", None), at(0)),
        (whole("suite-ab"), at(10)),
        (cust("suite-B", Some("b@suite.test")), at(10)),
        (cust("suite-a-z", Some("a-z@suite.test")), at(10)),
        (cust("suite-new", None), at(20)),
    ]
}

/// Create the suite's rows through the port.
async fn seed<W: World>(w: &W, adapter: &str) {
    for (c, now) in rows() {
        assert!(
            w.repo()
                .create_customer_at(&c, now)
                .await
                .unwrap_or_else(|e| panic!("{adapter}: seed {}: {e:?}", c.id)),
            "{adapter}: seed {} is a fresh birth",
            c.id
        );
    }
}

/// The row as the port must answer it: what was sent, stamped with the
/// instant it was born at.
fn born(mut c: Customer, now: DateTime<Utc>) -> Customer {
    c.created_at = Some(now);
    c
}

/// The `suite-` rows of a listing, in the order answered.
async fn suite_list<W: World>(w: &W, adapter: &str) -> Vec<Customer> {
    w.repo()
        .list_customers()
        .await
        .unwrap_or_else(|e| panic!("{adapter}: list: {e:?}"))
        .into_iter()
        .filter(|c| c.id.starts_with("suite-"))
        .collect()
}

/// The birth fact the port promises for `c`: the row as sent, contact
/// and metadata included — the log is the system of record, and the
/// rebuild reproduces the row from this payload alone.
fn birth(c: &Customer) -> (String, Value) {
    (
        CUSTOMER_CREATED.to_string(),
        json!({
            "id": c.id,
            "name": c.name,
            "email": c.email,
            "phone": c.phone,
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
async fn a_customer_reads_back_whole_and_a_stranger_is_none<W: World>(w: &W, adapter: &str) {
    seed(w, adapter).await;
    let want = born(whole("suite-ab"), at(10));
    assert_eq!(
        w.repo().get_customer("suite-ab").await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );
    let listed = suite_list(w, adapter).await;
    assert!(listed.contains(&want), "{adapter}: {listed:?}");
    assert_eq!(
        w.repo().get_customer("suite-stranger").await.expect("get"),
        None,
        "{adapter}"
    );
}

/// A fresh create answers `true`, reads back as sent stamped with its
/// instant, and records exactly one birth fact carrying the row.
async fn a_created_customer_reads_back_whole_and_records_its_birth<W: World>(w: &W, adapter: &str) {
    let fresh = whole("suite-fresh");
    assert!(
        w.repo()
            .create_customer_at(&fresh, at(30))
            .await
            .expect("create"),
        "{adapter}: a fresh id is a birth"
    );
    assert_eq!(
        w.repo().get_customer("suite-fresh").await.expect("get"),
        Some(born(fresh.clone(), at(30))),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![birth(&fresh)], "{adapter}");
}

/// A create on a held id is the idempotent re-checkout: it answers
/// `false`, leaves the held row exactly as it was — even when the new
/// body carries another row's email — and records nothing.
async fn a_create_on_a_held_id_answers_false_changes_nothing_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let mut renamed = whole("suite-ab");
    renamed.name = "Someone Else".into();
    renamed.phone = None;
    let mut claims_another = cust("suite-ab", Some("B@SUITE.TEST"));
    claims_another.name = "Impostor".into();
    for body in [renamed, claims_another] {
        assert!(
            !w.repo()
                .create_customer_at(&body, at(40))
                .await
                .unwrap_or_else(|e| panic!("{adapter}: re-create {}: {e:?}", body.name)),
            "{adapter}: a held id is not a birth"
        );
    }
    assert_eq!(
        w.repo().get_customer("suite-ab").await.expect("get"),
        Some(born(whole("suite-ab"), at(10))),
        "{adapter}: the held row is untouched"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}

/// A DIFFERENT id carrying a registered email — in any letter case — is
/// a caller bug: `Invalid` naming the email, no row, no fact.
async fn a_registered_email_under_a_new_id_is_invalid_and_lands_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    let before = w.facts().await;
    let impostor = cust("suite-impostor", Some("SUITE-AB@suite.test"));
    match w.repo().create_customer_at(&impostor, at(40)).await {
        Err(CustomersError::Invalid(m)) => {
            assert!(m.contains("SUITE-AB@suite.test"), "{adapter}: {m}")
        }
        other => panic!("{adapter}: a registered email under a new id: {other:?}"),
    }
    assert_eq!(
        w.repo().get_customer("suite-impostor").await.expect("get"),
        None,
        "{adapter}: no row"
    );
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}

/// The unique key is `types::email_key`, as the port words it (backlog
/// e1b08aaa): surrounding ASCII whitespace and ASCII letter case do not
/// make a second address; non-ASCII case and non-ASCII space do, on both
/// adapters alike — never by the database's locale. A refused address
/// lands no row and records nothing.
async fn an_email_is_one_address_padded_or_in_ascii_case_but_not_in_other_case<W: World>(
    w: &W,
    adapter: &str,
) {
    seed(w, adapter).await;
    // The held one is `b@suite.test` (suite-B).
    for (id, email) in [
        ("suite-padded", " b@suite.test"),
        ("suite-cased", "B@Suite.Test"),
        ("suite-both", "\tB@SUITE.TEST\r\n"),
    ] {
        let before = w.facts().await;
        match w
            .repo()
            .create_customer_at(&cust(id, Some(email)), at(40))
            .await
        {
            Err(CustomersError::Invalid(m)) => assert!(m.contains(email), "{adapter}: {m}"),
            other => panic!("{adapter}: {email:?} is the held address: {other:?}"),
        }
        assert_eq!(
            w.repo().get_customer(id).await.expect("get"),
            None,
            "{adapter}: no row for {email:?}"
        );
        assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
    }
    // Non-ASCII case is not folded and non-ASCII space is not trimmed:
    // each of these is an address of its own.
    for (id, email) in [
        ("suite-e-acute", "\u{e9}lise@suite.test"),
        ("suite-E-acute", "\u{c9}lise@suite.test"),
        ("suite-nbsp", "\u{a0}b@suite.test"),
    ] {
        assert!(
            w.repo()
                .create_customer_at(&cust(id, Some(email)), at(40))
                .await
                .unwrap_or_else(|e| panic!("{adapter}: {email:?}: {e:?}")),
            "{adapter}: {email:?} is its own address"
        );
    }
    // The ASCII letters around a non-ASCII one still fold.
    assert!(
        matches!(
            w.repo()
                .create_customer_at(
                    &cust("suite-E-shout", Some("\u{c9}LISE@SUITE.TEST")),
                    at(40)
                )
                .await,
            Err(CustomersError::Invalid(_))
        ),
        "{adapter}: the ASCII letters of a held non-ASCII address fold"
    );
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
        .create_customer_at(&cust("suite-fine", None), fine)
        .await
        .expect("create");
    let got = w
        .repo()
        .get_customer("suite-fine")
        .await
        .expect("get")
        .and_then(|c| c.created_at);
    assert_eq!(got, Some(micro), "{adapter}");
}

/// A NUL byte cannot be stored in TEXT or JSONB. A write carrying one —
/// in any field, or anywhere inside the metadata — is refused `Invalid`
/// naming the field and writes nothing. A read keyed by one is the miss
/// it is: no stored id can hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    let mut id = cust("suite-n\0ul", None);
    id.name = "Nul Id".into();
    let mut name = cust("suite-nul-name", None);
    name.name = "Nu\0l".into();
    let email = cust("suite-nul-email", Some("nu\0l@suite.test"));
    let mut phone = cust("suite-nul-phone", None);
    phone.phone = Some("555\u{0}0100".into());
    let mut value = cust("suite-nul-value", None);
    value.metadata = json!({"source": ["shop", "a\0b"]});
    let mut key = cust("suite-nul-key", None);
    key.metadata = json!({"nested": {"a\0b": 1}});
    for (field, bad) in [
        ("id", id),
        ("name", name),
        ("email", email),
        ("phone", phone),
        ("metadata", value),
        ("metadata", key),
    ] {
        match w.repo().create_customer_at(&bad, at(60)).await {
            Err(CustomersError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    assert_eq!(suite_list(w, adapter).await, vec![], "{adapter}: no row");
    assert_eq!(w.facts().await, vec![], "{adapter}: nothing recorded");
    assert_eq!(
        w.repo().get_customer("suite-n\0ul").await.expect("a miss"),
        None,
        "{adapter}"
    );
}
