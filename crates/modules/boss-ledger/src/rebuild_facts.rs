//! Rebuild `financial_facts` from `audit_log` via the
//! `gl_fact_projection_rules` registry.
//!
//! Each rule maps one real-world `audit_log.kind` 1:1 to one
//! `financial_facts.kind`. The projection extracts `source_id`,
//! `happened_on`, and `created_by` from `event.payload` via JSON
//! pointers (RFC 6901), passes the payload through verbatim, and
//! upserts via `record_fact_in_tx` — idempotent on the natural key
//! `(kind, source_table, source_id)`.
//!
//! Determinism: `fact_id` is UUIDv5 over `(event_id, fact_kind)` so
//! the same audit_log event always produces the same fact UUID.
//! UUIDs aren't load-bearing for equality (the natural key is) but
//! determinism makes diffs cheaper for the replay-check.
//!
//! v1 ships UPSERT-only semantics. Operators who want a clean
//! rebuild truncate `financial_facts` (cascading through
//! `gl_journal_entries`) externally first. The replay-check
//! (separate function) is the read-only verifier.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::LedgerError;
use crate::events::{FactWrite, record_fact_in_tx};
use crate::supersede::replay_supersede_events_in_tx;

/// Advisory-lock key for the fact-rebuild, derived from the projection
/// name. Serializes concurrent fact-rebuilds the same way `rebuild`
/// serializes concurrent ledger-rebuilds.
const REBUILD_FACTS_LOCK_KEY: i64 = boss_core::rebuild::lock_key("ledger-facts");

/// Namespace UUID for deterministic `fact_id` derivation. UUIDv5 over
/// `(event_id, fact_kind)` under this namespace gives a stable,
/// repeatable id per (audit_log row, projected fact kind).
const FACT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x71004230_67f0_5fac_70ad_d11a51141a6c);

#[derive(Debug, Clone)]
pub struct ProjectionRule {
    pub event_kind: String,
    pub fact_kind: String,
    pub source_table: String,
    pub source_id_path: String,
    pub happened_on_path: Option<String>,
    pub created_by_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RebuildFactsReport {
    pub rules_loaded: u64,
    pub events_scanned: u64,
    pub facts_written: u64,
    pub events_skipped_missing_field: u64,
    /// `ledger.fact.superseded` events applied after the projection
    /// pass. Non-zero values mean the operator retired one or more
    /// historically-bad facts via the supersede endpoint; the
    /// rebuild reproduces those retractions so the live state stays
    /// stable across rebuilds.
    pub supersedes_applied: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error(
        "event payload missing required field at {path} (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    MissingField {
        path: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
    #[error(
        "event payload field at {path} is not a string (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    WrongType {
        path: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
    #[error(
        "happened_on path {path} resolved to {value} which is not a valid YYYY-MM-DD date (rule {rule_event_kind} → {rule_fact_kind})"
    )]
    InvalidDate {
        path: String,
        value: String,
        rule_event_kind: String,
        rule_fact_kind: String,
    },
}

/// Project one audit_log event into a `FactWrite` per the rule.
/// Returns `None` if the rule doesn't apply (kind mismatch — this is
/// caller-side filtered already, but the check is cheap).
pub fn project_event(
    rule: &ProjectionRule,
    event_id: Uuid,
    event_timestamp: DateTime<Utc>,
    event_source: &str,
    event_payload: &Value,
) -> Result<ProjectedFact, ProjectionError> {
    let source_id = pointer_string(event_payload, &rule.source_id_path).ok_or_else(|| {
        ProjectionError::MissingField {
            path: rule.source_id_path.clone(),
            rule_event_kind: rule.event_kind.clone(),
            rule_fact_kind: rule.fact_kind.clone(),
        }
    })?;

    let happened_on = match &rule.happened_on_path {
        Some(path) => {
            let raw = pointer_string(event_payload, path).ok_or_else(|| {
                ProjectionError::MissingField {
                    path: path.clone(),
                    rule_event_kind: rule.event_kind.clone(),
                    rule_fact_kind: rule.fact_kind.clone(),
                }
            })?;
            NaiveDate::parse_from_str(&raw, "%Y-%m-%d").map_err(|_| {
                ProjectionError::InvalidDate {
                    path: path.clone(),
                    value: raw,
                    rule_event_kind: rule.event_kind.clone(),
                    rule_fact_kind: rule.fact_kind.clone(),
                }
            })?
        }
        None => event_timestamp.date_naive(),
    };

    let created_by = match &rule.created_by_path {
        Some(path) => {
            pointer_string(event_payload, path).unwrap_or_else(|| event_source.to_string())
        }
        None => event_source.to_string(),
    };

    let fact_id = derive_fact_id(event_id, &rule.fact_kind);

    Ok(ProjectedFact {
        fact_id,
        fact_kind: rule.fact_kind.clone(),
        happened_on,
        payload: event_payload.clone(),
        source_table: rule.source_table.clone(),
        source_id,
        created_by,
    })
}

