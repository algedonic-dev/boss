//! Idempotent fact-write helper for `financial_facts`.
//!
//! Architecture: `financial_facts` is a 1:1 projection of real-world
//! events recorded in `audit_log`. CPAs get `financial_facts` as a
//! clean independent log; auditors who need provenance can rebuild
//! it from `audit_log` via the deterministic projection (next step).
//! The fact log is not itself in `audit_log` — it is *derived from*
//! `audit_log`, so writing audit_log entries on every fact write
//! would create a cycle.
//!
//! Live writers (the HTTP handlers in this crate, plus the
//! cross-crate fact-write paths in boss-inventory and boss-commerce)
//! call this helper inline, in the same transaction as the
//! real-world event that triggered it. ACID with the domain write.
//! The rebuilder will run the same projection over `audit_log` as a
//! batch and produce a byte-identical `financial_facts` table.
//!
//! Idempotency: the helper is keyed on the existing
//! `(kind, source_table, source_id)` unique index. Re-running the
//! same call is a no-op. Manual entries carry NULL source columns;
//! Postgres treats NULLs as distinct in the unique index, so each
//! manual-entry call inserts a fresh row.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::Postgres;
use uuid::Uuid;

use boss_core::publisher::DomainPublisher;

use crate::error::LedgerError;

/// Fire the upstream `ledger.*` event for a fact-write site. Wraps
/// the fire-and-forget pattern: emit when a publisher is configured,
/// no-op otherwise. The `recorded_at` timestamp is stamped onto the
/// audit_log row so a rebuild from audit_log produces identical
/// `created_at` ordering to the live system.
///
/// Called *after* the caller's transaction commits — same fire-and-
/// forget contract every other service uses. A delayed/failed emit
/// is recoverable: the natural-key idempotency on `(kind,
/// source_table, source_id)` means a follow-up emit that lands won't
/// double-write the fact.
pub async fn emit_after_commit(
    publisher: &Option<Arc<DomainPublisher>>,
    kind: &str,
    payload: Value,
    recorded_at: DateTime<Utc>,
) {
    if let Some(p) = publisher {
        p.emit_at(kind, payload, recorded_at).await;
    }
}

/// Input shape for `record_fact_in_tx`. `source_table` / `source_id`
/// are `Option` so manual entries (which carry no source row) don't
/// have to thread sentinel strings.
pub struct FactWrite<'a> {
    /// UUID hint. Used if we insert; ignored if a row already exists
    /// for `(kind, source_table, source_id)`.
    pub fact_id: Uuid,
    pub kind: &'a str,
    pub happened_on: NaiveDate,
    pub payload: &'a Value,
    pub source_table: Option<&'a str>,
    pub source_id: Option<&'a str>,
    pub created_by: &'a str,
}

/// Write a financial fact in the caller's transaction. Returns the
/// canonical `fact_id` (either the just-inserted UUID or the
/// pre-existing row's UUID — callers thread this into `FactRef` for
/// `post_fact_in_tx`).
pub async fn record_fact_in_tx(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    params: FactWrite<'_>,
) -> Result<Uuid, LedgerError> {
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING \
         RETURNING id",
    )
    .bind(params.fact_id)
    .bind(params.kind)
    .bind(params.happened_on)
    .bind(params.payload)
    .bind(params.source_table)
    .bind(params.source_id)
    .bind(params.created_by)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    match inserted {
        Some((id,)) => Ok(id),
        None => {
            // Conflict triggered → resolve the canonical id of the
            // pre-existing row. The conflict only fires when
            // (source_table, source_id) are both NOT NULL (NULLs
            // are distinct in the unique index), so the equality
            // lookup is safe.
            let (id,): (Uuid,) = sqlx::query_as(
                "SELECT id FROM financial_facts \
                 WHERE kind = $1 AND source_table = $2 AND source_id = $3",
            )
            .bind(params.kind)
            .bind(params.source_table)
            .bind(params.source_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
            Ok(id)
        }
    }
}
