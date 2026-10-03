//! Hexagonal port: `CommerceRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, Utc};

use crate::types::{AccountOpenAr, Invoice, InvoiceSummary, RevenueLine};

#[derive(Debug, thiserror::Error)]
pub enum CommerceError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// The request itself is wrong — a body no store can keep as a true
    /// document, a page bound that names no page. A 400. Until the
    /// adapters-agree suite (backlog be459ab9) these were `Storage`,
    /// so the caller's mistake read as the server's (a 500).
    #[error("invalid: {0}")]
    Invalid(String),
}

impl CommerceError {
    /// The refusal `InvoiceStatus::transition_to` answers `Refused` for,
    /// worded once so both adapters name the same transition
    /// (backlog 203ef806). A `Conflict`, so the API answers 409.
    pub fn refused_transition(id: &str, from: &str, to: &str) -> Self {
        Self::Conflict(format!(
            "invoice {id}: refused transition '{from}' -> '{to}': \
             paid and written-off are terminal, only an owed invoice moves"
        ))
    }

    /// The refusal of a create whose id already names a different
    /// invoice, worded once so both adapters name the same fields
    /// (backlog 9d2af748). A `Conflict`, so the API answers 409.
    pub fn another_invoice_under_this_id(id: &str, differing: &[&str]) -> Self {
        Self::Conflict(format!(
            "invoice {id}: this id already names an invoice that differs from this body in: {}",
            differing.join(", ")
        ))
    }

    /// The refusal of a create whose line id another invoice already
    /// holds, worded once so both adapters name the same line (backlog
    /// be459ab9: Postgres answered its primary-key error as `Storage`,
    /// a 500, and the double stored the line twice). A `Conflict`, so
    /// the API answers 409.
    pub fn line_id_taken(id: &str, line: &str) -> Self {
        Self::Conflict(format!(
            "invoice {id}: line id {line} is already held by another invoice"
        ))
    }

    /// The refusal of a page bound that names no page (backlog
    /// be459ab9). `Invalid`, so the API answers 400.
    pub fn negative_page_bound(limit: i64, offset: i64) -> Option<Self> {
        let field = if limit < 0 {
            "limit"
        } else if offset < 0 {
            "offset"
        } else {
            return None;
        };
        Some(Self::Invalid(format!(
            "{field} must not be negative (limit {limit}, offset {offset})"
        )))
    }
}

/// How an account may pay an invoice — the values `invoices.payment_method`
/// accepts (its CHECK in `23-commerce.sql`). A fact living twice, so it
/// is pinned: the adapters-agree suite creates an invoice under each on
/// Postgres, and the CHECK refuses any this list holds and it does not.
pub const PAYMENT_METHODS: [&str; 4] = ["ach", "wire", "check", "card"];

/// What [`CommerceRepository::create_invoice_at`] did with the id it was
/// handed (backlog 9d2af748). Both arms carry the invoice AS STORED.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum InvoiceCreate {
    /// The invoice was written, and `commerce.invoice.created` recorded
    /// with it.
    Created(Invoice),
    /// The id already named this invoice: nothing was written, nothing
    /// recorded.
    AlreadyCreated(Invoice),
}

impl InvoiceCreate {
    pub fn into_invoice(self) -> Invoice {
        match self {
            Self::Created(inv) | Self::AlreadyCreated(inv) => inv,
        }
    }
}

/// Read-only persistence port for invoices and revenue.
#[async_trait]
pub trait CommerceRepository: Send + Sync {
    /// Every stored line rolled up by the month its invoice was issued
    /// and its revenue category, over every invoice of any status:
    /// newest month first, categories in BYTE order (backlog be459ab9 —
    /// the double answered only what a test seeded, Postgres ordered
    /// categories in the database's locale).
    async fn all_revenue(&self) -> Result<Vec<RevenueLine>, CommerceError>;

    /// Return every invoice, as stored: newest `issued_on` first, ties
    /// in BYTE order of id — the order a page of `list_invoices` is a
    /// window on (backlog be459ab9: the double listed in insertion
    /// order, Postgres left ties unordered).
    async fn all_invoices(&self) -> Result<Vec<Invoice>, CommerceError>;

    /// Return a page of invoices with total count, in the order of
    /// `all_invoices`; a negative `limit` or `offset` is refused
    /// `Invalid`.
    /// `account_id` filters to a single account when `Some`. The account
    /// detail view uses this to scope the finance/A-R section.
    async fn list_invoices(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Invoice>, i64), CommerceError>;

    /// Open receivables per account, summed over EVERY invoice: one
    /// row per account that still owes anything (a status outside
    /// `InvoiceStatus::NOT_OWED`), in BYTE order of `account_id`. Unpaged,
    /// because it is an aggregate — its size is the number of accounts
    /// with open AR, not the number of invoices (backlog 5257bfa9).
    async fn open_ar_by_account(&self) -> Result<Vec<AccountOpenAr>, CommerceError>;

    /// Return a single invoice by ID, or `None` if not found.
    async fn invoice_by_id(&self, id: &str) -> Result<Option<Invoice>, CommerceError>;

