//! Postgres adapter for `InventoryRepository`.
//!
//! Queries `inventory_items` and `purchase_orders` + `purchase_order_lines`
//! tables and assembles them into domain structs.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::port::{InventoryError, InventoryRepository};
use crate::types::*;

pub struct PgInventory {
    pool: PgPool,
}

impl PgInventory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl InventoryRepository for PgInventory {
    async fn all_items(&self) -> Result<Vec<InventoryItem>, InventoryError> {
        let rows: Vec<InventoryItemRow> =
            sqlx::query_as("SELECT * FROM inventory_items ORDER BY part_sku")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|r| r.into_item()).collect())
    }

    async fn item_by_sku(&self, part_sku: &str) -> Result<Option<InventoryItem>, InventoryError> {
        let row: Option<InventoryItemRow> =
            sqlx::query_as("SELECT * FROM inventory_items WHERE part_sku = $1")
                .bind(part_sku)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;

        Ok(row.map(|r| r.into_item()))
    }

    async fn upsert_item_at(
        &self,
        item: &InventoryItem,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        sqlx::query(
            "INSERT INTO inventory_items \
                (part_sku, bin, on_hand, allocated, reorder_point, reorder_qty, \
                 trailing_90d_usage, value_cents, vendor_price_cents, vendor_category, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (part_sku) DO UPDATE SET \
                bin = EXCLUDED.bin, \
                on_hand = EXCLUDED.on_hand, \
                allocated = EXCLUDED.allocated, \
                reorder_point = EXCLUDED.reorder_point, \
                reorder_qty = EXCLUDED.reorder_qty, \
                trailing_90d_usage = EXCLUDED.trailing_90d_usage, \
                value_cents = EXCLUDED.value_cents, \
                vendor_price_cents = EXCLUDED.vendor_price_cents, \
                vendor_category = EXCLUDED.vendor_category, \
                updated_at = EXCLUDED.updated_at",
        )
        .bind(&item.part_sku)
        .bind(&item.bin)
        .bind(item.on_hand as i32)
        .bind(item.allocated as i32)
        .bind(item.reorder_point as i32)
        .bind(item.reorder_qty as i32)
        .bind(item.trailing_90d_usage as i32)
        .bind(item.value_cents)
        .bind(item.vendor_price_cents)
        .bind(&item.vendor_category)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn all_purchase_orders(&self) -> Result<Vec<PurchaseOrder>, InventoryError> {
        let rows: Vec<PurchaseOrderRow> =
            sqlx::query_as("SELECT * FROM purchase_orders ORDER BY placed_on DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;

        let mut orders = Vec::with_capacity(rows.len());
        for row in rows {
            let order = self.assemble(row).await?;
            orders.push(order);
        }
        Ok(orders)
    }

    async fn purchase_order_by_id(
        &self,
        id: &str,
    ) -> Result<Option<PurchaseOrder>, InventoryError> {
        let row: Option<PurchaseOrderRow> =
            sqlx::query_as("SELECT * FROM purchase_orders WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(self.assemble(r).await?)),
            None => Ok(None),
        }
    }

    async fn consume_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        now: chrono::DateTime<chrono::Utc>,
        source_id: &str,
    ) -> Result<ConsumeApplied, InventoryError> {
        // One tx wraps: (1) the proportional value drain + on_hand
        // decrement, (2) the `finance.inventory.transferred` fact
        // sized at exactly the drained value. The two land atomically
        // — there is no code path that decrements raw inventory
        // without moving the same cents raw → WIP in the same tx, so
        // balance(1300) == Σ value_cents holds by construction
        // (costing PR 6a; docs/design/inventory-value-conservation.md).
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        // Idempotency guard. `on_hand -= qty` is a relative mutation, so a
        // redelivered step-effect event (at-least-once JetStream delivery)
        // would double-decrement. The financial_fact this consume writes
        // is the proof-of-application — if one with this source_id already
        // exists, the consume committed on a prior delivery, so skip it
        // and return the current row unchanged, with NO fact payload: the
        // caller then emits no audit event, so a replay appends nothing.
        // (Also dodges a spurious InsufficientStock on replay once stock
        // has since fallen below qty.) Sound when the consume drains value
        // (the only case that writes a fact); a zero-value row writes none
        // and isn't guarded — no GL impact, and the seeded brewery gives
        // every part an opening value.
        if fact_exists(
            &mut tx,
            "finance.inventory.transferred",
            "inventory_consume",
            source_id,
        )
        .await?
        {
            drop(tx);
            let item = self
                .item_by_sku(part_sku)
                .await?
                .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
            return Ok(ConsumeApplied {
                item,
                fact_payload: None,
            });
        }

        // Read the row under lock, compute the exact drain in one
        // place, then apply it. The drain is the proportional share
        // round-half-up(value × qty / on_hand); consuming the last
        // unit takes the whole remaining value, so zero on_hand
        // forces zero value — nothing strands.
        let before: Option<(i32, i64)> = sqlx::query_as(
            "SELECT on_hand, value_cents FROM inventory_items \
             WHERE part_sku = $1 FOR UPDATE",
        )
        .bind(part_sku)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let Some((on_hand_before, value_before)) = before else {
            drop(tx);
            return Err(InventoryError::NotFound(part_sku.to_string()));
        };
        if (on_hand_before as u32) < qty {
            drop(tx);
            return Err(InventoryError::InsufficientStock(
                part_sku.to_string(),
                on_hand_before as u32,
                qty,
            ));
        }
        let drained_cents = if on_hand_before as u32 == qty {
            value_before
        } else {
            // i128 keeps value × qty exact for any realistic row.
            (((value_before as i128) * (qty as i128) + (on_hand_before as i128) / 2)
                / (on_hand_before as i128)) as i64
        };

        let row: Option<InventoryItemRow> = sqlx::query_as(
            "UPDATE inventory_items SET \
                on_hand = on_hand - $2, \
                value_cents = value_cents - $3, \
                updated_at = $4 \
             WHERE part_sku = $1 RETURNING *",
        )
        .bind(part_sku)
        .bind(qty as i32)
        .bind(drained_cents)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        let item = match row {
            Some(r) => r.into_item(),
            None => {
                drop(tx);
                return Err(InventoryError::NotFound(part_sku.to_string()));
            }
        };

        // Model B: ingredient consumption moves value raw → WIP, not
        // raw → COGS: every drained cent leaving 1300 Raw arrives in
        // 1310 WIP and waits for the packaging step
        // (`products.produce`) to roll it forward to 1320. COGS is
        // recognized later, at sale time, against 1320. This payload
        // is the ONE construction — the caller emits the audit event
        // from the returned copy verbatim, so the rebuilt fact is
        // byte-identical to this live one (the fact-level
        // replay-check compares payloads).
        let fact_payload = if drained_cents > 0 {
            let payload = serde_json::json!({
                "total_cost_cents": drained_cents,
                "debit_account": "1310",
                "credit_account": "1300",
                "memo": format!(
                    "Production — consumed {qty} × {part_sku} (raw → WIP, value drain)",
                ),
                "part_sku": part_sku,
                "qty": qty,
                "source_id": source_id,
                "consumed_on": now.date_naive(),
            });
            insert_fact(
                &mut tx,
                "finance.inventory.transferred",
                now.date_naive(),
                &payload,
                "inventory_consume",
                source_id,
            )
            .await?;
            Some(payload)
        } else {
            None
        };

        tx.commit()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(ConsumeApplied { item, fact_payload })
    }

    async fn inbound_reserved_for_part(&self, part_sku: &str) -> Result<i64, InventoryError> {
        // Cross-domain projection — sums `expected_qty` across
        // every open ingredient-restock Job's receiving step
        // whose `expected_items` array carries `part_sku`. We
        // count `expected_qty`, NOT `expected_qty - received_qty`:
        // the brewery seeds `received_qty = expected_qty` upfront
        // on step materialization (the value is the projected
        // post-receive state, not "actually received now"). The
        // step status is the source of truth — `status != 'done'`
        // means the receive hasn't fired yet, so the full
        // expected_qty is real upcoming supply. Once the step
        // is done, the receive has already landed in `on_hand`.
        //
        // Cheap unindexed scan for now (open restocks rarely
        // exceed ~50); if this becomes hot, materialize as a
        // per-part projection via the step.upserted event stream.
        // Outer COALESCE wraps SUM() so we never get NULL, then cast
        // to BIGINT so sqlx decodes into i64 (Postgres SUM(bigint)
        // returns NUMERIC, which won't auto-decode).
        let total: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(SUM( \
                  COALESCE((item->>'expected_qty')::int, 0)::bigint \
              ), 0)::BIGINT \
             FROM jobs j \
             JOIN steps s ON s.job_id = j.id \
             CROSS JOIN LATERAL jsonb_array_elements( \
                 s.metadata->'expected_items' \
             ) AS item \
             WHERE j.kind = 'ingredient-restock' \
               AND j.status = 'open' \
               AND s.kind = 'receiving' \
               AND s.status != 'completed' \
               AND item->>'part_sku' = $1",
        )
        .bind(part_sku)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(total.unwrap_or(0))
    }

    async fn open_po_exists_for_part(&self, part_sku: &str) -> Result<bool, InventoryError> {
        // Open PO = status NOT IN ('received', 'closed', 'cancelled') AND
        // has a line for this part_sku. The dispatcher's
        // reorder-threshold rule uses this as the idempotency check —
        // it's the architecturally correct version of the
        // inbound_reserved_for_part quantity proxy.
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 \
             FROM purchase_order_lines pol \
             JOIN purchase_orders po ON po.id = pol.po_id \
             WHERE pol.part_sku = $1 \
               AND po.status NOT IN ('received', 'closed', 'cancelled') \
             LIMIT 1",
        )
        .bind(part_sku)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(row.is_some())
    }

    async fn primary_vendor_for_part(
        &self,
        part_sku: &str,
    ) -> Result<Option<String>, InventoryError> {
        // Most recently associated vendor for the SKU via PO lines —
        // the recency heuristic (whoever supplied it last is who we'd
        // reorder from).
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT po.vendor \
             FROM purchase_order_lines pol \
             JOIN purchase_orders po ON po.id = pol.po_id \
             WHERE pol.part_sku = $1 \
             ORDER BY po.placed_on DESC NULLS LAST \
             LIMIT 1",
        )
        .bind(part_sku)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        if let Some((vendor,)) = row {
            return Ok(Some(vendor));
        }

        // No PO history (e.g. the first auto-restock of this part):
        // fall back to a category-appropriate supplier — any vendor
        // whose `category` matches the part's declared
        // `vendor_category`. This is the data-driven "curated
        // vendor_for_part projection": the SKU→category mapping lives
        // as data on the item (seeded from the tenant's parts.toml),
        // and the match here is fully generic — no SKU knowledge in
        // code. Returns None when the part declares no category or no
        // vendor serves it (the caller's rule must then not fire,
        // surfacing the gap rather than inventing a bad vendor).
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT v.id \
             FROM vendors v \
             JOIN inventory_items i ON i.vendor_category = v.category \
             WHERE i.part_sku = $1 \
             ORDER BY v.id \
             LIMIT 1",
        )
        .bind(part_sku)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(row.map(|(v,)| v))
    }

    async fn record_overhead_absorbed(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
    ) -> Result<(uuid::Uuid, bool), InventoryError> {
        if total_cost_cents <= 0 {
            return Err(InventoryError::Storage(
                "total_cost_cents must be positive".to_string(),
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        let payload = serde_json::json!({
            "total_cost_cents": total_cost_cents,
            "debit_account": debit_account,
            "credit_account": credit_account,
            "memo": memo,
            "happened_on": happened_on,
            "source_id": source_id,
        });

        let inserted = insert_fact(
            &mut tx,
            "finance.inventory.transferred",
            happened_on,
            &payload,
            "ledger_overhead_absorbed",
            source_id,
        )
        .await?;

        // Read back the canonical fact_id so the journal entry posts
        // against the right row even if the INSERT was a no-op on the
        // ON CONFLICT path (rebuild replay).
        let (fact_id,): (uuid::Uuid,) = sqlx::query_as(
            "SELECT id FROM financial_facts              WHERE kind = $1 AND source_table = $2 AND source_id = $3",
        )
        .bind("finance.inventory.transferred")
        .bind("ledger_overhead_absorbed")
        .bind(source_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        let fact_ref = boss_ledger::FactRef {
            id: fact_id,
            kind: "finance.inventory.transferred",
            happened_on,
            payload: &payload,
        };
        boss_ledger::post_fact_in_tx(&mut tx, &fact_ref)
            .await
            .map_err(|e| match e {
                // A bad account code is request data, not storage: the
                // step author (or seed) named an account the chart
                // doesn't hold. Surfaced as InvalidAccount so the HTTP
                // layer answers 422 with the offending code instead of
                // a generic 500.
                boss_ledger::LedgerError::UnknownAccount(code) => InventoryError::InvalidAccount(
                    format!("GL account code `{code}` is not in the chart of accounts"),
                ),
                e => InventoryError::Storage(format!("ledger post: {e}")),
            })?;

        tx.commit()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok((fact_id, inserted))
    }

    async fn record_inventory_je(
        &self,
        total_cost_cents: i64,
        debit_account: &str,
        credit_account: &str,
        memo: &str,
        source_table: &str,
        source_id: &str,
        happened_on: chrono::NaiveDate,
    ) -> Result<JeRecorded, InventoryError> {
        if total_cost_cents <= 0 {
            return Err(InventoryError::Storage(
                "total_cost_cents must be positive".to_string(),
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        // source_table folded in like the ledger movement endpoints do:
        // the emitted event must let rebuild reproduce the original
        // provenance tag (payload-authoritative source_table).
        let payload = serde_json::json!({
            "total_cost_cents": total_cost_cents,
            "debit_account": debit_account,
            "credit_account": credit_account,
            "memo": memo,
            "happened_on": happened_on.to_string(),
            "source_table": source_table,
            "source_id": source_id,
        });

        let inserted = insert_fact(
            &mut tx,
            "finance.inventory.transferred",
            happened_on,
            &payload,
            source_table,
            source_id,
        )
        .await?;

        let (fact_id,): (uuid::Uuid,) = sqlx::query_as(
            "SELECT id FROM financial_facts \
             WHERE kind = $1 AND source_table = $2 AND source_id = $3",
        )
        .bind("finance.inventory.transferred")
        .bind(source_table)
        .bind(source_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        let fact_ref = boss_ledger::FactRef {
            id: fact_id,
            kind: "finance.inventory.transferred",
            happened_on,
            payload: &payload,
        };
        boss_ledger::post_fact_in_tx(&mut tx, &fact_ref)
            .await
            .map_err(|e| InventoryError::Storage(format!("ledger post: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(JeRecorded {
            fact_id,
            inserted,
            payload,
        })
    }

    async fn receive_part_at(
        &self,
        part_sku: &str,
        qty: u32,
        unit_cost_cents: Option<i64>,
        now: chrono::DateTime<chrono::Utc>,
        source_id: &str,
    ) -> Result<ReceiveApplied, InventoryError> {
        // One tx wraps: (1) the idempotency check, (2) the on_hand
        // increment (+ weighted-avg-cost update), (3) the
        // `finance.inventory.received` proof-fact insert. The fact is
        // a DEDUP + AUDIT marker ONLY — unlike the consume's
        // `finance.inventory.transferred`, it drives NO GL journal
        // line. The matching DR-1300 stays on the idempotent
        // bill-approval path, so emitting a GL-driving fact here would
        // double-post it (the opposite of the bug we're fixing). The
        // ledger's RuleSet has no arm for this kind and no
        // gl_fact_projection_rules row maps it; `insert_dedup_fact`
        // writes the row WITHOUT calling the ledger, keeping it inert.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        // Idempotency guard. `on_hand += qty` is a relative mutation,
        // so a redelivered step-effect event (at-least-once JetStream
        // delivery) would double-increment, inflating on_hand while
        // the once-posted DR-1300 stays put — GL 1300 drifts. The
        // proof-fact this receive writes is its proof-of-application:
        // an existing fact with this source_id means the receive
        // committed on a prior delivery, so skip the increment and
        // return the current row unchanged. Mirrors `consume_part_at`.
        if fact_exists(
            &mut tx,
            "finance.inventory.received",
            "inventory_receipt",
            source_id,
        )
        .await?
        {
            drop(tx);
            let item = self
                .item_by_sku(part_sku)
                .await?
                .ok_or_else(|| InventoryError::NotFound(part_sku.to_string()))?;
            return Ok(ReceiveApplied {
                item,
                receipt_payload: None,
            });
        }

        // Value-primary receive: the exact line total (qty × the PO
        // unit price) lands on the row — no re-averaging arithmetic
        // exists on the add side, so nothing truncates. The single
        // UPDATE serializes concurrent receives on the row lock.
        // When the caller passes None (no cost data — manual
        // replenishment), only on_hand moves and the row's value is
        // untouched (the honest reading of "we don't know what this
        // cost"; the derived display average dilutes accordingly).
        let row: Option<InventoryItemRow> = match unit_cost_cents {
            Some(unit_cost) if unit_cost > 0 => sqlx::query_as(
                "UPDATE inventory_items SET \
                    on_hand = on_hand + $2, \
                    value_cents = value_cents + ($3::bigint * $2), \
                    updated_at = $4 \
                 WHERE part_sku = $1 RETURNING *",
            )
            .bind(part_sku)
            .bind(qty as i32)
            .bind(unit_cost)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?,
            _ => sqlx::query_as(
                "UPDATE inventory_items SET on_hand = on_hand + $2, updated_at = $3 \
                 WHERE part_sku = $1 RETURNING *",
            )
            .bind(part_sku)
            .bind(qty as i32)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?,
        };

        let item = match row {
            Some(r) => r.into_item(),
            None => {
                drop(tx);
                return Err(InventoryError::NotFound(part_sku.to_string()));
            }
        };

        // Proof-of-application fact. Idempotent on the unique
        // (kind, source_table, source_id) index; written in the SAME
        // tx as the increment so the guard above and the on_hand
        // mutation commit atomically. GL-inert by construction — no
        // ledger post here. The payload is the ONE construction: the
        // caller emits ITEM_RECEIVED from the returned copy verbatim,
        // so the fact rebuilt from the event is byte-identical — and a
        // replay (guard above) returns None and emits nothing.
        let payload = serde_json::json!({
            "part_sku": part_sku,
            "qty": qty,
            "unit_cost_cents": unit_cost_cents,
            "received_on": now.date_naive(),
            "source_id": source_id,
        });
        insert_dedup_fact(
            &mut tx,
            "finance.inventory.received",
            now.date_naive(),
            &payload,
            "inventory_receipt",
            source_id,
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(ReceiveApplied {
            item,
            receipt_payload: Some(payload),
        })
    }

    async fn create_purchase_order_at(
        &self,
        po: &PurchaseOrder,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        // Upsert so re-emitted POs (the sim re-POSTs on every status
        // transition, see the generator's `emit_po` path) update the
        // status + received_on on conflict instead of silently dropping.
        // Without this, a PO stays in its initial Draft state forever
        // and the vendor_invoices chain loses its trigger signal.
        //
        // `vendor_id` is populated alongside `vendor` so the FK
        // (`vendor_id REFERENCES vendors(id)`) is actually wired.
        // Today the brewery sim's vendor string IS the vendor id
        // (`vnd-bigseed-NNN`); the soft-FK lint that backstops this
        // would warn if the value didn't resolve, but in production
        // we want the hard FK doing the work.
        let status_str = po_status_str(&po.status);
        sqlx::query(
            "INSERT INTO purchase_orders (id, vendor_id, vendor, status, placed_on, expected_on, received_on, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (id) DO UPDATE SET \
                vendor_id = EXCLUDED.vendor_id, \
                status = EXCLUDED.status, \
                expected_on = EXCLUDED.expected_on, \
                received_on = EXCLUDED.received_on",
        )
        .bind(&po.id)
        .bind(&po.vendor)        // vendor_id — same string as vendor today
        .bind(&po.vendor)
        .bind(status_str)
        .bind(po.placed_on)
        .bind(po.expected_on)
        .bind(po.received_on)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        for line in &po.lines {
            sqlx::query(
                "INSERT INTO purchase_order_lines (po_id, part_sku, qty, unit_cost_cents, currency) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (po_id, part_sku) DO NOTHING",
            )
            .bind(&po.id)
            .bind(&line.part_sku)
            .bind(line.qty as i32)
            .bind(line.unit_cost_cents)
            .bind(&line.currency)
            .execute(&self.pool)
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        }

        Ok(())
    }

    async fn update_po_status(&self, id: &str, status: &str) -> Result<(), InventoryError> {
        // Validate status.
        let _ = parse_po_status(status)?;

        let result = sqlx::query("UPDATE purchase_orders SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(InventoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn all_vendors(&self) -> Result<Vec<Vendor>, InventoryError> {
        let rows: Vec<VendorRow> =
            sqlx::query_as("SELECT id, name, contact_name, contact_email, city, state, lead_time_days, payment_terms, category, behavior FROM vendors ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(|r| r.into_vendor()).collect())
    }

    async fn create_vendor_at(
        &self,
        vendor: &Vendor,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, InventoryError> {
        sqlx::query(
            "INSERT INTO vendors (id, name, contact_name, contact_email, city, state, lead_time_days, payment_terms, category, behavior, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&vendor.id)
        .bind(&vendor.name)
        .bind(&vendor.contact_name)
        .bind(&vendor.contact_email)
        .bind(&vendor.city)
        .bind(&vendor.state)
        .bind(vendor.lead_time_days as i16)
        .bind(&vendor.payment_terms)
        .bind(&vendor.category)
        .bind(vendor.behavior.as_ref().map(|b| serde_json::to_value(b).unwrap_or_default()))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("duplicate key") || msg.contains("unique constraint") {
                InventoryError::Conflict(format!("vendor {} already exists", vendor.id))
            } else {
                InventoryError::Storage(msg)
            }
        })?;
        Ok(vendor.id.clone())
    }

    async fn update_vendor(&self, id: &str, vendor: &Vendor) -> Result<(), InventoryError> {
        let result = sqlx::query(
            "UPDATE vendors SET name = $1, contact_name = $2, contact_email = $3, \
             city = $4, state = $5, lead_time_days = $6, payment_terms = $7, category = $8, \
             behavior = $9 \
             WHERE id = $10",
        )
        .bind(&vendor.name)
        .bind(&vendor.contact_name)
        .bind(&vendor.contact_email)
        .bind(&vendor.city)
        .bind(&vendor.state)
        .bind(vendor.lead_time_days as i16)
        .bind(&vendor.payment_terms)
        .bind(&vendor.category)
        .bind(
            vendor
                .behavior
                .as_ref()
                .map(|b| serde_json::to_value(b).unwrap_or_default()),
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(InventoryError::NotFound(format!("vendor {id}")));
        }
        Ok(())
    }

    async fn delete_vendor(&self, id: &str) -> Result<(), InventoryError> {
        let result = sqlx::query("DELETE FROM vendors WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(InventoryError::NotFound(format!("vendor {id}")));
        }
        Ok(())
    }

    async fn upsert_vendor_invoice_at(
        &self,
        invoice: &VendorInvoice,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), InventoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;

        sqlx::query(
            "INSERT INTO vendor_invoices (
                id, po_id, vendor, vendor_invoice_no, amount_cents, currency, received_on,
                matched_on, approved_on, paid_on, status,
                discrepancy_cents, discrepancy_kind, created_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             ON CONFLICT (id) DO UPDATE SET
                po_id             = EXCLUDED.po_id,
                vendor            = EXCLUDED.vendor,
                vendor_invoice_no = EXCLUDED.vendor_invoice_no,
                amount_cents      = EXCLUDED.amount_cents,
                currency          = EXCLUDED.currency,
                received_on       = EXCLUDED.received_on,
                matched_on        = EXCLUDED.matched_on,
                approved_on       = EXCLUDED.approved_on,
                paid_on           = EXCLUDED.paid_on,
                status            = EXCLUDED.status,
                discrepancy_cents = EXCLUDED.discrepancy_cents,
                discrepancy_kind  = EXCLUDED.discrepancy_kind",
        )
        .bind(&invoice.id)
        .bind(&invoice.po_id)
        .bind(&invoice.vendor)
        .bind(&invoice.vendor_invoice_no)
        .bind(invoice.amount_cents)
        .bind(&invoice.currency)
        .bind(invoice.received_on)
        .bind(invoice.matched_on)
        .bind(invoice.approved_on)
        .bind(invoice.paid_on)
        .bind(invoice.status.as_str())
        .bind(invoice.discrepancy_cents)
        .bind(invoice.discrepancy_kind.as_ref().map(|k| k.as_str()))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        // Emit financial facts for state transitions this upsert represents.
        // Idempotent via the unique (kind, source) index — replays and
        // repeated upserts on an already-approved/paid invoice are no-ops.
        if let Some(approved_on) = invoice.approved_on {
            // Shared helper so this in-tx fact and the emitted
            // `inventory.vendor_invoice.approved` event (http/vendor_invoices.rs)
            // are byte-identical on rebuild. `lines` (when present) is the
            // authoritative source for the `bill_approved` JE amount.
            let payload = bill_approved_payload(invoice, approved_on);
            insert_fact(
                &mut tx,
                "finance.bill.approved",
                approved_on,
                &payload,
                "vendor_invoices",
                &invoice.id,
            )
            .await?;
        }
        if let Some(paid_on) = invoice.paid_on {
            let payload = bill_paid_payload(invoice, paid_on);
            insert_fact(
                &mut tx,
                "finance.bill.paid",
                paid_on,
                &payload,
                "vendor_invoices",
                &invoice.id,
            )
            .await?;
        }

        tx.commit()
            .await
            .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn all_vendor_invoices(
        &self,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<VendorInvoice>, InventoryError> {
        let rows: Vec<VendorInvoiceRow> = match status {
            Some(s) => sqlx::query_as(
                "SELECT id, po_id, vendor, vendor_invoice_no, amount_cents, currency, received_on,
                    matched_on, approved_on, paid_on, status,
                    discrepancy_cents, discrepancy_kind
                 FROM vendor_invoices WHERE status = $1
                 ORDER BY received_on DESC LIMIT $2",
            )
            .bind(s)
            .bind(limit)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query_as(
                "SELECT id, po_id, vendor, vendor_invoice_no, amount_cents, currency, received_on,
                    matched_on, approved_on, paid_on, status,
                    discrepancy_cents, discrepancy_kind
                 FROM vendor_invoices
                 ORDER BY received_on DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(|r| r.into_domain()).collect())
    }

    async fn vendor_invoice_by_id(
        &self,
        id: &str,
    ) -> Result<Option<VendorInvoice>, InventoryError> {
        let row: Option<VendorInvoiceRow> = sqlx::query_as(
            "SELECT id, po_id, vendor, vendor_invoice_no, amount_cents, currency, received_on,
                matched_on, approved_on, paid_on, status,
                discrepancy_cents, discrepancy_kind
             FROM vendor_invoices WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;
        Ok(row.map(|r| r.into_domain()))
    }

    async fn ap_aging(&self, today: chrono::NaiveDate) -> Result<ApAging, InventoryError> {
        // Unpaid vendor invoices bucketed by days since `received_on`.
        // MVP-simple: no vendor join for due-date computation, so "1-30"
        // means "received 1-30 days ago" rather than "past due 1-30"
        // days. Close enough for an eyeball rollup; a future v2 can
        // join vendors.payment_terms for true past-due aging.
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT \
                CASE \
                    WHEN $1::date - received_on <= 0 THEN 'current' \
                    WHEN $1::date - received_on <= 30 THEN '1-30' \
                    WHEN $1::date - received_on <= 60 THEN '31-60' \
                    WHEN $1::date - received_on <= 90 THEN '61-90' \
                    ELSE '90+' \
                END as label, \
                COUNT(*)::bigint, \
                COALESCE(SUM(amount_cents), 0)::bigint \
             FROM vendor_invoices \
             WHERE status <> 'paid' \
             GROUP BY 1",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| InventoryError::Storage(e.to_string()))?;

        let mut map: std::collections::HashMap<&'static str, (i64, i64)> =
            std::collections::HashMap::new();
        for (label, count, total_cents) in &rows {
            let key: &'static str = match label.as_str() {
                "current" => "current",
                "1-30" => "1-30",
                "31-60" => "31-60",
                "61-90" => "61-90",
                _ => "90+",
            };
            map.insert(key, (*count, *total_cents));
        }
        let buckets = crate::in_memory::canonical_buckets(&map);
        let total_outstanding_cents: i64 = buckets.iter().map(|b| b.total_cents).sum();
        let total_invoice_count: i64 = buckets.iter().map(|b| b.count).sum();

        Ok(ApAging {
            buckets,
            total_outstanding_cents,
            total_invoice_count,
            currency: "USD".to_string(),
        })
    }
}

impl PgInventory {
    /// Fetch lines for a purchase order and assemble a full `PurchaseOrder`.
    async fn assemble(&self, row: PurchaseOrderRow) -> Result<PurchaseOrder, InventoryError> {
        let lines: Vec<PoLineRow> =
            sqlx::query_as("SELECT * FROM purchase_order_lines WHERE po_id = $1 ORDER BY part_sku")
                .bind(&row.id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| InventoryError::Storage(e.to_string()))?;

        Ok(PurchaseOrder {
            id: row.id,
            vendor: row.vendor,
            status: parse_po_status(&row.status).unwrap_or(PoStatus::Draft),
            placed_on: row.placed_on,
            expected_on: row.expected_on,
            received_on: row.received_on,
            lines: lines.into_iter().map(|l| l.into_line()).collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct InventoryItemRow {
    part_sku: String,
    bin: String,
    on_hand: i32,
    allocated: i32,
    reorder_point: i32,
    reorder_qty: i32,
    trailing_90d_usage: i32,
    value_cents: i64,
    avg_cost_cents: i64,
    vendor_price_cents: Option<i64>,
    vendor_category: Option<String>,
}

impl InventoryItemRow {
    fn into_item(self) -> InventoryItem {
        InventoryItem {
            part_sku: self.part_sku,
            bin: self.bin,
            on_hand: self.on_hand as u32,
            allocated: self.allocated as u32,
            reorder_point: self.reorder_point as u32,
            reorder_qty: self.reorder_qty as u32,
            trailing_90d_usage: self.trailing_90d_usage as u32,
            value_cents: self.value_cents,
            avg_cost_cents: self.avg_cost_cents,
            vendor_price_cents: self.vendor_price_cents,
            vendor_category: self.vendor_category,
        }
    }
}

#[derive(sqlx::FromRow)]
struct VendorRow {
    id: String,
    // Identity-first: descriptive columns are nullable (see Vendor).
    name: Option<String>,
    contact_name: Option<String>,
    contact_email: Option<String>,
    city: Option<String>,
    state: Option<String>,
    lead_time_days: i16,
    payment_terms: Option<String>,
    category: Option<String>,
    behavior: Option<serde_json::Value>,
}

impl VendorRow {
    fn into_vendor(self) -> Vendor {
        Vendor {
            id: self.id,
            name: self.name,
            contact_name: self.contact_name,
            contact_email: self.contact_email,
            city: self.city,
            state: self.state,
            lead_time_days: self.lead_time_days as u16,
            payment_terms: self.payment_terms,
            category: self.category,
            // A malformed profile degrades to None rather than failing the
            // whole vendor read.
            behavior: self.behavior.and_then(|v| serde_json::from_value(v).ok()),
        }
    }
}

#[derive(sqlx::FromRow)]
struct PurchaseOrderRow {
    id: String,
    // Identity-first: a Draft PO may carry none of these until placed.
    vendor: Option<String>,
    status: String,
    placed_on: Option<chrono::NaiveDate>,
    expected_on: Option<chrono::NaiveDate>,
    received_on: Option<chrono::NaiveDate>,
}

#[derive(sqlx::FromRow)]
struct PoLineRow {
    #[allow(dead_code)]
    po_id: String,
    part_sku: String,
    qty: i32,
    unit_cost_cents: i64,
    currency: String,
}

impl PoLineRow {
    fn into_line(self) -> PurchaseOrderLine {
        PurchaseOrderLine {
            part_sku: self.part_sku,
            qty: self.qty as u32,
            unit_cost_cents: self.unit_cost_cents,
            currency: self.currency,
        }
    }
}

// ---------------------------------------------------------------------------
// Enum parsing
// ---------------------------------------------------------------------------

/// Does a financial_fact with this natural key already exist in this tx's
/// view? Used as the idempotency guard for relative stock mutations: the
/// fact a mutation writes is its proof-of-application, so an existing fact
/// means a redelivered event already applied the mutation.
async fn fact_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    source_table: &str,
    source_id: &str,
) -> Result<bool, InventoryError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| InventoryError::Storage(e.to_string()))?;
    Ok(row.is_some())
}

/// Insert a financial fact inside an existing transaction WITHOUT posting
/// it to the ledger. This is the dedup + audit marker path: the fact row
/// is a proof-of-application for a relative stock mutation (its existence
/// gates a redelivered event), and an audit record of the receipt — but it
/// is deliberately GL-INERT. It must NOT be projected to a journal line:
/// the `receive` path's DR-1300 rides the idempotent bill-approval path, so
/// posting here would double-post it. Contrast `insert_fact`, which DOES
/// call `post_fact_in_tx` because its fact kinds drive the GL. Idempotent
/// on the unique (kind, source_table, source_id) index.
async fn insert_dedup_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    happened_on: chrono::NaiveDate,
    payload: &serde_json::Value,
    source_table: &str,
    source_id: &str,
) -> Result<(), InventoryError> {
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'inventory') \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING",
    )
    .bind(boss_ledger::deterministic_fact_id(
        kind,
        source_table,
        source_id,
    ))
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_table)
    .bind(source_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| InventoryError::Storage(e.to_string()))?;
    Ok(())
}

/// Insert a financial fact inside an existing transaction, then post it
/// synchronously to the ledger. Idempotent — replay of the same fact is a
/// no-op for both the fact row (unique index) and the journal entry
/// (unique (fact_id, rule_version_id)).
/// Insert the fact idempotently; `Ok(true)` = this call inserted it,
/// `Ok(false)` = the (kind, source_table, source_id) key already
/// existed (replay). Callers gate occurrence-event emits on the flag.
async fn insert_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    happened_on: chrono::NaiveDate,
    payload: &serde_json::Value,
    source_table: &str,
    source_id: &str,
) -> Result<bool, InventoryError> {
    let result = sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'inventory') \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING",
    )
    .bind(boss_ledger::deterministic_fact_id(
        kind,
        source_table,
        source_id,
    ))
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_table)
    .bind(source_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| InventoryError::Storage(e.to_string()))?;

    let (fact_id,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 AND source_id = $3",
    )
    .bind(kind)
    .bind(source_table)
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| InventoryError::Storage(e.to_string()))?;

    let fact_ref = boss_ledger::FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    boss_ledger::post_fact_in_tx(tx, &fact_ref)
        .await
        .map_err(|e| InventoryError::Storage(format!("ledger post failed: {e}")))?;
    Ok(result.rows_affected() > 0)
}

pub(crate) fn po_status_str(s: &PoStatus) -> &'static str {
    match s {
        PoStatus::Draft => "draft",
        PoStatus::Submitted => "submitted",
        PoStatus::Acknowledged => "acknowledged",
        PoStatus::InTransit => "in-transit",
        PoStatus::Received => "received",
        PoStatus::Closed => "closed",
    }
}

fn parse_po_status(s: &str) -> Result<PoStatus, InventoryError> {
    match s {
        "draft" => Ok(PoStatus::Draft),
        "submitted" => Ok(PoStatus::Submitted),
        "acknowledged" => Ok(PoStatus::Acknowledged),
        "in-transit" => Ok(PoStatus::InTransit),
        "received" => Ok(PoStatus::Received),
        "closed" => Ok(PoStatus::Closed),
        other => Err(InventoryError::Storage(format!(
            "unknown PO status: {other}"
        ))),
    }
}

#[derive(sqlx::FromRow)]
struct VendorInvoiceRow {
    id: String,
    po_id: String,
    vendor: String,
    vendor_invoice_no: String,
    amount_cents: i64,
    currency: String,
    received_on: chrono::NaiveDate,
    matched_on: Option<chrono::NaiveDate>,
    approved_on: Option<chrono::NaiveDate>,
    paid_on: Option<chrono::NaiveDate>,
    status: String,
    discrepancy_cents: Option<i64>,
    discrepancy_kind: Option<String>,
}

impl VendorInvoiceRow {
    fn into_domain(self) -> VendorInvoice {
        VendorInvoice {
            id: self.id,
            po_id: self.po_id,
            vendor: self.vendor,
            vendor_invoice_no: self.vendor_invoice_no,
            amount_cents: self.amount_cents,
            currency: self.currency,
            received_on: self.received_on,
            matched_on: self.matched_on,
            approved_on: self.approved_on,
            paid_on: self.paid_on,
            status: VendorInvoiceStatus::parse(&self.status)
                .unwrap_or(VendorInvoiceStatus::Received),
            discrepancy_cents: self.discrepancy_cents,
            // Free-text code lifted to the Class registry; trust the
            // stored value (validation lives at the API boundary).
            discrepancy_kind: self.discrepancy_kind.map(DiscrepancyKind),
            // vendor_invoice_lines isn't a persisted table yet —
            // the lines breakdown is captured at fact-emit time
            // from step metadata and lives on the financial_facts
            // payload, not on the vendor_invoice row. Re-reads
            // from the DB therefore return an empty lines vec; the
            // bill_approved rule's lump-amount fallback handles
            // re-reads correctly.
            lines: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_po_statuses() {
        let cases = [
            "draft",
            "submitted",
            "acknowledged",
            "in-transit",
            "received",
            "closed",
        ];
        for s in cases {
            assert!(parse_po_status(s).is_ok(), "failed to parse PO status: {s}");
        }
        assert!(parse_po_status("pending").is_err());
    }
}
