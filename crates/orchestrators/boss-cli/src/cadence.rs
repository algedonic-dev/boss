//! `boss train cadence` — the conductor's cadence as protocol data.
//!
//! The train's scheduling knowledge used to live in two systemd
//! timers (06:00/18:00 boarding, 10-minute reconcile) — outside the
//! system, invisible to the log, changeable only by an operator with
//! sudo. Per docs/design/protocol-cadence.md (David, 2026-08-12,
//! bacca14e: "We want every protocol internalized so we can measure,
//! experiment, and update"), the schedule is now rows in the
//! `cadence_rules` registry (114-cadence-rules.sql): each rule names
//! the `boss train` verb it fires, its basis — `wall` (interval),
//! `clock` (times-of-day), `queue-depth` (parked ready cars) — and
//! the basis' parameters. This loop is the executor: each tick it
//! reads boss-clock time (never wall-clock — the no-wallclock lint's
//! invariant), evaluates the active rules, claims a deterministic
//! firing row (`cadence:<name>:<window-stamp>`), and runs the verb
//! as a child of this same binary. systemd is demoted to what an OS
//! is for: keeping this process alive (infra/train/boss-train.service).
//!
//! Exactly-once, restated as data: the firing id is a pure function
//! of (rule, window), so a re-evaluated tick, a restarted loop, or a
//! second cadence instance all compute the same id and the
//! `cadence_firings` primary key dedupes the claim. Catch-up after
//! downtime fires at most the single most-recent missed window per
//! rule — a deliberate no-thundering-backfill choice matching the
//! conductor's one-window-at-a-time cadence (protocol-cadence Q3).
//! An in-flight verb cannot be double-fired: the loop runs verbs to
//! completion before the next tick, and a manually-started conductor
//! holds the flock, which makes the child exit clean.

use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use boss_clock_client::{ClockClient, ReqwestClockClient};
use chrono::{DateTime, Duration, NaiveTime, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};

use crate::train;

/// The `boss train` verbs a cadence rule may fire — the same set the
/// CLI exposes. Pinned here so a hand-edited registry row cannot make
/// the loop spawn arbitrary arguments.
const VERBS: &[&str] = &["preflight", "reconcile", "board", "run"];

fn log(msg: impl std::fmt::Display) {
    println!("cadence: {msg}");
}

// ---------------------------------------------------------------------------
// Rules — the registry rows, typed.
// ---------------------------------------------------------------------------

/// One active cadence rule: fire `verb` on `basis`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CadenceRule {
    pub name: String,
    /// A `boss train` verb: preflight | reconcile | board | run.
    pub verb: String,
    pub basis: Basis,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Basis {
    /// Fire once per `every_minutes` bucket, buckets anchored at
    /// midnight UTC of the current boss-clock day.
    Wall { every_minutes: u32 },
    /// Fire once per time-of-day window (UTC), e.g. 06:00 and 18:00.
    Clock { at: Vec<NaiveTime> },
    /// Fire when the dock holds at least `min_depth` parked ready
    /// cars, at most once per `cooldown_minutes`.
    QueueDepth {
        min_depth: u32,
        cooldown_minutes: u32,
    },
}

impl Basis {
    fn as_str(&self) -> &'static str {
        match self {
            Basis::Wall { .. } => "wall",
            Basis::Clock { .. } => "clock",
            Basis::QueueDepth { .. } => "queue-depth",
        }
    }
}

