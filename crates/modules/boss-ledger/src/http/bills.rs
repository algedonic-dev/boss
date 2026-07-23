use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::*;

// --- ledger bills (general accounts payable) ------------------------------
// A "bill" is a general AP obligation (rent, utilities, insurance,
// services, …) owned by the ledger and routed to a GL debit account by its
// free-text `bill_category` via bill_accounts.toml. Decoupled from the
// inventory parts vendor-invoice. Approve posts DR <category>/CR 2100; pay
// posts DR 2100/CR 1000. See boss-ledger/src/bills.rs + the reused-unchanged
// finance.bill.{approved,paid} rules in rules.rs.

#[derive(Deserialize)]
pub(super) struct CreateBillBody {
    id: String,
    vendor: String,
    /// Free text; routed to a GL debit account by bill_accounts.toml.
    bill_category: String,
    amount_cents: i64,
    #[serde(default = "default_currency")]
    currency: String,
    /// Issue date. Defaults to the clock's "now" date.
    #[serde(default)]
    issued_on: Option<NaiveDate>,
    #[serde(default)]
    due_on: Option<NaiveDate>,
    /// Approval date — becomes the JE's posted_on. Defaults to issued_on.
    #[serde(default)]
    approved_on: Option<NaiveDate>,
    /// Opaque per-bill metadata bag (no part_sku). Defaults to `[]`.
    #[serde(default)]
    lines: Option<serde_json::Value>,
    #[serde(default)]
    memo: Option<String>,
}

/// Record a `finance.bill.*` fact + post its journal entry inside the
/// caller's tx. The caller owns commit + the `ledger.bill.*` emit, so the
/// subledger row, the fact, and the JE land (or roll back) atomically.
async fn record_and_post_bill_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    fact_kind: &'static str,
    bill_id: &str,
    happened_on: NaiveDate,
    payload: &serde_json::Value,
) -> Result<(), Response> {
    let live_fact_id = crate::events::record_fact_in_tx(
        tx,
        crate::events::FactWrite {
            kind: fact_kind,
            happened_on,
            payload,
            source_table: Some("ledger_bills"),
            source_id: Some(bill_id),
            // Matches the event source ("ledger") — the projection's
            // created_by fallback — so rebuilt facts match live ones.
            created_by: "ledger",
        },
    )
    .await
    .map_err(ledger_err)?
    .id;

    let fact = crate::types::FactRef {
        id: live_fact_id,
        kind: fact_kind,
        happened_on,
        payload,
    };
    crate::postgres::post_fact_in_tx(tx, &fact)
        .await
        .map_err(ledger_err)?;
    Ok(())
}

/// Approve a bill: record the subledger row + post the accrual JE
/// (DR `<bill_account(category)>` / CR 2100) in one tx, then emit
/// `ledger.bill.approved` for rebuild parity. Idempotent on the bill id.
pub(super) async fn create_bill(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateBillBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let issued_on = body.issued_on.unwrap_or_else(|| now.date_naive());
    let approved_on = body.approved_on.unwrap_or(issued_on);
    let lines = body.lines.clone().unwrap_or_else(|| serde_json::json!([]));

    // Idempotent replay: same expense-bill step → same id → return the
    // existing (already-posted) bill without re-posting.
    match crate::bills::get(&state.pool, &body.id).await {
        Ok(Some(existing)) => return Json(existing).into_response(),
        Ok(None) => {}
        Err(e) => return ledger_err(e),
    }

    // Outbox phase 2: `ledger.bill.approved` records inside this tx.
    let stamp = super::event_stamp(&state, &user, now).await;
    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => return storage_err(e),
    };

    let bill = match crate::bills::upsert_approved_in_tx(
        &mut tx,
        crate::bills::NewBill {
            id: &body.id,
            vendor: &body.vendor,
            bill_category: &body.bill_category,
            amount_cents: body.amount_cents,
            currency: &body.currency,
            issued_on,
            due_on: body.due_on,
            approved_on,
            lines: &lines,
            memo: body.memo.as_deref(),
            created_by: &user.id,
        },
    )
    .await
    {
        Ok(b) => b,
        Err(e) => return ledger_err(e),
    };

    // NB: no `lines` key in the GL fact payload. A general bill posts as a
    // lump (DR <category> / CR 2100 at amount_cents); the `bill_approved`
    // rule treats any `lines` array as a PO-line breakdown that must sum to
    // the amount, which our opaque metadata bag isn't. That bag stays on the
    // `ledger_bills` subledger row, out of the GL fact.
    let payload = serde_json::json!({
        "bill_id": bill.id,
        "vendor": bill.vendor,
        "bill_category": bill.bill_category,
        "amount_cents": bill.amount_cents,
        "currency": bill.currency,
        "approved_on": approved_on,
    });

    if let Err(e) = record_and_post_bill_fact(
        &mut tx,
        "finance.bill.approved",
        &bill.id,
        approved_on,
        &payload,
    )
    .await
    {
        return e;
    }
    if let Err(e) =
        crate::events::record_ledger_event_in_tx(&mut tx, &stamp, "ledger.bill.approved", payload)
            .await
    {
        return ledger_err(e);
    }

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    (StatusCode::CREATED, Json(bill)).into_response()
}

