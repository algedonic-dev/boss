//! `POST /api/ledger/tax/batch` — the tenant declares its tax regime:
//! filing kinds and sales-tax rates (backlog 7f163e58; the chart
//! door's shape, 41af5195). Insert-if-absent by kind and by state,
//! Create on `tax-regime` (backlog 59deda40), one `ledger.tax_kind.declared` /
//! `ledger.sales_tax_rate.declared` staged on the outbox per INSERTED
//! row in the insert's own transaction, and an answer that names every
//! kept row and the declared fields it differs on. The work is in
//! `crate::tax_registry`; this is the door. The two GETs read the
//! registry back in the declaration's own shape.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{CurrentUser, Resource};

use super::*;
use crate::tax_registry::TaxSeed;

/// Create on `tax-regime` ([`super::authorize_declaration`], backlog
/// 59deda40) — platform-admin's alone in the core defaults, the role
/// `boss tenant publish` signs as.
pub(super) async fn declare_tax_batch(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(seed): Json<TaxSeed>,
) -> Response {
    let stamp = match super::authorize_declaration(&state, &user, Resource::tax_regime()).await {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };
    match crate::tax_registry::declare_tax(&state.pool, &seed, &stamp).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn list_tax_kinds(State(state): State<Arc<LedgerApiState>>) -> Response {
    match crate::tax_registry::list_tax_kinds(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => ledger_err(e),
    }
}

pub(super) async fn list_sales_tax_rates(State(state): State<Arc<LedgerApiState>>) -> Response {
    match crate::tax_registry::list_sales_tax_rates(&state.pool).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => ledger_err(e),
    }
}
