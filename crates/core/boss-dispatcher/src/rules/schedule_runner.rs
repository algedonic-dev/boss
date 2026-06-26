//! Clock-driven schedule runner — the timing-trigger counterpart of the
//! event-driven [`RulesRunner`](super::runner::RulesRunner).
//!
//! The event runner fires a rule's `do_steps` when a NATS event matches
//! its `on_event` topic. This runner fires a rule's `do_steps` when the
//! streaming clock crosses a sim-DAY boundary the rule's `schedule`
//! selects. Same handler-dispatch machinery (`handler::dispatch`); the
//! only difference is what decides "fire now" — a calendar cadence
//! instead of an inbound event.
//!
//! The design splits into three pure decisions (heavily unit-tested) and
//! one thin async shell:
//!
//! 1. [`super::registry::schedule_fires_on`] — does ONE schedule fire on
//!    ONE day? (calendar postponement; lives in registry.rs next to the
//!    cadence math.)
//! 2. [`advance_cursor`] — given the last-fired sim-day, an incoming
//!    `ClockNow`, and a catch-up cap, which days must fire now and what
//!    is the new cursor? (pause / restart / backward-jump / first-obs /
//!    cap edge cases live here.)
//! 3. [`run`] — the shell: fetch + cache calendars, load + persist the
//!    cursor, consume the clock stream, and for each day-to-fire dispatch
//!    every schedule rule's `do_steps`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar_client::CalendarClient;
use boss_clock_client::ClockNow;
use boss_core::calendar::BusinessCalendar;
use chrono::{Days, NaiveDate};
use futures::StreamExt;
use serde_json::json;
use tracing::{debug, info, warn};

use super::handler::{self, HandlerRegistry};
use super::registry::{MatchedInvocation, MatchedRule, Registry};

/// Catch-up cap: the maximum number of past sim-days the runner will fire
/// in a single clock observation. A cold start or a huge warp gap can
/// surface a multi-day (or multi-year) jump; without a cap that would
/// fire thousands of daily rules at once. We fire only the most recent
/// `N` days and report the rest as `skipped` (logged). 90 days covers a
/// quarter of catch-up — enough that a brief dispatcher outage during a
/// warp run still fires every monthly/quarterly schedule, while bounding
/// a pathological gap.
pub const DEFAULT_CATCHUP_CAP: u64 = 90;

/// The outcome of advancing the sim-day cursor against one `ClockNow`.
/// Pure data; [`advance_cursor`] computes it and the caller acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdvance {
    /// Sim-days that must fire now, oldest first. Empty when nothing
    /// advanced (paused / restart / first-observation / backward-jump /
    /// same-day).
    pub days_to_fire: Vec<NaiveDate>,
    /// The cursor to persist after handling this observation. `None`
    /// only in the degenerate case where there was no prior cursor AND
    /// the clock gave nothing usable (never happens for a normal
    /// `ClockNow`, which always carries a date).
    pub new_cursor: Option<NaiveDate>,
    /// Days dropped by the catch-up cap (older than the most recent
    /// `N`). Reported so the caller can log "skipped M days".
    pub skipped: u64,
}