/// Owned form of a projection result. The lifetime-bearing
/// `FactWrite` shape is too painful to thread out of an inner loop;
/// callers convert this to a `FactWrite` at the insertion site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedFact {
    pub fact_id: Uuid,
    pub fact_kind: String,
    pub happened_on: NaiveDate,
    pub payload: Value,
    pub source_table: String,
    pub source_id: String,
    pub created_by: String,
}

impl ProjectedFact {
    pub fn as_write(&self) -> FactWrite<'_> {
        FactWrite {
            fact_id: self.fact_id,
            kind: &self.fact_kind,
            happened_on: self.happened_on,
            payload: &self.payload,
            source_table: Some(&self.source_table),
            source_id: Some(&self.source_id),
            created_by: &self.created_by,
        }
    }
}

/// Walk `audit_log` and project every event matching a registered
/// rule into `financial_facts`. UPSERT semantics — idempotent against
/// re-runs.
pub async fn rebuild_facts(pool: &PgPool) -> Result<RebuildFactsReport, LedgerError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_FACTS_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let report = rebuild_facts_in_tx(&mut tx).await?;

    tx.commit()
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    Ok(report)
}

/// Caller-controlled-transaction variant. Useful when composing with
/// `rebuild` for an audit-log-rooted replay-check that wants both
/// stages inside one rollback boundary.
pub async fn rebuild_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<RebuildFactsReport, LedgerError> {
    let rules = load_rules_in_tx(tx).await?;
    let event_kinds: Vec<String> = rules.keys().cloned().collect();

    // TRUNCATE-then-replay model. financial_facts is a pure
    // projection of audit_log; no row may live here that doesn't
    // trace back to an event. The gl_journal_entries FK references
    // financial_facts.id so the CASCADE drops the JE rows in the
    // same step (the ledger-journal rebuild step in rebuild-all
    // re-projects them right after). Opening balances are emitted
    // as `seed.opening_balance.recorded` audit events at seed time
    // so they survive the truncate via re-projection, NOT via
    // source_table-scoped exclusion.
    sqlx::query("TRUNCATE financial_facts CASCADE")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // The gl_account_daily rollup is NOT reachable by the CASCADE above
    // (it FKs gl_accounts, not the journal), but the journal it summarizes
    // was just wiped. Clear it too so the full journal re-post below
    // (post_fact_in_tx increments it per entry) rebuilds it from a clean
    // slate — keeping rebuild_facts self-consistent even when run without
    // a following `rebuild()` (which would also re-aggregate it).
    sqlx::query("TRUNCATE gl_account_daily")
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let event_rows = sqlx::query(
        "SELECT event_id, timestamp, source, kind, payload \
         FROM audit_log \
         WHERE kind = ANY($1) \
         ORDER BY timestamp, event_id",
    )
    .bind(&event_kinds)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut events_scanned: u64 = 0;
    let mut facts_written: u64 = 0;
    let mut events_skipped_missing_field: u64 = 0;

    for row in &event_rows {
        events_scanned += 1;
        let event_id: Uuid = row.get("event_id");
        let timestamp: DateTime<Utc> = row.get("timestamp");
        let source: String = row.get("source");
        let kind: String = row.get("kind");
        let payload: Value = row.get("payload");

        let Some(rule) = rules.get(&kind) else {
            continue;
        };

        let projected = match project_event(rule, event_id, timestamp, &source, &payload) {
            Ok(p) => p,
            Err(ProjectionError::MissingField { .. }) => {
                events_skipped_missing_field += 1;
                continue;
            }
            Err(e) => return Err(LedgerError::Storage(e.to_string())),
        };

        record_fact_in_tx(tx, projected.as_write()).await?;
        facts_written += 1;
    }

    // GL-inert reprojection pass — kept OFF the `gl_fact_projection_rules`
    // registry on purpose. The registry exists to drive the GL: every
    // fact kind it reconstructs is also posted to a journal by the active
    // RuleSet. `finance.inventory.received` is the lone fact that must be
    // reconstructable from the log AND post nothing — the goods-receipt's
    // DR-1300 rides the idempotent bill-approval path, so a GL entry here
    // would double-post it. Registering it would force exactly that. So
    // its reprojection lives here, hardcoded, and the journal-posting
    // stage (`post_fact_in_tx`) skips the inert kind. The result is
    // symmetric with consume's INVENTORY_TRANSFERRED reconstruction
    // (emit event → rebuild the fact from it), minus the GL leg.
    facts_written += rebuild_inert_received_facts_in_tx(tx).await?;

    // After projection, re-apply every recorded supersede so the
    // rebuilt set matches the live state. Without this pass, a
    // rebuild from audit_log would resurrect rows that the operator
    // had explicitly retired via the supersede endpoint, and the
    // replay-check would flag false-positive divergences.
    let supersedes_applied = replay_supersede_events_in_tx(tx).await?;

    Ok(RebuildFactsReport {
        rules_loaded: rules.len() as u64,
        events_scanned,
        facts_written,
        events_skipped_missing_field,
        supersedes_applied,
    })
}

