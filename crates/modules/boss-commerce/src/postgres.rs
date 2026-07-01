//! Postgres adapter for `CommerceRepository`.
//!
//! Queries `opportunities` and `invoices` tables and assembles into
//! domain structs.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::port::{CommerceError, CommerceRepository};
use crate::types::*;

pub struct PgCommerce {
    pool: PgPool,
}

impl PgCommerce {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load line items for every invoice in the provided slice in a
    /// single query and stitch them back into the parent invoices.
    /// Keeps list endpoints O(1 query) instead of O(N) per page.
    async fn attach_line_items(&self, invoices: &mut [Invoice]) -> Result<(), CommerceError> {
        if invoices.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = invoices.iter().map(|i| i.id.clone()).collect();
        let lines: Vec<LineItemRow> = sqlx::query_as(
            "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                    sku, qty, cost_basis_cents \
             FROM invoice_line_items WHERE invoice_id = ANY($1) ORDER BY id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut by_invoice: std::collections::HashMap<String, Vec<InvoiceLineItem>> =
            std::collections::HashMap::new();
        for row in lines {
            by_invoice
                .entry(row.invoice_id.clone())
                .or_default()
                .push(row.into_line_item());
        }
        for inv in invoices {
            if let Some(lines) = by_invoice.remove(&inv.id) {
                inv.line_items = lines;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CommerceRepository for PgCommerce {
    async fn all_revenue(&self) -> Result<Vec<RevenueLine>, CommerceError> {
        // Derive monthly revenue from invoice line items. Each line
        // already carries its own `revenue_category`, so the rollup
        // is a direct GROUP BY without any category mapping. The
        // month comes from the parent invoice's `issued_on` since
        // line items don't have their own issue date — a revenue
        // recognition schedule (ASC 606) comes with the later GL
        // track; for now we recognize at invoice-issue time.
        let rows: Vec<RevenueLineRow> = sqlx::query_as(
            "SELECT \
                date_trunc('month', i.issued_on)::date AS month, \
                l.revenue_category AS category, \
                SUM(l.amount_cents)::bigint AS amount_cents \
             FROM invoice_line_items l \
             JOIN invoices i ON i.id = l.invoice_id \
             GROUP BY month, l.revenue_category \
             ORDER BY month DESC, l.revenue_category",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into_revenue_line()).collect())
    }

    async fn all_invoices(&self) -> Result<Vec<Invoice>, CommerceError> {
        let rows: Vec<InvoiceRow> =
            sqlx::query_as("SELECT * FROM invoices ORDER BY issued_on DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut invoices: Vec<Invoice> = rows.into_iter().map(|r| r.into_invoice()).collect();
        self.attach_line_items(&mut invoices).await?;
        Ok(invoices)
    }

    async fn list_invoices(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<Invoice>, i64), CommerceError> {
        let (total,): (i64,) = match account_id {
            Some(cid) => {
                sqlx::query_as("SELECT count(*) FROM invoices WHERE account_id = $1")
                    .bind(cid)
                    .fetch_one(&self.pool)
                    .await
            }
            None => {
                sqlx::query_as("SELECT count(*) FROM invoices")
                    .fetch_one(&self.pool)
                    .await
            }
        }
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let rows: Vec<InvoiceRow> = match account_id {
            Some(cid) => {
                sqlx::query_as(
                    "SELECT * FROM invoices WHERE account_id = $1 \
                 ORDER BY issued_on DESC LIMIT $2 OFFSET $3",
                )
                .bind(cid)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as("SELECT * FROM invoices ORDER BY issued_on DESC LIMIT $1 OFFSET $2")
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let mut invoices: Vec<Invoice> = rows.into_iter().map(|r| r.into_invoice()).collect();
        self.attach_line_items(&mut invoices).await?;
        Ok((invoices, total))
    }

    async fn invoice_by_id(&self, id: &str) -> Result<Option<Invoice>, CommerceError> {
        let row: Option<InvoiceRow> = sqlx::query_as("SELECT * FROM invoices WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        let Some(row) = row else { return Ok(None) };
        let mut invoice = row.into_invoice();
        let lines: Vec<LineItemRow> = sqlx::query_as(
            "SELECT id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                    sku, qty, cost_basis_cents \
             FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
        )
        .bind(&invoice.id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        invoice.line_items = lines.into_iter().map(|l| l.into_line_item()).collect();
        Ok(Some(invoice))
    }

    async fn create_invoice_at(
        &self,
        inv: &Invoice,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Invoice, CommerceError> {
        // Invariant: line-item revenue + sales tax must equal the
        // header rollup. Enforce in the adapter so a buggy caller
        // can't persist a document whose total lies about its
        // contents. An invoice with tax_cents=0 reduces this to
        // `line_sum == amount_cents`; a tax-bearing invoice adds
        // tax_cents to the RHS.
        let line_sum: i64 = inv.line_items.iter().map(|l| l.amount_cents).sum();
        if inv.line_items.is_empty() {
            return Err(CommerceError::Storage(format!(
                "invoice {} has no line items",
                inv.id
            )));
        }
        if inv.tax_cents < 0 {
            return Err(CommerceError::Storage(format!(
                "invoice {} tax_cents={} must be non-negative",
                inv.id, inv.tax_cents
            )));
        }
        if inv.tax_cents > 0 && inv.tax_jurisdiction.is_none() {
            return Err(CommerceError::Storage(format!(
                "invoice {} has tax_cents={} but no tax_jurisdiction",
                inv.id, inv.tax_cents
            )));
        }
        if line_sum + inv.tax_cents != inv.amount_cents {
            return Err(CommerceError::Storage(format!(
                "invoice {} amount_cents={} but line items ({}) + tax ({}) sum to {}",
                inv.id,
                inv.amount_cents,
                line_sum,
                inv.tax_cents,
                line_sum + inv.tax_cents
            )));
        }
        if inv.line_items.iter().any(|l| l.currency != inv.currency) {
            return Err(CommerceError::Storage(format!(
                "invoice {} line items disagree on currency with header {}",
                inv.id, inv.currency
            )));
        }

        // Transparent newtype — the bare kebab code the column stores.
        let status = inv.status.as_str();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        sqlx::query(
            "INSERT INTO invoices (id, account_id, issued_on, due_on, paid_on, status, \
                                   amount_cents, currency, tax_cents, tax_jurisdiction, \
                                   payment_method, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (id) DO UPDATE SET \
                account_id = EXCLUDED.account_id, \
                issued_on = EXCLUDED.issued_on, \
                due_on = EXCLUDED.due_on, \
                status = EXCLUDED.status, \
                paid_on = EXCLUDED.paid_on, \
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
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // On re-emission of an existing invoice, wipe its old line
        // items before re-inserting so the document stays consistent.
        // Cheaper than diffing and replay is the only path that
        // re-emits today.
        sqlx::query("DELETE FROM invoice_line_items WHERE invoice_id = $1")
            .bind(&inv.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // FG drawdown loop. When a line item names a SKU (FG sale),
        // look up the current weighted cost basis + decrement on_hand
        // in the same tx as the invoice insert.
        // The cost basis we observed gets stamped onto the line
        // so the `invoice_issued` posting rule can emit matching
        // COGS lines (DR 5100 / CR 1320) in the same JE without
        // doing a DB lookup of its own (rules are pure functions
        // of the event payload).
        //
        // Insufficient stock fails the tx — invoices for goods we
        // don't have are not a real sale. Surfacing the 409 here
        // mirrors the inventory.consume contract and stops the model
        // from posting fictive revenue.
        //
        // Idempotency guard for the relative FG `on_hand -= qty`. The
        // `finance.invoice.issued` fact this tx writes (deterministic
        // source_id = inv.id = `inv-step-{step_id}`) is the invoice's
        // proof-of-issuance. If it already exists, a prior delivery of the
        // same `step.done` event (JetStream at-least-once) already drew the
        // FG stock down — re-applying the relative decrement would double-
        // draw, decoupling GL 1320 from physical FG. So on replay we still
        // re-look-up the cost basis (to re-stamp it onto the line items for
        // the COGS leg) but SKIP the decrement. The fact itself is reused —
        // `insert_fact` below is the same idempotent ON CONFLICT no-op.
        let already_issued =
            fact_exists(&mut tx, "finance.invoice.issued", "invoices", &inv.id).await?;

        // `enriched_lines` is the post-FG-lookup view we use for
        // both the invoice_line_items insert AND the
        // finance.invoice.issued event payload.
        let mut enriched_lines: Vec<InvoiceLineItem> = Vec::with_capacity(inv.line_items.len());
        // Deadlock prevention: take the FG `FOR UPDATE` row locks in a
        // deterministic (SKU-sorted) order so two concurrent invoice txs
        // sharing SKUs acquire them in the same global order and queue
        // instead of deadlocking. Line-item read order is unaffected
        // (projections read ORDER BY id) and the per-SKU drawdown math is
        // order-independent, so reordering the loop is semantically inert.
        let mut ordered: Vec<&InvoiceLineItem> = inv.line_items.iter().collect();
        ordered.sort_by(|a, b| a.sku.cmp(&b.sku));
        for line in ordered {
            let mut enriched = line.clone();
            if let (Some(sku), Some(qty)) = (line.sku.as_deref(), line.qty)
                && qty > 0
            {
                // Look up FG row (+ decrement on first issuance only).
                let row: Option<(i32, i64)> = sqlx::query_as(
                    "SELECT on_hand, production_cost_cents \
                     FROM finished_product_inventory \
                     WHERE product_sku = $1 \
                     ORDER BY on_hand DESC \
                     LIMIT 1 FOR UPDATE",
                )
                .bind(sku)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| CommerceError::Storage(e.to_string()))?;
                match row {
                    // First issuance: stock is sufficient → draw it down.
                    Some((on_hand, cost_basis)) if !already_issued && on_hand >= qty => {
                        sqlx::query(
                            "UPDATE finished_product_inventory \
                             SET on_hand = on_hand - $2, updated_at = NOW() \
                             WHERE product_sku = $1 AND on_hand >= $2",
                        )
                        .bind(sku)
                        .bind(qty)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| CommerceError::Storage(e.to_string()))?;
                        enriched.cost_basis_cents = Some(cost_basis);
                    }
                    // Redelivery: the drawdown already happened on the prior
                    // delivery (the issued fact exists). Re-stamp cost_basis
                    // for the COGS leg, but do NOT decrement again. Note the
                    // current on_hand may now be < qty (the stock left on the
                    // first delivery) — that's expected, not a shortage.
                    //
                    // CAVEAT: this re-stamps the CURRENT FG standard cost. If
                    // the SKU is re-standardized between the two deliveries,
                    // the line-item projection (last-write-wins on
                    // commerce.invoice.created) takes the new basis while the
                    // COGS JE (first-write-wins, ON CONFLICT DO NOTHING) keeps
                    // the original — a cosmetic line-item-vs-GL cost drift on
                    // that one invoice, NOT a GL/physical decouple (physical
                    // was drawn once; the GL stays internally balanced). Needs
                    // a redelivery straddling a re-standardization, so rare.
                    Some((_on_hand, cost_basis)) if already_issued => {
                        enriched.cost_basis_cents = Some(cost_basis);
                    }
                    Some((on_hand, _)) => {
                        return Err(CommerceError::Storage(format!(
                            "invoice {} line {sku}: insufficient FG stock — on_hand={on_hand}, need={qty}",
                            inv.id
                        )));
                    }
                    None => {
                        // No FG row at all — treat as a service / non-FG
                        // line that happened to carry an SKU. Skip the
                        // drawdown; cost_basis stays None; rule emits
                        // revenue only.
                    }
                }
            }
            sqlx::query(
                "INSERT INTO invoice_line_items \
                    (id, invoice_id, revenue_category, amount_cents, currency, description, ref_id, \
                     sku, qty, cost_basis_cents, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(&enriched.id)
            .bind(&inv.id)
            .bind(enriched.revenue_category.as_str())
            .bind(enriched.amount_cents)
            .bind(&enriched.currency)
            .bind(&enriched.description)
            .bind(&enriched.ref_id)
            .bind(&enriched.sku)
            .bind(enriched.qty)
            .bind(enriched.cost_basis_cents)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
            enriched_lines.push(enriched);
        }

        // Emit the invoice.issued financial fact in the same tx. Idempotent
        // via the unique (kind, source) index — replay re-emission is a no-op.
        // tax_lines is omitted entirely on zero-tax invoices so the payload
        // stays clean.
        let mut payload_map = serde_json::Map::new();
        payload_map.insert("invoice_id".into(), serde_json::json!(inv.id));
        payload_map.insert("account_id".into(), serde_json::json!(inv.account_id));
        payload_map.insert("amount_cents".into(), serde_json::json!(inv.amount_cents));
        payload_map.insert("currency".into(), serde_json::json!(inv.currency));
        payload_map.insert("issued_on".into(), serde_json::json!(inv.issued_on));
        payload_map.insert(
            "line_items".into(),
            serde_json::json!(
                enriched_lines
                    .iter()
                    .map(|l| {
                        let mut m = serde_json::Map::new();
                        m.insert(
                            "category".into(),
                            serde_json::Value::String(l.revenue_category.as_str().to_string()),
                        );
                        m.insert("amount_cents".into(), serde_json::json!(l.amount_cents));
                        m.insert(
                            "currency".into(),
                            serde_json::Value::String(l.currency.clone()),
                        );
                        // Pass through SKU/qty/cost_basis so the
                        // `invoice_issued` posting rule can draw COGS.
                        if let Some(sku) = &l.sku {
                            m.insert("sku".into(), serde_json::Value::String(sku.clone()));
                        }
                        if let Some(qty) = l.qty {
                            m.insert("qty".into(), serde_json::json!(qty));
                        }
                        if let Some(cb) = l.cost_basis_cents {
                            m.insert("cost_basis_cents".into(), serde_json::json!(cb));
                        }
                        serde_json::Value::Object(m)
                    })
                    .collect::<Vec<_>>()
            ),
        );
        // tax_lines via the shared helper so the live fact and the
        // commerce.invoice.created audit event (http.rs) can't drift —
        // both feed the ledger `invoice_issued` rule's CR 2300.
        if let Some(tax_lines) =
            crate::events::tax_lines_for(inv.tax_cents, inv.tax_jurisdiction.as_deref())
        {
            payload_map.insert("tax_lines".into(), tax_lines);
        }
        let issued_payload = serde_json::Value::Object(payload_map);
        insert_fact(
            &mut tx,
            "finance.invoice.issued",
            inv.issued_on,
            &issued_payload,
            "invoices",
            &inv.id,
        )
        .await?;

        // If the invoice is being created already-paid (replay path), emit
        // the paid fact too — but ONLY when `payment_method` is unset.
        // When the caller supplies a method (sim's two-phase bank-clearing
        // flow), the bank-settlement POST is responsible for emitting
        // `finance.payment.received` + creating the projection row, so
        // double-posting `finance.invoice.paid` here would credit A/R
        // twice.
        // The unique index handles the double-emission case where
        // mark_invoice_paid also fires later.
        if inv.status.is_paid()
            && let Some(paid_on) = inv.paid_on
            && inv.payment_method.is_none()
        {
            let paid_payload = serde_json::json!({
                "invoice_id": inv.id,
                "account_id": inv.account_id,
                "amount_cents": inv.amount_cents,
                "currency": inv.currency,
                "paid_on": paid_on,
            });
            insert_fact(
                &mut tx,
                "finance.invoice.paid",
                paid_on,
                &paid_payload,
                "invoices",
                &inv.id,
            )
            .await?;
        }

        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Return the enriched invoice (line_items now carry the
        // FG cost_basis_cents observed during drawdown) so callers
        // can emit the audit event with the same shape the
        // financial_fact persists. Without this, audit_log replay
        // can't reconstruct COGS legs.
        let mut enriched_invoice = inv.clone();
        enriched_invoice.line_items = enriched_lines;
        Ok(enriched_invoice)
    }

    async fn mark_invoice_paid_at(
        &self,
        id: &str,
        paid_on: chrono::NaiveDate,
    ) -> Result<(), CommerceError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Status flip + paid_on stamp. We DON'T emit a
        // `finance.invoice.paid` fact from this path: the
        // canonical AR drain runs through the two-phase
        // bank-clearing chain — `ledger.payment.received` debits
        // 1010 Cash-in-Transit + credits 1100 AR; settled flips
        // 1010 → 1000. Emitting finance.invoice.paid here would
        // ALSO credit AR (via BossRuleSet::invoice_paid's
        // DR Cash / CR AR shortcut), and the live brewery hit
        // exactly this — AR went structurally negative -$6.5M
        // because every payment got AR-credited twice.
        //
        // The create-already-paid path keeps emitting the fact
        // (single-shot tenants who don't model bank float) but only
        // when `payment_method` is unset; once a tenant uses the
        // two-phase chain, neither path emits and the bank-settlement
        // is the sole AR drain.
        //
        // Tenants that need the single-shot DR Cash CR AR rule
        // can still author the fact directly via
        // `record_fact_in_tx`; we just don't auto-emit one for
        // every PUT /paid.
        let row: Option<(String, i64, String, chrono::NaiveDate)> = sqlx::query_as(
            "UPDATE invoices SET status = 'paid', paid_on = $2 \
             WHERE id = $1 RETURNING account_id, amount_cents, currency, paid_on",
        )
        .bind(id)
        .bind(paid_on)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        if row.is_none() {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        }

        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn mark_invoice_past_due(&self, id: &str) -> Result<(), CommerceError> {
        // No financial fact / journal entry on past-due — it's a
        // status flip used for reporting + collections workflow,
        // not a posting event. The original revenue accrual on
        // INVOICE_CREATED already debited A/R; past-due just
        // ages it.
        let result = sqlx::query("UPDATE invoices SET status = 'past-due' WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        }
        Ok(())
    }

    async fn mark_invoice_written_off(&self, id: &str) -> Result<(), CommerceError> {
        // Flip to terminal `written-off` AND record the
        // `finance.invoice.written_off` fact that posts DR 6700 Bad Debt
        // Expense / CR 1100 A/R. The fact is written LIVE here — exactly
        // like the `finance.invoice.issued` fact written at creation —
        // because live facts come only from explicit `insert_fact` calls;
        // `gl_fact_projection_rules` is the REBUILD path. A previous
        // version posted nothing here on the theory that "the projection
        // path owns it," but that path runs only in rebuild-all, so the
        // live ledger silently dropped every write-off (live 6700 = 0)
        // while the rebuild reconstructed them — a determinism gap.
        //
        // `happened_on` is the invoice's `issued_on`, matching the
        // projection rule's `/issued_on` happened_on path, so the live
        // and rebuilt postings land on the same date and reconcile to
        // the cent.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let row: Option<(i64, String, String, chrono::NaiveDate)> = sqlx::query_as(
            "UPDATE invoices SET status = 'written-off' WHERE id = $1 \
             RETURNING amount_cents, account_id, currency, issued_on",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let Some((amount_cents, account_id, currency, issued_on)) = row else {
            return Err(CommerceError::NotFound(format!("invoice {id}")));
        };
        let payload = serde_json::json!({
            "invoice_id": id,
            "account_id": account_id,
            "amount_cents": amount_cents,
            "currency": currency,
            "issued_on": issued_on,
        });
        insert_fact(
            &mut tx,
            "finance.invoice.written_off",
            issued_on,
            &payload,
            "invoices",
            id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn invoice_summary(
        &self,
        today: chrono::NaiveDate,
    ) -> Result<InvoiceSummary, CommerceError> {
        // Trailing-12-months revenue grouped by revenue account.
        // Source: gl_journal_lines × gl_accounts. Revenue accounts
        // are `kind = 'revenue'`; their credit_cents sums to the
        // recognized revenue for the period. Sourcing from the
        // ledger (not invoice_line_items) protects against drift —
        // any manual journal entry or adjusting entry to a revenue
        // account shows up here, and a malformed invoice that
        // failed to post to the GL doesn't double-count.
        //
        // The category label IS the GL account name. We used to
        // run `revenue_category_for_account_code` to invert the
        // ledger's mapping into a category code, but that
        // hardcoded the device-shop's category vocabulary
        // (new-sales / used-sales / contracts) — when the brewery
        // posts to the same chart codes, it showed up under the
        // wrong names. Reading the account name keeps the label
        // tenant-shaped without any inverse mapping.
        // Exclude `finance.period.closed` lines from the revenue
        // tally. Those entries DR-revenue / CR-retained-earnings at
        // year-end — bookkeeping movement, not negative revenue.
        // Without this filter, TTM windows that straddle a close-out
        // date can flip a low-volume category (e.g. Seasonal &
        // Specialty in the brewery) net-negative because the close
        // debit exceeds the post-close credits. Trial balance + the
        // income statement aggregate over the full ledger and don't
        // hit this because they don't filter by a sliding window.
        let revenue_rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT a.code, a.name, COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             JOIN financial_facts f ON e.fact_id = f.id \
             WHERE a.kind = 'revenue' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
               AND f.kind <> 'finance.period.closed' \
             GROUP BY a.code, a.name \
             ORDER BY a.code",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Real COGS for the TTM window.
        //
        // The `finance.cogs.recognized` payload carries the
        // originating `revenue_category` (e.g. "wholesale", "retail"),
        // tagged at consume time by the shipment step's
        // `consumes_products` array (see boss-products consume() +
        // boss-inventory-sim-bridge `ProductsConsumeEmitter`). We sum
        // tagged COGS exactly per category and pro-rate only the
        // untagged remainder by revenue share, so a category gets an
        // exact margin whenever its COGS facts carry the tag.
        let total_revenue_ttm_cents: i64 = revenue_rows.iter().map(|(_, _, r)| *r).sum();
        let total_cogs_ttm_cents: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(l.debit_cents - l.credit_cents), 0)::bigint \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             JOIN financial_facts f ON e.fact_id = f.id \
             WHERE a.code LIKE '5%' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
               AND f.kind <> 'finance.period.closed'",
        )
        .bind(today)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Tagged COGS — sum cents from `finance.cogs.recognized`
        // facts whose payload carries a `revenue_category`.
        // GROUP BY the tag so each category gets exact COGS.
        let tagged_cogs_rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT \
                f.payload->>'revenue_category' AS revenue_category, \
                COALESCE(SUM(((f.payload->>'total_cost_cents')::bigint)), 0)::bigint \
             FROM financial_facts f \
             WHERE f.kind = 'finance.cogs.recognized' \
               AND f.happened_on >= $1::date - INTERVAL '12 months' \
               AND f.payload ? 'revenue_category' \
             GROUP BY f.payload->>'revenue_category'",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Invert the ledger's category → account map so we can find,
        // for each revenue account row, which category tags belong
        // to it ("retail" + "merchandise" both → 4110).
        let category_to_account = boss_ledger::revenue_accounts_map();
        let mut account_to_categories: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (category, account_code) in category_to_account.iter() {
            account_to_categories
                .entry(account_code)
                .or_default()
                .push(category.as_str());
        }

        let tagged_cogs_by_category: std::collections::HashMap<String, i64> =
            tagged_cogs_rows.into_iter().collect();
        let total_tagged_cogs_cents: i64 = tagged_cogs_by_category.values().copied().sum();
        let untagged_cogs_cents: i64 = (total_cogs_ttm_cents - total_tagged_cogs_cents).max(0);
        // Untagged remainder is the revenue total whose categories
        // have no tagged COGS attached — only those revenue rows
        // share in the pro-rated pool. Revenue rows with any tagged
        // COGS get the exact amount and contribute zero to the
        // pro-rated denominator.
        let untagged_revenue_total: i64 = revenue_rows
            .iter()
            .filter(|(code, _, _)| {
                account_to_categories
                    .get(code.as_str())
                    .map(|cats| {
                        !cats
                            .iter()
                            .any(|c| tagged_cogs_by_category.contains_key(*c))
                    })
                    .unwrap_or(true)
            })
            .map(|(_, _, r)| *r)
            .sum();

        let revenue_ttm: Vec<CategoryMargin> = revenue_rows
            .into_iter()
            .map(|(code, name, revenue_cents)| {
                let category = name;
                // Per-row COGS = (tagged COGS for any category mapped
                // to this account) + (pro-rated share of untagged
                // COGS by revenue mix). If the account has at least
                // one tagged category, the pro-rated share is zero.
                let categories = account_to_categories
                    .get(code.as_str())
                    .cloned()
                    .unwrap_or_default();
                let tagged_for_row: i64 = categories
                    .iter()
                    .filter_map(|c| tagged_cogs_by_category.get(*c).copied())
                    .sum();
                let has_tagged = categories
                    .iter()
                    .any(|c| tagged_cogs_by_category.contains_key(*c));
                let pro_rated_cogs = if !has_tagged && untagged_revenue_total > 0 {
                    ((untagged_cogs_cents as i128) * (revenue_cents as i128)
                        / (untagged_revenue_total as i128)) as i64
                } else {
                    0
                };
                let cogs_cents = tagged_for_row + pro_rated_cogs;
                let gross_margin_cents = revenue_cents - cogs_cents;
                let margin_pct = if revenue_cents > 0 {
                    (gross_margin_cents as f64 / revenue_cents as f64 * 100.0).round() as i64
                } else {
                    0
                };
                CategoryMargin {
                    category,
                    revenue_cents,
                    cogs_cents,
                    gross_margin_cents,
                    margin_pct,
                }
            })
            .collect();

        let total_gross_margin_ttm_cents: i64 = total_revenue_ttm_cents - total_cogs_ttm_cents;

        // AR aging: every unpaid invoice bucketed by how many days past its
        // due date. `current` means "due in the future or due today".
        let aging_rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT \
                CASE \
                    WHEN $1::date - due_on <= 0 THEN 'current' \
                    WHEN $1::date - due_on <= 30 THEN '1-30' \
                    WHEN $1::date - due_on <= 60 THEN '31-60' \
                    WHEN $1::date - due_on <= 90 THEN '61-90' \
                    ELSE '90+' \
                END as label, \
                COUNT(*)::bigint as count, \
                COALESCE(SUM(amount_cents), 0)::bigint as total_cents \
             FROM invoices \
             WHERE status <> 'paid' \
             GROUP BY 1",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Emit the buckets in canonical order even when some are empty, so
        // the frontend always sees the same 5-row shape.
        let mut ar_map: std::collections::HashMap<String, (i64, i64)> =
            std::collections::HashMap::new();
        for (label, count, total_cents) in aging_rows {
            ar_map.insert(label, (count, total_cents));
        }
        let canonical_order = ["current", "1-30", "31-60", "61-90", "90+"];
        let ar_aging: Vec<ArAgingBucket> = canonical_order
            .iter()
            .map(|label| {
                let (count, total_cents) = ar_map.get(*label).copied().unwrap_or((0, 0));
                ArAgingBucket {
                    label: label.to_string(),
                    count,
                    total_cents,
                }
            })
            .collect();

        let total_outstanding_cents: i64 = ar_aging.iter().map(|b| b.total_cents).sum();

        let total_invoice_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM invoices")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CommerceError::Storage(e.to_string()))?;

        // Monthly revenue for the last 12 months, oldest-first. The Pulse
        // panel uses the last entry for MTD and the prior entry for pace
        // comparison. Sourced from the GL — each invoice-issued fact
        // maps to exactly one journal entry, so `COUNT(DISTINCT e.fact_id)`
        // in revenue-producing entries matches invoice count without
        // double-counting adjusting entries. `SUM(credit_cents -
        // debit_cents)` on revenue accounts handles refunds cleanly
        // (a refund debits revenue, reducing the month's net).
        let monthly_rows: Vec<(chrono::NaiveDate, i64, i64)> = sqlx::query_as(
            "SELECT \
                date_trunc('month', e.posted_on)::date as month, \
                COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint as revenue_cents, \
                COUNT(DISTINCT e.fact_id)::bigint as invoice_count \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON l.account_id = a.id \
             JOIN gl_journal_entries e ON l.journal_entry_id = e.id \
             WHERE a.kind = 'revenue' \
               AND e.posted_on >= $1::date - INTERVAL '12 months' \
             GROUP BY 1 \
             ORDER BY 1 ASC",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CommerceError::Storage(e.to_string()))?;
        let revenue_by_month: Vec<MonthlyRevenue> = monthly_rows
            .into_iter()
            .map(|(month, revenue_cents, invoice_count)| MonthlyRevenue {
                month: month.to_string(),
                revenue_cents,
                invoice_count,
            })
            .collect();

        Ok(InvoiceSummary {
            revenue_ttm,
            total_revenue_ttm_cents,
            total_cogs_ttm_cents,
            total_gross_margin_ttm_cents,
            ar_aging,
            total_outstanding_cents,
            total_invoice_count,
            revenue_by_month,
            currency: "USD".to_string(),
        })
    }
}

/// COGS percentages by revenue category. Mirrors the rates the old
/// client-side `deriveMargins` in `apps/web/src/seed/api.tsx` used.
/// New and used device sales carry different COGS — used devices are
/// cheaper to acquire, which is why used-sales margin is higher.
/// Insert a financial fact inside an existing transaction, then post it
/// synchronously to the ledger. Idempotent — replay of the same fact is a
/// no-op for both the fact row (unique index) and the journal entry
/// (unique (fact_id, rule_version_id)).
/// Does a financial_fact with this natural key already exist? Used to
/// gate the relative FG `on_hand -= qty` drawdown in `create_invoice_at`
/// against a redelivered `step.done` event (JetStream at-least-once):
/// the `finance.invoice.issued` fact this invoice writes in-tx is its
/// proof-of-issuance, so its prior existence means the drawdown already
/// applied on an earlier delivery — skip it. Mirrors the
/// `fact_exists` guard `consume_part_at` uses on the inventory side.
async fn fact_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    source_table: &str,
    source_id: &str,
) -> Result<bool, CommerceError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;
    Ok(row.is_some())
}

async fn insert_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    happened_on: chrono::NaiveDate,
    payload: &serde_json::Value,
    source_table: &str,
    source_id: &str,
) -> Result<(), CommerceError> {
    // INSERT ... ON CONFLICT ... RETURNING only returns a row on new
    // inserts, so the conflict path gets an empty result. We upsert + then
    // look up the id separately so both paths produce the same fact_id.
    // created_by carries fact provenance at service granularity — the
    // same fallback the gl_fact_projection_rules engine uses when a
    // rule has no created_by_path (event.source). The old schema
    // DEFAULT 'system' silently swallowed the missing bind.
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'commerce') \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_table)
    .bind(source_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;

    let (fact_id,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| CommerceError::Storage(e.to_string()))?;

    let fact_ref = boss_ledger::FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    boss_ledger::post_fact_in_tx(tx, &fact_ref)
        .await
        .map_err(|e| CommerceError::Storage(format!("ledger post failed: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct InvoiceRow {
    id: String,
    account_id: String,
    issued_on: chrono::NaiveDate,
    due_on: chrono::NaiveDate,
    paid_on: Option<chrono::NaiveDate>,
    status: String,
    amount_cents: i64,
    currency: String,
    #[sqlx(default)]
    tax_cents: i64,
    #[sqlx(default)]
    tax_jurisdiction: Option<String>,
    #[sqlx(default)]
    payment_method: Option<String>,
}

impl InvoiceRow {
    fn into_invoice(self) -> Invoice {
        Invoice {
            id: self.id,
            account_id: self.account_id,
            issued_on: self.issued_on,
            due_on: self.due_on,
            paid_on: self.paid_on,
            // Free-text Class code; the column holds the kebab string,
            // so the newtype wraps it as-is.
            status: InvoiceStatus::new(self.status),
            amount_cents: self.amount_cents,
            currency: self.currency,
            tax_cents: self.tax_cents,
            tax_jurisdiction: self.tax_jurisdiction,
            payment_method: self.payment_method,
            line_items: Vec::new(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct LineItemRow {
    id: String,
    invoice_id: String,
    revenue_category: String,
    amount_cents: i64,
    currency: String,
    description: String,
    ref_id: Option<String>,
    sku: Option<String>,
    qty: Option<i32>,
    cost_basis_cents: Option<i64>,
}

impl LineItemRow {
    fn into_line_item(self) -> InvoiceLineItem {
        InvoiceLineItem {
            id: self.id,
            invoice_id: self.invoice_id,
            revenue_category: RevenueCategory::from(self.revenue_category),
            amount_cents: self.amount_cents,
            currency: self.currency,
            description: self.description,
            ref_id: self.ref_id,
            sku: self.sku,
            qty: self.qty,
            cost_basis_cents: self.cost_basis_cents,
        }
    }
}

#[derive(sqlx::FromRow)]
struct RevenueLineRow {
    month: chrono::NaiveDate,
    category: String,
    amount_cents: i64,
}

impl RevenueLineRow {
    fn into_revenue_line(self) -> RevenueLine {
        RevenueLine {
            month: self.month,
            category: RevenueCategory::from(self.category),
            amount_cents: self.amount_cents,
            currency: "USD".to_string(),
        }
    }
}

// InvoiceStatus and RevenueCategory are both newtypes around String
// accepting arbitrary values, so callers wrap the database column
// directly (`InvoiceStatus::new(self.status)` /
// `RevenueCategory::from(self.revenue_category)`) — no parse step.
// Status values are validated against the Class registry at the
// commerce API boundary, not in this storage adapter.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoice_status_round_trips_through_serde() {
        // Transparent newtype serializes to the bare kebab code the
        // column stores and round-trips back.
        for code in [
            InvoiceStatus::PAID,
            InvoiceStatus::OUTSTANDING,
            InvoiceStatus::PAST_DUE,
            InvoiceStatus::WRITTEN_OFF,
        ] {
            let st = InvoiceStatus::new(code);
            assert_eq!(st.as_str(), code);
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{code}\""));
            let back: InvoiceStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, st);
        }
    }
}