/// The most recent recorded firing of a rule — what evaluation
/// compares the candidate window against.
#[derive(Debug, Clone)]
pub(crate) struct LastFiring {
    pub firing_id: String,
    pub fired_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Evaluation — pure functions of (rule, boss-clock now, last firing,
// dock depth). No I/O; the whole cadence semantic is testable here.
// ---------------------------------------------------------------------------

/// Minute-resolution window stamp — the deterministic half of a
/// firing id. Two evaluations of the same window always agree.
pub(crate) fn window_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%MZ").to_string()
}

/// `cadence:<rule>:<window-stamp>` — the exactly-once key
/// (protocol-cadence Q3).
pub(crate) fn firing_id(rule: &str, window: DateTime<Utc>) -> String {
    format!("cadence:{rule}:{}", window_stamp(window))
}

/// Evaluate one rule against boss-clock `now`: `Some(window)` means
/// the rule is due for that window and no firing for it is recorded
/// yet. `dock_depth` is the parked-ready-car count when the tick
/// probed it (`None` = not probed or probe failed — queue-depth
/// rules hold rather than fire blind).
pub(crate) fn due_window(
    rule: &CadenceRule,
    now: DateTime<Utc>,
    last: Option<&LastFiring>,
    dock_depth: Option<u32>,
) -> Option<DateTime<Utc>> {
    let window = match &rule.basis {
        Basis::Wall { every_minutes } => {
            // The bucket of `now` on the interval grid anchored at
            // midnight UTC of the boss-clock day. Older elapsed
            // buckets never fire — catch-up is one window at most.
            let every = i64::from(*every_minutes);
            if every == 0 {
                return None;
            }
            let midnight = now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();
            let elapsed_min = (now - midnight).num_minutes();
            midnight + Duration::minutes((elapsed_min / every) * every)
        }
        Basis::Clock { at } => {
            // The most recent elapsed time-of-day window: today's
            // where already reached, else yesterday's. Anything
            // older was missed and stays missed — no backfill.
            let today = now.date_naive();
            let yesterday = today.pred_opt()?;
            at.iter()
                .map(|t| {
                    let w = today.and_time(*t).and_utc();
                    if w <= now {
                        w
                    } else {
                        yesterday.and_time(*t).and_utc()
                    }
                })
                .filter(|w| *w <= now)
                .max()?
        }
        Basis::QueueDepth {
            min_depth,
            cooldown_minutes,
        } => {
            // Hold below the threshold, and hold when depth is
            // unknown — a failed probe must never board a train.
            if dock_depth? < *min_depth {
                return None;
            }
            // The cooldown is the re-fire guard: a dock that stays
            // deep (cars skipped on conflicts) re-fires at most once
            // per cooldown instead of every tick.
            if let Some(last) = last
                && now - last.fired_at < Duration::minutes(i64::from(*cooldown_minutes))
            {
                return None;
            }
            // The window is the evaluation minute — deterministic,
            // so two instances in the same minute claim one id.
            now.with_second(0)?.with_nanosecond(0)?
        }
    };
    // Fired already? The recorded id for this rule + window says so —
    // across ticks, restarts, and instances alike.
    if last.is_some_and(|l| l.firing_id == firing_id(&rule.name, window)) {
        return None;
    }
    Some(window)
}

/// The soonest scheduled window strictly after `now` across the
/// rules — the heartbeat's "next due". Wall rules promise their next
/// grid bucket; clock rules today's next time-of-day or tomorrow's
/// first; queue-depth rules fire on dock state, not on time, and
/// promise nothing. None when no rule carries a schedule.
pub(crate) fn next_due(rules: &[CadenceRule], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    rules
        .iter()
        .filter_map(|rule| match &rule.basis {
            Basis::Wall { every_minutes } => {
                let every = i64::from(*every_minutes);
                if every == 0 {
                    return None;
                }
                let midnight = now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();
                let elapsed_min = (now - midnight).num_minutes();
                Some(midnight + Duration::minutes((elapsed_min / every + 1) * every))
            }
            Basis::Clock { at } => {
                let today = now.date_naive();
                let tomorrow = today.succ_opt()?;
                at.iter()
                    .map(|t| {
                        let w = today.and_time(*t).and_utc();
                        if w > now {
                            w
                        } else {
                            tomorrow.and_time(*t).and_utc()
                        }
                    })
                    .min()
            }
            Basis::QueueDepth { .. } => None,
        })
        .min()
}

/// Parse the registry's `at_times` JSONB (`["06:00","18:00"]`) into
/// times-of-day. Rejects empty lists and non-"HH:MM" entries loudly —
/// a rule that cannot be read must be skipped visibly, not fire never
/// and silently.
pub(crate) fn parse_at_times(v: &Value) -> Result<Vec<NaiveTime>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("at_times must be a JSON array"))?;
    if arr.is_empty() {
        bail!("at_times must name at least one time");
    }
    arr.iter()
        .map(|e| {
            let s = e
                .as_str()
                .ok_or_else(|| anyhow!("at_times entries are \"HH:MM\" strings"))?;
            NaiveTime::parse_from_str(s, "%H:%M")
                .with_context(|| format!("parsing at_times entry {s:?}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Registry + measurement I/O — thin adapters over cadence_rules /
// cadence_firings. Timestamps are always bound from boss-clock time,
// never SQL NOW().
// ---------------------------------------------------------------------------

fn rule_from_row(row: &PgRow) -> Result<CadenceRule> {
    let name: String = row.try_get("name")?;
    let verb: String = row.try_get("verb")?;
    if !VERBS.contains(&verb.as_str()) {
        bail!("unknown verb {verb:?}");
    }
    let basis: String = row.try_get("basis")?;
    let positive = |field: &str| -> Result<u32> {
        let v: Option<i32> = row.try_get(field)?;
        v.ok_or_else(|| anyhow!("{field} is required for basis {basis:?}"))?
            .try_into()
            .with_context(|| format!("{field} must be positive"))
    };
    let basis = match basis.as_str() {
        "wall" => Basis::Wall {
            every_minutes: positive("every_minutes")?,
        },
        "clock" => {
            let at: Option<Value> = row.try_get("at_times")?;
            let at = at.ok_or_else(|| anyhow!("at_times is required for basis \"clock\""))?;
            Basis::Clock {
                at: parse_at_times(&at)?,
            }
        }
        "queue-depth" => Basis::QueueDepth {
            min_depth: positive("min_dock_depth")?,
            cooldown_minutes: positive("cooldown_minutes")?,
        },
        other => bail!("unknown basis {other:?}"),
    };
    Ok(CadenceRule { name, verb, basis })
}

async fn load_rules(pool: &PgPool) -> Result<Vec<CadenceRule>> {
    let rows = sqlx::query(
        "SELECT name, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes \
         FROM cadence_rules WHERE status = 'active' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .context("loading cadence_rules")?;
    let mut out = Vec::new();
    for row in &rows {
        let name: String = row.try_get("name")?;
        match rule_from_row(row) {
            Ok(rule) => out.push(rule),
            // A malformed row is skipped LOUDLY every tick, not
            // dropped once at startup: the registry is editable data.
            Err(e) => log(format!("skipping unreadable rule {name}: {e:#}")),
        }
    }
    Ok(out)
}

async fn last_firing(pool: &PgPool, rule: &str) -> Result<Option<LastFiring>> {
    let row = sqlx::query(
        "SELECT firing_id, fired_at FROM cadence_firings WHERE rule_name = $1 \
         ORDER BY fired_at DESC LIMIT 1",
    )
    .bind(rule)
    .fetch_optional(pool)
    .await
    .context("reading the last cadence firing")?;
    match row {
        None => Ok(None),
        Some(r) => Ok(Some(LastFiring {
            firing_id: r.try_get("firing_id")?,
            fired_at: r.try_get("fired_at")?,
        })),
    }
}

/// Claim a firing id. `false` = the window was already claimed (a
/// concurrent instance, or a re-run after a crash mid-verb) — the
/// caller must not run the verb.
async fn claim_firing(
    pool: &PgPool,
    id: &str,
    rule: &CadenceRule,
    now: DateTime<Utc>,
    dock_depth: Option<u32>,
) -> Result<bool> {
    let detail = match (&rule.basis, dock_depth) {
        (Basis::QueueDepth { .. }, Some(d)) => json!({"dock_depth": d}),
        _ => json!({}),
    };
    let res = sqlx::query(
        "INSERT INTO cadence_firings (firing_id, rule_name, verb, basis, fired_at, detail) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (firing_id) DO NOTHING",
    )
    .bind(id)
    .bind(&rule.name)
    .bind(&rule.verb)
    .bind(rule.basis.as_str())
    .bind(now) // boss-clock time, bound — never the DB's wallclock
    .bind(detail)
    .execute(pool)
    .await
    .context("claiming the cadence firing")?;
    Ok(res.rows_affected() == 1)
}

/// Merge the verb's outcome into the firing row — the runtime and
/// exit code are what make "what did the cadence cost" a query.
async fn record_outcome(pool: &PgPool, id: &str, rc: i32, runtime_secs: u64) -> Result<()> {
    sqlx::query("UPDATE cadence_firings SET detail = detail || $2 WHERE firing_id = $1")
        .bind(id)
        .bind(json!({"rc": rc, "runtime_secs": runtime_secs}))
        .execute(pool)
        .await
        .context("recording the cadence outcome")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The dock probe — parked ready cars, counted from the jobs API with
// the same predicate boarding itself collects by (train::parked_ready).
// ---------------------------------------------------------------------------

async fn get_json(http: &reqwest::Client, base: &str, path: &str) -> Result<Option<Value>> {
    let resp = http
        .get(format!("{base}{path}"))
        .header("content-type", "application/json")
        .header("x-boss-user", train::boss_user())
        .send()
        .await
        .with_context(|| format!("GET {path}"))?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        bail!("GET {path}: HTTP {status}: {}", body.trim());
    }
    if body.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(
            serde_json::from_str(&body).with_context(|| format!("parsing GET {path} response"))?,
        ))
    }
}

async fn probe_dock_depth() -> Result<u32> {
    let jobs = train::env_or("BOSS_JOBS_URL", "http://127.0.0.1:7900");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let listed = train::rows(
        get_json(
            &http,
            &jobs,
            "/api/jobs?kind=ship-a-change&status=open&limit=100",
        )
        .await?,
    )?;
    let mut depth = 0u32;
    for j in listed {
        let Some(id) = j.get("id").and_then(Value::as_str) else {
            continue;
        };
        let job = get_json(&http, &jobs, &format!("/api/jobs/{id}"))
            .await?
            .ok_or_else(|| anyhow!("job {id} came back empty"))?;
        if train::parked_ready(&job) {
            depth += 1;
        }
    }
    Ok(depth)
}

// ---------------------------------------------------------------------------
// The executor — evaluate, claim, run the verb, record what happened.
// ---------------------------------------------------------------------------

/// Run one `boss train <verb>` as a child of this same binary and
/// return its exit code. The conductor's own flock makes an overlap
/// with a manually-started run exit clean, and a preflight exit 3
/// lands here as data instead of killing the loop.
fn run_verb(verb: &str) -> Result<i32> {
    if !VERBS.contains(&verb) {
        bail!("refusing to run unknown verb {verb:?}");
    }
    let exe = std::env::current_exe().context("resolving the boss binary path")?;
    let status = Command::new(exe)
        .args(["train", verb])
        .status()
        .with_context(|| format!("spawning boss train {verb}"))?;
    Ok(status.code().unwrap_or(-1))
}

/// What a tick saw — fodder for the heartbeat line.
struct TickSummary {
    rules: usize,
    next_due: Option<DateTime<Utc>>,
}

async fn tick(pool: &PgPool, clock: &dyn ClockClient, dry: bool) -> Result<TickSummary> {
    // Boss-clock time is the only "now" in this loop (clock-as-service;
    // the no-wallclock invariant). In the wall-mode production deploy
    // it IS wall time — served by the one authoritative clock.
    let now = clock.now().await.now;
    let rules = load_rules(pool).await?;
    // One dock probe per tick, and only when a queue-depth rule is
    // active. A failed probe holds those rules; it never fires blind.
    let mut dock_depth: Option<u32> = None;
    if rules
        .iter()
        .any(|r| matches!(r.basis, Basis::QueueDepth { .. }))
    {
        match probe_dock_depth().await {
            Ok(d) => dock_depth = Some(d),
            Err(e) => log(format!("dock probe failed — queue-depth rules hold: {e:#}")),
        }
    }
    for rule in &rules {
        let last = last_firing(pool, &rule.name).await?;
        let Some(window) = due_window(rule, now, last.as_ref(), dock_depth) else {
            continue;
        };
        let id = firing_id(&rule.name, window);
        if dry {
            log(format!(
                "DRY: would fire {} ({id}) verb={}",
                rule.name, rule.verb
            ));
            continue;
        }
        if !claim_firing(pool, &id, rule, now, dock_depth).await? {
            continue; // someone else holds this window
        }
        let depth_note = match (&rule.basis, dock_depth) {
            (Basis::QueueDepth { .. }, Some(d)) => format!(" dock_depth={d}"),
            _ => String::new(),
        };
        log(format!(
            "fired {} ({id}) verb={} basis={}{depth_note}",
            rule.name,
            rule.verb,
            rule.basis.as_str()
        ));
        let started = Instant::now();
        let rc = run_verb(&rule.verb)?;
        let runtime_secs = started.elapsed().as_secs();
        record_outcome(pool, &id, rc, runtime_secs).await?;
        log(format!(
            "{} verb={} rc={rc} in {runtime_secs}s",
            rule.name, rule.verb
        ));
    }
    Ok(TickSummary {
        rules: rules.len(),
        next_due: next_due(&rules, now),
    })
}

/// The `boss train cadence` entry: the supervised loop
/// (infra/train/boss-train.service) or, with `once`, a single
/// evaluated tick for an operator or a test.
pub async fn run(once: bool, dry: bool) -> Result<()> {
    let pg_url = train::env_or("BOSS_POSTGRES_URL", "postgres://boss:boss@127.0.0.1/boss");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&pg_url)
        .await
        .context("connecting to Postgres for cadence_rules")?;
    let clock_url = train::env_or("BOSS_CLOCK_URL", &boss_ports::url("clock"));
    let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(clock_url.clone()));
    let tick_secs: u64 = train::env_or("BOSS_TRAIN_CADENCE_TICK_SECONDS", "60")
        .parse()
        .context("parsing BOSS_TRAIN_CADENCE_TICK_SECONDS")?;
    // The heartbeat cadence: one "alive" line every N ticks (~30 min
    // at the 60s default). Silence must be diagnosable — a hung loop
    // and a quiet loop cannot be allowed to look identical in the
    // journal; this makes "is it hung?" a one-line grep.
    let heartbeat_ticks = train::env_or("BOSS_TRAIN_HEARTBEAT_TICKS", "30")
        .parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .unwrap_or(30);
    log(format!(
        "loop starting — rules from cadence_rules, clock at {clock_url}, tick {tick_secs}s{}",
        if dry { ", DRY" } else { "" }
    ));
    let mut tick_n: u64 = 0;
    loop {
        tick_n += 1;
        let outcome = tick(&pool, clock.as_ref(), dry).await;
        if once {
            return outcome.map(|_| ());
        }
        match outcome {
            Err(e) => {
                // The loop survives a bad tick — supervision is systemd's
                // job, coordination is this loop's; a transient jobs-api
                // or Postgres outage must not kill the schedule.
                log(format!("tick failed: {e:#}"));
            }
            Ok(summary) if tick_n.is_multiple_of(heartbeat_ticks) => {
                let due = summary
                    .next_due
                    .map_or_else(|| "?".to_string(), window_stamp);
                log(format!(
                    "alive (tick {tick_n}, {} rules, next due {due})",
                    summary.rules
                ));
            }
            Ok(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(tick_secs)).await;
    }
}

// ---------------------------------------------------------------------------
// Tests — the cadence semantics, pinned before the implementation.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn wall_rule(every: u32) -> CadenceRule {
        CadenceRule {
            name: "train-reconcile".into(),
            verb: "reconcile".into(),
            basis: Basis::Wall {
                every_minutes: every,
            },
        }
    }

    fn clock_rule() -> CadenceRule {
        CadenceRule {
            name: "train-window".into(),
            verb: "run".into(),
            basis: Basis::Clock {
                at: vec![at(6, 0), at(18, 0)],
            },
        }
    }

    fn depth_rule(min: u32, cooldown: u32) -> CadenceRule {
        CadenceRule {
            name: "train-board-on-dock-depth".into(),
            verb: "board".into(),
            basis: Basis::QueueDepth {
                min_depth: min,
                cooldown_minutes: cooldown,
            },
        }
    }

    fn fired(rule: &CadenceRule, window: DateTime<Utc>) -> LastFiring {
        LastFiring {
            firing_id: firing_id(&rule.name, window),
            fired_at: window,
        }
    }

    // -- wall basis: interval buckets ------------------------------------

    #[test]
    fn wall_first_start_fires_current_bucket() {
        let rule = wall_rule(10);
        // 06:07 sits in the 06:00 bucket of the 10-minute grid.
        let now = utc(2026, 8, 12, 6, 7, 30);
        assert_eq!(
            due_window(&rule, now, None, None),
            Some(utc(2026, 8, 12, 6, 0, 0))
        );
    }

    #[test]
    fn wall_holds_within_a_fired_bucket() {
        let rule = wall_rule(10);
        let window = utc(2026, 8, 12, 6, 0, 0);
        let last = fired(&rule, window);
        // Re-evaluated later in the same bucket: idempotent, no re-fire.
        for min in [0u32, 3, 9] {
            let now = utc(2026, 8, 12, 6, min, 59);
            assert_eq!(due_window(&rule, now, Some(&last), None), None);
        }
    }

    #[test]
    fn wall_fires_the_next_bucket() {
        let rule = wall_rule(10);
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        let now = utc(2026, 8, 12, 6, 10, 0);
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 6, 10, 0))
        );
    }

    #[test]
    fn wall_downtime_catches_up_one_window_only() {
        let rule = wall_rule(10);
        // Last fired 06:00; the loop was down through 06:10..06:40.
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        let now = utc(2026, 8, 12, 6, 47, 12);
        // Only the CURRENT bucket fires — no thundering backfill.
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 6, 40, 0))
        );
    }

    // -- clock basis: times-of-day ---------------------------------------

    #[test]
    fn clock_fires_the_most_recent_elapsed_window_only() {
        let rule = clock_rule();
        // 19:00 with yesterday's 18:00 recorded: today's 06:00 was
        // missed too, but only today's 18:00 (the most recent) fires.
        let last = fired(&rule, utc(2026, 8, 11, 18, 0, 0));
        let now = utc(2026, 8, 12, 19, 0, 0);
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 18, 0, 0))
        );
    }

    #[test]
    fn clock_holds_between_windows_once_fired() {
        let rule = clock_rule();
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        // 17:59 — the most recent elapsed window is still 06:00.
        let now = utc(2026, 8, 12, 17, 59, 0);
        assert_eq!(due_window(&rule, now, Some(&last), None), None);
    }

    #[test]
    fn clock_reaches_back_across_midnight() {
        let rule = clock_rule();
        // 01:00 with nothing recorded: yesterday 18:00 is the most
        // recent elapsed window — fire it (Persistent=true semantics).
        let now = utc(2026, 8, 12, 1, 0, 0);
        assert_eq!(
            due_window(&rule, now, None, None),
            Some(utc(2026, 8, 11, 18, 0, 0))
        );
        // ... and once recorded, 01:00 holds.
        let last = fired(&rule, utc(2026, 8, 11, 18, 0, 0));
        assert_eq!(due_window(&rule, now, Some(&last), None), None);
    }

    #[test]
    fn clock_fires_exactly_at_the_window_instant() {
        let rule = clock_rule();
        let now = utc(2026, 8, 12, 6, 0, 0);
        assert_eq!(due_window(&rule, now, None, None), Some(now));
    }

    #[test]
    fn clock_with_no_times_never_fires() {
        let rule = CadenceRule {
            name: "empty".into(),
            verb: "run".into(),
            basis: Basis::Clock { at: vec![] },
        };
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 12, 0, 0), None, None),
            None
        );
    }

    // -- queue-depth basis: dock pressure --------------------------------

    #[test]
    fn queue_depth_threshold_edge() {
        let rule = depth_rule(4, 120);
        let now = utc(2026, 8, 12, 12, 0, 30);
        // Below threshold: hold. At and above: fire (window = the
        // evaluation minute — the id is still deterministic per minute).
        assert_eq!(due_window(&rule, now, None, Some(3)), None);
        assert_eq!(
            due_window(&rule, now, None, Some(4)),
            Some(utc(2026, 8, 12, 12, 0, 0))
        );
        assert_eq!(
            due_window(&rule, now, None, Some(9)),
            Some(utc(2026, 8, 12, 12, 0, 0))
        );
    }

    #[test]
    fn queue_depth_respects_the_cooldown() {
        let rule = depth_rule(4, 120);
        let last = fired(&rule, utc(2026, 8, 12, 11, 0, 0));
        // 30 minutes after a firing, a deep dock still holds...
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 11, 30, 0), Some(&last), Some(8)),
            None
        );
        // ... and fires again once the cooldown has fully elapsed.
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 13, 0, 0), Some(&last), Some(8)),
            Some(utc(2026, 8, 12, 13, 0, 0))
        );
    }

    #[test]
    fn queue_depth_never_fires_blind() {
        // Depth unknown (probe failed / not probed): hold, never fire.
        let rule = depth_rule(1, 1);
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 12, 0, 0), None, None),
            None
        );
    }

    // -- registry parsing --------------------------------------------------

    #[test]
    fn at_times_parse_and_reject() {
        assert_eq!(
            parse_at_times(&serde_json::json!(["06:00", "18:30"])).unwrap(),
            vec![at(6, 0), at(18, 30)]
        );
        for bad in [
            serde_json::json!([]),
            serde_json::json!(["6am"]),
            serde_json::json!([600]),
            serde_json::json!("06:00"),
        ] {
            assert!(parse_at_times(&bad).is_err(), "accepted {bad}");
        }
    }

    // -- the heartbeat's next-due -----------------------------------------
    //
    // One journal line every N ticks says the loop is alive and what
    // it is waiting for — a hung loop and a quiet loop must not look
    // identical. `next_due` is the schedule half of that line.

    #[test]
    fn next_due_is_the_soonest_scheduled_window() {
        let rules = vec![wall_rule(10), clock_rule()];
        // 06:07: the wall grid's next bucket (06:10) beats 18:00.
        assert_eq!(
            next_due(&rules, utc(2026, 8, 12, 6, 7, 30)),
            Some(utc(2026, 8, 12, 6, 10, 0))
        );
    }

    #[test]
    fn next_due_rolls_a_clock_rule_to_tomorrow() {
        // 19:00 — both of today's windows (06:00, 18:00) are behind;
        // the promise is tomorrow 06:00.
        assert_eq!(
            next_due(&[clock_rule()], utc(2026, 8, 12, 19, 0, 0)),
            Some(utc(2026, 8, 13, 6, 0, 0))
        );
    }

    #[test]
    fn queue_depth_rules_promise_no_window() {
        // Depth rules fire on dock state, not on time — a registry of
        // only depth rules gives the heartbeat nothing to promise.
        assert_eq!(
            next_due(&[depth_rule(3, 120)], utc(2026, 8, 12, 6, 0, 0)),
            None
        );
        assert_eq!(next_due(&[], utc(2026, 8, 12, 6, 0, 0)), None);
    }

    // -- firing ids -------------------------------------------------------

    #[test]
    fn firing_id_is_deterministic_per_window() {
        let w = utc(2026, 8, 12, 6, 0, 0);
        assert_eq!(firing_id("train-window", w), firing_id("train-window", w));
        assert_eq!(
            firing_id("train-window", w),
            "cadence:train-window:2026-08-12T06:00Z"
        );
        // Seconds within the minute collapse to one id.
        assert_eq!(
            firing_id("r", utc(2026, 8, 12, 6, 0, 59)),
            firing_id("r", utc(2026, 8, 12, 6, 0, 1))
        );
    }
}

