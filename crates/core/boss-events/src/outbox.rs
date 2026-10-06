//! Transactional event outbox + relay drain — Option B of
//! [docs/design/transactional-audit-log.md].
//!
//! `record_event_in_tx` is the emitting side: the event INSERT joins
//! the caller's domain transaction, so the durable hand-off is atomic
//! with the state change it describes — and the ref-check trigger
//! (`event_outbox_check_refs_trg`) runs *inside* that transaction,
//! aborting a write whose payload references a missing projection row
//! instead of punching a post-commit provenance hole (the 2026-07-13
//! phantom-account incident class).
//!
//! `drain_outbox_once` is the relay side, one batch: claim pending
//! rows in id order → INSERT into `audit_log` (the chain-hash trigger
//! runs there exactly as for legacy writers; the relay's short batch
//! transaction is the only holder of the chain lock per batch) →
//! commit → publish to the bus → stamp `delivered_at`. Every crash
//! point retries safely:
//!
//! - crash before the audit commit → rows untouched, re-drained.
//! - crash between audit commit and publish → audit row exists, row
//!   still pending; the re-drain skips the audit INSERT (NOT EXISTS
//!   by `event_id` — deliberately not ON CONFLICT, whose pre-conflict
//!   trigger fire would consume a sequence id and manufacture the id
//!   gaps the integrity checker treats as anomalies) and re-publishes.
//! - publish failure mid-batch → the batch STOPS (publish order is
//!   the outbox order; later events must not overtake a failed one),
//!   the failed row stays pending, the next drain resumes from it.
//! - publish REFUSED mid-batch (`EventBusError::Refused`: the bus will
//!   never carry this event — over NATS max_payload, invalid subject)
//!   → the row is DEAD-LETTERED and the batch continues. Retrying it
//!   in order was a poison pill (backlog e4019cbc): the head row was
//!   re-selected on every drain and nothing behind it, for any
//!   service, ever reached the bus or audit_log. The fact is not lost
//!   — its audit row committed in phase 1, and the outbox row keeps
//!   its payload whole beside `dead_letter_reason` — and the same
//!   transaction stages `events.outbox.dead_lettered` naming the row,
//!   its kind and the reason, which the dispatcher rule
//!   `open-a-packet-when-an-event-is-dead-lettered` turns into a
//!   backlog-item a person reads. A dead letter is a delivery that
//!   overtook nothing: rows behind it had nothing to wait for.
//!
//! Consumers tolerate the resulting at-least-once publishes — that is
//! the standing NAK-redelivery contract.
//!
//! `prune_delivered_outbox` is the outbox's retention: a delivered row
//! older than the window, whose fact `audit_log` holds, is deleted in
//! bounded batches, each recorded by an `events.outbox.pruned` fact
//! (backlog eec0c1f3). The relay runs it hourly.

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError};
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// The largest event, as the bus would encode it, that
/// `record_event_in_tx` will stage: 960 KiB.
///
/// The ceiling is NATS's `max_payload`. The cluster's server runs with
/// no `-max_payload` flag (infra/cluster/manifests/boss.yaml, the
/// `nats` container's args), so the server default of 1 MiB applies,
/// and the client refuses any larger message before sending it
/// (async-nats `MaxPayloadExceeded`). The size measured here is the
/// relay's own encoding — `serde_json::to_vec(&Event)`, envelope
/// included — so the only drift between this check and the relay's
/// publish is the JSONB round trip in between (key order, number
/// spelling), which moves a payload by bytes, not kilobytes. The 64
/// KiB margin covers that drift many times over and still refuses
/// nothing a normal fact carries; it is kept a margin rather than a
/// second, lower policy so the bound refuses only what the bus could
/// never carry, not what someone thought an event ought to weigh
/// (backlog e4019cbc). A write it refuses fails in its own
/// transaction, naming its kind — the cheapest place for the mistake —
/// instead of committing a row the relay has to dead-letter.
pub const MAX_STAGED_EVENT_BYTES: usize = 1024 * 1024 - 64 * 1024;

/// The alarm kind the relay stages when it dead-letters a row.
pub const DEAD_LETTERED_KIND: &str = "events.outbox.dead_lettered";

/// What one `drain_outbox_once` batch accomplished.
#[derive(Debug, Default, Clone, Copy)]
pub struct DrainStats {
    /// Rows fully delivered (audit + bus + stamped).
    pub delivered: u64,
    /// Rows the bus refused deterministically, set aside with their
    /// reason so the rows behind them could be delivered.
    pub dead_lettered: u64,
    /// Audit rows actually inserted this batch (< claimed when a
    /// prior crashed drain already landed some).
    pub audit_inserted: u64,
}

impl DrainStats {
    /// Rows this batch finished with, either way. Zero means the
    /// outbox had nothing the relay could move — the idle signal.
    pub fn moved(&self) -> u64 {
        self.delivered + self.dead_lettered
    }
}

