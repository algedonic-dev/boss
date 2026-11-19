//! What PostgreSQL was doing while a jobs request stalled (d9b39685).
//!
//! On 2026-10-29 every hour of the series failed the 2 s line, with write
//! maxima of 15.8-77.1 s, and no record said why: the forge's bounded reader
//! is triggered through the stalling API and keeps no history, and the pod
//! log names slow statements but no wait. This reads the moment instead,
//! while the request is still running: wait events grouped by client and
//! backend type (server background processes included, so a WAL or
//! checkpoint stall shows), each lock wait with the backends blocking it,
//! the serving pool's own counters, and the membership refresh in flight.
//!
//! WAL and IO timing (923d75bd): four hours of these moments named
//! LWLock:WALWrite in every stall and zero lock waits, which names the wait
//! but cannot say how long a WAL write took. Each moment now also reads the
//! cumulative `pg_stat_wal` counters and `pg_stat_io`'s write and fsync
//! totals per backend type, and subtracts the previous moment this process
//! read, so a stall carries milliseconds of WAL write and sync over a stated
//! number of seconds. The times need `track_wal_io_timing` and
//! `track_io_timing`, which the database's pod arguments turn on; with
//! either off its times read null, never zero.
//!
//! Cheap and bounded by construction: one read-only statement over
//! `pg_stat_activity` with fixed row limits (plus the two fixed-size
//! statistics views above, at most 16 IO rows), `pg_blocking_pids` called only
//! for rows already waiting on a lock, no query text or bind values (the
//! precedent of `infra/forge/jobs-database-observation.sql`), a 2 s
//! statement timeout under a 3 s total, on a connection that is NOT the
//! serving pool's, so a queue on that pool is observed rather than joined.
use boss_core::request_latency::SlowObserver;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

const DATABASE_BOUND: Duration = Duration::from_secs(3);

