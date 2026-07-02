//! Domain event subjects for commerce operations.
//!
//! Per `docs/design/projection-rebuilders.md`: state events carry
//! the full `Invoice` row state (header + line_items) so the
//! rebuild path can reconstruct both `invoices` and
//! `invoice_line_items` from the event log alone.

/// Full Invoice payload on initial creation OR re-emission. Rebuild
/// UPSERTs the parent row and replaces the line_items wholesale.
pub const INVOICE_CREATED: &str = "commerce.invoice.created";

/// Full Invoice payload after the mark-paid update applies. Same
/// shape as CREATED — rebuild just replays the full state.
pub const INVOICE_PAID: &str = "commerce.invoice.paid";

/// Full Invoice payload after a past-due transition. Mirrors the
/// shape of INVOICE_PAID. Rebuild treats it as a status change on
/// the existing row.
pub const INVOICE_PAST_DUE: &str = "commerce.invoice.past_due";

/// Full Invoice payload after a write-off transition. Mirrors the
/// shape of INVOICE_PAST_DUE. Fired by the brewery's
/// `[counterparty.bad-debt-writeoff]` ~60 sim-days after a
/// past-due transition (or by an operator explicit decision via
/// `PUT /api/commerce/invoices/{id}/write-off`). The ledger
/// projects this to `finance.invoice.written_off` which posts
/// DR 6700 Bad Debt Expense / CR 1100 A/R, taking the receivable
/// off the books without flagging it as collected.
pub const INVOICE_WRITTEN_OFF: &str = "commerce.invoice.written_off";

/// Build the `tax_lines` array the ledger's `invoice_issued` posting
/// rule reads (`tax_lines[]` → CR 2300 Sales Tax Payable). Returns
/// `None` for zero-tax invoices so the payload stays clean.
///
/// SINGLE SOURCE OF TRUTH: both the live `finance.invoice.issued` fact
/// (`postgres.rs::create_invoice_at`) AND the `commerce.invoice.created`
/// audit event (`http.rs`, which serializes the `Invoice` struct — that
/// carries only scalar `tax_cents`/`tax_jurisdiction`, not `tax_lines`)
/// call this so the rebuilt fact gets the SAME `tax_lines` the live fact
/// got. Without the shared helper the two shapes drift and a rebuild
/// silently stops accruing 2300.
pub fn tax_lines_for(tax_cents: i64, tax_jurisdiction: Option<&str>) -> Option<serde_json::Value> {
    match (tax_cents > 0, tax_jurisdiction) {
        (true, Some(jurisdiction)) => Some(serde_json::json!([{
            "account": "2300",
            "jurisdiction": jurisdiction,
            "amount_cents": tax_cents,
        }])),
        _ => None,
    }
}

/// Full `commerce.invoice.created` / `finance.invoice.issued` payload:
/// the serialized `Invoice` (header + line_items) plus the derived
/// `tax_lines`. SINGLE SOURCE OF TRUTH — the live in-tx fact
/// (`postgres.rs::create_invoice_at`) and the emitted
/// `commerce.invoice.created` event (`http.rs`) both build the payload
/// from this, off the SAME enriched invoice, so the live fact and the
/// fact rebuilt from the event are byte-identical (modulo the publisher
/// envelope, which `rebuild_facts::strip_envelope` removes). The event
/// must carry the full struct because it ALSO rebuilds the `invoices`
/// projection (`rebuild.rs` `from_value::<Invoice>`), so the fact grows
/// to match it rather than the event shrinking.
pub fn invoice_created_payload(inv: &crate::types::Invoice) -> serde_json::Value {
    let mut payload = serde_json::to_value(inv).unwrap_or_default();
    if let (Some(obj), Some(tax_lines)) = (
        payload.as_object_mut(),
        tax_lines_for(inv.tax_cents, inv.tax_jurisdiction.as_deref()),
    ) {
        obj.insert("tax_lines".into(), tax_lines);
    }
    payload
}

/// Service agreement upserted (POST /api/commerce/agreements).
/// Payload mirrors the full `ServiceAgreement` row state. Status
/// changes ride on the same event kind via the handler's
/// ON CONFLICT DO UPDATE path. `rebuild_commerce` consumes this
/// event by UPSERTing into `service_agreements`, so agreements
/// survive a rebuild rather than being lost via the FK CASCADE on
/// accounts.
pub const SERVICE_AGREEMENT_UPSERTED: &str = "commerce.service_agreement.upserted";
