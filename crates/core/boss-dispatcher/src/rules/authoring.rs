//! Authoring writes for the `dispatcher_rules` registry — the control-plane
//! behind the rule-authoring UX.
//!
//! Mirrors the step_plugins versioned-registry semantics: append-only
//! `(name, version)`, exactly one active per name (the partial unique
//! index), draft → active via [`publish`] which retires the prior active in
//! the same transaction. The read/runtime path is `load_active_rules`
//! (registry.rs); these are the writes.
//!
//! The running dispatcher loads its registry at startup, so a published
//! change takes effect on the next dispatcher restart (live hot-reload is a
//! planned follow-up). [`validate`] reuses the runtime `Rule::from_raw`, so
//! a draft that validates here loads cleanly there.

use serde::Serialize;
use sqlx::{PgPool, Row};

use super::registry::{RawDoStep, RawRule, RegistryError, Rule};

/// One stored `dispatcher_rules` row: the rule content + its lifecycle.
#[derive(Debug, Clone, Serialize)]
pub struct RuleVersion {
    pub name: String,
    pub version: i32,
    pub status: String,
    pub on_event: String,
    pub when: Option<String>,
    // Serialize as `do` to match the cascade-viz feed (RawRule) + the SPA's
    // DispatcherRuleDo type — one rule-content shape across the API.
    #[serde(rename = "do")]
    pub do_steps: Vec<RawDoStep>,
    pub delay: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthoringError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid rule: {0}")]
    Invalid(String),
    #[error("storage: {0}")]
    Storage(String),
}

fn store<E: std::fmt::Display>(e: E) -> AuthoringError {
    AuthoringError::Storage(e.to_string())
}

const SELECT_COLS: &str = "name, version, status, on_event, when_expr, do_steps, delay, created_at";

/// Parse a draft through the SAME `Rule::from_raw` the runtime uses, so an
/// authoring error (bad topic / `when` / arg expr) surfaces before persist.
/// Pure — no I/O.
pub fn validate(raw: &RawRule) -> Result<(), RegistryError> {
    Rule::from_raw(raw.clone()).map(|_| ())
}

fn row_to_version(row: &sqlx::postgres::PgRow) -> Result<RuleVersion, AuthoringError> {
    let do_json: serde_json::Value = row.try_get("do_steps").map_err(store)?;
    let do_steps: Vec<RawDoStep> = serde_json::from_value(do_json)
        .map_err(|e| AuthoringError::Storage(format!("do_steps: {e}")))?;
    Ok(RuleVersion {
        name: row.try_get("name").map_err(store)?,
        version: row.try_get("version").map_err(store)?,
        status: row.try_get("status").map_err(store)?,
        on_event: row.try_get("on_event").map_err(store)?,
        when: row.try_get("when_expr").map_err(store)?,
        do_steps,
        delay: row.try_get("delay").map_err(store)?,
        created_at: row.try_get("created_at").map_err(store)?,
    })
}

/// All versions of a rule name, oldest first (draft + active + retired).
pub async fn list_versions(pool: &PgPool, name: &str) -> Result<Vec<RuleVersion>, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 ORDER BY version");
    let rows = sqlx::query(&sql)
        .bind(name)
        .fetch_all(pool)
        .await
        .map_err(store)?;
    rows.iter().map(row_to_version).collect()
}

/// A specific version.
pub async fn get_version(
    pool: &PgPool,
    name: &str,
    version: i32,
) -> Result<RuleVersion, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 AND version = $2");
    let row = sqlx::query(&sql)
        .bind(name)
        .bind(version)
        .fetch_optional(pool)
        .await
        .map_err(store)?
        .ok_or_else(|| AuthoringError::NotFound(format!("{name} v{version}")))?;
    row_to_version(&row)
}

/// The active version of a rule, or `NotFound` if none is active.
pub async fn get_active(pool: &PgPool, name: &str) -> Result<RuleVersion, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 AND status = 'active'");
    let row = sqlx::query(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(store)?
        .ok_or_else(|| AuthoringError::NotFound(format!("no active {name}")))?;
    row_to_version(&row)
}

/// Append a new draft version of `raw.name`. Validates first (a draft that
/// can't load is rejected with `Invalid`, no row written), assigns
/// `max(version) + 1`, status = `draft`.
pub async fn create_draft(pool: &PgPool, raw: &RawRule) -> Result<RuleVersion, AuthoringError> {
    validate(raw).map_err(|e| AuthoringError::Invalid(e.to_string()))?;
    let do_json = serde_json::to_value(&raw.do_steps).map_err(store)?;
    let mut tx = pool.begin().await.map_err(store)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM dispatcher_rules WHERE name = $1",
    )
    .bind(&raw.name)
    .fetch_one(&mut *tx)
    .await
    .map_err(store)?;
    sqlx::query(
        "INSERT INTO dispatcher_rules \
            (name, version, status, on_event, when_expr, do_steps, delay) \
         VALUES ($1, $2, 'draft', $3, $4, $5, $6)",
    )
    .bind(&raw.name)
    .bind(next)
    .bind(&raw.on_event)
    .bind(&raw.when)
    .bind(&do_json)
    .bind(&raw.delay)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    tx.commit().await.map_err(store)?;
    get_version(pool, &raw.name, next).await
}

/// Activate the latest draft of `name`, retiring the prior active in the
/// same tx (so the one-active-per-name index never trips mid-flight).
pub async fn publish(pool: &PgPool, name: &str) -> Result<RuleVersion, AuthoringError> {
    let mut tx = pool.begin().await.map_err(store)?;
    let draft: Option<i32> = sqlx::query_scalar(
        "SELECT version FROM dispatcher_rules \
         WHERE name = $1 AND status = 'draft' ORDER BY version DESC LIMIT 1",
    )
    .bind(name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(store)?;
    let Some(v) = draft else {
        return Err(AuthoringError::NotFound(format!(
            "no draft to publish for {name}"
        )));
    };
    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    sqlx::query("UPDATE dispatcher_rules SET status = 'active' WHERE name = $1 AND version = $2")
        .bind(name)
        .bind(v)
        .execute(&mut *tx)
        .await
        .map_err(store)?;
    tx.commit().await.map_err(store)?;
    get_version(pool, name, v).await
}

/// Retire the active version of `name` (idempotent — no-op if none active).
pub async fn retire(pool: &PgPool, name: &str) -> Result<(), AuthoringError> {
    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .execute(pool)
    .await
    .map_err(store)?;
    Ok(())
}
