//! Shared TRUNCATE-replay skeleton for projection rebuilders.
//!
//! Every projection rebuilder shares the same control flow: open a
//! transaction, take the rebuild advisory lock, wipe the projection
//! tables, stream the matching `audit_log` rows in id order, fold each
//! through a domain-specific `apply` step, then commit. Only three
//! things actually vary between rebuilders — the lock key, the wipe
//! statements, and the event filter — plus the apply step itself,
//! which is the whole point. This module owns the invariant skeleton so
//! each rebuilder keeps only its genuinely per-projection parts.
//!
//! The skeleton is the place the five-property correctness protocol
//! cares about: one audited definition of "replay the log into a
//! projection" (one transaction, advisory-locked, audit-ordered) beats a
//! dozen copies that can drift apart. The apply step stays in the domain
//! crate, where the projection logic belongs.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// One `audit_log` row handed to a projection's apply step.
pub struct ReplayEvent {
    /// `audit_log.id` — the monotonic sequence the replay walks in
    /// order. Carried so the apply step can log which event it skipped.
    pub audit_id: i64,
    /// `audit_log.event_id` — the event's own id, which a record can
    /// name as its cause (a sign-off stamp's `voided_by_event`, design
    /// 87329a13) and a replay must reproduce exactly.
    pub event_id: uuid::Uuid,
    pub kind: String,
    /// `audit_log.timestamp` — the recorded event time. Apply steps that
    /// stamp a projection column from it read it here; those that don't
    /// (e.g. messages) ignore it.
    pub ts: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// Whether the apply step folded the event into the projection or passed
/// on it (unknown kind, malformed payload, no-op delete). The driver
/// tallies these into [`ReplayStats`]; everything richer is the apply
/// step's own report.
pub enum Applied {
    Yes,
    Skipped,
}

/// What the driver itself counts. The per-projection report (domain
/// counters like "employees upserted") is owned by the caller's apply
/// closure, which captures its own report and mutates it in place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayStats {
    pub processed: u64,
    pub skipped: u64,
}

/// Replay the `audit_log` into a projection inside one advisory-locked
/// transaction.
///
/// Steps, in order:
/// 1. `BEGIN` a transaction.
/// 2. Take `pg_advisory_xact_lock(lock_key)` — held for the whole
///    transaction, so concurrent domain writes briefly queue and two
///    rebuilds of the same projection never interleave. Derive `lock_key`
///    from [`boss_core::rebuild::lock_key`].
/// 2a. When `wipe` clears anything, prove the log holds every committed
///    write — lock the wiped tables and `event_outbox`, count what the
///    relay has not copied yet, and refuse on any
///    ([`crate::outbox::lock_and_assert_log_complete`], design
///    b046f510). The tables are read off the wipe statements.
/// 3. Run each statement in `wipe` (a `TRUNCATE … CASCADE`, a single
///    `DELETE`, or a list of `DELETE`s — whatever clears this projection).
/// 4. Stream every `audit_log` row matching `kind_filter`, in `id` order,
///    and fold each through `apply`.
/// 5. `COMMIT`.
///
/// `kind_filter` is interpolated into the query verbatim, so it must be a
/// trusted constant (e.g. `"kind LIKE 'people.employee.%'"`), never
/// caller input. Every call site passes a `&'static str`.
///
/// `apply` receives a `&mut PgConnection` borrowed from the transaction —
/// the same handle the domain helpers (`upsert_*`, `insert_*`) already
/// take — and the decoded [`ReplayEvent`]. It returns [`Applied`] so the
/// driver can keep the processed/skipped tally; any richer accounting is
/// the closure's to do against its own captured report.
pub async fn replay_projection<F>(
    pool: &PgPool,
    lock_key: i64,
    wipe: &[&str],
    kind_filter: &str,
    apply: F,
) -> Result<ReplayStats, String>
where
    F: AsyncFnMut(&mut PgConnection, ReplayEvent) -> Result<Applied, String>,
{
    replay_projection_checked(
        pool,
        lock_key,
        wipe,
        kind_filter,
        async |_, _| Ok(()),
        apply,
    )
    .await
}

