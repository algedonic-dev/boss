use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

// --- manual journal entries -----------------------------------------------

/// Admin-authored journal entry. Lines go in `lines`; `posted_on` defaults
/// to today. The body is materialized into a `finance.manual.entry` fact
/// and projected through the same rule pipeline as every other posting,
/// so period lock + the double-entry trigger apply uniformly.
#[derive(Deserialize)]
pub(super) struct ManualEntryBody {
    #[serde(default)]
    posted_on: Option<NaiveDate>,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    lines: Vec<ManualEntryLine>,
}

#[derive(Deserialize)]
struct ManualEntryLine {
    account_code: String,
    #[serde(default)]
    debit_cents: i64,
    #[serde(default)]
    credit_cents: i64,
    #[serde(default)]
    memo: Option<String>,
}

#[derive(Serialize)]
struct ManualEntryResponse {
    fact_id: Uuid,
    entry_id: Uuid,
    posted_on: NaiveDate,
}

pub(super) async fn create_manual_entry(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ManualEntryBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.lines.len() < 2 {
        return (
            StatusCode::BAD_REQUEST,
            "manual entry needs at least 2 lines".to_string(),
        )
            .into_response();
    }

    let (mut total_debits, mut total_credits) = (0i64, 0i64);
    for line in &body.lines {
        if line.debit_cents < 0 || line.credit_cents < 0 {
            return (
                StatusCode::BAD_REQUEST,
                "line amounts must be non-negative".to_string(),
            )
                .into_response();
        }
        if (line.debit_cents == 0) == (line.credit_cents == 0) {
            return (
                StatusCode::BAD_REQUEST,
                "each line must have exactly one of debit_cents or credit_cents".to_string(),
            )
                .into_response();
        }
        total_debits += line.debit_cents;
        total_credits += line.credit_cents;
    }
    if total_debits != total_credits {
        return (
            StatusCode::BAD_REQUEST,
            format!("entry unbalanced: debits={total_debits} credits={total_credits}",),
        )
            .into_response();
    }

    let posted_on = body
        .posted_on
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let created_by = body.created_by.unwrap_or_else(|| "admin".to_string());
    let fact_id = Uuid::new_v4();

    let lines_json: Vec<serde_json::Value> = body
        .lines
        .iter()
        .map(|l| {
            serde_json::json!({
                "account_code": l.account_code,
                "debit_cents": l.debit_cents,
                "credit_cents": l.credit_cents,
                "memo": l.memo,
            })
        })
        .collect();
    let entry_natural_id = fact_id.to_string();
    let payload = serde_json::json!({
        "entry_id": entry_natural_id,
        "posted_on": posted_on,
        "created_by": created_by,
        "memo": body.memo,
        "lines": lines_json,
    });

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    let fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            fact_id,
            kind: "finance.manual.entry",
            happened_on: posted_on,
            payload: &payload,
            source_table: Some("manual_entries"),
            source_id: Some(&entry_natural_id),
            created_by: &created_by,
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: fact_id,
        kind: "finance.manual.entry",
        happened_on: posted_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    let entry_row: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&mut *tx)
            .await;
    let entry_id = match entry_row {
        Ok((id,)) => id,
        Err(e) => return storage_err(e),
    };

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    crate::events::emit_after_commit(
        &state.publisher,
        "ledger.manual_entry.submitted",
        payload.clone(),
        boss_clock_client::now_from(&state.clock).await,
    )
    .await;

    Json(ManualEntryResponse {
        fact_id,
        entry_id,
        posted_on,
    })
    .into_response()
}

// --- COGS recognition -----------------------------------------------------