/// THE sim-day cursor decision — pure, total, deterministic.
///
/// Given the last-processed sim-day `cursor`, an incoming `now`, and a
/// catch-up cap `cap`, decide which days to fire and the new cursor.
/// Rules (in order):
///
/// - **Paused / restart-in-progress** → fire nothing, cursor unchanged.
///   (A paused sim isn't advancing time; a mid-flight epoch restart is
///   trimming the log + rebuilding — firing into either is wrong.)
/// - **First observation** (`cursor == None`) → adopt today as the
///   baseline, fire nothing. We do NOT backfill history: the runner only
///   fires days it actually observes the clock cross, so a fresh deploy
///   doesn't dump a year of past schedules.
/// - **Backward jump** (`d < cursor`, e.g. an epoch restart reset the
///   clock to a past date) → reset the cursor to `d`, fire nothing. The
///   new epoch re-establishes its own baseline.
/// - **Same day** (`d == cursor`) → nothing.
/// - **Forward** (`d > cursor`) → fire the half-open range `(cursor, d]`.
///   If that exceeds `cap`, fire only the most recent `cap` days and
///   report the remainder as `skipped`. New cursor is `d`.
pub fn advance_cursor(cursor: Option<NaiveDate>, now: &ClockNow, cap: u64) -> CursorAdvance {
    // Frozen states: don't move the cursor, don't fire.
    if now.paused || now.restart_in_progress {
        return CursorAdvance {
            days_to_fire: vec![],
            new_cursor: cursor,
            skipped: 0,
        };
    }
    let d = now.now.date_naive();
    match cursor {
        // First observation: establish the baseline, fire nothing.
        None => CursorAdvance {
            days_to_fire: vec![],
            new_cursor: Some(d),
            skipped: 0,
        },
        Some(c) if d <= c => {
            // Same day OR a backward jump. Same-day is a no-op (cursor
            // unchanged); a backward jump resets the cursor to the new
            // (earlier) day so the next forward tick fires from there.
            CursorAdvance {
                days_to_fire: vec![],
                new_cursor: Some(d.min(c)),
                skipped: 0,
            }
        }
        Some(c) => {
            // Forward: fire (c, d]. Count = number of days strictly after
            // c through d inclusive.
            let total = (d - c).num_days().max(0) as u64;
            let (start_after, skipped) = if total > cap {
                // Fire only the most recent `cap` days: (d - cap, d].
                let first_fired = d - chrono::Duration::days(cap as i64);
                (first_fired, total - cap)
            } else {
                (c, 0)
            };
            // Walk forward from the day AFTER `start_after` through `d`.
            // The count is known: `d - start_after` days.
            let n_fire = (d - start_after).num_days().max(0);
            let mut days = Vec::with_capacity(n_fire as usize);
            let mut day = start_after;
            while let Some(next) = day.checked_add_days(Days::new(1)) {
                if next > d {
                    break;
                }
                day = next;
                days.push(day);
            }
            CursorAdvance {
                days_to_fire: days,
                new_cursor: Some(d),
                skipped,
            }
        }
    }
}

/// What the schedule runner needs to operate.
pub struct ScheduleRunner {
    pub registry: Registry,
    pub handlers: HandlerRegistry,
    /// Clock SSE base URL (e.g. `http://127.0.0.1:7060`).
    pub clock_url: String,
    /// Postgres pool — the single-row `dispatcher_clock_cursor` lives here.
    pub pool: sqlx::PgPool,
    /// Calendar client used at startup to fetch the business calendars
    /// the schedule rules reference.
    pub calendar: Arc<dyn CalendarClient>,
    /// Catch-up cap (days). [`DEFAULT_CATCHUP_CAP`] in production.
    pub catchup_cap: u64,
}

/// The distinct business-calendar codes referenced by the schedule rules
/// in `registry`. Drives the startup calendar fetch. Pure — no pool, no
/// I/O — so it's unit-testable without a runtime.
pub fn referenced_calendar_codes(registry: &Registry) -> HashSet<String> {
    registry
        .rules()
        .iter()
        .filter_map(|r| r.schedule())
        .filter_map(|s| s.business_calendar.clone())
        .collect()
}

/// True iff `registry` has any schedule-triggered rule. When false the
/// schedule runner has nothing to do and `run` returns immediately. Pure.
pub fn has_schedule_rules(registry: &Registry) -> bool {
    registry.rules().iter().any(|r| r.schedule().is_some())
}

impl ScheduleRunner {
    /// The distinct business-calendar codes referenced by this runner's
    /// schedule rules. (Thin wrapper over [`referenced_calendar_codes`].)
    pub fn calendar_codes(&self) -> HashSet<String> {
        referenced_calendar_codes(&self.registry)
    }