/// Stage an event inside the caller's transaction. The INSERT fires
/// the outbox ref-check trigger, so a rejection surfaces here as
/// `Err` — the caller must abort (the transaction is already poisoned
/// by the failed statement, so commit would fail anyway). An event
/// over [`MAX_STAGED_EVENT_BYTES`] is refused before the INSERT,
/// naming its kind.
pub async fn record_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    event: &Event,
) -> Result<(), String> {
    let size = serde_json::to_vec(event).map_err(|e| e.to_string())?.len();
    if size > MAX_STAGED_EVENT_BYTES {
        return Err(format!(
            "event kind {} encodes to {size} bytes, over the {MAX_STAGED_EVENT_BYTES}-byte \
             staging bound: the bus (NATS max_payload, 1 MiB) could never publish it, so it \
             is refused here rather than staged",
            event.kind
        ));
    }
    sqlx::query(
        "INSERT INTO event_outbox (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event.id)
    .bind(event.timestamp)
    .bind(&event.source)
    .bind(&event.kind)
    .bind(&event.payload)
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Pending outbox rows: neither delivered nor dead-lettered, so the
/// relay still owes them a move. Used by the relay's idle check,
/// tests, and the epoch-restart quiescence gate.
pub async fn pending_count(pool: &PgPool) -> Result<i64, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox \
         WHERE delivered_at IS NULL AND dead_lettered_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Open dead letters: rows the relay set aside that no operator has
/// resolved. Every reader that waits on [`pending_count`] names this
/// number beside it (backlog e22b692e), because a dead letter is not
/// lag — the relay has finished with it — and a reader that counted
/// it as pending waited out its bound and blamed a stuck relay.
pub async fn dead_lettered_count(pool: &PgPool) -> Result<i64, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox \
         WHERE dead_lettered_at IS NOT NULL AND dead_letter_resolved_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Committed writes whose event has not yet reached `audit_log`: how
/// many, and since when (backlog 72c50b8b). This is the exposure window
/// of every audit_log reader — a rebuild replaying the log now misses
/// exactly these rows — and until this read nothing measured it.
///
/// The span is anchored at the outbox's OWN newest row, not the wall
/// clock — the same choice `AuditStats` makes for the log (no-wallclock):
/// `behind_head_seconds` is how much of the write history, measured
/// back from the newest staged write, audit_log does not yet hold. On a
/// live system that is the oldest row's age; when the undrained row IS
/// the newest write it reads 0, and `oldest_created_at` still dates it.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Undrained {
    pub count: i64,
    /// `created_at` of the oldest such row — when its write committed.
    pub oldest_created_at: Option<DateTime<Utc>>,
    /// Seconds from `oldest_created_at` to the newest outbox row's
    /// `created_at`. `None` when nothing is undrained.
    pub behind_head_seconds: Option<f64>,
}

/// What "committed but not yet in audit_log" means, as SQL — spelled
/// once, so the count and the row a refusal names cannot disagree
/// (CLAUDE.md §9a). `o` is the outbox row.
const UNDRAINED_ROWS: &str = "FROM event_outbox o \
     WHERE o.delivered_at IS NULL AND o.dead_lettered_at IS NULL \
       AND NOT EXISTS (SELECT 1 FROM audit_log a WHERE a.event_id = o.event_id)";

/// The ONE query for [`Undrained`] (CLAUDE.md §9a): the events door
/// serves it on the pool, and design b046f510's rebuild runs it on its
/// own locked transaction — hence generic over the executor.
///
/// Scoped to PENDING rows, so it walks the `event_outbox_pending`
/// partial index rather than every row staged (delivered rows are
/// retained for [`DEFAULT_OUTBOX_RETENTION`]). Same answer as the unscoped anti-join: the relay commits
/// a row's audit INSERT before it stamps `delivered_at` or
/// `dead_lettered_at`, so a row with either stamp already has its
/// audit_log row. That matters most to the rebuild, which counts while
/// holding a lock that stalls every writer.
pub async fn undrained<'e, E>(executor: E) -> Result<Undrained, String>
where
    E: sqlx::PgExecutor<'e>,
{
    // The head is the newest row by id — the primary key, so one index
    // probe — which is the newest staged write.
    let sql = format!(
        "SELECT COUNT(*)::BIGINT, MIN(o.created_at), \
            EXTRACT(EPOCH FROM \
                (SELECT created_at FROM event_outbox ORDER BY id DESC LIMIT 1) \
                - MIN(o.created_at))::FLOAT8 \
         {UNDRAINED_ROWS}"
    );
    let (count, oldest_created_at, behind_head_seconds): (i64, Option<DateTime<Utc>>, Option<f64>) =
        sqlx::query_as(&sql)
            .fetch_one(executor)
            .await
            .map_err(|e| e.to_string())?;
    Ok(Undrained {
        count,
        oldest_created_at,
        behind_head_seconds,
    })
}

/// The oldest undrained row — `(event_outbox.id, event_id, kind)` — so a
/// refusal names the row a reader goes to look at, not only a count
/// (review F4 on design b046f510).
async fn oldest_undrained_row(
    conn: &mut sqlx::PgConnection,
) -> Result<Option<(i64, Uuid, String)>, String> {
    sqlx::query_as(&format!(
        "SELECT o.id, o.event_id, o.kind {UNDRAINED_ROWS} ORDER BY o.id LIMIT 1"
    ))
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| e.to_string())
}

/// The relay's delivery lag over a window: `delivered_at - created_at`
/// for every row delivered in the `window_hours` up to the newest
/// delivery — anchored at the outbox's own head like [`Undrained`], so
/// a quiet relay still answers its last day of work (backlog 72c50b8b).
/// The p95 and max are the measured basis design b046f510's drain
/// timeout asks for. Nothing delivered answers `delivered: 0` and no
/// percentiles — never a zero lag, which would read as an instant
/// relay.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct RelayLag {
    pub window_hours: i32,
    /// Rows delivered in the window — the sample size.
    pub delivered: i64,
    pub p50_seconds: Option<f64>,
    pub p95_seconds: Option<f64>,
    pub max_seconds: Option<f64>,
    /// Hours the sample ACTUALLY spans: from the oldest delivery in it to
    /// the newest delivery in the outbox. Retention deletes delivered
    /// rows past its window (a week by default, backlog eec0c1f3), so a
    /// `window_hours` wider than that answers no older sample, and this
    /// is the window measured — read it, not `window_hours`, as the
    /// sample's reach (review afdc2d5d, N2). `None` when nothing was
    /// delivered in the window.
    pub covered_hours: Option<f64>,
}