/// Body for `POST /api/ledger/cogs-recognized`. Emitted by the
/// sim's `products.consume` side-effect handler when finished
/// product leaves inventory; the payload carries the aggregated
/// `qty × unit_cost_cents` total for one Job step's worth of
/// consumption.
///
/// Real COGS sourced from actual consumption, not a percentage-of-
/// revenue estimate (see the COGS comment in
/// `rules.rs::invoice_issued`).
#[derive(Deserialize)]
pub(super) struct CogsRecognizedBody {
    /// Total cost of goods consumed, in cents. Required, positive.
    total_cost_cents: i64,
    /// Override the default COGS account (5100).
    #[serde(default)]
    cogs_account: Option<String>,
    /// Override the default inventory account (1300).
    #[serde(default)]
    inventory_account: Option<String>,
    /// Free-text memo for the JE.
    #[serde(default)]
    memo: Option<String>,
    /// When the consumption actually happened (defaults to today).
    #[serde(default)]
    happened_on: Option<NaiveDate>,
    /// Provenance — the table + id of the row that triggered this
    /// recognition (typically `("jobs_step", step_id)`). Required.
    /// The `(kind, source_table, source_id)` triple is unique in
    /// `financial_facts` so a replay re-firing the same step is a
    /// no-op. Required, not defaulted: a synthetic default would let
    /// a caller mint a journal entry with no traceable origin —
    /// a provenance violation (correctness-protocol §1).
    source_table: String,
    source_id: String,
    #[serde(default)]
    created_by: Option<String>,
}

#[derive(Serialize)]
struct CogsRecognizedResponse {
    fact_id: Uuid,
    entry_id: Uuid,
    posted_on: NaiveDate,
}

pub(super) async fn cogs_recognized_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CogsRecognizedBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.total_cost_cents <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            "total_cost_cents must be positive".to_string(),
        )
            .into_response();
    }

    let posted_on = body
        .happened_on
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let created_by = body.created_by.unwrap_or_else(|| "ledger".to_string());
    let fact_id = Uuid::new_v4();
    let cogs_account = body.cogs_account.unwrap_or_else(|| "5100".to_string());
    let inventory_account = body.inventory_account.unwrap_or_else(|| "1300".to_string());

    let mut payload = serde_json::json!({
        "total_cost_cents": body.total_cost_cents,
        "cogs_account": cogs_account,
        "inventory_account": inventory_account,
    });
    if let Some(memo) = &body.memo {
        payload["memo"] = serde_json::Value::String(memo.clone());
    }

    // source_table and source_id are required at the type layer —
    // a synthetic-default escape hatch would be a provenance
    // violation.
    let source_table = body.source_table;
    let source_id = body.source_id;
    if source_table.is_empty() || source_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "source_table and source_id are required (must be non-empty)".to_string(),
        )
            .into_response();
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    let fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            fact_id,
            kind: "finance.cogs.recognized",
            happened_on: posted_on,
            payload: &payload,
            source_table: Some(&source_table),
            source_id: Some(&source_id),
            created_by: &created_by,
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return ledger_err(e),
    };

    let fact = crate::types::FactRef {
        id: fact_id,
        kind: "finance.cogs.recognized",
        happened_on: posted_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }

    let entry_row: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&mut *tx)
            .await;
    let entry_id = match entry_row {
        Ok((id,)) => id,
        Err(e) => return storage_err(e),
    };

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    Json(CogsRecognizedResponse {
        fact_id,
        entry_id,
        posted_on,
    })
    .into_response()
}

// --- Inventory transferred (Model B cost flow) ----------------------------

/// Body for `POST /api/ledger/inventory-transferred`. Emitted by
/// the inventory + products side-effect handlers when value moves
/// between asset accounts (raw → WIP → finished goods) along the
/// production cost flow. Same projection-rule shape as the COGS
/// endpoint but the JE is asset-to-asset, not asset-to-expense.
#[derive(Deserialize)]
pub(super) struct InventoryTransferredBody {
    total_cost_cents: i64,
    debit_account: String,
    credit_account: String,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    happened_on: Option<NaiveDate>,
    /// Provenance — required, not defaulted: a synthetic default
    /// would let a caller mint a JE with no traceable origin
    /// (a provenance violation, correctness-protocol §1).
    source_table: String,
    source_id: String,
    #[serde(default)]
    created_by: Option<String>,
}