    /// True iff this runner has any schedule-triggered rule.
    pub fn has_schedule_rules(&self) -> bool {
        has_schedule_rules(&self.registry)
    }

    /// Fetch every referenced calendar into a code→calendar map. A
    /// calendar that is absent (404) or errors is logged and SKIPPED —
    /// the day-firing logic treats a missing calendar as "all days are
    /// business days" (the `cal: None` branch of `schedule_fires_on`),
    /// which degrades to firing on the nominal cadence day without
    /// weekend/holiday postponement. That is a deliberate
    /// fail-OPEN: a calendar outage must not silently stop scheduled
    /// work (payroll, tax, sweeps) from firing.
    pub async fn load_calendars(&self) -> HashMap<String, BusinessCalendar> {
        let mut out = HashMap::new();
        for code in self.calendar_codes() {
            match self.calendar.get_business_calendar(&code).await {
                Ok(Some(cal)) => {
                    debug!(calendar = %code, "schedule runner: loaded business calendar");
                    out.insert(code, cal);
                }
                Ok(None) => {
                    warn!(
                        calendar = %code,
                        "schedule runner: business calendar not found; \
                         scheduled rules using it will fire on the nominal cadence day \
                         (no business-day postponement)"
                    );
                }
                Err(e) => {
                    warn!(
                        calendar = %code, error = %e,
                        "schedule runner: failed to fetch business calendar; \
                         scheduled rules using it will fire on the nominal cadence day"
                    );
                }
            }
        }
        out
    }

    /// Load the persisted cursor (the last sim-day already fired), or
    /// `None` if the runner has never run / the row is absent.
    pub async fn load_cursor(&self) -> Result<Option<NaiveDate>> {
        let row: Option<(Option<NaiveDate>,)> =
            sqlx::query_as("SELECT last_sim_day FROM dispatcher_clock_cursor WHERE id = 1")
                .fetch_optional(&self.pool)
                .await
                .context("loading dispatcher_clock_cursor")?;
        Ok(row.and_then(|(d,)| d))
    }

    /// Persist the cursor (upsert the single row id=1).
    pub async fn save_cursor(&self, day: NaiveDate) -> Result<()> {
        sqlx::query(
            "INSERT INTO dispatcher_clock_cursor (id, last_sim_day) VALUES (1, $1) \
             ON CONFLICT (id) DO UPDATE SET last_sim_day = EXCLUDED.last_sim_day",
        )
        .bind(day)
        .execute(&self.pool)
        .await
        .context("persisting dispatcher_clock_cursor")?;
        Ok(())
    }