/// Pay one bill: flip `approved` → `paid`, post the drain JE
/// (DR 2100 / CR 1000), emit `ledger.bill.paid`. Shared by the single-pay
/// endpoint + the batch pay-run.
async fn pay_one_bill(
    state: &LedgerApiState,
    bill: &crate::bills::Bill,
    paid_on: NaiveDate,
    stamp: &boss_core::publisher::EventStamp,
) -> Result<crate::bills::Bill, Response> {
    let mut tx = state.pool.begin().await.map_err(storage_err)?;

    let paid = match crate::bills::mark_paid_in_tx(&mut tx, &bill.id, paid_on).await {
        Ok(Some(b)) => b,
        // A concurrent pay already flipped it — nothing to post.
        Ok(None) => return Ok(bill.clone()),
        Err(e) => return Err(ledger_err(e)),
    };

    let payload = serde_json::json!({
        "bill_id": paid.id,
        "vendor": paid.vendor,
        "bill_category": paid.bill_category,
        "amount_cents": paid.amount_cents,
        "currency": paid.currency,
        "paid_on": paid_on,
    });
    record_and_post_bill_fact(&mut tx, "finance.bill.paid", &paid.id, paid_on, &payload).await?;
    crate::events::record_ledger_event_in_tx(&mut tx, stamp, "ledger.bill.paid", payload)
        .await
        .map_err(ledger_err)?;

    tx.commit().await.map_err(storage_err)?;
    Ok(paid)
}

#[derive(Deserialize)]
pub(super) struct PayBillBody {
    /// Settlement date. Defaults to the clock's "now" date.
    #[serde(default)]
    paid_on: Option<NaiveDate>,
}

pub(super) async fn pay_bill(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<PayBillBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let paid_on = body.paid_on.unwrap_or_else(|| now.date_naive());

    let existing = match crate::bills::get(&state.pool, &id).await {
        Ok(Some(b)) => b,
        Ok(None) => return (StatusCode::NOT_FOUND, "bill not found").into_response(),
        Err(e) => return ledger_err(e),
    };
    if existing.status == "paid" {
        return Json(existing).into_response(); // idempotent
    }

    let stamp = super::event_stamp(&state, &user, now).await;
    match pay_one_bill(&state, &existing, paid_on, &stamp).await {
        Ok(bill) => Json(bill).into_response(),
        Err(e) => e,
    }
}

#[derive(Deserialize)]
pub(super) struct BatchPayBillsBody {
    /// Settlement date stamped onto each bill. Defaults to the clock's now.
    #[serde(default)]
    paid_on: Option<NaiveDate>,
    /// Cap how many bills to settle in this run. Defaults to 1000.
    #[serde(default)]
    max_count: Option<i64>,
}

#[derive(Serialize)]
struct BatchPayBillsResponse {
    paid_count: usize,
    total_paid_cents: i64,
    bill_ids: Vec<String>,
}

/// Settle every approved ledger bill — the monthly facility-overhead pay
/// step's batch. Re-runnable: already-paid bills are skipped because the
/// listing filter is `approved`.
pub(super) async fn batch_pay_bills(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<BatchPayBillsBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let paid_on = body.paid_on.unwrap_or_else(|| now.date_naive());
    let limit = body.max_count.unwrap_or(1000);

    let approved = match crate::bills::list(&state.pool, Some("approved"), limit).await {
        Ok(rows) => rows,
        Err(e) => return ledger_err(e),
    };

    let stamp = super::event_stamp(&state, &user, now).await;
    let mut total: i64 = 0;
    let mut ids = Vec::with_capacity(approved.len());
    for bill in &approved {
        match pay_one_bill(&state, bill, paid_on, &stamp).await {
            Ok(paid) => {
                total += paid.amount_cents;
                ids.push(paid.id);
            }
            Err(e) => return e,
        }
    }

    Json(BatchPayBillsResponse {
        paid_count: ids.len(),
        total_paid_cents: total,
        bill_ids: ids,
    })
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct ListBillsQuery {
    status: Option<String>,
    limit: Option<i64>,
}

pub(super) async fn list_bills(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<ListBillsQuery>,
) -> Response {
    match crate::bills::list(&state.pool, q.status.as_deref(), q.limit.unwrap_or(500)).await {
        Ok(bills) => Json(bills).into_response(),
        Err(e) => ledger_err(e),
    }
}