#[derive(Serialize)]
struct InventoryTransferredResponse {
    fact_id: Uuid,
    entry_id: Uuid,
    posted_on: NaiveDate,
}

pub(super) async fn inventory_transferred_handler(
    State(state): State<Arc<LedgerApiState>>,
    user: CurrentUser,
    Json(body): Json<InventoryTransferredBody>,
) -> Response {
    post_inventory_movement(
        state,
        user,
        body,
        "finance.inventory.transferred",
        "ledger.inventory.transferred",
    )
    .await
}

/// `POST /api/ledger/inventory-capitalized` — goods-receipt
/// capitalization (DR 1300 raw / CR 2110 GR-IR). The `inventory.receive`
/// handler posts here when a delivery lands, so raw inventory tracks
/// physical on_hand regardless of when the vendor bill arrives. The
/// distinct `finance.inventory.capitalized` kind keeps goods-receipt
/// provenance separate from inter-tier transfers; the JE body is the
/// same value-movement shape.
pub(super) async fn inventory_capitalized_handler(
    State(state): State<Arc<LedgerApiState>>,
    user: CurrentUser,
    Json(body): Json<InventoryTransferredBody>,
) -> Response {
    post_inventory_movement(
        state,
        user,
        body,
        "finance.inventory.capitalized",
        "ledger.inventory.capitalized",
    )
    .await
}

/// Shared body for the inventory value-movement endpoints (transfer +
/// capitalization): validate, record the fact, post the JE, and emit the
/// audit event the rebuild registry re-projects from. Parameterized on
/// the fact + event kind.
async fn post_inventory_movement(
    state: Arc<LedgerApiState>,
    CurrentUser(user): CurrentUser,
    body: InventoryTransferredBody,
    fact_kind: &'static str,
    event_kind: &'static str,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.total_cost_cents <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            "total_cost_cents must be positive".to_string(),
        )
            .into_response();
    }
    let posted_on = body
        .happened_on
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let created_by = body.created_by.unwrap_or_else(|| "ledger".to_string());
    let fact_id = Uuid::new_v4();
    let mut payload = serde_json::json!({
        "total_cost_cents": body.total_cost_cents,
        "debit_account": body.debit_account,
        "credit_account": body.credit_account,
    });
    if let Some(memo) = &body.memo {
        payload["memo"] = serde_json::Value::String(memo.clone());
    }
    // source_table + source_id are required at the type layer.
    let source_table = body.source_table;
    let source_id = body.source_id;
    if source_table.is_empty() || source_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "source_table and source_id are required (must be non-empty)".to_string(),
        )
            .into_response();
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };
    let fact_id = match crate::events::record_fact_in_tx(
        &mut tx,
        crate::events::FactWrite {
            fact_id,
            kind: fact_kind,
            happened_on: posted_on,
            payload: &payload,
            source_table: Some(&source_table),
            source_id: Some(&source_id),
            created_by: &created_by,
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return ledger_err(e),
    };
    let fact = crate::types::FactRef {
        id: fact_id,
        kind: "finance.inventory.transferred",
        happened_on: posted_on,
        payload: &payload,
    };
    if let Err(e) = crate::postgres::post_fact_in_tx(&mut tx, &fact).await {
        return ledger_err(e);
    }
    let entry_row: Result<(Uuid,), _> =
        sqlx::query_as("SELECT id FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&mut *tx)
            .await;
    let entry_id = match entry_row {
        Ok((id,)) => id,
        Err(e) => return storage_err(e),
    };
    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }
    // Emit an audit_log event so the projection survives a
    // TRUNCATE-then-replay rebuild from audit_log. Without this,
    // the financial_fact + journal entry would be re-created on
    // every live call but disappear on rebuild (no event to
    // re-project from). Brewery's opening balance JEs route here
    // via the seed_parts path; this audit event is what lets them
    // survive an epoch-loop restart.
    if let Some(pub_) = &state.publisher {
        let actor = user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
        let mut event_payload = payload.clone();
        event_payload["source_table"] = serde_json::Value::String(source_table.clone());
        event_payload["source_id"] = serde_json::Value::String(source_id.clone());
        event_payload["happened_on"] = serde_json::Value::String(posted_on.to_string());
        let now = boss_clock_client::now_from(&state.clock).await;
        pub_.emit_with_actor_at(event_kind, actor, event_payload, now)
            .await;
    }
    Json(InventoryTransferredResponse {
        fact_id,
        entry_id,
        posted_on,
    })
    .into_response()
}

