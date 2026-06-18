//! Audit-log integrity scan.
//!
//! Layer 1 of the immutable-audit-log story (see
//! `docs/architecture-decisions.md` §Correctness protocol & the
//! audit log). The schema-level trigger
//! makes UPDATE / DELETE / TRUNCATE *fail*, but defending against an
//! operator who drops the trigger first requires a second-line check:
//! scan for evidence that rows went missing or were re-ordered after
//! the fact.
//!
//! Two signals:
//!
//! - **`id` gaps** — `audit_log.id` is a `BIGSERIAL`. Sequence values
//!   that never landed as a row (rolled-back transactions) also show
//!   up as gaps, so a gap is *suspicious* not *proof of tampering*.
//!   The integrity report surfaces them for human review.
//! - **`created_at` regressions** — `created_at` defaults to `NOW()`,
//!   which is monotonic non-decreasing in transaction-start order on
//!   a single primary. A row whose `created_at` is earlier than the
//!   prior row's `created_at` (by `id` order) is the strongest signal
//!   that someone rewrote history.
//!
//! The scan is a single window-function query so the row count of
//! the result is bounded by the number of anomalies, not the size of
//! `audit_log`.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// One missing-id gap. `prev_id + 1 ..= id - 1` are the absent ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdGap {
    pub prev_id: i64,
    pub id: i64,
}

impl IdGap {
    pub fn missing_count(&self) -> i64 {
        self.id - self.prev_id - 1
    }
}

/// `created_at` went backwards between two adjacent rows in `id` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedAtRegression {
    pub prev_id: i64,
    pub prev_created_at: DateTime<Utc>,
    pub id: i64,
    pub created_at: DateTime<Utc>,
}

/// One row whose stored `row_hash` does not match the hash recomputed
/// from its predecessor. The break can be the row itself (tampered
/// content) or its predecessor (deletion / insertion shifting the
/// chain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBreak {
    pub id: i64,
    pub stored_hash: Vec<u8>,
    pub computed_hash: Vec<u8>,
}

/// One audit_log payload that references a foreign id which has no
/// matching `*.created` event earlier in the log. The classic shape
/// is a `commerce.invoice.created` row whose `account_id` was never
/// emitted as `accounts.account.created` — the projection rebuilder
/// silently drops the dependency and the SPA's account-detail link
/// 404s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanglingForeignRef {
    /// The audit_log row that carries the dangling reference.
    pub id: i64,
    /// Event kind at `id` (e.g. `commerce.invoice.created`).
    pub kind: String,
    /// Foreign id that's missing a parent (e.g.
    /// `acc-bigseed-9999`).
    pub foreign_id: String,
    /// Which payload field the foreign id was read from
    /// (e.g. `account_id`).
    pub field: String,
    /// What event-kind the parent should have been
    /// (e.g. `accounts.account.created`).
    pub expected_parent_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub total_rows: i64,
    pub gaps: Vec<IdGap>,
    pub regressions: Vec<CreatedAtRegression>,
    pub chain_breaks: Vec<ChainBreak>,
    pub dangling_refs: Vec<DanglingForeignRef>,
}

impl IntegrityReport {
    /// True when no gaps, regressions, chain breaks, or dangling
    /// foreign refs were observed.
    pub fn is_clean(&self) -> bool {
        self.gaps.is_empty()
            && self.regressions.is_empty()
            && self.chain_breaks.is_empty()
            && self.dangling_refs.is_empty()
    }
}

type ScanRow = (i64, Option<i64>, DateTime<Utc>, Option<DateTime<Utc>>);

/// Scan `audit_log` once for id gaps and `created_at` regressions.
pub async fn check_audit_log_integrity(pool: &PgPool) -> Result<IntegrityReport, sqlx::Error> {
    let total_rows: (i64,) = sqlx::query_as("SELECT COUNT(*)::BIGINT FROM audit_log")
        .fetch_one(pool)
        .await?;

    let rows: Vec<ScanRow> = sqlx::query_as(
        "WITH scan AS (
             SELECT
                 id,
                 LAG(id)         OVER (ORDER BY id) AS prev_id,
                 created_at,
                 LAG(created_at) OVER (ORDER BY id) AS prev_created_at
             FROM audit_log
         )
         SELECT id, prev_id, created_at, prev_created_at
         FROM scan
         WHERE (prev_id IS NOT NULL AND id - prev_id > 1)
            OR (prev_created_at IS NOT NULL AND created_at < prev_created_at)
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut gaps = Vec::new();
    let mut regressions = Vec::new();
    for (id, prev_id, created_at, prev_created_at) in rows {
        if let Some(prev_id) = prev_id
            && id - prev_id > 1
        {
            gaps.push(IdGap { prev_id, id });
        }
        if let Some(prev_created_at) = prev_created_at
            && created_at < prev_created_at
        {
            regressions.push(CreatedAtRegression {
                prev_id: prev_id.unwrap_or(0),
                prev_created_at,
                id,
                created_at,
            });
        }
    }

    let chain_breaks = verify_chain(pool).await?;
    let dangling_refs = check_foreign_refs(pool).await?;

    Ok(IntegrityReport {
        total_rows: total_rows.0,
        gaps,
        regressions,
        chain_breaks,
        dangling_refs,
    })
}

