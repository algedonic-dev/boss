//! Period lock + unlock operations. Locking freezes the interpretation
//! for a month: every entry in the period keeps its current rule version
//! and can't be modified (enforced by DB trigger + app check). A checksum
//! captures the state at lock time so an auditor can verify the period
//! hasn't drifted.

use chrono::NaiveDate;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::LedgerError;

#[derive(Debug, Serialize)]
pub struct Period {
    pub id: Uuid,
    pub kind: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub status: String,
    pub locked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub locked_by: Option<String>,
    pub locked_rule_version: Option<i32>,
    pub locked_checksum: Option<String>,
    pub entry_count: i64,
    pub total_debits: i64,
    pub total_credits: i64,
}

/// List all periods with per-period totals. Default order: newest first.
pub async fn list_periods(pool: &PgPool) -> Result<Vec<Period>, LedgerError> {
    let rows = sqlx::query(
        "SELECT p.id, p.kind, p.starts_on, p.ends_on, p.status, \
                p.locked_at, p.locked_by, rv.version AS locked_rule_version, p.locked_checksum, \
                COALESCE(entry_counts.n, 0) AS entry_count, \
                COALESCE(line_totals.debits, 0) AS total_debits, \
                COALESCE(line_totals.credits, 0) AS total_credits \
         FROM gl_periods p \
         LEFT JOIN gl_rule_versions rv ON rv.id = p.locked_rule_version_id \
         LEFT JOIN LATERAL ( \
            SELECT COUNT(*) AS n FROM gl_journal_entries e WHERE e.period_id = p.id \
         ) entry_counts ON true \
         LEFT JOIN LATERAL ( \
            SELECT COALESCE(SUM(l.debit_cents), 0)::bigint AS debits, \
                   COALESCE(SUM(l.credit_cents), 0)::bigint AS credits \
            FROM gl_journal_lines l \
            JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
            WHERE e.period_id = p.id \
         ) line_totals ON true \
         ORDER BY p.starts_on DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|r| Period {
            id: r.get("id"),
            kind: r.get("kind"),
            starts_on: r.get("starts_on"),
            ends_on: r.get("ends_on"),
            status: r.get("status"),
            locked_at: r.get("locked_at"),
            locked_by: r.get("locked_by"),
            locked_rule_version: r.get("locked_rule_version"),
            locked_checksum: r.get("locked_checksum"),
            entry_count: r.get("entry_count"),
            total_debits: r.get("total_debits"),
            total_credits: r.get("total_credits"),
        })
        .collect())
}

/// Lock a period: set status='locked', pin the active rule version, and
/// write a deterministic checksum over every entry + line in the period.
/// Returns the checksum. Idempotent-ish: re-locking an already-locked
/// period returns the existing checksum without rewriting.
pub async fn lock_period(
    pool: &PgPool,
    period_id: Uuid,
    locked_by: &str,
) -> Result<String, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let existing: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT status, locked_checksum FROM gl_periods WHERE id = $1")
            .bind(period_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let (status, existing_checksum) = match existing {
        Some(row) => row,
        None => {
            return Err(LedgerError::Storage(format!(
                "period {period_id} not found"
            )));
        }
    };
    if status == "locked"
        && let Some(cs) = existing_checksum
    {
        return Ok(cs);
    }

    let (active_version_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM gl_rule_versions WHERE is_active = true")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let checksum = compute_period_checksum(&mut tx, period_id).await?;

    sqlx::query(
        "UPDATE gl_periods SET \
            status = 'locked', \
            locked_at = NOW(), \
            locked_by = $2, \
            locked_rule_version_id = $3, \
            locked_checksum = $4 \
         WHERE id = $1",
    )
    .bind(period_id)
    .bind(locked_by)
    .bind(active_version_id)
    .bind(&checksum)
    .execute(&mut *tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(checksum)
}

/// Unlock a period: clear the lock fields, set status back to 'open'.
/// Operator-tier action; the HTTP layer is where auth gating happens.
pub async fn unlock_period(pool: &PgPool, period_id: Uuid) -> Result<(), LedgerError> {
    let result = sqlx::query(
        "UPDATE gl_periods SET \
            status = 'open', \
            locked_at = NULL, \
            locked_by = NULL, \
            locked_rule_version_id = NULL, \
            locked_checksum = NULL \
         WHERE id = $1",
    )
    .bind(period_id)
    .execute(pool)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(LedgerError::Storage(format!(
            "period {period_id} not found"
        )));
    }
    Ok(())
}

/// Deterministic hash over every entry + line in a period. Two periods with
/// byte-identical entries produce identical checksums; any divergence after
/// lock means tampering or a bug.
pub async fn compute_period_checksum(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    period_id: Uuid,
) -> Result<String, LedgerError> {
    let entries: Vec<(Uuid, NaiveDate, String)> = sqlx::query_as(
        "SELECT e.id, e.posted_on, \
                COALESCE( \
                    (SELECT string_agg( \
                        l.account_id::text || ',' || l.debit_cents::text || ',' \
                        || l.credit_cents::text || ',' || l.sort_order::text, \
                        ';' ORDER BY l.id) \
                     FROM gl_journal_lines l WHERE l.journal_entry_id = e.id), \
                    '' \
                ) AS line_payload \
         FROM gl_journal_entries e \
         WHERE e.period_id = $1 \
         ORDER BY e.id",
    )
    .bind(period_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut hasher = Sha256::new();
    for (id, posted_on, line_payload) in entries {
        hasher.update(id.as_bytes());
        hasher.update(posted_on.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(line_payload.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    Ok(format!("sha256:{:x}", digest))
}
