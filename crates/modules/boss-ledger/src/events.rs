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

/// Namespace UUID for deterministic fact ids — UUIDv5 over the natural
/// key `(kind, source_table, source_id)`, the same identity the unique
/// index enforces.
pub const FACT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x71004230_67f0_5fac_70ad_d11a51141a6c);

/// The one fact-id derivation, shared by every live writer (via
/// `record_fact_in_tx` and the cross-crate insert paths in
/// boss-inventory / boss-commerce / boss-products) AND the audit-log
/// rebuild. A rebuilt fact therefore carries the SAME id as its live
/// twin: journal entries keyed `(fact_id, rule_version_id)` compare
/// across live and replay, and any reference to a fact id survives a
/// rebuild. The previous split — live writers minting random v4, the
/// rebuild deriving v5 over `(event_id, fact_kind)` — made the deep
/// replay-check's entry diff structurally unmatchable for live-written
/// facts.
pub fn deterministic_fact_id(kind: &str, source_table: &str, source_id: &str) -> Uuid {
    let mut input = Vec::with_capacity(kind.len() + source_table.len() + source_id.len() + 2);
    input.extend_from_slice(kind.as_bytes());
    input.push(0);
    input.extend_from_slice(source_table.as_bytes());
    input.push(0);
    input.extend_from_slice(source_id.as_bytes());
    Uuid::new_v5(&FACT_ID_NAMESPACE, &input)
}

/// Input shape for `record_fact_in_tx`. `source_table` / `source_id`
/// are `Option` so manual entries (which carry no source row) don't
/// have to thread sentinel strings. The fact id is NOT a caller input:
/// it is derived from the natural key inside `record_fact_in_tx`
/// (see [`deterministic_fact_id`]) so live and rebuilt ids can't drift.
pub struct FactWrite<'a> {
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
    let fact_id = match (params.source_table, params.source_id) {
        (Some(t), Some(s)) => deterministic_fact_id(params.kind, t, s),
        // A NULL-key fact has no natural identity to derive from (and
        // no idempotency — NULLs are distinct in the unique index), so
        // each insert mints a fresh random id.
        _ => Uuid::new_v4(),
    };
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (kind, source_table, source_id) DO NOTHING \
         RETURNING id",
    )
    .bind(fact_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_id_is_deterministic_over_the_natural_key() {
        let a = deterministic_fact_id("finance.tax.accrued", "tax_accruals", "excise-1");
        let b = deterministic_fact_id("finance.tax.accrued", "tax_accruals", "excise-1");
        assert_eq!(a, b);
        // This is the whole point: a live write and an audit-log replay
        // of the same natural key land on the same id, so entries keyed
        // (fact_id, rule_version_id) compare across live and rebuild.
    }

    #[test]
    fn fact_id_differs_when_any_key_component_differs() {
        let base = deterministic_fact_id("finance.tax.accrued", "tax_accruals", "excise-1");
        assert_ne!(
            base,
            deterministic_fact_id("finance.tax.remitted", "tax_accruals", "excise-1")
        );
        assert_ne!(
            base,
            deterministic_fact_id("finance.tax.accrued", "tax_filings", "excise-1")
        );
        assert_ne!(
            base,
            deterministic_fact_id("finance.tax.accrued", "tax_accruals", "excise-2")
        );
        // The NUL separator keeps concatenation unambiguous: ("a","bc")
        // must not collide with ("ab","c").
        assert_ne!(
            deterministic_fact_id("k", "a", "bc"),
            deterministic_fact_id("k", "ab", "c")
        );
    }
}