    /// Create a new invoice. Convenience overload stamps
    /// `created_at = Utc::now()` and a platform-automation event
    /// stamp; handlers use `create_invoice_at` so the projection
    /// write and the audit_log event share one timestamp AND the
    /// caller's actor. See `docs/design/projection-rebuilders.md`.
    async fn create_invoice(&self, invoice: &Invoice) -> Result<Invoice, CommerceError> {
        let stamp = EventStamp::new(
            "commerce",
            boss_core::actor::ActorId::Automation("platform".into()),
        );
        self.create_invoice_at(invoice, stamp.timestamp, &stamp)
            .await
            .map(InvoiceCreate::into_invoice)
    }
    /// Persists the invoice and returns it as stored
    /// ([`Invoice::as_stored`]). A body [`Invoice::malformed`] names is
    /// refused `Invalid` and writes nothing; a line id another invoice
    /// holds is refused [`CommerceError::line_id_taken`].
    ///
    /// OUTBOX (transactional-audit-log phase 2): the adapter records
    /// the `commerce.invoice.created` event — enriched via `stamp` —
    /// inside the SAME transaction as the projection write, so the
    /// event and the state commit or abort together (and the
    /// subject_edges trigger can reject a ghost account_id BEFORE it
    /// becomes state). Callers no longer publish this kind
    /// post-commit; boss-event-relay moves it to audit_log + NATS.
    ///
    /// ONCE PER ID (backlog 9d2af748). An id that already names an
    /// invoice writes nothing and records nothing: the same body is
    /// answered [`InvoiceCreate::AlreadyCreated`] with the invoice as
    /// stored — a redelivered issue converges — and a body that differs
    /// from it in a field fixed at issuance
    /// ([`Invoice::issuance_differences`]) is refused as a `Conflict`
    /// naming those fields. Status and `paid_on` are not compared: the
    /// status verbs move them after issuance, and a create never moves
    /// them back. Until then every call recorded
    /// `commerce.invoice.created` again, and that event is what drives
    /// the finished-goods consume. Pinned on both adapters by
    /// `tests/an_invoice_is_created_once_per_id.rs`.
    async fn create_invoice_at(
        &self,
        invoice: &Invoice,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<InvoiceCreate, CommerceError>;

    /// Mark an invoice as paid (sets status='paid', paid_on=today).
    /// Convenience overload uses `Utc::now().date_naive()`.
    async fn mark_invoice_paid(&self, id: &str) -> Result<(), CommerceError> {
        let stamp = EventStamp::new(
            "commerce",
            boss_core::actor::ActorId::Automation("platform".into()),
        );
        self.mark_invoice_paid_at(id, stamp.timestamp.date_naive(), &stamp)
            .await
    }
    /// Records `commerce.invoice.paid` (full post-update row state)
    /// in the same transaction as the status flip — outbox phase 2.
    /// The three status verbs move an invoice only as
    /// `InvoiceStatus::transition_to` allows: an already-paid invoice
    /// is Ok with no event, a written-off one is refused by name.
    async fn mark_invoice_paid_at(
        &self,
        id: &str,
        paid_on: NaiveDate,
        stamp: &EventStamp,
    ) -> Result<(), CommerceError>;

    /// Mark an invoice as past-due (sets status='past-due'). The
    /// AR aging counterparty fires this on the inverse branch of
    /// the same probability roll that fires mark-paid, so an
    /// invoice's status flips to one or the other after the
    /// net-30-ish delay — never both, never neither.
    /// Records `commerce.invoice.past_due` (full post-update row
    /// state) in the same transaction as the flip — outbox phase 2.
    /// A paid or written-off invoice is refused by name
    /// (`InvoiceStatus::transition_to`): it had no guard on Pg, so
    /// either could re-enter the receivable (backlog 203ef806).
    async fn mark_invoice_past_due(
        &self,
        id: &str,
        stamp: &EventStamp,
    ) -> Result<(), CommerceError>;

    /// Flip an invoice to the terminal `written-off` status and record
    /// the bad-debt fact (DR 6700 / CR 1100 via the
    /// `finance.invoice.written_off` posting rule). Returns `true` when
    /// THIS call performed the flip, `false` when the row was already
    /// written off — the drive legitimately arrives once per past-due
    /// copy (the counterparty chain + the system webhook copy), so
    /// callers gate their event emit on `true` to keep the double
    /// delivery convergent. Writing off a `paid` invoice is a
    /// `Conflict` (`InvoiceStatus::transition_to`): the paid and
    /// past-due counterparty branches are mutually exclusive, so that
    /// drive means model drift, not a race.
    /// See the brewery `[counterparty.bad-debt-writeoff]` for the
    /// 60-day-after-past-due trigger that drives this in sim.
    /// Outbox phase 2: the `commerce.invoice.written_off` event
    /// records in the same transaction as the flip + fact, gated on
    /// the flip winning — so the emit-exactly-once contract the
    /// callers used to enforce by checking the returned bool is now
    /// structural.
    async fn mark_invoice_written_off(
        &self,
        id: &str,
        stamp: &EventStamp,
    ) -> Result<bool, CommerceError>;

    /// Aggregated financial summary for the Finance dashboard.
    /// SQL-aggregated server-side so the UI renders correct totals
    /// without downloading millions of rows.
    ///
    /// `today` is the reference date for AR-aging buckets ("days
    /// past due_on") and the TTM revenue window. The HTTP handler
    /// sources it from `ClockClient.now()` so sim-mode shows
    /// sim-today, not wallclock. Pre-Clock-as-service this used
    /// PostgreSQL's `CURRENT_DATE` (wallclock) which bucketed every
    /// sim-time invoice into "90+" against today's calendar.
    async fn invoice_summary(
        &self,
        today: chrono::NaiveDate,
    ) -> Result<InvoiceSummary, CommerceError>;
}
