//! The arithmetic and the facts of an FG inventory delta, built ONCE for
//! both adapters. Until the adapters-agree suite (backlog be459ab9) only
//! the Postgres adapter built the GL moves and the JE payload, inline,
//! and the double answered no GL move, recorded no fact and minted a
//! random fact id — so no test answered by the double could see what a
//! produce, a consume or an opening JE puts on the log.

use chrono::{DateTime, DurationRound, NaiveDate, TimeDelta, Utc};

use crate::port::GlMove;
use crate::types::ProductInventory;

/// The fact a produce's WIP→FG move writes (DR 1320 / CR 1310), and an
/// opening / adjustment JE's.
pub const TRANSFER_FACT: &str = "finance.inventory.transferred";
/// The fact a consume's COGS recognition writes (DR 5100 / CR 1320).
pub const COGS_FACT: &str = "finance.cogs.recognized";
/// `source_table` of a produce's fact — with `source_id`, its proof of
/// application.
pub const PRODUCE_SOURCE: &str = "products_produce";
/// `source_table` of a consume's fact.
pub const CONSUME_SOURCE: &str = "products_consume";

/// An instant as the TIMESTAMPTZ column keeps it: to the microsecond.
pub fn as_stored(at: DateTime<Utc>) -> DateTime<Utc> {
    at.duration_trunc(TimeDelta::microseconds(1)).unwrap_or(at)
}

/// The display per-unit cost the `production_cost_cents` generated
/// column derives (`25-products.sql`).
pub fn unit_cost(on_hand: i32, value_cents: i64) -> i64 {
    if on_hand > 0 {
        value_cents / on_hand as i64
    } else {
        0
    }
}

/// The value a consume of `qty` drains: round(value × qty / on_hand),
/// the final unit taking the remainder so zero on hand forces zero
/// value. The caller has checked `on_hand >= qty > 0`.
pub fn drained_cents(on_hand: i32, value_cents: i64, qty: i32) -> i64 {
    if on_hand == qty {
        value_cents
    } else {
        (((value_cents as i128) * (qty as i128) + (on_hand as i128) / 2) / (on_hand as i128)) as i64
    }
}

/// The payload of `products.inventory.upserted`, built from the row AS
/// STORED — its derived per-unit cost and the write's instant — by every
/// write that records one, on both adapters. An absolute upsert used to
/// record the caller's row instead, so the log said a cost and a time the
/// table never held (backlog 797b6168).
pub fn inventory_upserted(stored: &ProductInventory) -> serde_json::Value {
    serde_json::to_value(stored).unwrap_or_default()
}

/// The WIP→FG move a produce of `qty` costing `total` cents answers and
/// records. `source_id` + `happened_on` ride in the payload so the
/// projection rule's `/source_id` + `/happened_on` pointers find them
/// on rebuild.
pub fn produced_move(
    sku: &str,
    location_id: &str,
    qty: i32,
    total: i64,
    now: DateTime<Utc>,
    source_id: String,
) -> GlMove {
    let happened_on = now.date_naive();
    let payload = serde_json::json!({
        "total_cost_cents": total,
        "debit_account": "1320",
        "credit_account": "1310",
        "memo": format!(
            "Production — produced {qty} × {sku} (WIP → FG, exact line total)"
        ),
        "sku": sku,
        "location_id": location_id,
        "qty": qty,
        "source_id": source_id,
        "happened_on": happened_on.to_string(),
    });
    GlMove {
        source_id,
        happened_on,
        payload,
    }
}

/// The COGS recognition a consume of `qty` draining `total` cents
/// answers and records, tagged with `revenue_category` when one is given
/// so per-category gross margin rolls up exactly.
pub fn consumed_move(
    sku: &str,
    location_id: &str,
    qty: i32,
    total: i64,
    revenue_category: Option<&str>,
    now: DateTime<Utc>,
    source_id: String,
) -> GlMove {
    let happened_on = now.date_naive();
    let mut payload = serde_json::json!({
        "total_cost_cents": total,
        "cogs_account": "5100",
        "inventory_account": "1320",
        "memo": format!("COGS — sold {qty} × {sku} (value drain)"),
        "sku": sku,
        "location_id": location_id,
        "qty": qty,
        "source_id": source_id,
        "happened_on": happened_on.to_string(),
    });
    if let Some(cat) = revenue_category {
        payload["revenue_category"] = serde_json::Value::String(cat.to_string());
    }
    GlMove {
        source_id,
        happened_on,
        payload,
    }
}

/// The payload of an opening / adjustment JE. `source_table` is folded
/// in like the ledger movement endpoints do: the emitted event must let
/// rebuild reproduce the original provenance tag.
pub fn je_payload(
    total_cost_cents: i64,
    debit_account: &str,
    credit_account: &str,
    memo: &str,
    source_table: &str,
    source_id: &str,
    happened_on: NaiveDate,
) -> serde_json::Value {
    serde_json::json!({
        "total_cost_cents": total_cost_cents,
        "debit_account": debit_account,
        "credit_account": credit_account,
        "memo": memo,
        "happened_on": happened_on.to_string(),
        "source_table": source_table,
        "source_id": source_id,
    })
}