const MOMENT_SQL: &str = r#"
WITH activity AS MATERIALIZED (
    SELECT pid, backend_type, application_name, state, wait_event_type, wait_event,
           EXTRACT(EPOCH FROM clock_timestamp() - xact_start)::float8 AS transaction_age_seconds,
           EXTRACT(EPOCH FROM clock_timestamp() - query_start)::float8 AS query_age_seconds
    FROM pg_stat_activity
    WHERE (datname = current_database() OR backend_type <> 'client backend')
      AND pid <> pg_backend_pid()
), wait_counts AS (
    SELECT application_name, backend_type, state, wait_event_type, wait_event,
           count(*) AS backends,
           max(transaction_age_seconds) AS oldest_transaction_age_seconds
    FROM activity
    GROUP BY application_name, backend_type, state, wait_event_type, wait_event
), lock_waits AS MATERIALIZED (
    SELECT pid, application_name, backend_type, wait_event,
           query_age_seconds, transaction_age_seconds,
           pg_blocking_pids(pid) AS blocking_pids
    FROM activity
    WHERE wait_event_type = 'Lock'
    ORDER BY query_age_seconds DESC NULLS LAST, pid
    LIMIT 8
)
SELECT jsonb_build_object(
    'state', 'observed',
    'observed_at', clock_timestamp(),
    'database', current_database(),
    'reads_all_activity', pg_has_role(current_user, 'pg_read_all_stats', 'USAGE'),
    'activity_count', (SELECT count(*) FROM activity),
    'wait_group_count', (SELECT count(*) FROM wait_counts),
    'wait_groups', (SELECT COALESCE(jsonb_agg(to_jsonb(g) ORDER BY g.backends DESC,
            g.application_name, g.backend_type, g.state, g.wait_event_type, g.wait_event), '[]'::jsonb)
        FROM (SELECT * FROM wait_counts ORDER BY backends DESC,
            application_name, backend_type, state, wait_event_type, wait_event LIMIT 32) g),
    'lock_wait_count', (SELECT count(*) FROM activity WHERE wait_event_type = 'Lock'),
    'lock_waits', (SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY l.query_age_seconds DESC NULLS LAST, l.pid), '[]'::jsonb)
        FROM lock_waits l),
    'blockers', (SELECT COALESCE(jsonb_agg(to_jsonb(b) ORDER BY b.pid), '[]'::jsonb) FROM (
        SELECT pid, application_name, backend_type, state, wait_event_type, wait_event,
               transaction_age_seconds
        FROM activity
        WHERE pid IN (SELECT unnest(blocking_pids) FROM lock_waits)
        ORDER BY pid LIMIT 8) b),
    -- Cumulative, server-wide. Times are null while their switch is off, as
    -- in infra/forge/jobs-database-observation.sql: a zero would read as fast.
    'wal', (SELECT jsonb_build_object(
        'source_view', 'pg_stat_wal',
        'stats_reset', stats_reset,
        'sampled_at_epoch', EXTRACT(EPOCH FROM clock_timestamp())::float8,
        'track_wal_io_timing', current_setting('track_wal_io_timing'),
        'counters', jsonb_strip_nulls(jsonb_build_object(
            'wal_records', wal_records, 'wal_fpi', wal_fpi, 'wal_bytes', wal_bytes,
            'wal_buffers_full', wal_buffers_full, 'wal_write', wal_write, 'wal_sync', wal_sync,
            'wal_write_time_ms', CASE WHEN current_setting('track_wal_io_timing') = 'on' THEN wal_write_time END,
            'wal_sync_time_ms', CASE WHEN current_setting('track_wal_io_timing') = 'on' THEN wal_sync_time END))
    ) FROM pg_stat_wal),
    'track_io_timing', current_setting('track_io_timing'),
    'io_stats_reset', (SELECT max(stats_reset) FROM pg_stat_io),
    'io', (SELECT COALESCE(jsonb_agg(jsonb_strip_nulls(to_jsonb(i)) ORDER BY i.backend_type), '[]'::jsonb) FROM (
        SELECT backend_type, sum(writes)::float8 AS writes, sum(fsyncs)::float8 AS fsyncs,
               CASE WHEN current_setting('track_io_timing') = 'on' THEN sum(write_time) END AS write_time_ms,
               CASE WHEN current_setting('track_io_timing') = 'on' THEN sum(fsync_time) END AS fsync_time_ms
        FROM pg_stat_io
        GROUP BY backend_type
        HAVING COALESCE(sum(writes), 0) + COALESCE(sum(fsyncs), 0) > 0
        ORDER BY backend_type LIMIT 16) i),
    'scope', 'pg_stat_activity of this database plus server background processes; at most 32 wait groups, 8 lock waits and 8 blockers; server-wide pg_stat_wal and at most 16 pg_stat_io backend types; no query text or bind values'
)
"#;

/// The `pg_stat_io` fields a moment keeps per backend type, and subtracts.
const IO_FIELDS: [&str; 4] = ["writes", "fsyncs", "write_time_ms", "fsync_time_ms"];

/// The cumulative counters one moment read, kept for the next moment to
/// subtract. Server-wide, so any process's earlier read would do; this one
/// keeps its own, because a moment must not depend on another reader.
#[derive(Clone, Debug, PartialEq)]
struct WalSample {
    at_epoch: f64,
    /// Both views' `stats_reset`, as read: a change means the counters
    /// restarted, and a subtraction across it would be a fiction.
    resets: String,
    counters: BTreeMap<String, f64>,
}

impl WalSample {
    fn from_moment(moment: &serde_json::Value) -> Option<Self> {
        let wal = moment.get("wal")?;
        let at_epoch = wal.get("sampled_at_epoch")?.as_f64()?;
        let wal_counters = wal
            .get("counters")?
            .as_object()?
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_f64()?)));
        let io_counters = moment
            .get("io")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row, row.get("backend_type")?.as_str()?)))
            .flat_map(|(row, backend)| {
                IO_FIELDS.iter().filter_map(move |field| {
                    Some((format!("io.{backend}.{field}"), row.get(*field)?.as_f64()?))
                })
            });
        let null = serde_json::Value::Null;
        Some(Self {
            at_epoch,
            resets: format!(
                "{}|{}",
                wal.get("stats_reset").unwrap_or(&null),
                moment.get("io_stats_reset").unwrap_or(&null)
            ),
            counters: wal_counters.chain(io_counters).collect(),
        })
    }
}