/// Validate the complete selected log under the same locks BEFORE a wipe.
/// A refusal rolls back the transaction without clearing any projection.
/// Existing callers use [`replay_projection`] and retain their apply behavior.
pub async fn replay_projection_checked<P, F>(
    pool: &PgPool,
    lock_key: i64,
    wipe: &[&str],
    kind_filter: &str,
    mut validate: P,
    mut apply: F,
) -> Result<ReplayStats, String>
where
    P: AsyncFnMut(&mut PgConnection, &[ReplayEvent]) -> Result<(), String>,
    F: AsyncFnMut(&mut PgConnection, ReplayEvent) -> Result<Applied, String>,
{
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    // A wipe deletes live rows, so it may run only against a log that
    // holds every committed write (design b046f510). A replay that
    // wipes nothing folds onto what is there and cannot lose a row, so
    // it takes no lock and stalls no writer.
    if !wipe.is_empty() {
        let tables = wipe
            .iter()
            .map(|stmt| wiped_tables(stmt))
            .collect::<Result<Vec<_>, _>>()?
            .concat();
        crate::outbox::lock_and_assert_log_complete(&mut tx, &tables).await?;
    }

    let rows: Vec<(i64, uuid::Uuid, String, DateTime<Utc>, serde_json::Value)> =
        sqlx::query_as(&format!(
            "SELECT id, event_id, kind, timestamp, payload FROM audit_log \
             WHERE {kind_filter} ORDER BY id"
        ))
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    let events: Vec<_> = rows
        .into_iter()
        .map(|(audit_id, event_id, kind, ts, payload)| ReplayEvent {
            audit_id,
            event_id,
            kind,
            ts,
            payload,
        })
        .collect();
    validate(&mut tx, &events).await?;
    for stmt in wipe {
        sqlx::query(stmt)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut stats = ReplayStats::default();
    for event in events {
        stats.processed += 1;
        match apply(&mut *tx, event).await? {
            Applied::Yes => {}
            Applied::Skipped => stats.skipped += 1,
        }
    }

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(stats)
}

/// The tables a wipe statement clears, read off the statement itself so
/// the tables the log check locks cannot drift from the tables the wipe
/// deletes (CLAUDE.md §9a). Two shapes, the two every caller uses:
/// `DELETE FROM <table>` and `TRUNCATE <table>[, <table>…] [CASCADE]`.
/// Anything else — a `WHERE`, a schema-qualified name — is refused
/// rather than guessed at.
fn wiped_tables(stmt: &str) -> Result<Vec<&str>, String> {
    let list = stmt.strip_prefix("DELETE FROM ").or_else(|| {
        stmt.strip_prefix("TRUNCATE ")
            .map(|rest| rest.strip_suffix(" CASCADE").unwrap_or(rest))
    });
    let tables: Vec<&str> = list
        .map(|l| l.split(',').map(str::trim).collect())
        .unwrap_or_default();
    let readable = !tables.is_empty()
        && tables.iter().all(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if readable {
        Ok(tables)
    } else {
        Err(format!(
            "cannot read the tables a wipe clears from {stmt:?}: a replay wipe is \
             `DELETE FROM <table>` or `TRUNCATE <table>[, <table>…] [CASCADE]`"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::wiped_tables;

    /// Every shape a rebuilder passes today, read to the tables it clears.
    #[test]
    fn a_wipe_names_the_tables_it_clears() {
        assert_eq!(wiped_tables("DELETE FROM messages"), Ok(vec!["messages"]));
        assert_eq!(wiped_tables("TRUNCATE subjects"), Ok(vec!["subjects"]));
        assert_eq!(
            wiped_tables(
                "TRUNCATE employees, employee_skills, employee_certifications, requisitions CASCADE"
            ),
            Ok(vec![
                "employees",
                "employee_skills",
                "employee_certifications",
                "requisitions"
            ])
        );
    }

    /// A statement the reader cannot name every table of is refused,
    /// never half-read: a lock on fewer tables than the wipe clears is
    /// the drift the reading exists to prevent.
    #[test]
    fn a_wipe_that_cannot_be_read_is_refused() {
        for stmt in [
            "DELETE FROM jobs WHERE id = $1",
            "DELETE FROM public.jobs",
            "UPDATE jobs SET x = 1",
            "TRUNCATE ",
            "TRUNCATE a,, b",
        ] {
            let err = wiped_tables(stmt).expect_err(stmt);
            assert!(err.contains("cannot read the tables"), "{stmt}: {err}");
        }
    }
}