/// Measure [`RelayLag`]. `delivered_at` carries no index, so this reads
/// the table; it answers an operator on demand, never a hot path.
pub async fn relay_lag(pool: &PgPool, window_hours: i32) -> Result<RelayLag, String> {
    let (delivered, p50_seconds, p95_seconds, max_seconds, covered_hours): (
        i64,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    ) = sqlx::query_as(
        "WITH head AS (SELECT MAX(delivered_at) AS at FROM event_outbox), \
              lag AS ( \
             SELECT EXTRACT(EPOCH FROM delivered_at - created_at)::FLOAT8 AS s, delivered_at \
             FROM event_outbox \
             WHERE delivered_at >= (SELECT at FROM head) - make_interval(hours => $1)) \
         SELECT COUNT(*)::BIGINT, \
                percentile_cont(0.5) WITHIN GROUP (ORDER BY s), \
                percentile_cont(0.95) WITHIN GROUP (ORDER BY s), \
                MAX(s), \
                (EXTRACT(EPOCH FROM (SELECT at FROM head) - MIN(delivered_at)) / 3600)::FLOAT8 \
         FROM lag",
    )
    .bind(window_hours)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(RelayLag {
        window_hours,
        delivered,
        p50_seconds,
        p95_seconds,
        max_seconds,
        covered_hours,
    })
}

/// Prove, inside a rebuild's own transaction, that `audit_log` holds
/// every committed write — or refuse to let it wipe anything (design
/// b046f510, backlog d6656496).
///
/// A live writer stages its event in `event_outbox` in the SAME
/// transaction as its projection row, and the relay copies it into
/// `audit_log` later. A rebuild that wipes a projection and replays
/// `audit_log` between those two moments deletes that row and has no
/// fact to put it back from. So, before the wipe:
///
/// 1. `ACCESS EXCLUSIVE` on each table the step wipes — waits for an
///    in-flight writer of those tables to commit (its outbox row with
///    it) and holds new ones off until the rebuild commits.
/// 2. `SHARE` on `event_outbox` — waits for any other in-flight staging
///    to commit and holds new ones off, including a writer that touches
///    no wiped table but stages a fact the replay folds. It does not
///    conflict with the relay's row claim, so what is already staged can
///    still reach `audit_log`; the relay's `delivered_at` stamp waits.
/// 3. [`undrained`] on this transaction. Every write committed before
///    step 2 is now either in `audit_log` or counted here, and every
///    later one waits for the rebuild — so a zero is the whole log for
///    this window, not a reading that can go stale before the wipe.
///
/// Steps 1 and 2 CAN deadlock against a live writer, and no fixed order
/// prevents it: taking the wiped tables first deadlocks the jobs-create
/// shape (parent row, its fact, then the child row, against a wipe that
/// lists the child first), and taking the outbox first deadlocks the
/// plainer shape (projection row, then its fact) — both reproduced in
/// review. So the rebuild is made the side that yields: the locks are
/// taken under a `lock_timeout` shorter than Postgres's deadlock check,
/// inside a savepoint, and on a timeout or a deadlock the savepoint is
/// rolled back (releasing what this attempt took), the rebuild backs
/// off and tries again, a bounded number of times. The live writer is
/// never the one aborted by our cycle, and the rebuild never sits in the
/// lock queue in front of every reader for longer than the timeout.
///
/// Non-zero is `Err`, naming the count, the oldest row and the likely
/// cause; the caller returns it and its transaction rolls back
/// untouched. No shared lock with the relay and no change to any
/// writer: the locks are the step's own and die with its transaction.
///
/// `tables` are interpolated into `LOCK TABLE`, so each must be a bare
/// identifier — every caller passes constants, and anything else is
/// refused rather than quoted.
pub async fn lock_and_assert_log_complete(
    conn: &mut sqlx::PgConnection,
    tables: &[&str],
) -> Result<(), String> {
    if let Some(bad) = tables.iter().find(|t| !is_bare_identifier(t)) {
        return Err(format!(
            "refusing to lock {bad:?}: a rebuild names its tables as bare identifiers"
        ));
    }
    take_rebuild_locks(conn, tables).await?;
    let owed = undrained(&mut *conn).await?;
    if owed.count > 0 {
        let oldest = owed
            .oldest_created_at
            .map_or_else(|| "unknown".to_string(), |t| t.to_rfc3339());
        let row = oldest_undrained_row(conn).await?.map_or_else(
            || "row unreadable".to_string(),
            |(id, event_id, kind)| {
                format!("event_outbox id {id}, event_id {event_id}, kind {kind}")
            },
        );
        return Err(format!(
            "refusing to replay {}: {} committed write(s) have not reached audit_log \
             (oldest staged {oldest}: {row}) — the event relay has not copied them yet, \
             most often a relay stuck on publish (the bus down, or more than a batch \
             behind); replaying now would drop them from the projection, so run it again \
             once the relay has caught up (design b046f510)",
            tables.join(", "),
            owed.count
        ));
    }
    Ok(())
}

/// Each lock attempt waits at most this long — under Postgres's default
/// `deadlock_timeout` (1 s), so when a writer's lock order and the
/// rebuild's form a cycle, the rebuild's own timeout fires before either
/// side's deadlock check and the rebuild, not the writer, lets go.
const REBUILD_LOCK_TIMEOUT_MS: u64 = 500;

/// Attempts before the rebuild gives up on its locks, backing off 250 ms
/// longer each time — a few seconds in all, far past any one write.
const REBUILD_LOCK_ATTEMPTS: u32 = 5;