/// What moved between two samples: every counter present in both, nonzero
/// differences only. Pure, so every refusal below is a unit test.
fn since_previous(previous: Option<&WalSample>, now: &WalSample) -> serde_json::Value {
    let Some(previous) = previous else {
        return serde_json::json!({"state": "no earlier sample",
            "reason": "the first moment this process read has nothing to subtract"});
    };
    if previous.resets != now.resets {
        return serde_json::json!({"state": "reset",
            "reason": "pg_stat_wal or pg_stat_io statistics were reset between the two samples"});
    }
    let seconds = now.at_epoch - previous.at_epoch;
    if seconds.is_nan() || seconds <= 0.0 {
        return serde_json::json!({"state": "unavailable",
            "reason": "the database clock did not advance between the two samples"});
    }
    let deltas: Result<serde_json::Map<String, serde_json::Value>, &String> = now
        .counters
        .iter()
        .filter_map(|(key, value)| Some((key, value - previous.counters.get(key)?)))
        .filter(|(_, delta)| *delta != 0.0)
        .map(|(key, delta)| {
            if delta < 0.0 {
                Err(key)
            } else {
                Ok((key.clone(), serde_json::json!(delta)))
            }
        })
        .collect();
    match deltas {
        Err(key) => serde_json::json!({"state": "reset",
            "reason": format!("{key} went backwards between the two samples")}),
        Ok(deltas) => serde_json::json!({
            "state": "observed",
            "seconds": seconds,
            "deltas": deltas,
            "meaning": "cumulative counters as flushed to the statistics store, subtracted from this process's previous moment; a counter absent here did not move or was not in both samples; a backend still inside a stalled write has not reported it yet, so its time lands in a later delta",
        }),
    }
}

pub struct PgSlowObserver {
    serving: PgPool,
    observer: PgPool,
    /// The newest WAL sample this observer read; a moment subtracts it.
    previous_wal: Mutex<Option<WalSample>>,
}

impl PgSlowObserver {
    /// `serving` is only counted, never queried; `observer` must be a
    /// different pool, or the observation queues behind the stall it reads.
    pub fn new(serving: PgPool, observer: PgPool) -> Self {
        Self {
            serving,
            observer,
            previous_wal: Mutex::new(None),
        }
    }

    /// The moment with `wal_since_previous` added, and the newest sample
    /// kept. Two observations can overlap (each is bounded, not serialised),
    /// so a sample older than the kept one is subtracted but never kept.
    fn with_wal_delta(&self, mut moment: serde_json::Value) -> serde_json::Value {
        let delta = match WalSample::from_moment(&moment) {
            None => serde_json::json!({"state": "unavailable",
                "reason": "the moment carried no pg_stat_wal sample"}),
            Some(now) => {
                let mut kept = self.previous_wal.lock().unwrap_or_else(|e| e.into_inner());
                let delta = since_previous(kept.as_ref(), &now);
                if kept
                    .as_ref()
                    .is_none_or(|before| before.at_epoch < now.at_epoch)
                {
                    *kept = Some(now);
                }
                delta
            }
        };
        if let Some(object) = moment.as_object_mut() {
            object.insert("wal_since_previous".into(), delta);
        }
        moment
    }

    async fn read_database(&self) -> Result<serde_json::Value, sqlx::Error> {
        let mut tx = self.observer.begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        let moment: serde_json::Value = sqlx::query_scalar(MOMENT_SQL).fetch_one(&mut *tx).await?;
        tx.rollback().await?;
        Ok(moment)
    }
}

