//! `rebuild_classes` — replay the Class registry's facts
//! (`class.declared`, `class.updated`, `class.retired`) from `audit_log`
//! onto the `classes` rows the migrations seed.
//!
//! WHY (backlog 3c6d0186, from the adversarial review of car abc2e9d5,
//! MED-2: closure). The registry's three write doors each stage their
//! fact on the outbox, and nothing read them back: boss-rebuild had no
//! Class step, so a log-rooted rebuild onto a fresh database — the
//! epoch / DR path, migrations then the log — came back with the
//! migration-seeded Classes and without every tenant declare, every
//! edit and every retirement, although each one's fact was in the log.
//! The rows are born in migrations or through the declare door, so this
//! is not a truncate-and-reproject: it folds the log onto what the
//! table holds, through the same `apply_change` the edit door writes
//! with (the subject-kinds `rebuild_subject_kinds` shape).
//!
//! It never writes a row the log disagrees with. Per Class, the pure
//! [`replay_class`] answers: already at the log's head (a live table, or
//! a second run — nothing written), at the log's start (a fresh database
//! — the head is written), or at neither (drift — the whole rebuild
//! fails naming the Class and the audit rows, and rolls back).

use std::collections::{BTreeMap, BTreeSet};

use boss_core::primitives::Class;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::port::{
    CLASS_DECLARED, CLASS_RETIRED, CLASS_UPDATED, ClassFact, is_backfill, replay_class,
};

/// Held for the whole transaction so two rebuilds of the registry never
/// interleave — the lock every `boss-rebuild-all` step takes. This one
/// took none until backlog 8d5ac7c5.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("classes");

/// What one rebuild did. `events_processed` is the name boss-rebuild's
/// tally reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClassesRebuildReport {
    pub events_processed: u64,
    pub classes_written: u64,
    pub classes_already_current: u64,
}

type HeldRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Value,
    i32,
    Option<DateTime<Utc>>,
);

pub async fn rebuild_classes(pool: &PgPool) -> Result<ClassesRebuildReport, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("taking the classes rebuild lock: {e}"))?;
    let rows: Vec<(i64, String, Value)> =
        sqlx::query_as("SELECT id, kind, payload FROM audit_log WHERE kind = ANY($1) ORDER BY id")
            .bind([CLASS_DECLARED, CLASS_UPDATED, CLASS_RETIRED].map(String::from))
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("reading the class facts: {e}"))?;

    let mut report = ClassesRebuildReport::default();
    let mut by_class: BTreeMap<(String, String), Vec<(i64, ClassFact)>> = BTreeMap::new();
    let mut backfilled: BTreeSet<i64> = BTreeSet::new();
    for (id, kind, payload) in rows {
        let (key, fact) =
            ClassFact::from_logged(&kind, &payload).map_err(|e| format!("audit_log {id}: {e}"))?;
        if kind == CLASS_DECLARED && is_backfill(&payload) {
            backfilled.insert(id);
        }
        by_class
            .entry((key.subject_kind, key.code))
            .or_default()
            .push((id, fact));
        report.events_processed += 1;
    }
    // A backfilled declare (backlog 9d345f9b) records a birth that
    // preceded every other fact its Class has, but it was appended after
    // them — so it is read FIRST. Otherwise in log order.
    for facts in by_class.values_mut() {
        facts.sort_by_key(|(id, _)| (!backfilled.contains(id), *id));
    }

    for ((subject_kind, code), facts) in &by_class {
        let held: Option<HeldRow> = sqlx::query_as(
            "SELECT subject_kind, code, display_name, parent_code, member_attribute, \
                    metadata, sort_order, retired_at \
               FROM classes WHERE subject_kind = $1 AND code = $2 FOR UPDATE",
        )
        .bind(subject_kind)
        .bind(code)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("reading class ({subject_kind}, {code}): {e}"))?;
        let held = held.map(|r| Class {
            subject_kind: r.0,
            code: r.1,
            display_name: r.2,
            parent_code: r.3,
            member_attribute: r.4,
            metadata: r.5,
            sort_order: r.6,
            retired_at: r.7,
        });
        let log: Vec<ClassFact> = facts.iter().map(|(_, f)| f.clone()).collect();
        let head = replay_class(held.as_ref(), &log).map_err(|e| {
            let ids: Vec<String> = facts.iter().map(|(id, _)| id.to_string()).collect();
            format!(
                "class ({subject_kind}, {code}) (audit_log {}): {e}",
                ids.join(", ")
            )
        })?;
        let Some(row) = head else {
            report.classes_already_current += 1;
            continue;
        };
        // One statement for both ends: a fresh database may lack a row
        // the log declared, or hold the seeded one the log edits. The
        // parent FK is DEFERRABLE INITIALLY DEFERRED, so a child
        // declared before its parent in key order still commits.
        sqlx::query(
            "INSERT INTO classes \
               (subject_kind, code, display_name, parent_code, member_attribute, \
                metadata, sort_order, retired_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (subject_kind, code) DO UPDATE SET \
               display_name = EXCLUDED.display_name, parent_code = EXCLUDED.parent_code, \
               member_attribute = EXCLUDED.member_attribute, metadata = EXCLUDED.metadata, \
               sort_order = EXCLUDED.sort_order, retired_at = EXCLUDED.retired_at",
        )
        .bind(&row.subject_kind)
        .bind(&row.code)
        .bind(&row.display_name)
        .bind(&row.parent_code)
        .bind(&row.member_attribute)
        .bind(&row.metadata)
        .bind(row.sort_order)
        .bind(row.retired_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("writing class ({subject_kind}, {code}): {e}"))?;
        report.classes_written += 1;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(report)
}