    /// Build the [`MatchedRule`] set for one sim-day: every schedule rule
    /// that fires on `day` (using the cached calendar), with its
    /// `do_steps` args evaluated against the synthetic clock-day payload.
    ///
    /// Pure given `(registry, calendars, day)` — no I/O. The synthetic
    /// payload is `{"_day": "YYYY-MM-DD"}` so a `jobs.spawn` arg like
    /// `subject = "_day"` (or a `metadata.x` field) can reference the
    /// firing day. (Mirrors the sim PeriodicEngine's `inject_day`.)
    pub fn matched_for_day(
        registry: &Registry,
        calendars: &HashMap<String, BusinessCalendar>,
        day: NaiveDate,
    ) -> (Vec<MatchedRule>, serde_json::Value) {
        let payload = json!({ "_day": day.format("%Y-%m-%d").to_string() });
        let mut matched = Vec::new();
        for rule in registry.rules() {
            let Some(sched) = rule.schedule() else {
                continue;
            };
            let cal = sched
                .business_calendar
                .as_deref()
                .and_then(|c| calendars.get(c));
            if !sched.fires_on(cal, day) {
                continue;
            }
            // Schedule rules carry literal args only (no event payload to
            // bind against), so the expr context's payload is the
            // synthetic day object and there are no helpers to resolve.
            // Evaluate each do-step's args against it.
            let ctx = super::expr::Context {
                payload: &payload,
                helpers: &super::expr::NoHelpers,
            };
            let mut invocations = Vec::with_capacity(rule.do_steps.len());
            let mut arg_error = false;
            for ds in &rule.do_steps {
                let mut args = Vec::with_capacity(ds.args.len());
                for (k, expr) in &ds.args {
                    match super::expr::eval(expr, &ctx) {
                        Ok(v) => args.push((k.clone(), v)),
                        Err(e) => {
                            warn!(
                                rule = %rule.name, handler = %ds.handler, arg = %k,
                                error = %e,
                                "schedule runner: arg eval failed; skipping rule for this day"
                            );
                            arg_error = true;
                            break;
                        }
                    }
                }
                if arg_error {
                    break;
                }
                invocations.push(MatchedInvocation {
                    handler: ds.handler.clone(),
                    args,
                });
            }
            if arg_error {
                continue;
            }
            matched.push(MatchedRule {
                rule_name: rule.name.clone(),
                invocations,
            });
        }
        (matched, payload)
    }

