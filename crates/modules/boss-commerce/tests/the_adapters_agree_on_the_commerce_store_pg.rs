//! The invoice store answers the same on both `CommerceRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the next MODULE port of the
//! census after the products store (backlog be459ab9). Tests elsewhere
//! (the HTTP suites under `tests/common`) are answered by
//! `InMemoryCommerce`, while production is answered by `PgCommerce`.
//!
//! WHY IT EXISTS. The port was held to two adapters on two contracts
//! (`an_invoice_is_created_once_per_id.rs`, `invoice_transitions.rs`)
//! and to one adapter on the rest; this file is the one statement of
//! every method of the port, each case holding each adapter to the same
//! stated answer, and every write also judged by the facts it leaves —
//! the double's `recorded_events` and Postgres's `event_outbox`, staged
//! in the write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - LIST ORDER. Postgres listed invoices `issued_on DESC` with ties in
//!   no stated order, and lines, accounts and revenue categories in the
//!   database's locale; the double listed in insertion order and summed
//!   accounts in byte order. Both now list newest issue first, ties and
//!   every text key in BYTE order (`COLLATE "C"`, backlog 2987fb2d).
//! - AS STORED. Postgres stores each line under the header's id and
//!   reads the lines back in id order, but answered a create with the
//!   body as sent; the double stored the body as sent. Both now store,
//!   answer and record the invoice as stored.
//! - REVENUE. Postgres derives revenue lines from the stored line items;
//!   the double answered whatever a test seeded through `with_revenue`,
//!   so a created invoice never reached it. The double now derives it
//!   from its own invoices, the same rollup.
//! - THE SUMMARY'S COUNT. The double counted only its seed invoices, so
//!   every invoice created through the port was missing from it.
//! - TAX. The double refused a taxed invoice (it summed lines against
//!   the total with no tax) and accepted a negative tax or a tax with no
//!   jurisdiction; both now judge one body the same way.
//! - REFUSALS. A malformed body was refused `Storage` (a 500) by both,
//!   and Postgres refused a three-letter-less currency, an unknown
//!   payment method, a repeated line id and a NUL byte with its own
//!   constraint or encoding error, also `Storage`, while the double
//!   stored them. Both now refuse each `Invalid` (a 400) naming the
//!   field; a line id another invoice holds is a `Conflict` (a 409); a
//!   negative page bound is `Invalid` rather than Postgres's error or
//!   the double's wrapped `usize`.
//!
//! NOT JUDGED HERE, on purpose: the summary's REVENUE half (TTM revenue,
//! COGS, margin, revenue by month). Postgres reads it from the general
//! ledger, which is the ledger module's store and holds postings no
//! invoice made; the double has no ledger and answers it empty. The AR
//! half and the count are invoices alone, and are judged.
//!
//! The world: the suite's invoices are written THROUGH the port, so
//! each adapter seeds itself the way production does. Postgres posts
//! each invoice to the ledger, which needs its revenue categories
//! declared; the suite declares its own (`suite-B`, `suite-a-z`,
//! `suite-ab` — a case pair and a punctuation pair, the only shape that
//! catches the database's locale disagreeing with the double's byte
//! order).

use boss_commerce::events::{
    INVOICE_CREATED, INVOICE_PAID, INVOICE_PAST_DUE, INVOICE_WRITTEN_OFF, invoice_created_payload,
};
use boss_commerce::port::{CommerceError, CommerceRepository, InvoiceCreate, PAYMENT_METHODS};
use boss_commerce::types::{
    AccountOpenAr, ArAgingBucket, Invoice, InvoiceLineItem, InvoiceStatus, RevenueCategory,
    RevenueLine,
};
use boss_commerce::{InMemoryCommerce, PgCommerce};
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::Value;

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: CommerceRepository;
    fn repo(&self) -> &Self::R;
    /// Every fact about a `suite-` invoice, as `(kind, payload)`, in the
    /// order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(payload: &Value) -> bool {
    payload["id"]
        .as_str()
        .is_some_and(|v| v.starts_with("suite-"))
}

struct InMemory(InMemoryCommerce);