/// Steps 1 and 2 of [`lock_and_assert_log_complete`], yielding to live
/// writers: see its doc for why no fixed order is safe.
async fn take_rebuild_locks(conn: &mut sqlx::PgConnection, tables: &[&str]) -> Result<(), String> {
    let mut statements: Vec<String> = Vec::new();
    if !tables.is_empty() {
        statements.push(format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
            tables.join(", ")
        ));
    }
    statements.push("LOCK TABLE event_outbox IN SHARE MODE".to_string());
    let run = async |conn: &mut sqlx::PgConnection, sql: &str| {
        sqlx::query(sql)
            .execute(&mut *conn)
            .await
            .map(|_| ())
            .map_err(|e| format!("{sql}: {e}"))
    };

    let mut last = String::new();
    for attempt in 1..=REBUILD_LOCK_ATTEMPTS {
        // SET LOCAL inside the savepoint: rolled back with it on a
        // retry, and reset explicitly on success so the replay's own
        // statements never inherit the short timeout.
        run(conn, "SAVEPOINT rebuild_locks").await?;
        run(
            conn,
            &format!("SET LOCAL lock_timeout = '{REBUILD_LOCK_TIMEOUT_MS}ms'"),
        )
        .await?;
        let mut taken = Ok(());
        for sql in &statements {
            taken = sqlx::query(sql).execute(&mut *conn).await.map(|_| ());
            if taken.is_err() {
                break;
            }
        }
        match taken {
            Ok(()) => {
                run(conn, "RELEASE SAVEPOINT rebuild_locks").await?;
                run(conn, "SET LOCAL lock_timeout TO DEFAULT").await?;
                return Ok(());
            }
            Err(e) if yields_to_a_writer(&e) => {
                run(conn, "ROLLBACK TO SAVEPOINT rebuild_locks").await?;
                last = e.to_string();
                tracing::warn!(
                    attempt,
                    tables = %tables.join(", "),
                    error = %last,
                    "a rebuild yielded its locks to a live writer; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(attempt)))
                    .await;
            }
            Err(e) => return Err(format!("locking {}: {e}", tables.join(", "))),
        }
    }
    Err(format!(
        "could not lock {} and event_outbox in {REBUILD_LOCK_ATTEMPTS} attempts \
         ({REBUILD_LOCK_TIMEOUT_MS} ms each): live writers kept holding them, and the \
         rebuild yields rather than deadlock a write; run it again (last: {last})",
        tables.join(", ")
    ))
}

/// A lock timeout (55P03) or a deadlock (40P01): the rebuild's cue to
/// let go and retry rather than fail or make a writer the victim.
fn yields_to_a_writer(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "55P03" || c == "40P01")
}

/// `[a-z_][a-z0-9_]*` — the only table spelling
/// [`lock_and_assert_log_complete`] interpolates.
fn is_bare_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// How long a rebuild waits, unlocked, for the relay to drain before
/// its locked check decides — the `--drain-timeout` default (design
/// b046f510). The relay idles 250 ms between polls and a live instance
/// normally empties within one, so thirty seconds makes a refusal the
/// exception rather than the rule on a busy instance.
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 30;

/// Re-read cadence of [`wait_for_drain`] — the relay's own idle poll.
const DRAIN_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Wait, taking no lock, until [`undrained`] reads zero or `timeout`
/// passes, and return the last reading. It decides nothing: a rebuild
/// calls it before it locks so that a relay a poll behind is waited out
/// rather than refused, and [`lock_and_assert_log_complete`] under the
/// lock is still the only check that counts. A reading still owed at
/// the deadline is logged, so a refusal that follows has the wait
/// beside it.
pub async fn wait_for_drain(
    pool: &PgPool,
    timeout: std::time::Duration,
) -> Result<Undrained, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let owed = undrained(pool).await?;
        if owed.count == 0 {
            return Ok(owed);
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                undrained = owed.count,
                oldest_created_at = ?owed.oldest_created_at,
                timeout_secs = timeout.as_secs_f64(),
                "event_outbox has not drained in time; the rebuild's locked check decides"
            );
            return Ok(owed);
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}

/// The kind `boss events redeliver` records when it puts a dead letter
/// back on the relay's queue.
pub const REDELIVERED_KIND: &str = "events.outbox.redelivered";

/// The kind `boss events redeliver --resolve` records when it closes a
/// dead letter without redelivering it.
pub const RESOLVED_KIND: &str = "events.outbox.resolved";

/// One dead letter, as the operator's act names it: the row, the
/// event's identity and size, and the refusal that set it aside —
/// never the payload, which is what the bus refused and stays on the
/// row.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeadLetter {
    pub outbox_id: i64,
    pub event_id: Uuid,
    pub event_kind: String,
    pub event_source: String,
    /// The event as the relay encodes it for the bus.
    pub payload_bytes: usize,
    pub dead_lettered_at: DateTime<Utc>,
    pub dead_letter_reason: String,
    /// A redelivery only: how many LATER outbox rows had already been
    /// delivered when this one was put back — the events subscribers
    /// saw before they will see this one. Redelivery is out-of-order
    /// delivery, and this is its size (review M2). `None` on a
    /// resolution, which moves nothing.
    pub overtaken_by: Option<i64>,
}

/// Why an act on a dead letter did not happen. Every refusal changes
/// nothing; the message names what the row is, never its payload.
#[derive(Debug, Clone, PartialEq)]
pub enum DeadLetterError {
    /// No outbox row has this id.
    NotFound(String),
    /// The row exists and is not an open dead letter (pending,
    /// delivered, already resolved), or is over the staging bound.
    Refused(String),
    /// The request itself is incomplete: a resolution with no reason.
    Invalid(String),
    /// The database failed.
    Storage(String),
}