/// Audit-log kind the inert receive reprojection consumes.
const RECEIVE_EVENT_KIND: &str = "inventory.item.received";
/// The GL-inert dedup-fact kind it reconstructs. Deliberately absent
/// from `gl_fact_projection_rules` AND from the RuleSet match in
/// `rules.rs`, so the journal-posting path (`post_fact_in_tx`) skips it
/// via `crate::rules::is_gl_inert`.
const RECEIVE_FACT_KIND: &str = "finance.inventory.received";
/// `source_table` written verbatim — matches the in-tx
/// `insert_dedup_fact` call in boss-inventory so the live fact and the
/// rebuilt fact share a natural key and the replay-check diff is clean.
const RECEIVE_SOURCE_TABLE: &str = "inventory_receipt";

/// Reproject `inventory.item.received` audit events into the GL-inert
/// `finance.inventory.received` dedup-fact. Mirrors the registry pass
/// (`record_fact_in_tx`, idempotent on the natural key, deterministic
/// `fact_id`) but stays hardcoded here precisely BECAUSE it must not be
/// in `gl_fact_projection_rules` — see the call site for why. The fact's
/// `(source_table, source_id, happened_on, payload, created_by)` are
/// reproduced byte-for-byte from what boss-inventory wrote in-tx
/// (`inventory_receipt` / `/source_id` / `/received_on` / payload
/// verbatim / `inventory`), so a live receive and a rebuilt receive land
/// the identical row. Returns the number of facts written.
async fn rebuild_inert_received_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<u64, LedgerError> {
    let rows = sqlx::query(
        "SELECT event_id, timestamp, payload \
         FROM audit_log \
         WHERE kind = $1 \
         ORDER BY timestamp, event_id",
    )
    .bind(RECEIVE_EVENT_KIND)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut written: u64 = 0;
    for row in &rows {
        let event_id: Uuid = row.get("event_id");
        let timestamp: DateTime<Utc> = row.get("timestamp");
        let payload: Value = row.get("payload");

        // `source_id` keys the dedup-fact; an event without it is malformed
        // — skip rather than fail the whole rebuild (same leniency the
        // registry pass gives a missing source_id field).
        let Some(source_id) = pointer_string(&payload, "/source_id") else {
            continue;
        };
        // `received_on` carries the dedup-fact's happened_on. Fall back to
        // the event date if (impossibly) absent, mirroring the registry's
        // NULL-happened_on_path behavior.
        let happened_on = pointer_string(&payload, "/received_on")
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .unwrap_or_else(|| timestamp.date_naive());

        let fact_id = derive_fact_id(event_id, RECEIVE_FACT_KIND);
        record_fact_in_tx(
            tx,
            FactWrite {
                fact_id,
                kind: RECEIVE_FACT_KIND,
                happened_on,
                payload: &payload,
                source_table: Some(RECEIVE_SOURCE_TABLE),
                source_id: Some(&source_id),
                // Matches the `created_by` the in-tx `insert_dedup_fact`
                // stamps ('inventory'); keeps the replay-check diff clean.
                created_by: "inventory",
            },
        )
        .await?;
        written += 1;
    }
    Ok(written)
}