#[async_trait::async_trait]
impl SlowObserver for PgSlowObserver {
    async fn observe(&self) -> serde_json::Value {
        // Counters first: they cost nothing and survive a database that
        // cannot answer, which is itself the evidence pool exhaustion and
        // a server-side stall differ by.
        let serving_pool = serde_json::json!({
            "size": self.serving.size(),
            "idle": self.serving.num_idle(),
            "max_connections": self.serving.options().get_max_connections(),
            "closed": self.serving.is_closed(),
            "consistency": "separately sampled atomics; SQLx exposes no waiter count",
        });
        let observed_at = boss_clock_client::wall_now();
        let database = match tokio::time::timeout(DATABASE_BOUND, self.read_database()).await {
            Ok(Ok(moment)) => self.with_wal_delta(moment),
            Ok(Err(error)) => serde_json::json!({"state": "unavailable",
                "reason": format!("database observation refused: {error}")}),
            Err(_) => serde_json::json!({"state": "unavailable",
                "reason": "database observation did not answer within its bound",
                "bound_ms": DATABASE_BOUND.as_millis()}),
        };
        serde_json::json!({
            "state": "observed",
            "observed_at": observed_at,
            "serving_pool": serving_pool,
            "database": database,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(
        at: f64,
        reset: &str,
        write_ms: f64,
        checkpointer_fsync_ms: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "wal": {"stats_reset": reset, "sampled_at_epoch": at,
                "counters": {"wal_write": 10.0 + write_ms, "wal_write_time_ms": write_ms,
                    "wal_sync": 3.0}},
            "io_stats_reset": "io-reset",
            "io": [{"backend_type": "checkpointer", "fsyncs": 2.0,
                "fsync_time_ms": checkpointer_fsync_ms}],
        })
    }

    fn sample(value: serde_json::Value) -> WalSample {
        WalSample::from_moment(&value).unwrap()
    }

    #[test]
    fn a_delta_is_the_nonzero_movement_over_the_stated_seconds() {
        let before = sample(moment(100.0, "r", 5.0, 1.0));
        let now = sample(moment(112.5, "r", 4005.0, 900.0));
        let delta = since_previous(Some(&before), &now);
        assert_eq!(delta["state"], "observed");
        assert_eq!(delta["seconds"], 12.5);
        assert_eq!(delta["deltas"]["wal_write_time_ms"], 4000.0);
        assert_eq!(delta["deltas"]["wal_write"], 4000.0);
        assert_eq!(delta["deltas"]["io.checkpointer.fsync_time_ms"], 899.0);
        assert!(
            delta["deltas"].get("wal_sync").is_none(),
            "a counter that did not move is left out: {delta}"
        );
    }

    #[test]
    fn nothing_is_subtracted_across_a_reset_a_first_sample_or_a_stopped_clock() {
        let before = sample(moment(100.0, "r", 5.0, 1.0));
        assert_eq!(since_previous(None, &before)["state"], "no earlier sample");
        let reset = sample(moment(110.0, "r2", 9.0, 1.0));
        assert_eq!(since_previous(Some(&before), &reset)["state"], "reset");
        let backwards = sample(moment(110.0, "r", 1.0, 1.0));
        assert_eq!(since_previous(Some(&before), &backwards)["state"], "reset");
        let same_instant = sample(moment(100.0, "r", 9.0, 1.0));
        assert_eq!(
            since_previous(Some(&before), &same_instant)["state"],
            "unavailable"
        );
    }

    #[test]
    fn a_time_read_null_while_its_switch_is_off_is_never_a_zero() {
        let mut off = moment(100.0, "r", 0.0, 0.0);
        off["wal"]["counters"]
            .as_object_mut()
            .unwrap()
            .remove("wal_write_time_ms");
        let off = sample(off);
        let on = sample(moment(105.0, "r", 50.0, 0.0));
        let delta = since_previous(Some(&off), &on);
        assert_eq!(delta["state"], "observed");
        assert!(
            delta["deltas"].get("wal_write_time_ms").is_none(),
            "a time absent from either sample is not subtracted: {delta}"
        );
    }
}