impl std::fmt::Display for DeadLetterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(m) | Self::Refused(m) | Self::Invalid(m) | Self::Storage(m) => {
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for DeadLetterError {}

fn storage(e: impl std::fmt::Display) -> DeadLetterError {
    DeadLetterError::Storage(e.to_string())
}

/// A dead letter's row and the fact an act staged for it, read back
/// from the database after the act commits (review M3): the verb
/// prints what the record holds, not what the operator sent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReadBack {
    /// `pending`, `delivered`, `dead-lettered` or
    /// `dead-lettered, resolved` — a redelivered row may already be
    /// delivered by the time this reads it.
    pub state: String,
    /// `dead_letter_resolution` as the row holds it.
    pub resolution: Option<String>,
    /// The kind of act looked for.
    pub act_kind: String,
    /// `event_outbox.id` of the newest staged row of `act_kind` naming
    /// this outbox row — the fact the act recorded.
    pub act_outbox_id: Option<i64>,
}

/// Read one outbox row's state and the newest `act_kind` fact naming it.
pub async fn read_back(
    pool: &PgPool,
    outbox_id: i64,
    act_kind: &str,
) -> Result<ReadBack, DeadLetterError> {
    let row: Option<(bool, bool, bool, Option<String>)> = sqlx::query_as(
        "SELECT delivered_at IS NOT NULL, dead_lettered_at IS NOT NULL, \
                dead_letter_resolved_at IS NOT NULL, dead_letter_resolution \
         FROM event_outbox WHERE id = $1",
    )
    .bind(outbox_id)
    .fetch_optional(pool)
    .await
    .map_err(storage)?;
    let Some((delivered, dead, resolved, resolution)) = row else {
        return Err(DeadLetterError::NotFound(format!(
            "no outbox row {outbox_id} to read back"
        )));
    };
    let state = match (delivered, dead, resolved) {
        (true, _, _) => "delivered",
        (false, true, true) => "dead-lettered, resolved",
        (false, true, false) => "dead-lettered",
        (false, false, _) => "pending",
    };
    let act_outbox_id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM event_outbox \
         WHERE kind = $1 AND payload->>'outbox_id' = $2 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(act_kind)
    .bind(outbox_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(storage)?;
    Ok(ReadBack {
        state: state.to_string(),
        resolution,
        act_kind: act_kind.to_string(),
        act_outbox_id,
    })
}

type LetterRow = (
    Uuid,
    DateTime<Utc>,
    String,
    String,
    serde_json::Value,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<DateTime<Utc>>,
);

/// Lock one outbox row inside `tx` and hand it back only if it is an
/// OPEN dead letter; anything else is refused naming what the row is,
/// so an operator who typed the wrong id learns which wrong id.
async fn claim_dead_letter(
    tx: &mut Transaction<'_, Postgres>,
    outbox_id: i64,
) -> Result<DeadLetter, DeadLetterError> {
    let row: Option<LetterRow> = sqlx::query_as(
        "SELECT event_id, timestamp, source, kind, payload, delivered_at, \
                dead_lettered_at, dead_letter_reason, dead_letter_resolved_at \
         FROM event_outbox WHERE id = $1 FOR UPDATE",
    )
    .bind(outbox_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    let Some((event_id, timestamp, source, kind, payload, delivered, dead, reason, resolved)) = row
    else {
        return Err(DeadLetterError::NotFound(format!(
            "no outbox row {outbox_id}: nothing to act on, and it is not dead-lettered"
        )));
    };
    if let Some(at) = delivered {
        return Err(DeadLetterError::Refused(format!(
            "outbox row {outbox_id} ({kind}) is not dead-lettered: it was delivered at {at}"
        )));
    }
    let Some(dead_lettered_at) = dead else {
        return Err(DeadLetterError::Refused(format!(
            "outbox row {outbox_id} ({kind}) is not dead-lettered: it is pending, and the \
             relay still owes it a publish"
        )));
    };
    // A RESOLUTION IS FINAL (review L3). It is the record of a person's
    // decision that this fact stays off the bus; a later redelivery
    // would contradict the record rather than amend it, so there is no
    // un-resolve. A resolution made in error is answered by a new fact,
    // not by rewriting this row.
    if let Some(at) = resolved {
        return Err(DeadLetterError::Refused(format!(
            "outbox row {outbox_id} ({kind}) was dead-lettered and then resolved at {at}: \
             a resolution is final, so it is neither redelivered nor resolved again"
        )));
    }
    let event = Event {
        id: event_id,
        timestamp,
        source: source.clone(),
        kind: kind.clone(),
        payload,
    };
    let payload_bytes = serde_json::to_vec(&event).map_err(storage)?.len();
    Ok(DeadLetter {
        outbox_id,
        event_id,
        event_kind: kind,
        event_source: source,
        payload_bytes,
        dead_lettered_at,
        dead_letter_reason: reason.unwrap_or_default(),
        overtaken_by: None,
    })
}

/// The act's payload: the letter's identity, never its payload.
fn act_payload(letter: &DeadLetter) -> serde_json::Value {
    serde_json::json!({
        "outbox_id": letter.outbox_id,
        "event_id": letter.event_id.to_string(),
        "event_kind": letter.event_kind,
        "event_source": letter.event_source,
        "payload_bytes": letter.payload_bytes,
        "dead_lettered_at": letter.dead_lettered_at.to_rfc3339(),
        "dead_letter_reason": letter.dead_letter_reason,
    })
}

/// How many rows after `outbox_id` had already been delivered — the
/// events a redelivered row now arrives behind. Counted from the rows
/// the outbox still holds, so for a dead letter older than the
/// retention window it is a LOWER bound: delivered rows past the window
/// have been deleted (backlog eec0c1f3), each recorded by an
/// `events.outbox.pruned` fact.
async fn overtaken_by(
    tx: &mut Transaction<'_, Postgres>,
    outbox_id: i64,
) -> Result<i64, DeadLetterError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE id > $1 AND delivered_at IS NOT NULL",
    )
    .bind(outbox_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)
}