impl World for InMemory {
    type R = InMemoryCommerce;
    fn repo(&self) -> &InMemoryCommerce {
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
    repo: PgCommerce,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgCommerce;
    fn repo(&self) -> &PgCommerce {
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

/// The suite's revenue categories, declared the way a tenant declares
/// one — a Class naming its GL account — so the ledger posting each
/// Postgres create makes can find an account.
async fn declare_suite_categories(pool: &sqlx::PgPool) {
    for code in CATEGORIES {
        sqlx::query(
            "INSERT INTO classes \
             (subject_kind, code, display_name, member_attribute, metadata, sort_order) \
             VALUES ('invoice', $1, $1, 'revenue_category', '{\"gl_account\": \"4100\"}'::jsonb, 0)",
        )
        .bind(code)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("declaring {code}: {e}"));
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryCommerce::new(Vec::new())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            declare_suite_categories(&db.pool).await;
            let world = Postgres {
                repo: PgCommerce::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        invoices_list_newest_issue_first_with_ties_in_byte_order_of_id,
        a_page_counts_every_match_and_an_account_narrows_it,
        a_negative_page_bound_is_refused_invalid,
        an_invoice_is_answered_read_and_listed_as_stored,
        a_create_records_once_and_a_repeat_or_a_rival_body_records_nothing,
        a_taxed_invoice_counts_its_tax_in_the_total,
        every_payment_method_the_store_names_is_accepted,
        a_malformed_invoice_is_refused_invalid_naming_what_is_wrong,
        a_line_id_another_invoice_holds_is_refused_conflict,
        paying_moves_an_owed_invoice_once_and_keeps_the_first_paid_on,
        past_due_moves_only_an_owed_invoice,
        a_write_off_answers_true_once_and_records_once,
        open_ar_sums_owed_invoices_per_account_in_byte_order,
        revenue_rolls_lines_up_by_month_newest_first_and_category_in_byte_order,
        the_summary_ages_the_owed_invoices_and_counts_every_invoice,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

const CATEGORIES: [&str; 3] = ["suite-B", "suite-a-z", "suite-ab"];

/// An instant `secs` after a fixed origin, carrying nanoseconds no
/// column keeps (none of this port's answers carries an instant).
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_789)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new("commerce", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

/// Day `n` of 2026.
fn day(n: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1).expect("a date") + chrono::Days::new(n)
}

fn line(invoice: &str, id: &str, category: &str, cents: i64) -> InvoiceLineItem {
    InvoiceLineItem {
        id: id.into(),
        invoice_id: invoice.into(),
        revenue_category: RevenueCategory::from(category),
        amount_cents: cents,
        currency: "USD".into(),
        description: format!("Line {id}"),
        ref_id: None,
        sku: None,
        qty: None,
        cost_basis_cents: None,
        cost_total_cents: None,
    }
}

/// An outstanding, untaxed invoice issued on `issued`, due thirty days
/// later, its total the sum of its lines.
fn invoice(id: &str, account: &str, issued: NaiveDate, lines: &[(&str, &str, i64)]) -> Invoice {
    let line_items: Vec<InvoiceLineItem> = lines
        .iter()
        .map(|(l, c, cents)| line(id, l, c, *cents))
        .collect();
    Invoice {
        id: id.into(),
        account_id: account.into(),
        issued_on: issued,
        due_on: issued + chrono::Days::new(30),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: line_items.iter().map(|l| l.amount_cents).sum(),
        currency: "USD".into(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items,
    }
}

/// One line of `cents` in category `suite-ab`.
fn simple(id: &str, account: &str, issued: NaiveDate, cents: i64) -> Invoice {
    invoice(
        id,
        account,
        issued,
        &[(&format!("{id}-l1"), "suite-ab", cents)],
    )
}

/// The invoice as the store keeps it: every line under the header's id,
/// the lines in byte order of id.
fn stored(inv: &Invoice) -> Invoice {
    let mut s = inv.clone();
    for l in &mut s.line_items {
        l.invoice_id = inv.id.clone();
    }
    s.line_items.sort_by(|a, b| a.id.cmp(&b.id));
    s
}

async fn create<W: World>(w: &W, inv: &Invoice, secs: i64) -> Result<InvoiceCreate, CommerceError> {
    w.repo()
        .create_invoice_at(inv, at(secs), &stamp(secs))
        .await
}

async fn created<W: World>(w: &W, adapter: &str, inv: &Invoice, secs: i64) {
    match create(w, inv, secs).await {
        Ok(InvoiceCreate::Created(_)) => {}
        other => panic!("{adapter}: create {}: {other:?}", inv.id),
    }
}

async fn suite_ids<W: World>(w: &W, adapter: &str) -> Vec<String> {
    w.repo()
        .all_invoices()
        .await
        .unwrap_or_else(|e| panic!("{adapter}: all_invoices: {e:?}"))
        .into_iter()
        .map(|i| i.id)
        .filter(|id| id.starts_with("suite-"))
        .collect()
}

async fn page<W: World>(
    w: &W,
    adapter: &str,
    limit: i64,
    offset: i64,
    account: Option<&str>,
) -> (Vec<String>, i64) {
    let (rows, total) = w
        .repo()
        .list_invoices(limit, offset, account)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: list {limit}/{offset}/{account:?}: {e:?}"));
    (rows.into_iter().map(|i| i.id).collect(), total)
}

async fn get<W: World>(w: &W, adapter: &str, id: &str) -> Option<Invoice> {
    w.repo()
        .invoice_by_id(id)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: invoice_by_id {id:?}: {e:?}"))
}

/// Five invoices on two accounts: three issued the same day, a case
/// pair and a punctuation pair among their ids, one older, one newer.
async fn seed_five<W: World>(w: &W, adapter: &str) {
    for (i, inv) in [
        simple("suite-old", "suite-acct-1", day(5), 1_000),
        simple("suite-ab", "suite-acct-1", day(10), 2_000),
        simple("suite-B", "suite-acct-2", day(10), 3_000),
        simple("suite-a-z", "suite-acct-1", day(10), 4_000),
        simple("suite-new", "suite-acct-2", day(20), 5_000),
    ]
    .iter()
    .enumerate()
    {
        created(w, adapter, inv, i as i64).await;
    }
}

// ----- cases ---------------------------------------------------------------

/// Every invoice lists newest issue first; invoices issued the same day
/// list in BYTE order of id — `suite-B` < `suite-a-z` < `suite-ab`, which
/// a locale collation orders the other way round.
async fn invoices_list_newest_issue_first_with_ties_in_byte_order_of_id<W: World>(
    w: &W,
    adapter: &str,
) {
    seed_five(w, adapter).await;
    assert_eq!(
        suite_ids(w, adapter).await,
        ["suite-new", "suite-B", "suite-a-z", "suite-ab", "suite-old"],
        "{adapter}"
    );
}

/// A page is a window on that order and its total counts every match;
/// an account narrows both, an account with no invoice is an empty page
/// of zero, and an offset past the end is an empty page of the total.
async fn a_page_counts_every_match_and_an_account_narrows_it<W: World>(w: &W, adapter: &str) {
    seed_five(w, adapter).await;
    assert_eq!(
        page(w, adapter, 2, 1, None).await,
        (vec!["suite-B".into(), "suite-a-z".into()], 5),
        "{adapter}"
    );
    assert_eq!(
        page(w, adapter, 10, 0, Some("suite-acct-1")).await,
        (
            vec!["suite-a-z".into(), "suite-ab".into(), "suite-old".into()],
            3
        ),
        "{adapter}"
    );
    assert_eq!(
        page(w, adapter, 1, 1, Some("suite-acct-1")).await,
        (vec!["suite-ab".into()], 3),
        "{adapter}"
    );
    assert_eq!(
        page(w, adapter, 10, 10, None).await,
        (vec![], 5),
        "{adapter}"
    );
    assert_eq!(
        page(w, adapter, 10, 0, Some("suite-stranger")).await,
        (vec![], 0),
        "{adapter}"
    );
}

/// A negative limit or offset names no page: refused `Invalid` naming
/// which, by both — Postgres errored on it and the double wrapped it
/// into a `usize`.
async fn a_negative_page_bound_is_refused_invalid<W: World>(w: &W, adapter: &str) {
    seed_five(w, adapter).await;
    for (limit, offset, field) in [(-1, 0, "limit"), (10, -1, "offset")] {
        match w.repo().list_invoices(limit, offset, None).await {
            Err(CommerceError::Invalid(m)) => assert!(m.contains(field), "{adapter}: {m}"),
            other => panic!("{adapter}: {limit}/{offset}: {other:?}"),
        }
    }
}

/// A create answers the invoice AS STORED — its lines under the
/// header's id whatever they carried, in byte order of line id — and
/// that is what a read by id and the list answer too. An id nobody
/// created is `None`, not an error.
async fn an_invoice_is_answered_read_and_listed_as_stored<W: World>(w: &W, adapter: &str) {
    let mut body = invoice(
        "suite-inv",
        "suite-acct-1",
        day(10),
        &[
            ("suite-l-ab", "suite-ab", 300),
            ("suite-l-B", "suite-B", 100),
            ("suite-l-a-z", "suite-a-z", 200),
        ],
    );
    body.line_items[1].invoice_id = "suite-elsewhere".into();
    body.line_items[0].ref_id = Some("job-1".into());
    body.line_items[0].sku = Some("sku-1".into());
    body.line_items[0].qty = Some(3);
    body.payment_method = Some("ach".into());
    let want = stored(&body);
    assert_eq!(
        want.line_items
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>(),
        ["suite-l-B", "suite-l-a-z", "suite-l-ab"]
    );
    match create(w, &body, 1).await {
        Ok(InvoiceCreate::Created(got)) => assert_eq!(got, want, "{adapter}"),
        other => panic!("{adapter}: create: {other:?}"),
    }
    assert_eq!(
        get(w, adapter, "suite-inv").await,
        Some(want.clone()),
        "{adapter}"
    );
    let listed = w.repo().all_invoices().await.expect("all");
    assert!(listed.contains(&want), "{adapter}: {listed:?}");
    let (paged, _) = w.repo().list_invoices(10, 0, None).await.expect("page");
    assert!(paged.contains(&want), "{adapter}: {paged:?}");
    assert_eq!(get(w, adapter, "suite-stranger").await, None, "{adapter}");
}

/// The first create records `commerce.invoice.created` carrying the
/// invoice as stored. The same body again is answered `AlreadyCreated`
/// with the stored invoice and records nothing; a body that differs in
/// a field fixed at issuance is refused `Conflict` and writes nothing.
async fn a_create_records_once_and_a_repeat_or_a_rival_body_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut body = invoice(
        "suite-once",
        "suite-acct-1",
        day(10),
        &[("suite-o-2", "suite-ab", 70), ("suite-o-1", "suite-B", 30)],
    );
    body.line_items[0].invoice_id = "suite-elsewhere".into();
    let want = stored(&body);
    created(w, adapter, &body, 1).await;
    assert_eq!(
        w.facts().await,
        vec![fact(1, INVOICE_CREATED, invoice_created_payload(&want))],
        "{adapter}"
    );
    match create(w, &body, 2).await {
        Ok(InvoiceCreate::AlreadyCreated(got)) => assert_eq!(got, want, "{adapter}"),
        other => panic!("{adapter}: repeat: {other:?}"),
    }
    let mut rival = body.clone();
    rival.due_on = day(90);
    match create(w, &rival, 3).await {
        Err(CommerceError::Conflict(m)) => assert!(m.contains("due_on"), "{adapter}: {m}"),
        other => panic!("{adapter}: rival: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-once").await, Some(want), "{adapter}");
    assert_eq!(w.facts().await.len(), 1, "{adapter}: recorded once");
}

/// A taxed invoice's total is its lines plus its tax: accepted by both,
/// read back with the tax and its jurisdiction, and recorded with the
/// tax lines the created payload derives from them.
async fn a_taxed_invoice_counts_its_tax_in_the_total<W: World>(w: &W, adapter: &str) {
    let mut body = simple("suite-tax", "suite-acct-1", day(10), 10_000);
    body.tax_cents = 825;
    body.tax_jurisdiction = Some("US-CA".into());
    body.amount_cents = 10_825;
    created(w, adapter, &body, 1).await;
    assert_eq!(
        get(w, adapter, "suite-tax").await,
        Some(stored(&body)),
        "{adapter}"
    );
    assert_eq!(
        w.facts().await,
        vec![fact(
            1,
            INVOICE_CREATED,
            invoice_created_payload(&stored(&body))
        )],
        "{adapter}"
    );
}

/// Every payment method the port names is one the store keeps — on
/// Postgres that holds the list equal to the column's CHECK.
async fn every_payment_method_the_store_names_is_accepted<W: World>(w: &W, adapter: &str) {
    for (i, method) in PAYMENT_METHODS.iter().enumerate() {
        let mut body = simple(&format!("suite-pm-{method}"), "suite-acct-1", day(10), 100);
        body.payment_method = Some((*method).into());
        created(w, adapter, &body, i as i64).await;
        assert_eq!(
            get(w, adapter, &body.id).await,
            Some(stored(&body)),
            "{adapter}"
        );
    }
}

/// A body the store cannot keep as a true document is refused `Invalid`
/// — the caller's mistake, a 400, never a 500 — naming what is wrong,
/// and writes nothing and records nothing.
async fn a_malformed_invoice_is_refused_invalid_naming_what_is_wrong<W: World>(
    w: &W,
    adapter: &str,
) {
    let base = || simple("suite-bad", "suite-acct-1", day(10), 1_000);
    let with = |f: &dyn Fn(&mut Invoice)| {
        let mut inv = base();
        f(&mut inv);
        inv
    };
    let bodies = [
        ("line items", with(&|i| i.line_items.clear())),
        ("amount_cents", with(&|i| i.amount_cents = 999)),
        (
            "tax_cents",
            with(&|i| {
                i.tax_cents = -1;
                i.amount_cents = 999;
                i.tax_jurisdiction = Some("US-CA".into());
            }),
        ),
        (
            "tax_jurisdiction",
            with(&|i| {
                i.tax_cents = 10;
                i.amount_cents = 1_010;
            }),
        ),
        (
            "currency",
            with(&|i| i.line_items[0].currency = "EUR".into()),
        ),
        (
            "currency",
            with(&|i| {
                i.currency = "US".into();
                i.line_items[0].currency = "US".into();
            }),
        ),
        (
            "payment_method",
            with(&|i| i.payment_method = Some("barter".into())),
        ),
        (
            "suite-bad-l1",
            with(&|i| {
                let again = i.line_items[0].clone();
                i.line_items.push(again);
                i.amount_cents = 2_000;
            }),
        ),
    ];
    for (needle, body) in bodies {
        match create(w, &body, 1).await {
            Err(CommerceError::Invalid(m)) => assert!(m.contains(needle), "{adapter}: {m}"),
            other => panic!("{adapter}: {needle}: {other:?}"),
        }
    }
    assert_eq!(get(w, adapter, "suite-bad").await, None, "{adapter}");
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// A line id is one line's for its life: a create whose line id another
/// invoice already holds is refused `Conflict` naming the line, and the
/// second invoice is not written.
async fn a_line_id_another_invoice_holds_is_refused_conflict<W: World>(w: &W, adapter: &str) {
    let first = invoice(
        "suite-first",
        "suite-acct-1",
        day(10),
        &[("suite-shared", "suite-ab", 100)],
    );
    let second = invoice(
        "suite-second",
        "suite-acct-1",
        day(10),
        &[("suite-shared", "suite-ab", 100)],
    );
    created(w, adapter, &first, 1).await;
    match create(w, &second, 2).await {
        Err(CommerceError::Conflict(m)) => assert!(m.contains("suite-shared"), "{adapter}: {m}"),
        other => panic!("{adapter}: second: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-second").await, None, "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-first").await,
        Some(stored(&first)),
        "{adapter}"
    );
    assert_eq!(w.facts().await.len(), 1, "{adapter}: only the first");
}

/// Paying an owed invoice — outstanding or past due — moves it and
/// records `commerce.invoice.paid` with the moved invoice. Paying it
/// again is Ok, records nothing, and keeps the first `paid_on`.
async fn paying_moves_an_owed_invoice_once_and_keeps_the_first_paid_on<W: World>(
    w: &W,
    adapter: &str,
) {
    let owed = simple("suite-pay", "suite-acct-1", day(10), 500);
    let mut late = simple("suite-late", "suite-acct-1", day(10), 600);
    late.status = InvoiceStatus::PAST_DUE.into();
    created(w, adapter, &owed, 1).await;
    created(w, adapter, &late, 2).await;
    let before = w.facts().await;
    w.repo()
        .mark_invoice_paid_at("suite-pay", day(40), &stamp(3))
        .await
        .expect("pay");
    w.repo()
        .mark_invoice_paid_at("suite-late", day(41), &stamp(4))
        .await
        .expect("pay late");
    w.repo()
        .mark_invoice_paid_at("suite-pay", day(45), &stamp(5))
        .await
        .expect("again");
    let paid = |inv: &Invoice, on: NaiveDate| Invoice {
        status: InvoiceStatus::PAID.into(),
        paid_on: Some(on),
        ..stored(inv)
    };
    assert_eq!(
        get(w, adapter, "suite-pay").await,
        Some(paid(&owed, day(40))),
        "{adapter}"
    );
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![
            fact(
                3,
                INVOICE_PAID,
                serde_json::to_value(paid(&owed, day(40))).unwrap()
            ),
            fact(
                4,
                INVOICE_PAID,
                serde_json::to_value(paid(&late, day(41))).unwrap()
            ),
        ],
        "{adapter}"
    );
    match w
        .repo()
        .mark_invoice_paid_at("suite-stranger", day(40), &stamp(6))
        .await
    {
        Err(CommerceError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: stranger: {other:?}"),
    }
}

/// An outstanding invoice moves to past due once and records
/// `commerce.invoice.past_due`; again is Ok and records nothing. A paid
/// or written-off invoice is refused `Conflict` and stays put; an id
/// nobody created is `NotFound`.
async fn past_due_moves_only_an_owed_invoice<W: World>(w: &W, adapter: &str) {
    let owed = simple("suite-owed", "suite-acct-1", day(10), 500);
    let paid = simple("suite-paid", "suite-acct-1", day(10), 600);
    let gone = simple("suite-gone", "suite-acct-1", day(10), 700);
    for (i, inv) in [&owed, &paid, &gone].into_iter().enumerate() {
        created(w, adapter, inv, i as i64).await;
    }
    let r = w.repo();
    r.mark_invoice_paid_at("suite-paid", day(20), &stamp(5))
        .await
        .expect("pay");
    assert!(
        r.mark_invoice_written_off("suite-gone", &stamp(6))
            .await
            .expect("write off")
    );
    let before = w.facts().await;
    r.mark_invoice_past_due("suite-owed", &stamp(7))
        .await
        .expect("past due");
    r.mark_invoice_past_due("suite-owed", &stamp(8))
        .await
        .expect("again");
    for id in ["suite-paid", "suite-gone"] {
        match r.mark_invoice_past_due(id, &stamp(9)).await {
            Err(CommerceError::Conflict(m)) => assert!(m.contains(id), "{adapter}: {m}"),
            other => panic!("{adapter}: {id}: {other:?}"),
        }
    }
    match r.mark_invoice_past_due("suite-stranger", &stamp(9)).await {
        Err(CommerceError::NotFound(_)) => {}
        other => panic!("{adapter}: stranger: {other:?}"),
    }
    let moved = Invoice {
        status: InvoiceStatus::PAST_DUE.into(),
        ..stored(&owed)
    };
    assert_eq!(
        get(w, adapter, "suite-owed").await,
        Some(moved.clone()),
        "{adapter}"
    );
    assert_eq!(
        get(w, adapter, "suite-paid").await.map(|i| i.status),
        Some(InvoiceStatus::PAID.into()),
        "{adapter}"
    );
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![fact(
            7,
            INVOICE_PAST_DUE,
            serde_json::to_value(&moved).unwrap()
        )],
        "{adapter}"
    );
}

/// A write-off answers `true` on the call that moved the invoice and
/// records `commerce.invoice.written_off` with it; the same drive again
/// answers `false` and records nothing. A paid invoice is refused
/// `Conflict`; an id nobody created is `NotFound`.
async fn a_write_off_answers_true_once_and_records_once<W: World>(w: &W, adapter: &str) {
    let mut late = simple("suite-wo", "suite-acct-1", day(10), 900);
    late.status = InvoiceStatus::PAST_DUE.into();
    let paid = simple("suite-wo-paid", "suite-acct-1", day(10), 100);
    created(w, adapter, &late, 1).await;
    created(w, adapter, &paid, 2).await;
    let r = w.repo();
    r.mark_invoice_paid_at("suite-wo-paid", day(20), &stamp(3))
        .await
        .expect("pay");
    let before = w.facts().await;
    assert!(
        r.mark_invoice_written_off("suite-wo", &stamp(4))
            .await
            .expect("first"),
        "{adapter}"
    );
    assert!(
        !r.mark_invoice_written_off("suite-wo", &stamp(5))
            .await
            .expect("again"),
        "{adapter}"
    );
    match r.mark_invoice_written_off("suite-wo-paid", &stamp(6)).await {
        Err(CommerceError::Conflict(m)) => assert!(m.contains("suite-wo-paid"), "{adapter}: {m}"),
        other => panic!("{adapter}: paid: {other:?}"),
    }
    match r
        .mark_invoice_written_off("suite-stranger", &stamp(6))
        .await
    {
        Err(CommerceError::NotFound(_)) => {}
        other => panic!("{adapter}: stranger: {other:?}"),
    }
    let gone = Invoice {
        status: InvoiceStatus::WRITTEN_OFF.into(),
        ..stored(&late)
    };
    assert_eq!(
        get(w, adapter, "suite-wo").await,
        Some(gone.clone()),
        "{adapter}"
    );
    let recorded: Vec<_> = w.facts().await.into_iter().skip(before.len()).collect();
    assert_eq!(
        recorded,
        vec![fact(
            4,
            INVOICE_WRITTEN_OFF,
            serde_json::to_value(&gone).unwrap()
        )],
        "{adapter}"
    );
}

/// Open AR is one row per account still owed anything, in BYTE order of
/// account id, summing only the owed invoices: paid and written-off are
/// out, and an account owed nothing has no row.
async fn open_ar_sums_owed_invoices_per_account_in_byte_order<W: World>(w: &W, adapter: &str) {
    let invoices = [
        simple("suite-1", "suite-ab", day(10), 100),
        simple("suite-2", "suite-ab", day(11), 200),
        simple("suite-3", "suite-B", day(10), 400),
        simple("suite-4", "suite-a-z", day(10), 800),
        simple("suite-5", "suite-a-z", day(10), 1_600),
        simple("suite-6", "suite-a-z", day(10), 3_200),
        simple("suite-7", "suite-settled", day(10), 6_400),
    ];
    for (i, inv) in invoices.iter().enumerate() {
        created(w, adapter, inv, i as i64).await;
    }
    let r = w.repo();
    r.mark_invoice_past_due("suite-2", &stamp(10))
        .await
        .expect("past due");
    r.mark_invoice_paid_at("suite-5", day(20), &stamp(11))
        .await
        .expect("pay");
    assert!(
        r.mark_invoice_written_off("suite-6", &stamp(12))
            .await
            .expect("write off")
    );
    r.mark_invoice_paid_at("suite-7", day(20), &stamp(13))
        .await
        .expect("pay");
    let row = |account: &str, cents, count| AccountOpenAr {
        account_id: account.into(),
        open_ar_cents: cents,
        open_count: count,
    };
    let got: Vec<AccountOpenAr> = r
        .open_ar_by_account()
        .await
        .expect("open ar")
        .into_iter()
        .filter(|a| a.account_id.starts_with("suite-"))
        .collect();
    assert_eq!(
        got,
        vec![
            row("suite-B", 400, 1),
            row("suite-a-z", 800, 1),
            row("suite-ab", 300, 2),
        ],
        "{adapter}"
    );
}

/// Revenue is every stored line rolled up by the month its invoice was
/// issued and its category — newest month first, categories in BYTE
/// order — over every invoice, a written-off one included (the
/// write-off is a bad-debt expense, not a revenue reversal).
async fn revenue_rolls_lines_up_by_month_newest_first_and_category_in_byte_order<W: World>(
    w: &W,
    adapter: &str,
) {
    let invoices = [
        invoice(
            "suite-r1",
            "suite-acct-1",
            day(10),
            &[
                ("suite-r1-a", "suite-ab", 100),
                ("suite-r1-b", "suite-B", 200),
            ],
        ),
        invoice(
            "suite-r2",
            "suite-acct-1",
            day(15),
            &[
                ("suite-r2-a", "suite-a-z", 50),
                ("suite-r2-b", "suite-ab", 1),
            ],
        ),
        invoice(
            "suite-r3",
            "suite-acct-1",
            day(40),
            &[("suite-r3-a", "suite-ab", 7)],
        ),
    ];
    for (i, inv) in invoices.iter().enumerate() {
        created(w, adapter, inv, i as i64).await;
    }
    assert!(
        w.repo()
            .mark_invoice_written_off("suite-r3", &stamp(9))
            .await
            .expect("write off")
    );
    let month = |m| NaiveDate::from_ymd_opt(2026, m, 1).expect("a month");
    let rev = |m, category: &str, cents| RevenueLine {
        month: month(m),
        category: RevenueCategory::from(category),
        amount_cents: cents,
        currency: "USD".into(),
    };
    let got: Vec<RevenueLine> = w
        .repo()
        .all_revenue()
        .await
        .expect("revenue")
        .into_iter()
        .filter(|l| l.category.as_str().starts_with("suite-"))
        .collect();
    assert_eq!(
        got,
        vec![
            rev(2, "suite-ab", 7),
            rev(1, "suite-B", 200),
            rev(1, "suite-a-z", 50),
            rev(1, "suite-ab", 101),
        ],
        "{adapter}"
    );
}

/// The summary's AR half ages the owed invoices by days past due into
/// the five buckets and totals them; its count is every invoice the
/// store holds, of any status.
async fn the_summary_ages_the_owed_invoices_and_counts_every_invoice<W: World>(
    w: &W,
    adapter: &str,
) {
    let due = |id: &str, cents: i64, due_on: NaiveDate| Invoice {
        due_on,
        ..simple(id, "suite-acct-1", day(0), cents)
    };
    let invoices = [
        due("suite-s1", 1_000, day(110)),
        due("suite-s2", 2_000, day(80)),
        due("suite-s3", 3_000, day(50)),
        due("suite-s4", 4_000, day(0)),
        due("suite-s5", 5_000, day(50)),
        due("suite-s6", 6_000, day(0)),
    ];
    for (i, inv) in invoices.iter().enumerate() {
        created(w, adapter, inv, i as i64).await;
    }
    let r = w.repo();
    r.mark_invoice_past_due("suite-s3", &stamp(10))
        .await
        .expect("past due");
    r.mark_invoice_paid_at("suite-s5", day(60), &stamp(11))
        .await
        .expect("pay");
    assert!(
        r.mark_invoice_written_off("suite-s6", &stamp(12))
            .await
            .expect("write off")
    );
    let summary = r.invoice_summary(day(100)).await.expect("summary");
    let bucket = |label: &str, count, total_cents| ArAgingBucket {
        label: label.into(),
        count,
        total_cents,
    };
    assert_eq!(
        summary.ar_aging,
        vec![
            bucket("current", 1, 1_000),
            bucket("1-30", 1, 2_000),
            bucket("31-60", 1, 3_000),
            bucket("61-90", 0, 0),
            bucket("90+", 1, 4_000),
        ],
        "{adapter}"
    );
    assert_eq!(summary.total_outstanding_cents, 10_000, "{adapter}");
    assert_eq!(summary.total_invoice_count, 6, "{adapter}");
    assert_eq!(summary.currency, "USD", "{adapter}");
}

/// A NUL byte cannot be stored in TEXT. A create carrying one in any
/// text field of the header or a line is refused `Invalid` naming the
/// field and writes nothing. A read or a move keyed by one is the miss
/// it is: no stored id can hold one.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_read<W: World>(w: &W, adapter: &str) {
    let with = |f: &dyn Fn(&mut Invoice)| {
        let mut inv = simple("suite-nul", "suite-acct-1", day(10), 1_000);
        f(&mut inv);
        inv
    };
    let bodies = [
        ("id", with(&|i| i.id = "suite-n\0ul".into())),
        ("account_id", with(&|i| i.account_id = "a\0".into())),
        ("status", with(&|i| i.status = "out\0".into())),
        (
            "currency",
            with(&|i| {
                i.currency = "U\0D".into();
                i.line_items[0].currency = "U\0D".into();
            }),
        ),
        (
            "tax_jurisdiction",
            with(&|i| i.tax_jurisdiction = Some("US\0".into())),
        ),
        (
            "payment_method",
            with(&|i| i.payment_method = Some("a\0ch".into())),
        ),
        (
            "line_items[0].id",
            with(&|i| i.line_items[0].id = "l\0".into()),
        ),
        (
            "line_items[0].revenue_category",
            with(&|i| i.line_items[0].revenue_category = "suite-\0".into()),
        ),
        (
            "line_items[0].description",
            with(&|i| i.line_items[0].description = "d\0".into()),
        ),
        (
            "line_items[0].ref_id",
            with(&|i| i.line_items[0].ref_id = Some("r\0".into())),
        ),
        (
            "line_items[0].sku",
            with(&|i| i.line_items[0].sku = Some("s\0".into())),
        ),
    ];
    for (field, body) in bodies {
        match create(w, &body, 1).await {
            Err(CommerceError::Invalid(m)) => {
                assert!(m.contains(field) && m.contains("NUL"), "{adapter}: {m}")
            }
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    assert_eq!(
        suite_ids(w, adapter).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![], "{adapter}");
    let r = w.repo();
    assert_eq!(get(w, adapter, "suite-n\0ul").await, None, "{adapter}");
    assert_eq!(
        page(w, adapter, 10, 0, Some("a\0")).await,
        (vec![], 0),
        "{adapter}"
    );
    match r
        .mark_invoice_paid_at("suite-n\0ul", day(20), &stamp(2))
        .await
    {
        Err(CommerceError::NotFound(_)) => {}
        other => panic!("{adapter}: pay: {other:?}"),
    }
    match r.mark_invoice_past_due("suite-n\0ul", &stamp(2)).await {
        Err(CommerceError::NotFound(_)) => {}
        other => panic!("{adapter}: past due: {other:?}"),
    }
    match r.mark_invoice_written_off("suite-n\0ul", &stamp(2)).await {
        Err(CommerceError::NotFound(_)) => {}
        other => panic!("{adapter}: write off: {other:?}"),
    }
}
