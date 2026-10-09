//! Rebuild the `agent_runs` projection from `audit_log`.
//!
//! One state event drives it — `agents.run.recorded` — whose payload is
//! the whole run including the price it was charged at. The price is
//! replayed, NOT recomputed: the rate card is mutable data, and
//! re-pricing an old run against today's card would make a rebuild
//! change history. `recorded_at` comes from the audit row's own
//! timestamp, which is the instant the live write bound into the row.
//!
//! A run is usually one event. It is two when its first record held no
//! count and a later one replaced it (`super::port::replaces`): the
//! second names what it replaced in `detail`, and replays as a
//! replacement, in log order, so the rebuilt row is the live one.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

use super::postgres::insert_run_sql;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("agent_runs");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    /// Rows the rebuilt table holds: one per run, however many events
    /// recorded it.
    pub runs_inserted: u64,
    /// Placeholder rows a later record of the same run replaced.
    pub runs_replaced: u64,
}

/// Drop every `agent_runs` row and replay `agents.run.recorded` in
/// audit_log id order.
pub async fn rebuild_agent_runs(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM agent_runs"],
        "kind = 'agents.run.recorded'",
        async |conn, ev| {
            // The payload is a recorded run minus `recorded_at`, which
            // is the audit row's timestamp. Rebuild the full value by
            // pairing the two rather than defaulting the instant.
            let mut payload = ev.payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("recorded_at".into(), serde_json::json!(ev.ts));
            }
            let run: super::types::AgentRun = match serde_json::from_value(payload) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        event_id = ev.audit_id,
                        error = %e,
                        "skipping agents.run.recorded whose payload does not deserialize as an AgentRun"
                    );
                    return Ok(Applied::Skipped);
                }
            };
            // A record that REPLACED a placeholder (backlog b5a3a174)
            // says so in its own payload, and replays as it was applied:
            // the row the earlier event put here goes, and this one
            // takes its place. Without this the insert below would
            // collapse onto the placeholder — `DO NOTHING` — and a
            // rebuilt table would price the run at nothing while the
            // live one priced it in full.
            if super::port::is_replacement(&run.run) {
                let gone = sqlx::query("DELETE FROM agent_runs WHERE run_id = $1")
                    .bind(&run.run.run_id)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .rows_affected();
                report.runs_replaced += gone;
                report.runs_inserted -= gone.min(report.runs_inserted);
            }
            insert_run(&mut *conn, &run).await.map_err(|e| e.to_string())?;
            report.runs_inserted += 1;
            Ok(Applied::Yes)
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

/// Insert one replayed run. Separate from the live adapter's INSERT
/// because that one prices and this one replays the recorded price —
/// but the COLUMN LIST is shared, so the two cannot drift on shape.
async fn insert_run(
    conn: &mut sqlx::PgConnection,
    run: &super::types::AgentRun,
) -> Result<(), sqlx::Error> {
    sqlx::query(&insert_run_sql())
        .bind(&run.run.run_id)
        .bind(run.run.actor_id.to_string())
        // The resolved model, NOT the raw key: an event written before
        // the column existed (2026-09-10 to 2026-09-15) has no `model`
        // key and carries the model inside its colon-form actor id.
        // `NewAgentRun::model` reads that back by the rule the
        // migration's backfill used, so a rebuilt table and a migrated
        // one hold the same column (determinism). A newer event carries
        // the key and the same call returns it unchanged.
        .bind(run.run.model())
        .bind(run.run.started_at)
        .bind(run.run.finished_at)
        .bind(run.run.outcome.as_str())
        .bind(run.run.error.as_deref())
        // NULL for a run that reported no count — unknown, not zero,
        // the way the recorder wrote it (backlog 65c9c05a).
        .bind(
            run.run
                .tokens
                .total()
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .bind(
            run.run
                .tokens
                .input()
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .bind(
            run.run
                .tokens
                .output()
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .bind(i32::try_from(run.run.tool_calls).unwrap_or(i32::MAX))
        .bind(run.usd_micros.map(|m| i64::try_from(m).unwrap_or(i64::MAX)))
        .bind(run.priced_by.as_deref())
        .bind(run.run.job_id)
        .bind(run.run.branch.as_deref())
        .bind(&run.run.detail)
        // The admission decision as it was made, replayed like the
        // price; NULL for an event written before budgets were
        // consulted, which is "no decision", not "allowed".
        .bind(
            run.budget
                .as_ref()
                .map(|b| serde_json::to_value(b).unwrap_or_default()),
        )
        .bind(run.recorded_at)
        // A metered run's cache counts, replayed as recorded; NULL for
        // every other shape (backlog e6b2066f).
        .bind(
            run.run
                .tokens
                .cache_read()
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .bind(
            run.run
                .tokens
                .cache_write()
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rebuilder and the live adapter INSERT through the same
    /// generated statement, so this pins the thing that generation is
    /// for: as many placeholders as columns, always. The binds below are
    /// still hand-written, and a column added without a matching
    /// `.bind` is what this count makes loud.
    #[test]
    fn the_insert_has_one_placeholder_per_column() {
        let sql = insert_run_sql();
        let columns = super::super::postgres::RUN_COLUMNS.split(',').count();
        assert_eq!(
            columns, 20,
            "agent_runs has twenty columns (model joined on 2026-09-15, budget on 2026-09-16, \
             the two cache counts on 2026-09-24)"
        );
        for n in 1..=columns {
            assert!(
                sql.contains(&format!("${n}")),
                "placeholder ${n} is missing"
            );
        }
        assert!(
            !sql.contains(&format!("${}", columns + 1)),
            "one placeholder too many: every bind below would be off by one"
        );
    }

    #[test]
    fn the_lock_key_is_distinct_from_the_jobs_rebuilder() {
        assert_ne!(REBUILD_LOCK_KEY, boss_core::rebuild::lock_key("jobs"));
    }
}