    /// Consume the clock stream and fire schedule rules on each sim-day
    /// boundary. Runs forever (the SSE stream auto-reconnects); drop the
    /// future to stop.
    pub async fn run(&self, live: Arc<crate::liveness::DispatcherLiveness>) -> Result<()> {
        if !has_schedule_rules(&self.registry) {
            info!("schedule runner: no schedule-triggered rules, not starting");
            return Ok(());
        }
        let calendars = self.load_calendars().await;
        let mut cursor = self.load_cursor().await.unwrap_or_else(|e| {
            warn!(error = %e, "schedule runner: cursor load failed; starting fresh");
            None
        });
        info!(
            schedule_rule_count = self.registry.rules().iter().filter(|r| r.schedule().is_some()).count(),
            calendars = ?calendars.keys().collect::<Vec<_>>(),
            ?cursor,
            cap = self.catchup_cap,
            "schedule runner: starting clock-driven loop"
        );
        live.mark_schedule_running();

        let mut ticks = Box::pin(boss_clock_client::subscribe_ticks(self.clock_url.clone()));
        while let Some(now) = ticks.next().await {
            let advance = advance_cursor(cursor, &now, self.catchup_cap);
            if advance.skipped > 0 {
                warn!(
                    skipped = advance.skipped,
                    cap = self.catchup_cap,
                    "schedule runner: catch-up gap exceeded cap; skipped oldest days"
                );
            }
            // Fire each due day, persisting the cursor PER DAY (not once
            // after the whole range). The spawn is not idempotent, so a
            // crash between firing and persisting must re-fire at most the
            // single in-flight day on restart — never the whole catch-up
            // range. The in-memory cursor advances only in lockstep with the
            // persisted one; a persist failure stops the advance so we never
            // run ahead of what's durably recorded (we retry next tick).
            let mut persist_failed = false;
            for day in &advance.days_to_fire {
                let (matched, payload) = Self::matched_for_day(&self.registry, &calendars, *day);
                if !matched.is_empty() {
                    // Synthesize a clock-day dispatch context: the topic is
                    // `clock.day` and the "triggering event id" is the day
                    // itself, so audit provenance chains the spawned work
                    // back to the calendar day that produced it.
                    let event_id = format!("clock-day:{}", day.format("%Y-%m-%d"));
                    match handler::dispatch(
                        &matched,
                        &self.handlers,
                        &event_id,
                        "clock.day",
                        &payload,
                    )
                    .await
                    {
                        Ok(results) => {
                            let mut fired = 0u64;
                            let mut failed = 0u64;
                            for r in results {
                                match &r.outcome {
                                    Ok(()) => {
                                        fired += 1;
                                        debug!(
                                            rule = %r.rule_name, handler = %r.handler,
                                            day = %day, "schedule rule fired"
                                        );
                                    }
                                    Err(e) => {
                                        failed += 1;
                                        warn!(
                                            rule = %r.rule_name, handler = %r.handler,
                                            day = %day, error = %e,
                                            "schedule rule handler failed"
                                        );
                                    }
                                }
                            }
                            info!(day = %day, fired, failed, "schedule runner: fired day");
                            live.record_schedule();
                        }
                        Err(e) => {
                            // UnknownHandler is a registry/code drift — surface
                            // it loudly, but DON'T wedge the loop or skip the
                            // cursor advance (a bad rule must not freeze every
                            // other schedule). The day still counts as
                            // processed; the operator fixes the rule.
                            warn!(day = %day, error = %e, "schedule runner: dispatch error");
                        }
                    }
                }
                // Record this day done before moving to the next, so a crash
                // re-fires at most this one day. Stop advancing if the persist
                // fails — the in-memory cursor must never lead the durable one.
                if let Err(e) = self.save_cursor(*day).await {
                    warn!(
                        day = %day, error = %e,
                        "schedule runner: cursor persist failed; pausing advance until next tick"
                    );
                    persist_failed = true;
                    break;
                }
                cursor = Some(*day);
            }
            // Non-fire cursor moves (first-observation baseline, backward-jump
            // reset): `days_to_fire` is empty but the cursor still settles.
            if !persist_failed
                && advance.days_to_fire.is_empty()
                && advance.new_cursor != cursor
                && let Some(nc) = advance.new_cursor
            {
                if let Err(e) = self.save_cursor(nc).await {
                    warn!(error = %e, "schedule runner: failed to persist cursor");
                } else {
                    cursor = advance.new_cursor;
                }
            }
        }
        live.mark_schedule_stopped();
        info!("schedule runner: clock stream ended");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::NoHelpers;
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Build a `ClockNow` for a given date with the given flags.
    fn clock(day: NaiveDate, paused: bool, restart: bool) -> ClockNow {
        ClockNow {
            now: day.and_hms_opt(12, 0, 0).unwrap().and_utc(),
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused,
            restart_in_progress: restart,
        }
    }

    // ----- advance_cursor — the sim-day cursor decision -----

    #[test]
    fn first_observation_sets_baseline_fires_nothing() {
        let a = advance_cursor(None, &clock(d(2026, 4, 27), false, false), 90);
        assert_eq!(a.days_to_fire, Vec::<NaiveDate>::new());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn same_day_is_noop() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 27), false, false),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn advance_one_day_fires_that_day() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 28), false, false),
            90,
        );
        assert_eq!(a.days_to_fire, vec![d(2026, 4, 28)]);
        assert_eq!(a.new_cursor, Some(d(2026, 4, 28)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn multi_day_catch_up_fires_each_day_in_order() {
        // Cursor at 04-27, clock jumps to 04-30 → fire 28, 29, 30.
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 30), false, false),
            90,
        );
        assert_eq!(
            a.days_to_fire,
            vec![d(2026, 4, 28), d(2026, 4, 29), d(2026, 4, 30)]
        );
        assert_eq!(a.new_cursor, Some(d(2026, 4, 30)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn paused_does_not_advance_or_fire() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 5, 10), true, false),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(
            a.new_cursor,
            Some(d(2026, 4, 27)),
            "cursor unchanged while paused"
        );
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn restart_in_progress_does_not_advance_or_fire() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 5, 10), false, true),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn paused_from_no_cursor_stays_none() {
        // A paused clock observed before any baseline keeps the cursor
        // None (don't adopt a baseline off a frozen observation).
        let a = advance_cursor(None, &clock(d(2026, 4, 27), true, false), 90);
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, None);
    }

    #[test]
    fn backward_jump_resets_cursor_fires_nothing() {
        // Epoch restart: clock jumps from 2026-12 back to 2026-01.
        let a = advance_cursor(
            Some(d(2026, 12, 31)),
            &clock(d(2026, 1, 1), false, false),
            90,
        );
        assert!(a.days_to_fire.is_empty(), "no backfill on a backward jump");
        assert_eq!(
            a.new_cursor,
            Some(d(2026, 1, 1)),
            "cursor reset to the new (earlier) day"
        );
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn catch_up_cap_fires_only_most_recent_and_reports_skip() {
        // Cursor at day 0, clock jumps 100 days forward, cap = 10.
        let cursor = d(2026, 1, 1);
        let target = cursor + chrono::Duration::days(100);
        let a = advance_cursor(Some(cursor), &clock(target, false, false), 10);
        assert_eq!(a.days_to_fire.len(), 10, "exactly cap days fire");
        // The fired days are the most recent 10: (target-10, target].
        assert_eq!(
            *a.days_to_fire.first().unwrap(),
            target - chrono::Duration::days(9)
        );
        assert_eq!(*a.days_to_fire.last().unwrap(), target);
        assert_eq!(a.skipped, 90, "the older 90 days are skipped");
        assert_eq!(a.new_cursor, Some(target));
    }

    #[test]
    fn catch_up_exactly_at_cap_fires_all_no_skip() {
        let cursor = d(2026, 1, 1);
        let target = cursor + chrono::Duration::days(10);
        let a = advance_cursor(Some(cursor), &clock(target, false, false), 10);
        assert_eq!(a.days_to_fire.len(), 10);
        assert_eq!(a.skipped, 0);
        assert_eq!(
            *a.days_to_fire.first().unwrap(),
            cursor + chrono::Duration::days(1)
        );
        assert_eq!(*a.days_to_fire.last().unwrap(), target);
    }

    // ----- matched_for_day — which schedule rules fire on a day -----

    fn sched_registry() -> Registry {
        // Two schedule rules: a daily one (no calendar) and a monthly
        // one (no calendar), plus an event rule that must NOT fire.
        let toml = r#"
[[rule]]
name = "daily-sweep"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"bank-sweep\"", subject_kind = "\"account\"", subject = "_day" }

[[rule]]
name = "monthly-close"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"month-close\"", subject_kind = "\"account\"", subject = "\"acct-gl\"" }

