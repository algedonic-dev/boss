//! Append-only correction path for `financial_facts`.
//!
//! A bad row is retracted with a "supersede" marker rather than a
//! `DELETE` — deleting it would break the append-only invariant the
//! audit_log + financial_facts contract enforces, losing the bad-row
//! evidence and making the projection un-rederivable from history.
//! The supersede tombstone is the only way to retract a fact without
//! breaking provenance.
//!
//! The bad row stays in the table; `supersede_reason` records *why*
//! it was retired; the optional `superseded_by` documents a
//! corrected replacement fact. Rebuild + replay-check paths skip rows
//! where `supersede_reason IS NOT NULL`. The supersede itself emits a
//! `ledger.fact.superseded` audit_log event, so a full rebuild from
//! `audit_log` reproduces the live state.
//!
//! Convention recap (mirrors the schema/40-ledger.sql comment block):
//!   - `supersede_reason IS NULL` → live row
//!   - `supersede_reason IS NOT NULL` → retired row (skipped by
//!     rebuild / replay-check)
//!   - `superseded_by IS NOT NULL` → optional pointer to the
//!     corrected replacement fact

use serde::{Deserialize, Serialize};
use sqlx::Postgres;
use uuid::Uuid;

use crate::error::LedgerError;

/// Look-up key + correction metadata. We key supersedes on the
/// natural `(kind, source_table, source_id)` triple — not the
/// fact's UUID — because live writers assign random UUIDs while
/// rebuilds derive deterministic UUIDv5 ids; only the natural key
/// is stable across both paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupersedeRequest {
    pub kind: String,
    pub source_table: Option<String>,
    pub source_id: Option<String>,
    pub reason: String,
    /// Optional pointer to a corrected replacement fact. The
    /// replacement must be inserted separately via the originating
    /// domain's normal write path, with a *different* source_id so
    /// the unique index on `(kind, source_table, source_id)` keeps
    /// holding. We store its UUID here purely for documentation.
    #[serde(default)]
    pub superseded_by: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupersedeOutcome {
    /// The fact was active; it's now superseded and any
    /// dependent gl_journal_entries were dropped.
    Applied { fact_id: Uuid, entries_dropped: u64 },
    /// The fact was already superseded — caller should treat as
    /// a no-op (HTTP 409). We surface the existing reason so the
    /// caller can show it to the operator.
    AlreadySuperseded { fact_id: Uuid, reason: String },
    /// No fact exists for the natural key (HTTP 404).
    NotFound,
    /// The fact's period is locked. Mutating its totals would
    /// silently re-open a closed period; reject so the operator
    /// must explicitly unlock first (HTTP 409).
    LockedPeriod { fact_id: Uuid, period_id: Uuid },
}

/// Apply a supersede inside an existing transaction. Caller is
/// responsible for committing + emitting the
/// `ledger.fact.superseded` audit-log event.
pub async fn apply_supersede_in_tx(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    req: &SupersedeRequest,
) -> Result<SupersedeOutcome, LedgerError> {
    // 1. Resolve the natural key to a single fact row.
    let row: Option<(Uuid, chrono::NaiveDate, Option<String>)> = sqlx::query_as(
        "SELECT id, happened_on, supersede_reason \
         FROM financial_facts \
         WHERE kind = $1 \
           AND source_table IS NOT DISTINCT FROM $2 \
           AND source_id    IS NOT DISTINCT FROM $3 \
         LIMIT 1",
    )
    .bind(&req.kind)
    .bind(&req.source_table)
    .bind(&req.source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let Some((fact_id, happened_on, existing_reason)) = row else {
        return Ok(SupersedeOutcome::NotFound);
    };

    if let Some(reason) = existing_reason {
        return Ok(SupersedeOutcome::AlreadySuperseded { fact_id, reason });
    }

    // 2. Refuse to mutate a locked period. Locked-period totals
    //    are immutable by audit policy — reopening a period is a
    //    separate, audited action. The fact may not have a
    //    `gl_periods` row yet (events outside the open-period
    //    window stay un-bucketed); in that case we treat it as
    //    not-locked.
    let period_status: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM gl_periods \
         WHERE kind = 'month' \
           AND $1 BETWEEN starts_on AND ends_on \
         LIMIT 1",
    )
    .bind(happened_on)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    if let Some((period_id, status)) = period_status
        && status == "locked"
    {
        return Ok(SupersedeOutcome::LockedPeriod { fact_id, period_id });
    }

    // 3. Mark as superseded. supersede_reason being non-NULL is
    //    what the rebuild path uses to skip the row.
    sqlx::query(
        "UPDATE financial_facts \
         SET supersede_reason = $1, superseded_by = $2 \
         WHERE id = $3",
    )
    .bind(&req.reason)
    .bind(req.superseded_by)
    .bind(fact_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // 4. Drop the dependent journal-entry rows so the projection
    //    immediately reflects the retraction. The next rebuild
    //    won't recreate them (the fact-row filter excludes
    //    superseded rows). gl_journal_lines cascades via the FK
    //    on entry_id.
    let entries_dropped = sqlx::query("DELETE FROM gl_journal_entries WHERE fact_id = $1")
        .bind(fact_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?
        .rows_affected();

    Ok(SupersedeOutcome::Applied {
        fact_id,
        entries_dropped,
    })
}

/// Replay every `ledger.fact.superseded` event from `audit_log`
/// and re-apply it to `financial_facts`. Called by
/// `rebuild_facts_in_tx` after the projection pass so a clean
/// rebuild reproduces the live supersede state.
///
/// Re-applying is UPDATE-by-natural-key, so it's idempotent: if
/// a row was already marked superseded by a prior pass, the same
/// values get rewritten with no observable effect. Returns the
/// number of supersede events processed.
pub async fn replay_supersede_events_in_tx(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<u64, LedgerError> {
    let events: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT payload FROM audit_log \
         WHERE kind = 'ledger.fact.superseded' \
         ORDER BY timestamp, event_id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut applied: u64 = 0;
    for (payload,) in events {
        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let source_table = payload
            .get("source_table")
            .and_then(|v| v.as_str())
            .map(String::from);
        let source_id = payload
            .get("source_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let reason = payload.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let superseded_by = payload
            .get("superseded_by")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        if kind.is_empty() {
            continue;
        }

        sqlx::query(
            "UPDATE financial_facts \
             SET supersede_reason = $1, superseded_by = $2 \
             WHERE kind = $3 \
               AND source_table IS NOT DISTINCT FROM $4 \
               AND source_id    IS NOT DISTINCT FROM $5",
        )
        .bind(reason)
        .bind(superseded_by)
        .bind(kind)
        .bind(&source_table)
        .bind(&source_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

        applied += 1;
    }

    Ok(applied)
}
