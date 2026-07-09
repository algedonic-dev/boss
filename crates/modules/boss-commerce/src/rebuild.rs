//! Rebuild the `invoices` + `invoice_line_items` projections from
//! `audit_log`.
//!
//! Ninth (and last in this round) projection rebuilder. See
//! `docs/design/projection-rebuilders.md`.
//!
//! State events consumed:
//! - `commerce.invoice.created` / `commerce.invoice.paid` — full
//!   `Invoice` payload (header + line_items). Rebuild UPSERTs the
//!   header row and replaces the line_items wholesale.
//! - `commerce.service_agreement.upserted` — full `ServiceAgreement`
//!   row. Rebuild UPSERTs the agreement
//!   row; status changes ride the same kind via the
//!   ON CONFLICT DO UPDATE in `agreements::upsert_agreement`.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::warn;

use crate::types::Invoice;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("commerce");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub invoices_upserted: u64,
    pub agreements_upserted: u64,
}

pub async fn rebuild_commerce(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;

    // TRUNCATE-then-replay. Invoices + invoice_line_items are pure
    // projections of audit_log; no row may live here that doesn't
    // trace back to an event. Service agreements ride alongside
    // invoices.
    //
    // bank_settlements is *live* state (written by the
    // /api/ledger/bank-settlements/* HTTP path, not from audit_log)
    // with an FK to invoices(id). To survive the TRUNCATE we:
    //   1. snapshot (id → invoice_id) into a temp table,
    //   2. detach (NULL-out invoice_id) so CASCADE doesn't delete
    //      the rows,
    //   3. TRUNCATE invoices,
    //   4. replay invoice events to re-upsert invoices with the
    //      same deterministic id (inv-step-{step_id}),
    //   5. restore invoice_id from the snapshot.
    // The schema makes invoice_id nullable to make step 2 legal;
    // step 5 puts it back so the live FK semantics hold once the
    // rebuild commits.
    sqlx::query(
        "CREATE TEMP TABLE _bs_attachments \
         ON COMMIT DROP \
         AS SELECT id, invoice_id FROM bank_settlements \
         WHERE invoice_id IS NOT NULL",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    sqlx::query("UPDATE bank_settlements SET invoice_id = NULL WHERE invoice_id IS NOT NULL")
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    sqlx::query("TRUNCATE invoices, invoice_line_items, service_agreements CASCADE")
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;

    let rows: Vec<(i64, String, DateTime<Utc>, serde_json::Value)> = sqlx::query_as(
        "SELECT id, kind, timestamp, payload FROM audit_log \
         WHERE kind LIKE 'commerce.invoice.%' \
            OR kind LIKE 'commerce.service_agreement.%' \
         ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;

    let mut report = RebuildReport::default();
    for (audit_id, kind, ts, payload) in rows {
        report.events_processed += 1;
        match kind.as_str() {
            "commerce.invoice.created" => {
                let Some(invoice) = parse_invoice(audit_id, &kind, &payload, &mut report) else {
                    continue;
                };
                // Initial creation OR full re-emission — replace
                // line_items wholesale so the document stays canonical.
                upsert_invoice(&mut tx, &invoice, ts, true).await?;
                report.invoices_upserted += 1;
            }
            "commerce.invoice.paid"
            | "commerce.invoice.past_due"
            | "commerce.invoice.written_off" => {
                let Some(invoice) = parse_invoice(audit_id, &kind, &payload, &mut report) else {
                    continue;
                };
                // Status-flip events: production path only updates
                // the header row (status + paid_on for paid; status
                // alone for past-due). Line items don't change —
                // mirror that here so `created_at` stays at the
                // INVOICE_CREATED timestamp.
                upsert_invoice(&mut tx, &invoice, ts, false).await?;
                report.invoices_upserted += 1;
            }
            "commerce.service_agreement.upserted" => {
                let agreement: crate::agreements::ServiceAgreement =
                    match serde_json::from_value(payload.clone()) {
                        Ok(a) => a,
                        Err(e) => {
                            warn!(
                                event_id = audit_id,
                                error = %e,
                                "skipping malformed service_agreement payload"
                            );
                            report.events_skipped += 1;
                            continue;
                        }
                    };
                crate::agreements::upsert_agreement(&mut *tx, &agreement)
                    .await
                    .map_err(|e| RebuildError::Storage(e.to_string()))?;
                report.agreements_upserted += 1;
            }
            other => {
                warn!(event_id = audit_id, kind = %other, "unknown commerce.* event kind");
                report.events_skipped += 1;
            }
        }
    }

    // Step 5: re-attach bank_settlements rows to the freshly
    // upserted invoices. Settlements whose invoice didn't replay
    // (deleted upstream, schema drift) keep NULL invoice_id —
    // they're discoverable via a follow-up audit but don't break
    // the rebuild.
    sqlx::query(
        "UPDATE bank_settlements bs \
         SET invoice_id = a.invoice_id \
         FROM _bs_attachments a \
         WHERE bs.id = a.id \
           AND EXISTS (SELECT 1 FROM invoices i WHERE i.id = a.invoice_id)",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;

    Ok(report)
}

/// Parse an audit_log payload into an `Invoice`, recording a skip on
/// malformed input. Pulled out of the loop body so the dispatch
/// match stays per-kind without duplicating the warn + continue.
fn parse_invoice(
    audit_id: i64,
    kind: &str,
    payload: &serde_json::Value,
    report: &mut RebuildReport,
) -> Option<Invoice> {
    match serde_json::from_value(payload.clone()) {
        Ok(i) => Some(i),
        Err(e) => {
            warn!(
                event_id = audit_id,
                kind = %kind,
                error = %e,
                "skipping commerce.invoice event with non-Invoice payload \
                 (likely pre-enrichment id-only)"
            );
            report.events_skipped += 1;
            None
        }
    }
}

async fn upsert_invoice(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inv: &Invoice,
    ts: DateTime<Utc>,
    replace_lines: bool,
) -> Result<(), RebuildError> {
    // Transparent newtype — the bare kebab code the column stores.
    let status = inv.status.as_str();
    sqlx::query(
        "INSERT INTO invoices (id, account_id, issued_on, due_on, paid_on, status, \
                                amount_cents, currency, tax_cents, tax_jurisdiction, \
                                payment_method, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (id) DO UPDATE SET \
            account_id = EXCLUDED.account_id, \
            issued_on = EXCLUDED.issued_on, \
            due_on = EXCLUDED.due_on, \
            paid_on = EXCLUDED.paid_on, \
            status = EXCLUDED.status, \
            amount_cents = EXCLUDED.amount_cents, \
            currency = EXCLUDED.currency, \
            tax_cents = EXCLUDED.tax_cents, \
            tax_jurisdiction = EXCLUDED.tax_jurisdiction, \
            payment_method = EXCLUDED.payment_method",
    )
    .bind(&inv.id)
    .bind(&inv.account_id)
    .bind(inv.issued_on)
    .bind(inv.due_on)
    .bind(inv.paid_on)
    .bind(status)
    .bind(inv.amount_cents)
    .bind(&inv.currency)
    .bind(inv.tax_cents)
    .bind(inv.tax_jurisdiction.as_deref())
    .bind(inv.payment_method.as_deref())
    .bind(ts)
    .execute(&mut **tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;

    if !replace_lines {
        return Ok(());
    }
    // Replace line_items wholesale per upsert event.
    sqlx::query("DELETE FROM invoice_line_items WHERE invoice_id = $1")
        .bind(&inv.id)
        .execute(&mut **tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    for line in &inv.line_items {
        // Every column the live insert writes (postgres.rs
        // create_invoice_at) replays here — this list drifting behind
        // the live one silently nulled sku/qty/cost on every rebuild,
        // the same two-hand-written-column-lists trap the inventory
        // rebuilder documents. sku + qty matter operationally: the
        // products-consume-on-invoice-created rule reads them from the
        // EVENT (unaffected), but the projection is what operators and
        // margin views read back.
        sqlx::query(
            "INSERT INTO invoice_line_items \
                (id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                 sku, qty, cost_basis_cents, cost_total_cents, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&line.id)
        .bind(&inv.id)
        .bind(line.revenue_category.as_str())
        .bind(line.amount_cents)
        .bind(&line.currency)
        .bind(&line.description)
        .bind(&line.ref_id)
        .bind(&line.sku)
        .bind(line.qty)
        .bind(line.cost_basis_cents)
        .bind(line.cost_total_cents)
        .bind(ts)
        .execute(&mut **tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    }
    Ok(())
}
