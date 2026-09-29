//! `POST /api/ledger/accounts/batch` — the tenant declares its chart of
//! accounts (backlog 41af5195; design 18cf4272). The batch-door shape
//! the classes / locations / agents doors landed on (#418): a JSON
//! array of rows, insert-if-absent by code, Create on `ledger-account`
//! (backlog 59deda40), one
//! `ledger.account.declared` staged on the outbox per INSERTED row in
//! the insert's own transaction, and an answer that names every kept
//! row and how the declaration differs from it. The work is in
//! `crate::chart`; this is the door.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{CurrentUser, Resource};

use super::*;
use crate::chart::AccountInput;

/// Create on `ledger-account` ([`super::authorize_declaration`],
/// backlog 59deda40) — platform-admin's alone in the core defaults, the
/// role `boss tenant publish` signs as (`automation:tenant-seed`). The
/// read layer below already holds the `ledger` read grant; this is the
/// write gate, and the external auditor (`audit-readonly`), which holds
/// no write, is refused by it like every other reader.
pub(super) async fn declare_accounts_batch(
    State(state): State<Arc<LedgerApiState>>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<AccountInput>>,
) -> Response {
    let stamp = match super::authorize_declaration(&state, &user, Resource::ledger_account()).await
    {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };
    match crate::chart::declare_accounts(&state.pool, &rows, &stamp).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => ledger_err(e),
    }
}
