//! Hexagonal port: `CommerceRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use crate::types::{Invoice, InvoiceSummary, RevenueLine};

#[derive(Debug, thiserror::Error)]
pub enum CommerceError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Read-only persistence port for invoices and revenue.
#[async_trait]
pub trait CommerceRepository: Send + Sync {
    /// Return all revenue lines ordered by month descending, then category.
    async fn all_revenue(&self) -> Result<Vec<RevenueLine>, CommerceError>;

    /// Return every invoice.
    async fn all_invoices(&self) -> Result<Vec<Invoice>, CommerceError>;

    /// Return a page of invoices with total count.
    /// `account_id` filters to a single account when `Some`. The account
    /// detail view uses this to scope the finance/A-R section.
    async fn list_invoices(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Invoice>, i64), CommerceError>;

    /// Return a single invoice by ID, or `None` if not found.
    async fn invoice_by_id(&self, id: &str) -> Result<Option<Invoice>, CommerceError>;

    /// Create a new invoice. Convenience overload stamps
    /// `created_at = Utc::now()`; handlers that emit a domain
    /// event use `create_invoice_at` so the projection write and
    /// the audit_log event share one timestamp. See
    /// `docs/design/projection-rebuilders.md`.
    async fn create_invoice(&self, invoice: &Invoice) -> Result<Invoice, CommerceError> {
        self.create_invoice_at(invoice, Utc::now()).await
    }
    /// Persists the invoice and returns the same invoice with
    /// `line_items[].cost_basis_cents` enriched from the FG
    /// inventory rows looked up during drawdown. Callers that
    /// emit the `commerce.invoice.created` audit event MUST emit
    /// the returned (enriched) invoice — emitting the input
    /// directly leaves cost_basis_cents=null on every line, and
    /// the `invoice_issued` posting rule's audit-log-replay path
    /// then can't recover COGS (DR 5100 / CR 1320) on rebuild.
    async fn create_invoice_at(
        &self,
        invoice: &Invoice,
        now: DateTime<Utc>,
    ) -> Result<Invoice, CommerceError>;

    /// Mark an invoice as paid (sets status='paid', paid_on=today).
    /// Convenience overload uses `Utc::now().date_naive()`.
    async fn mark_invoice_paid(&self, id: &str) -> Result<(), CommerceError> {
        self.mark_invoice_paid_at(id, Utc::now().date_naive()).await
    }
    async fn mark_invoice_paid_at(&self, id: &str, paid_on: NaiveDate)
    -> Result<(), CommerceError>;

    /// Mark an invoice as past-due (sets status='past-due'). The
    /// AR aging counterparty fires this on the inverse branch of
    /// the same probability roll that fires mark-paid, so an
    /// invoice's status flips to one or the other after the
    /// net-30-ish delay — never both, never neither.
    async fn mark_invoice_past_due(&self, id: &str) -> Result<(), CommerceError>;

    /// Flip an invoice to `written-off` status. Idempotent — re-running
    /// against an already-written-off row is a no-op. The receivable
    /// stops counting toward A/R on the GL via the
    /// `finance.invoice.written_off` posting rule (DR 6700 / CR 1100).
    /// See the brewery `[counterparty.bad-debt-writeoff]` for the
    /// 60-day-after-past-due trigger that drives this in sim.
    async fn mark_invoice_written_off(&self, id: &str) -> Result<(), CommerceError>;

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
