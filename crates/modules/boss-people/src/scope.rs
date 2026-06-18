//! Employee scope lookup — the shape downstream services need for
//! row-level policy decisions.
//!
//! The gateway calls this on login to populate the signed session
//! cookie with the caller's territory + reports + department; every
//! downstream handler then reads those values off the `x-boss-user`
//! header without doing a lookup per request.
//!
//! Shape intentionally mirrors the non-tier fields of `boss_policy::User`
//! so the gateway's header JSON is a field-for-field copy plus the
//! session-derived `access_tier`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_policy_client::CurrentUser;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Clone)]
pub struct ScopeState {
    pub pool: Arc<PgPool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeScope {
    pub id: String,
    pub role: String,
    pub department: Option<String>,
    /// Accounts the employee is accountable for — union of
    /// territory-rep assignment and account-team membership.
    /// Used for Territory-scoped policy rules.
    pub territory_account_ids: Vec<String>,
    /// Employees who report to this one directly (not transitively).
    /// Used for Team-scoped policy rules.
    pub direct_report_ids: Vec<String>,
}

pub fn scope_router(pool: PgPool) -> Router {
    let state = ScopeState {
        pool: Arc::new(pool),
    };
    Router::new()
        .route("/api/people/{id}/scope", get(get_scope))
        .route(
            "/api/people/by-email/{email}/bootstrap",
            get(bootstrap_by_email),
        )
        .with_state(state)
}

async fn get_scope(
    State(state): State<ScopeState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    // Enumeration guard: leaks org structure (role/department/
    // territory/direct-reports). Allow operator-tier gateway
    // callers, admin-ish roles, or the caller looking up their
    // own scope.
    let tier_ok = matches!(user.access_tier, boss_policy::AccessTier::Operator);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    let self_lookup = user.id == id;
    if !(tier_ok || role_ok || self_lookup) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier, admin-ish role, or self-lookup required",
        )
            .into_response();
    }
    // Confirm the employee exists; a 404 is more useful to the gateway
    // than an empty scope that would silently grant nothing.
    let (role, department) = match sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT role, department FROM employees WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(state.pool.as_ref())
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("no employee {id}")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Territory = accounts where this employee is the territory rep
    // OR sits on the account team. DISTINCT handles the employee who
    // covers both relationships on the same account.
    let territory: Result<Vec<(String,)>, _> = sqlx::query_as(
        "SELECT id FROM accounts WHERE territory_rep_id = $1
         UNION
         SELECT account_id FROM account_team_members WHERE employee_id = $1",
    )
    .bind(&id)
    .fetch_all(state.pool.as_ref())
    .await;
    let territory_account_ids: Vec<String> = match territory {
        Ok(rows) => rows.into_iter().map(|(p,)| p).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Direct reports — one level of hierarchy, not transitive.
    let reports: Result<Vec<(String,)>, _> =
        sqlx::query_as("SELECT id FROM employees WHERE manager_id = $1")
            .bind(&id)
            .fetch_all(state.pool.as_ref())
            .await;
    let direct_report_ids: Vec<String> = match reports {
        Ok(rows) => rows.into_iter().map(|(r,)| r).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    Json(EmployeeScope {
        id,
        role,
        department,
        territory_account_ids,
        direct_report_ids,
    })
    .into_response()
}

/// Resolve an email claim (from whatever auth provider mints the
/// session) to the full `EmployeeScope` the gateway needs to mint a
/// session cookie. Same operator-tier / admin-ish guard as `/scope`
/// because exposing the email→employee index would leak roster
/// membership.
async fn bootstrap_by_email(
    State(state): State<ScopeState>,
    CurrentUser(user): CurrentUser,
    Path(email): Path<String>,
) -> Response {
    let tier_ok = matches!(user.access_tier, boss_policy::AccessTier::Operator);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    if !(tier_ok || role_ok) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or admin-ish role required",
        )
            .into_response();
    }

    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT id, role, department FROM employees WHERE lower(email) = lower($1)",
    )
    .bind(&email)
    .fetch_optional(state.pool.as_ref())
    .await;
    let (id, role, department) = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                format!("no employee with email {email}"),
            )
                .into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let territory: Result<Vec<(String,)>, _> = sqlx::query_as(
        "SELECT id FROM accounts WHERE territory_rep_id = $1
         UNION
         SELECT account_id FROM account_team_members WHERE employee_id = $1",
    )
    .bind(&id)
    .fetch_all(state.pool.as_ref())
    .await;
    let territory_account_ids: Vec<String> = match territory {
        Ok(rows) => rows.into_iter().map(|(p,)| p).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let reports: Result<Vec<(String,)>, _> =
        sqlx::query_as("SELECT id FROM employees WHERE manager_id = $1")
            .bind(&id)
            .fetch_all(state.pool.as_ref())
            .await;
    let direct_report_ids: Vec<String> = match reports {
        Ok(rows) => rows.into_iter().map(|(r,)| r).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    Json(EmployeeScope {
        id,
        role,
        department,
        territory_account_ids,
        direct_report_ids,
    })
    .into_response()
}