/// Put a dead letter back on the relay's queue once the cause of its
/// refusal is fixed (backlog e22b692e). One transaction clears
/// `dead_lettered_at` / `dead_letter_reason` — the relay's pending
/// predicate then selects the row again, and its audit insert is a
/// no-op because phase 1 committed that row before the first attempt —
/// and stages [`REDELIVERED_KIND`] signed by `actor`, which keeps the
/// refusal the row gives up.
///
/// Refused, changing nothing: a row that is not an open dead letter,
/// and a row over [`MAX_STAGED_EVENT_BYTES`] — the bound
/// `record_event_in_tx` applies to a new event, because the bus would
/// refuse this one again and the relay would set it aside again. The
/// bound is the only cause this can judge from the row; a refusal for
/// another reason (an invalid subject) is redelivered on the operator's
/// word, and if the cause is not fixed the relay dead-letters it again,
/// loudly, as it did the first time.
///
/// REDELIVERY IS OUT-OF-ORDER DELIVERY (review M2). The relay publishes
/// in outbox order, and every later row it delivered while this one
/// sat aside has reached subscribers first. The count is taken inside
/// this transaction and rides on the fact as `overtaken_by`, so a
/// consumer's author reading the log can tell how far behind its
/// neighbours this event arrived.
pub async fn redeliver_dead_letter(
    pool: &PgPool,
    outbox_id: i64,
    actor: ActorId,
) -> Result<DeadLetter, DeadLetterError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    let mut letter = claim_dead_letter(&mut tx, outbox_id).await?;
    if letter.payload_bytes > MAX_STAGED_EVENT_BYTES {
        return Err(DeadLetterError::Refused(format!(
            "outbox row {outbox_id} ({}) encodes to {} bytes, over the {MAX_STAGED_EVENT_BYTES}-byte \
             staging bound: the bus would refuse it again, so it stays dead-lettered. The fact \
             is already in audit_log: resolve it with a reason, or raise the bound together \
             with the server's max_payload and redeliver then",
            letter.event_kind, letter.payload_bytes
        )));
    }
    letter.overtaken_by = Some(overtaken_by(&mut tx, outbox_id).await?);
    sqlx::query(
        "UPDATE event_outbox SET dead_lettered_at = NULL, dead_letter_reason = NULL \
         WHERE id = $1",
    )
    .bind(outbox_id)
    .execute(&mut *tx)
    .await
    .map_err(storage)?;
    let mut payload = act_payload(&letter);
    payload["overtaken_by"] = serde_json::json!(letter.overtaken_by);
    let act = EventStamp::new("events", actor).event(REDELIVERED_KIND, payload);
    record_event_in_tx(&mut tx, &act)
        .await
        .map_err(DeadLetterError::Storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(letter)
}

/// Close a dead letter without redelivering it: the fact stays in
/// audit_log and off the bus BY DECISION (backlog e22b692e). One
/// transaction stamps `dead_letter_resolved_at` and
/// `dead_letter_resolution` — the row stays dead-lettered and is never
/// marked delivered — and stages [`RESOLVED_KIND`] signed by `actor`
/// with `reason`. A blank reason is refused: the reason is the record
/// of the decision. Refused the same way as a redelivery for a row
/// that is not an open dead letter. A RESOLUTION IS FINAL: a resolved
/// row is refused by both arms from then on (see `claim_dead_letter`).
pub async fn resolve_dead_letter(
    pool: &PgPool,
    outbox_id: i64,
    reason: &str,
    actor: ActorId,
) -> Result<DeadLetter, DeadLetterError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(DeadLetterError::Invalid(format!(
            "resolving outbox row {outbox_id} needs a reason: it is the only record of why \
             the fact stays off the bus"
        )));
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    let letter = claim_dead_letter(&mut tx, outbox_id).await?;
    let stamp = EventStamp::new("events", actor);
    sqlx::query(
        "UPDATE event_outbox SET dead_letter_resolved_at = $2, dead_letter_resolution = $3 \
         WHERE id = $1",
    )
    .bind(outbox_id)
    .bind(stamp.timestamp)
    .bind(reason)
    .execute(&mut *tx)
    .await
    .map_err(storage)?;
    let mut payload = act_payload(&letter);
    payload["resolution"] = serde_json::Value::String(reason.to_string());
    record_event_in_tx(&mut tx, &stamp.event(RESOLVED_KIND, payload))
        .await
        .map_err(DeadLetterError::Storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(letter)
}

/// The fact one retention batch stages, in the same transaction as the
/// rows it deleted (backlog eec0c1f3).
pub const PRUNED_KIND: &str = "events.outbox.pruned";

/// How long a DELIVERED outbox row is kept before retention deletes it:
/// seven days. The relay delivers in well under a minute (p95 0.63 s,
/// max 39 s over 30 days, measured 2026-10-01), so a week is a wide
/// margin for reading a recent delivery off the outbox — `relay_lag`'s
/// windows, a dead letter's neighbours — and the copy holds nothing
/// `audit_log` does not.
pub const DEFAULT_OUTBOX_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

/// Rows one retention batch deletes, in one short transaction.
pub const DEFAULT_PRUNE_BATCH: i64 = 5_000;

/// Rows one retention pass deletes at most. Bounds the WAL a single
/// pass writes on a volume that has already filled once: the first
/// pass on the live outbox owed ~455k rows (measured 2026-10-01), so it
/// is worked off over a few hourly passes rather than in one burst,
/// while a steady hour delivers ~4k rows — far under the cap.
pub const DEFAULT_PRUNE_MAX_ROWS: u64 = 100_000;

/// What one [`prune_delivered_outbox`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PruneStats {
    /// Outbox rows deleted, over every batch of the pass.
    pub deleted: u64,
    /// Batches that deleted at least one row — one fact each.
    pub batches: u64,
}