// --- supersede ------------------------------------------------------------

/// Body for `POST /api/ledger/financial-facts/{id}/supersede`. The
/// fact is identified by URL id (the live `financial_facts.id`); the
/// natural-key triple is read from that row and used both for the
/// in-tx UPDATE and for the audit-log replay payload.
#[derive(Deserialize)]
pub(super) struct SupersedeBody {
    /// Required, non-empty. Becomes `financial_facts.supersede_reason`
    /// and the reason field of the `ledger.fact.superseded` audit
    /// event. Should explain *why* the row is being retired (link to
    /// an incident, a duplicate-detection summary, etc.).
    reason: String,
    /// Optional pointer to a corrected replacement fact. The caller
    /// is responsible for inserting the replacement separately via
    /// the originating domain's normal write path with a *different*
    /// source_id; this field just records the link.
    #[serde(default)]
    superseded_by: Option<Uuid>,
}

#[derive(Serialize)]
struct SupersedeResponse {
    fact_id: Uuid,
    entries_dropped: u64,
}

/// Mark a financial_fact as superseded. Append-only correction
/// path — replaces the raw SQL DELETE that was used to
/// purge the AR-double-credit duplicates. Behavior:
///   - 200 + summary on success (entries dropped, fact id).
///   - 404 if no fact exists for the URL id.
///   - 409 if the fact was already superseded (returns the existing
///     reason in the body so the operator can decide).
///   - 409 if the fact's period is locked — operator must unlock
///     first (a separate, audited action).
pub(super) async fn supersede_fact_handler(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Path(fact_id): Path<Uuid>,
    Json(body): Json<SupersedeBody>,
) -> Response {
    if let Some(r) = reject_if_auditor(&user) {
        return r;
    }
    if body.reason.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "supersede reason must be non-empty".to_string(),
        )
            .into_response();
    }

    // Resolve the natural-key triple from the URL id. We need it
    // both for `apply_supersede_in_tx` (which keys on the triple)
    // and for the audit-log payload (which the rebuild path replays
    // by triple, since live UUIDs and rebuilt UUIDs diverge).
    let row: Result<Option<(String, Option<String>, Option<String>)>, _> = sqlx::query_as(
        "SELECT kind, source_table, source_id \
         FROM financial_facts WHERE id = $1",
    )
    .bind(fact_id)
    .fetch_optional(&state.pool)
    .await;
    let (kind, source_table, source_id) = match row {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                format!("no financial_fact with id {fact_id}"),
            )
                .into_response();
        }
        Err(e) => return storage_err(e),
    };

    let req = crate::supersede::SupersedeRequest {
        kind: kind.clone(),
        source_table: source_table.clone(),
        source_id: source_id.clone(),
        reason: body.reason.clone(),
        superseded_by: body.superseded_by,
    };

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_err(e),
    };

    let outcome = match crate::supersede::apply_supersede_in_tx(&mut tx, &req).await {
        Ok(o) => o,
        Err(e) => return ledger_err(e),
    };

    let (fact_id, entries_dropped) = match outcome {
        crate::supersede::SupersedeOutcome::Applied {
            fact_id,
            entries_dropped,
        } => (fact_id, entries_dropped),
        crate::supersede::SupersedeOutcome::AlreadySuperseded { fact_id, reason } => {
            return (
                StatusCode::CONFLICT,
                format!("fact {fact_id} already superseded: {reason}"),
            )
                .into_response();
        }
        crate::supersede::SupersedeOutcome::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                format!("no financial_fact with id {fact_id}"),
            )
                .into_response();
        }
        crate::supersede::SupersedeOutcome::LockedPeriod { fact_id, period_id } => {
            return (
                StatusCode::CONFLICT,
                format!(
                    "fact {fact_id} sits in locked period {period_id}; \
                     unlock the period before superseding"
                ),
            )
                .into_response();
        }
    };

    if let Err(e) = tx.commit().await {
        return storage_err(e);
    }

    // Emit the audit-log event so a full rebuild from audit_log
    // reproduces the supersede. Replay path
    // (`replay_supersede_events_in_tx`) keys on the natural-key
    // triple — UUIDs aren't stable across rebuilds.
    let payload = serde_json::json!({
        "fact_id": fact_id,
        "kind": kind,
        "source_table": source_table,
        "source_id": source_id,
        "reason": body.reason,
        "superseded_by": body.superseded_by,
    });
    crate::events::emit_after_commit(
        &state.publisher,
        "ledger.fact.superseded",
        payload,
        boss_clock_client::now_from(&state.clock).await,
    )
    .await;

    Json(SupersedeResponse {
        fact_id,
        entries_dropped,
    })
    .into_response()
}