async fn load_rules_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<HashMap<String, ProjectionRule>, LedgerError> {
    let rows = sqlx::query(
        "SELECT event_kind, fact_kind, source_table, source_id_path, \
                happened_on_path, created_by_path \
         FROM gl_fact_projection_rules",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    let mut out = HashMap::with_capacity(rows.len());
    for row in &rows {
        let rule = ProjectionRule {
            event_kind: row.get("event_kind"),
            fact_kind: row.get("fact_kind"),
            source_table: row.get("source_table"),
            source_id_path: row.get("source_id_path"),
            happened_on_path: row.get("happened_on_path"),
            created_by_path: row.get("created_by_path"),
        };
        out.insert(rule.event_kind.clone(), rule);
    }
    Ok(out)
}

fn pointer_string(value: &Value, path: &str) -> Option<String> {
    let v = value.pointer(path)?;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn derive_fact_id(event_id: Uuid, fact_kind: &str) -> Uuid {
    let mut input = Vec::with_capacity(16 + fact_kind.len() + 1);
    input.extend_from_slice(event_id.as_bytes());
    input.push(0);
    input.extend_from_slice(fact_kind.as_bytes());
    Uuid::new_v5(&FACT_ID_NAMESPACE, &input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(event_kind: &str, fact_kind: &str) -> ProjectionRule {
        ProjectionRule {
            event_kind: event_kind.into(),
            fact_kind: fact_kind.into(),
            source_table: "invoices".into(),
            source_id_path: "/id".into(),
            happened_on_path: Some("/issued_on".into()),
            created_by_path: None,
        }
    }

    #[test]
    fn projects_invoice_created_to_invoice_issued() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let event_id = Uuid::new_v4();
        let ts: DateTime<Utc> = "2026-04-01T12:00:00Z".parse().unwrap();
        let payload = serde_json::json!({
            "id": "inv-123",
            "issued_on": "2026-04-01",
            "amount_cents": 50000,
        });
        let projected = project_event(&r, event_id, ts, "commerce", &payload).unwrap();
        assert_eq!(projected.fact_kind, "finance.invoice.issued");
        assert_eq!(projected.source_id, "inv-123");
        assert_eq!(projected.happened_on.to_string(), "2026-04-01");
        assert_eq!(projected.created_by, "commerce");
        assert_eq!(projected.payload, payload);
    }

    #[test]
    fn missing_source_id_field_is_an_error() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"issued_on": "2026-04-01"});
        let result = project_event(&r, Uuid::new_v4(), Utc::now(), "commerce", &payload);
        assert!(matches!(result, Err(ProjectionError::MissingField { .. })));
    }

    #[test]
    fn missing_happened_on_field_is_an_error() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"id": "inv-123"});
        let result = project_event(&r, Uuid::new_v4(), Utc::now(), "commerce", &payload);
        assert!(matches!(result, Err(ProjectionError::MissingField { .. })));
    }

    #[test]
    fn null_happened_on_path_falls_back_to_event_timestamp() {
        let r = ProjectionRule {
            happened_on_path: None,
            ..rule("ledger.manual_entry.submitted", "finance.manual.entry")
        };
        let ts: DateTime<Utc> = "2026-04-01T15:30:00Z".parse().unwrap();
        let payload = serde_json::json!({"id": "ent-1"});
        let projected = project_event(&r, Uuid::new_v4(), ts, "ledger", &payload).unwrap();
        assert_eq!(projected.happened_on.to_string(), "2026-04-01");
    }

    #[test]
    fn null_created_by_path_falls_back_to_event_source() {
        let r = rule("commerce.invoice.created", "finance.invoice.issued");
        let payload = serde_json::json!({"id": "inv-1", "issued_on": "2026-01-01"});
        let projected =
            project_event(&r, Uuid::new_v4(), Utc::now(), "commerce", &payload).unwrap();
        assert_eq!(projected.created_by, "commerce");
    }

    #[test]
    fn fact_id_is_deterministic_for_same_event_and_kind() {
        let event_id = Uuid::new_v4();
        let a = derive_fact_id(event_id, "finance.invoice.issued");
        let b = derive_fact_id(event_id, "finance.invoice.issued");
        assert_eq!(a, b);
    }

    #[test]
    fn fact_id_differs_per_event() {
        let a = derive_fact_id(Uuid::new_v4(), "finance.invoice.issued");
        let b = derive_fact_id(Uuid::new_v4(), "finance.invoice.issued");
        assert_ne!(a, b);
    }

    #[test]
    fn fact_id_differs_per_kind() {
        let event_id = Uuid::new_v4();
        let a = derive_fact_id(event_id, "finance.invoice.issued");
        let b = derive_fact_id(event_id, "finance.invoice.paid");
        assert_ne!(a, b);
    }
}