/// Retention for the outbox: delete DELIVERED rows older than
/// `retention`, in batches of `batch`, at most `max_rows` per pass
/// (backlog eec0c1f3, incident d3c0a67c).
///
/// WHY. Nothing deleted an outbox row: the relay stamps `delivered_at`
/// and the row stayed, so every event was stored twice for good. On
/// 2026-10-01, the day the SoR's Postgres volume filled, the outbox
/// held 1,030,520 delivered rows — exactly `audit_log`'s count. A
/// delivered row's job is done: its fact is in `audit_log`, the system
/// of record, and on the bus.
///
/// WHAT IS NEVER DELETED. A row is deleted only when all three hold:
/// - `delivered_at` is set — a pending row is still owed a move, and a
///   dead letter is never stamped delivered (it keeps its payload for
///   `boss events redeliver`), so neither is ever touched;
/// - `delivered_at` is older than `retention`, measured back from the
///   pass's record stamp — wall time, like the database `NOW()` that
///   stamped delivery; the two clocks agree to seconds, against a
///   window of days;
/// - `audit_log` holds the same FACT: a row with its `event_id` AND its
///   `timestamp`, `source`, `kind` and `payload`, the five columns the
///   relay copies verbatim. The relay commits the audit row before it
///   stamps delivery, so this always holds for a relayed row; it is
///   checked anyway, because the deletion is only safe when the system
///   of record is PROVEN to hold the fact, not assumed to. The id alone
///   is not that proof (review afdc2d5d, N1): the relay's audit insert
///   skips an id audit_log already holds, so a second, different event
///   staged under a reused id after the first copy was pruned would be
///   stamped delivered with no audit row of its own — and deleted, had
///   the guard matched on the id. Unreachable while every id is a fresh
///   v4; the guard does not lean on that.
///
/// HOW. Each batch is one short transaction: `DELETE … RETURNING` on at
/// most `batch` rows chosen in id order with `FOR UPDATE SKIP LOCKED`
/// (row locks only — no table lock, no rewrite; a row another session
/// holds is left for the next pass), and the batch's
/// [`PRUNED_KIND`] fact staged on the outbox in the same transaction,
/// so a deletion and its record commit together or not at all. The
/// walk is keyset (`id > last deleted`) and bounded above by the first
/// row STAGED inside the window: a row staged after the cutoff cannot
/// have been delivered before it, so the scan never reads the retained
/// week. That bound is by staging order, which matches id order only to
/// within a transaction's length, so a row at the edge can wait one
/// more pass; it is never deleted early.
///
/// Postgres reuses the space the deleted rows held for new outbox rows
/// once (auto)vacuum has passed; it does not hand it back to the
/// filesystem — only `VACUUM FULL` (a table rewrite under an exclusive
/// lock) shrinks the file.
pub async fn prune_delivered_outbox(
    pool: &PgPool,
    retention: std::time::Duration,
    batch: i64,
    max_rows: u64,
) -> Result<PruneStats, String> {
    // One wall instant for the whole pass, minted by the record stamp
    // (the sanctioned wall source), so every batch's fact names the
    // same cutoff and carries the instant it was judged at.
    let stamp = EventStamp::new("events", ActorId::automation("event-relay"));
    let window = chrono::Duration::from_std(retention)
        .map_err(|e| format!("outbox retention {retention:?} out of range: {e}"))?;
    let cutoff: DateTime<Utc> = stamp.timestamp - window;
    // The first row staged inside the window — the walk's upper bound.
    // None: nothing staged since the cutoff, so the walk has no bound.
    let upper: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM event_outbox WHERE created_at >= $1 ORDER BY id LIMIT 1",
    )
    .bind(cutoff)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let upper = upper.unwrap_or(i64::MAX);

    let mut stats = PruneStats::default();
    let mut after: i64 = 0;
    while stats.deleted < max_rows {
        let limit = batch.min(i64::try_from(max_rows - stats.deleted).unwrap_or(i64::MAX));
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let ids: Vec<i64> = sqlx::query_scalar(
            "WITH doomed AS ( \
                 SELECT o.id FROM event_outbox o \
                  WHERE o.id > $1 AND o.id < $2 \
                    AND o.delivered_at IS NOT NULL AND o.delivered_at < $3 \
                    AND EXISTS (SELECT 1 FROM audit_log a \
                                 WHERE a.event_id = o.event_id \
                                   AND a.timestamp = o.timestamp \
                                   AND a.source = o.source \
                                   AND a.kind = o.kind \
                                   AND a.payload = o.payload) \
                  ORDER BY o.id \
                  LIMIT $4 \
                  FOR UPDATE SKIP LOCKED) \
             DELETE FROM event_outbox o USING doomed d WHERE o.id = d.id \
             RETURNING o.id",
        )
        .bind(after)
        .bind(upper)
        .bind(cutoff)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let (Some(&first), Some(&last)) = (ids.iter().min(), ids.iter().max()) else {
            // Nothing left below the bound; drop the empty transaction.
            break;
        };
        let deleted = ids.len() as u64;
        let fact = stamp.event(
            PRUNED_KIND,
            serde_json::json!({
                "deleted": deleted,
                "first_outbox_id": first,
                "last_outbox_id": last,
                "delivered_before": cutoff.to_rfc3339(),
                "retention_hours": retention.as_secs() / 3600,
            }),
        );
        record_event_in_tx(&mut tx, &fact).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        stats.deleted += deleted;
        stats.batches += 1;
        after = last;
        if (ids.len() as i64) < limit {
            // A short batch reached the bound: the pass is done.
            break;
        }
    }
    Ok(stats)
}

type OutboxRow = (i64, Uuid, DateTime<Utc>, String, String, serde_json::Value);