[[rule]]
name = "event-only"
on_event = "step.done.x"
[[rule.do]]
handler = "h"
"#;
        Registry::from_toml(toml).unwrap()
    }

    #[test]
    fn matched_for_day_fires_only_due_schedule_rules() {
        let reg = sched_registry();
        let cals = HashMap::new();
        // 2026-01-15: both daily AND monthly fire; event rule never.
        let (matched, payload) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 15));
        let names: Vec<&str> = matched.iter().map(|m| m.rule_name.as_str()).collect();
        assert!(names.contains(&"daily-sweep"));
        assert!(names.contains(&"monthly-close"));
        assert!(
            !names.contains(&"event-only"),
            "event rules never fire on a clock day"
        );
        assert_eq!(payload["_day"], "2026-01-15");

        // 2026-01-16: only daily fires.
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 16));
        let names: Vec<&str> = matched.iter().map(|m| m.rule_name.as_str()).collect();
        assert_eq!(names, vec!["daily-sweep"]);
    }

    #[test]
    fn matched_for_day_injects_day_into_args() {
        // The `subject = "_day"` arg on daily-sweep must resolve to the
        // firing day from the synthetic payload.
        let reg = sched_registry();
        let cals = HashMap::new();
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 3, 9));
        let daily = matched
            .iter()
            .find(|m| m.rule_name == "daily-sweep")
            .unwrap();
        let subj = daily.invocations[0]
            .args
            .iter()
            .find(|(k, _)| k == "subject")
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(
            *subj,
            super::super::expr::Value::String("2026-03-09".into())
        );
    }

    #[test]
    fn matched_for_day_respects_calendar_postponement() {
        // Monthly rule WITH a weekday-only calendar: the 15th-anchored
        // monthly fire on a month where the 15th is a Saturday postpones
        // to Monday the 17th. (2026-08-15 is a Saturday.)
        let toml = r#"
[[rule]]
name = "monthly-on-cal"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
business_calendar = "weekdays-only"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let mut cals = HashMap::new();
        cals.insert(
            "weekdays-only".to_string(),
            BusinessCalendar::new("weekdays-only", "Weekdays Only"),
        );
        // 15th (Sat): does NOT fire.
        assert!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 15))
                .0
                .is_empty()
        );
        // 16th (Sun): does NOT fire.
        assert!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 16))
                .0
                .is_empty()
        );
        // 17th (Mon): postponed fire.
        assert_eq!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 17))
                .0
                .len(),
            1
        );
    }

    #[test]
    fn matched_for_day_missing_calendar_fires_on_nominal_day() {
        // The rule names a calendar that wasn't loaded (absent/errored).
        // Degrade: fire on the nominal cadence day, no postponement.
        let toml = r#"
[[rule]]
name = "monthly-missing-cal"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
business_calendar = "does-not-exist"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let cals = HashMap::new(); // calendar NOT loaded
        // Fires on the nominal 15th even though it's a weekend, because
        // a missing calendar means "every day is a business day".
        assert_eq!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 15))
                .0
                .len(),
            1,
            "missing calendar must fail OPEN — fire on nominal day"
        );
    }

    // ----- referenced_calendar_codes / has_schedule_rules (pure) -----

    #[test]
    fn calendar_codes_collects_distinct_referenced_calendars() {
        let toml = r#"
[[rule]]
name = "a"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"

[[rule]]
name = "b"
[rule.schedule]
cadence = "weekly"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"

[[rule]]
name = "c"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-01"
business_calendar = "us-tax"
[[rule.do]]
handler = "h"

[[rule]]
name = "d-no-cal"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let codes = referenced_calendar_codes(&reg);
        assert_eq!(codes.len(), 2, "deduped + skips the no-calendar rule");
        assert!(codes.contains("us-banking"));
        assert!(codes.contains("us-tax"));
    }

    #[test]
    fn has_schedule_rules_true_only_with_a_schedule() {
        let with = Registry::from_toml(
            r#"
[[rule]]
name = "s"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        assert!(has_schedule_rules(&with));

        let without = Registry::from_toml(
            r#"
[[rule]]
name = "e"
on_event = "step.done.x"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        assert!(!has_schedule_rules(&without));
    }

    // ----- load_calendars fail-open (uses FakeCalendarClient) -----

    #[tokio::test]
    async fn load_calendars_skips_absent_calendar() {
        // FakeCalendarClient::get_business_calendar always returns None,
        // so every referenced calendar is "absent" → the map is empty and
        // no panic. (Fail-open behavior is exercised in matched_for_day.)
        // The pool is built lazily inside a Tokio context here and never
        // queried, so it never opens a socket.
        let reg = Registry::from_toml(
            r#"
[[rule]]
name = "s"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        let r = ScheduleRunner {
            registry: reg,
            handlers: HandlerRegistry::new(),
            clock_url: "http://127.0.0.1:7060".into(),
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://boss:boss@127.0.0.1/boss")
                .expect("lazy pool"),
            calendar: Arc::new(boss_calendar_client::FakeCalendarClient::new()),
            catchup_cap: DEFAULT_CATCHUP_CAP,
        };
        let cals = r.load_calendars().await;
        assert!(cals.is_empty(), "absent calendar is skipped, not faked");
        // Sanity: NoHelpers is the resolver matched_for_day uses.
        let _ = &NoHelpers;
    }
}