// --- cost-basis sum (read) ------------------------------------------------

/// Body for `POST /api/ledger/financial-facts/sum`. Read-only
/// aggregate: returns the summed `payload.total_cost_cents` over the
/// live `financial_facts` rows matching `(kind, source_table)` for any
/// of `source_ids`, plus how many matched. Superseded rows are excluded
/// (`supersede_reason IS NULL`) so the sum reflects the GL's effective
/// state, matching the rebuild projection's filter.
///
/// Why it exists: the dispatcher's `products.produce` `drain-actual-wip`
/// cost basis asks the ledger for the *real* WIP that a brew's
/// `inventory.parts.consume` legs capitalized (DR 1310, keyed
/// `source_table="inventory_consume"`, `source_id="{step_id}:{part_sku}"`).
/// Draining finished goods by that actual figure — instead of
/// re-deriving from a since-drifted `avg_cost` — is what keeps WIP from
/// trending negative. POST (not GET) only because the source-id list
/// rides in a body.
#[derive(Deserialize)]
pub(super) struct FactsSumBody {
    kind: String,
    source_table: String,
    #[serde(default)]
    source_ids: Vec<String>,
}

#[derive(Serialize)]
struct FactsSumResponse {
    total_cost_cents: i64,
    /// How many of the requested `source_ids` matched a live fact. Lets
    /// the caller distinguish "summed every leg" from a partial/empty
    /// match (a missing-fact signal) and fall back safely, without a
    /// second round-trip.
    matched: i64,
}

pub(super) async fn financial_facts_sum_handler(
    State(state): State<Arc<LedgerApiState>>,
    Json(body): Json<FactsSumBody>,
) -> Response {
    // Empty request → trivially zero; skip the round-trip.
    if body.source_ids.is_empty() {
        return Json(FactsSumResponse {
            total_cost_cents: 0,
            matched: 0,
        })
        .into_response();
    }
    let row: Result<(i64, i64), _> = sqlx::query_as(
        "SELECT \
            COALESCE(SUM((payload->>'total_cost_cents')::bigint), 0)::bigint, \
            COUNT(*)::bigint \
         FROM financial_facts \
         WHERE kind = $1 AND source_table = $2 \
           AND source_id = ANY($3) \
           AND supersede_reason IS NULL",
    )
    .bind(&body.kind)
    .bind(&body.source_table)
    .bind(&body.source_ids)
    .fetch_one(&state.pool)
    .await;
    match row {
        Ok((total_cost_cents, matched)) => Json(FactsSumResponse {
            total_cost_cents,
            matched,
        })
        .into_response(),
        Err(e) => storage_err(e),
    }
}