/// Drain one batch of pending rows through audit_log and the bus.
/// Returns Ok even when the bus is down — the undelivered rows simply
/// stay pending and the next drain retries them; only storage errors
/// are `Err`.
pub async fn drain_outbox_once(
    pool: &PgPool,
    bus: &Arc<dyn EventBus>,
    batch: i64,
) -> Result<DrainStats, String> {
    // Phase 1 — claim + audit-insert, one short transaction. FOR
    // UPDATE SKIP LOCKED lets a second relay instance (or the
    // epoch-restart TRUNCATE, which queues behind the row locks)
    // coexist without double-processing.
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT id, event_id, timestamp, source, kind, payload \
         FROM event_outbox \
         WHERE delivered_at IS NULL AND dead_lettered_at IS NULL \
         ORDER BY id \
         LIMIT $1 \
         FOR UPDATE SKIP LOCKED",
    )
    .bind(batch)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        // Nothing to do; drop the empty tx.
        return Ok(DrainStats::default());
    }

    let mut audit_inserted = 0u64;
    for (_, event_id, timestamp, source, kind, payload) in &rows {
        let res = sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             SELECT $1, $2, $3, $4, $5 \
             WHERE NOT EXISTS (SELECT 1 FROM audit_log WHERE event_id = $1)",
        )
        .bind(event_id)
        .bind(timestamp)
        .bind(source)
        .bind(kind)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        audit_inserted += res.rows_affected();
    }
    tx.commit().await.map_err(|e| e.to_string())?;

    // Phase 2 — publish + stamp, per row, in order. A transport
    // failure stops the batch so order is preserved across the retry;
    // a refusal dead-letters the row and the batch goes on.
    let mut delivered = 0u64;
    let mut dead_lettered = 0u64;
    for (id, event_id, timestamp, source, kind, payload) in rows {
        let event = Event {
            id: event_id,
            timestamp,
            source,
            kind,
            payload,
        };
        let event_kind = event.kind.clone();
        let event_source = event.source.clone();
        let payload_bytes = serde_json::to_vec(&event).map(|v| v.len()).unwrap_or(0);
        match bus.publish(event).await {
            Ok(()) => {
                sqlx::query("UPDATE event_outbox SET delivered_at = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                delivered += 1;
            }
            Err(EventBusError::Refused(reason)) => {
                tracing::error!(
                    outbox_id = id,
                    %event_id,
                    kind = %event_kind,
                    payload_bytes,
                    reason = %reason,
                    "bus REFUSED this event — dead-lettering the row (its audit row is \
                     already committed) and continuing the batch"
                );
                let refused = Refused {
                    outbox_id: id,
                    event_id,
                    kind: &event_kind,
                    source: &event_source,
                    payload_bytes,
                    reason: &reason,
                };
                if dead_letter(pool, &refused).await? {
                    dead_lettered += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    outbox_id = id,
                    kind = %event_kind,
                    error = %e,
                    "bus publish failed — row stays pending; batch stopped to preserve order"
                );
                break;
            }
        }
    }

    Ok(DrainStats {
        delivered,
        dead_lettered,
        audit_inserted,
    })
}

/// One row the bus refused, as the dead letter and its alarm name it.
struct Refused<'a> {
    outbox_id: i64,
    event_id: Uuid,
    kind: &'a str,
    source: &'a str,
    payload_bytes: usize,
    reason: &'a str,
}

/// Set a refused row aside and stage the alarm that names it, in ONE
/// transaction: a dead letter without its alarm would be a silent
/// discard, an alarm without its dead letter a false one. Guarded on
/// the row still being pending, so a re-run (a second relay, a retry
/// after a storage error) sets it aside once and alarms once — `false`
/// when another drain already did. The alarm carries the refused
/// event's identity and size, never its payload: the payload is what
/// the bus refused, and it stays readable on the outbox row. The row's
/// `dead_lettered_at` IS the alarm's timestamp — one instant, minted
/// by the stamp, so the row and the record cannot disagree about when.
async fn dead_letter(pool: &PgPool, r: &Refused<'_>) -> Result<bool, String> {
    let stamp = EventStamp::new("events", ActorId::automation("event-relay"));
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let marked = sqlx::query(
        "UPDATE event_outbox SET dead_lettered_at = $3, dead_letter_reason = $2 \
         WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL",
    )
    .bind(r.outbox_id)
    .bind(r.reason)
    .bind(stamp.timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected();
    if marked == 0 {
        return Ok(false);
    }
    let alarm = stamp.event(
        DEAD_LETTERED_KIND,
        serde_json::json!({
            "outbox_id": r.outbox_id,
            "event_id": r.event_id.to_string(),
            "event_kind": r.kind,
            "event_source": r.source,
            "payload_bytes": r.payload_bytes,
            "reason": r.reason,
        }),
    );
    record_event_in_tx(&mut tx, &alarm).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(true)
}

/// Pg-backed [`boss_core::port::EventRecorder`] — records each event
/// on the transactional outbox in a small transaction of its own.
/// The record-only sibling of `record_event_in_tx` for components
/// whose events have no accompanying row write (cybernetics
/// telemetry).
pub struct PgOutboxRecorder {
    pool: sqlx::PgPool,
}

impl PgOutboxRecorder {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// This pool's outbox as the port, the shape every service binary
    /// hands its machine gate (`boss_core::machine_gate::mount`), so the
    /// gate's would-refuse facts reach the audit log (design 21946380).
    pub fn shared(pool: &sqlx::PgPool) -> Arc<dyn boss_core::port::EventRecorder> {
        Arc::new(Self::new(pool.clone()))
    }
}

#[async_trait::async_trait]
impl boss_core::port::EventRecorder for PgOutboxRecorder {
    async fn record(&self, event: &Event) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        record_event_in_tx(&mut tx, event).await?;
        tx.commit().await.map_err(|e| e.to_string())
    }
}