/// Soft-FK rules. Each entry is "events of `child_kind` carry a
/// `field` whose value should also exist as the `id` field of an
/// earlier `parent_kind` event". The current set covers the
/// strongest invariants the brewery seed-pipeline has hit:
///
/// - `commerce.invoice.created.account_id` →
///   `accounts.account.created.id`
/// - `inventory.purchase_order.upserted.vendor_id` →
///   `inventory.vendor.created.id`
///
/// Add new rules here as services expose more cross-event ids; the
/// rule list is intentionally short — the bar for promotion is
/// "this ref has burned us at least once, or projection rebuild
/// would silently 404 if the parent went missing".
/// Soft-FK invariant rules (D6). A TOML-backed registry, not a closed
/// Rust const, so adding a new invariant is a row append in
/// `seeds/audit_invariant_rules.toml` + a restart, not a code edit +
/// rebuild. Per-tenant overrides via `BOSS_EVENTS_INVARIANT_RULES_TOML`.
/// Same data-as-data shape as D1/D2/D3/D4.
const AUDIT_INVARIANT_RULES_TOML: &str = include_str!("../seeds/audit_invariant_rules.toml");

#[derive(serde::Deserialize)]
struct InvariantRulesToml {
    rule: Vec<InvariantRule>,
}

#[derive(serde::Deserialize)]
struct InvariantRule {
    child_kind: String,
    field: String,
    parent_kind: String,
}

fn foreign_ref_rules() -> &'static [(String, String, String)] {
    static CACHE: std::sync::OnceLock<Vec<(String, String, String)>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let body = match std::env::var("BOSS_EVENTS_INVARIANT_RULES_TOML") {
            Ok(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "BOSS_EVENTS_INVARIANT_RULES_TOML unreadable; falling back to embedded defaults"
                );
                AUDIT_INVARIANT_RULES_TOML.to_string()
            }),
            Err(_) => AUDIT_INVARIANT_RULES_TOML.to_string(),
        };
        let parsed: InvariantRulesToml =
            toml::from_str(&body).expect("audit_invariant_rules.toml must parse");
        parsed
            .rule
            .into_iter()
            .map(|r| (r.child_kind, r.field, r.parent_kind))
            .collect()
    })
}

async fn check_foreign_refs(pool: &PgPool) -> Result<Vec<DanglingForeignRef>, sqlx::Error> {
    // The check is "parent exists anywhere in the log", not "parent
    // came earlier in id order". The projection rebuilders process
    // every `*.created` event before answering queries, so temporal
    // ordering between cross-event references doesn't matter for
    // the operational concern (does the projection resolve?).
    // Seed pipelines that run accounts-after-invoices (e.g. when
    // the brewery audit_log was generated before brewery-engine
    // emitted account events) still produce a clean rebuild as
    // long as the parent event lands eventually.
    let mut all = Vec::new();
    for (child_kind, field, parent_kind) in foreign_ref_rules() {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, payload ->> $1 AS foreign_id \
             FROM audit_log \
             WHERE kind = $2 \
               AND payload ->> $1 IS NOT NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM audit_log p \
                 WHERE p.kind = $3 \
                   AND p.payload ->> 'id' = audit_log.payload ->> $1 \
               ) \
             ORDER BY id",
        )
        .bind(field)
        .bind(child_kind)
        .bind(parent_kind)
        .fetch_all(pool)
        .await?;

        for (id, foreign_id) in rows {
            all.push(DanglingForeignRef {
                id,
                kind: child_kind.to_string(),
                foreign_id,
                field: field.to_string(),
                expected_parent_kind: parent_kind.to_string(),
            });
        }
    }
    Ok(all)
}

/// Walk the hash chain and return every row whose stored `row_hash`
/// does not match the hash recomputed from its predecessor. A clean
/// log returns an empty vec.
///
/// The recomputation runs in SQL (not Rust) so the canonical-byte
/// encoding stays in one place — the trigger and the verifier read
/// from the same `payload::text` representation, eliminating the
/// cross-language canonicalization bug.
async fn verify_chain(pool: &PgPool) -> Result<Vec<ChainBreak>, sqlx::Error> {
    // The verifier reproduces the trigger's canonical hash. Must
    // match `audit_log_compute_row_hash`'s encoding exactly — a
    // mismatch here surfaces every row as a "chain break" even
    // though the chain is fine, so any change to the trigger's
    // encoding has to land here too. The encoding uses
    // `convert_to(text, 'UTF8')`, not a `text::bytea` cast: the cast
    // fails on payloads carrying `\n` / unicode escapes.
    let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "WITH chain AS (
             SELECT
                 id,
                 row_hash AS stored,
                 digest(
                     LAG(row_hash, 1, decode(repeat('00', 32), 'hex'))
                         OVER (ORDER BY id)
                     ||
                     convert_to(
                         COALESCE(event_id::text, '') || '|' ||
                         COALESCE(timestamp::text, '') || '|' ||
                         COALESCE(source, '')          || '|' ||
                         COALESCE(kind, '')            || '|' ||
                         COALESCE(payload::text, ''),
                         'UTF8'
                     ),
                     'sha256'
                 ) AS computed
             FROM audit_log
         )
         SELECT id, stored, computed
         FROM chain
         WHERE stored IS DISTINCT FROM computed
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, stored, computed)| ChainBreak {
            id,
            stored_hash: stored,
            computed_hash: computed,
        })
        .collect())
}