// ---------------------------------------------------------------------------
// DB-backed tests — the registry seed and the exactly-once claim,
// pinned against real Postgres (boss_testing::TestDb).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use chrono::TimeZone;

    /// The 114 seed loads through the same reader the loop uses: the
    /// two retired timers as data, plus the queue-depth rule.
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_rules_load_and_parse() {
        let db = boss_testing::TestDb::new().await;
        let rules = load_rules(&db.pool).await.unwrap();
        let by_name = |n: &str| {
            rules
                .iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("seed rule {n} missing"))
        };
        let reconcile = by_name("train-reconcile");
        assert_eq!(reconcile.verb, "reconcile");
        assert_eq!(reconcile.basis, Basis::Wall { every_minutes: 10 });
        let window = by_name("train-window");
        assert_eq!(window.verb, "run");
        assert_eq!(
            window.basis,
            Basis::Clock {
                at: vec![
                    NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
                    NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
                ],
            }
        );
        let depth = by_name("train-board-on-dock-depth");
        assert_eq!(depth.verb, "board");
        assert_eq!(
            depth.basis,
            Basis::QueueDepth {
                min_depth: 4,
                cooldown_minutes: 120,
            }
        );
    }

    /// One window, one firing: the second claim of the same id loses,
    /// and the recorded firing holds the window on re-evaluation —
    /// the restart / second-instance idempotence contract end to end.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_window_claims_exactly_once() {
        let db = boss_testing::TestDb::new().await;
        let rule = CadenceRule {
            name: "train-window".into(),
            verb: "run".into(),
            basis: Basis::Clock {
                at: vec![NaiveTime::from_hms_opt(6, 0, 0).unwrap()],
            },
        };
        let now = Utc.with_ymd_and_hms(2026, 8, 12, 6, 0, 30).unwrap();
        let window = due_window(&rule, now, None, None).expect("window due");
        let id = firing_id(&rule.name, window);

        assert!(claim_firing(&db.pool, &id, &rule, now, None).await.unwrap());
        // A concurrent instance (or a restart mid-verb) computes the
        // same id and must lose the claim.
        assert!(!claim_firing(&db.pool, &id, &rule, now, None).await.unwrap());

        // The recorded firing is what evaluation sees next tick.
        let last = last_firing(&db.pool, &rule.name).await.unwrap().unwrap();
        assert_eq!(last.firing_id, id);
        assert_eq!(last.fired_at, now);
        assert_eq!(due_window(&rule, now, Some(&last), None), None);

        // The outcome merges into the claim's detail row.
        record_outcome(&db.pool, &id, 0, 42).await.unwrap();
        let detail: Value =
            sqlx::query_scalar("SELECT detail FROM cadence_firings WHERE firing_id = $1")
                .bind(&id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(detail.get("rc"), Some(&json!(0)));
        assert_eq!(detail.get("runtime_secs"), Some(&json!(42)));
    }

    /// The registry is append-only with one live row per name: a
    /// second 'active' version of a seeded rule must be refused by
    /// the partial unique index (supersede = retire + insert).
    #[tokio::test(flavor = "multi_thread")]
    async fn one_active_row_per_rule_name() {
        let db = boss_testing::TestDb::new().await;
        let dup = sqlx::query(
            "INSERT INTO cadence_rules (name, version, status, verb, basis, every_minutes) \
             VALUES ('train-reconcile', 2, 'active', 'reconcile', 'wall', 5)",
        )
        .execute(&db.pool)
        .await;
        assert!(dup.is_err(), "second active train-reconcile row accepted");
    }
}
