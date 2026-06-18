//! `PeriodicEngine` — calendar-driven cycles that fire on a cadence
//! regardless of any Boss event. Each spec declares a cadence
//! (daily / weekly / biweekly / monthly / quarterly / annually /
//! every-n-days), an anchor date, an optional business calendar
//! that postpones non-business firings, and an action — either
//! "open a Job of kind X" or "emit a raw event onto the bus".

use chrono::{Datelike, Days, NaiveDate};
use serde::Deserialize;
use serde_json::Value;

use crate::calendar::{BusinessCalendar, CalendarRegistry};
use crate::engines::{DayContext, SimEngine};

/// Built-in cadence shapes. Tenants pick one per spec; firing math
/// is below in `Cadence::fires_on` (date-only) and
/// `Cadence::fires_on_tick` (sub-day-aware).
///
/// Sub-day cadences (`Hourly`, `EveryNMinutes`) fire on every
/// tick whose window covers an integer multiple of the cadence
/// period; at day-tick (`tick_duration = "1d"`) they degenerate
/// to one fire per sim-day (the day-tick covers `[0, 24)` which
/// includes the anchor). See [`Cadence::fires_on_tick`].
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Cadence {
    Daily,
    Weekly,
    Biweekly,
    Monthly,
    Quarterly,
    Annually,
    /// Fires once per sim-hour (at minute :00 of each hour). At
    /// hourly tick granularity, fires once per tick. At day-tick,
    /// fires once per sim-day.
    Hourly,
    /// Fires every `n` sim-minutes from midnight. `n` must be
    /// > 0 and <= 1440. The tenant TOML loader does NOT validate
    /// > `n` divides 1440 evenly — non-divisors fire on the first
    /// > tick window covering each multiple, which spreads
    /// > reasonably even at sub-day granularities. At day-tick,
    /// > fires once per sim-day regardless of `n`.
    EveryNMinutes(u32),
}

impl Cadence {
    /// Does this cadence fire on `day`, given an anchor date and a
    /// business calendar? Calendar resolution happens *outside* this
    /// function — the engine itself postpones non-business firings.
    ///
    /// Sub-day cadences (`Hourly`, `EveryNMinutes`) return `true`
    /// for every day on or after the anchor — the per-tick
    /// resolution lives in [`fires_on_tick`]. Day-anchored callers
    /// get the right "yes/no per day" answer directly.
    pub fn fires_on(&self, anchor: NaiveDate, day: NaiveDate) -> bool {
        if day < anchor {
            return false;
        }
        match self {
            Cadence::Daily => true,
            Cadence::Weekly => day.weekday() == anchor.weekday(),
            Cadence::Biweekly => {
                day.weekday() == anchor.weekday() && (day - anchor).num_days() % 14 == 0
            }
            Cadence::Monthly => {
                day.day() == clamp_anchor_day(anchor.day(), day.year(), day.month())
            }
            Cadence::Quarterly => {
                if day.day() != clamp_anchor_day(anchor.day(), day.year(), day.month()) {
                    return false;
                }
                let months_diff = ((day.year() as i64 - anchor.year() as i64) * 12)
                    + (day.month() as i64 - anchor.month() as i64);
                months_diff >= 0 && months_diff % 3 == 0
            }
            Cadence::Annually => {
                day.month() == anchor.month()
                    && day.day() == clamp_anchor_day(anchor.day(), day.year(), day.month())
            }
            // Sub-day: every day on or after the anchor is a
            // candidate for at least one fire; the per-tick
            // gate decides which tick fires.
            Cadence::Hourly | Cadence::EveryNMinutes(_) => true,
        }
    }

    /// Tick-aware fire decision. Day-anchored cadences fire only
    /// on `tick.is_first_in_day`. Sub-day cadences fire on every
    /// tick whose window covers an integer multiple of the cadence
    /// period from midnight.
    ///
    /// At the day-tick (`Tick::day()`) sub-day cadences degenerate
    /// to one fire per sim-day — the day-tick window `[0, 24)`
    /// covers the first cadence anchor (minute 0) regardless of
    /// period, so a day-tick deployment treats them as "fire once
    /// a day."
    pub fn fires_on_tick(
        &self,
        anchor: NaiveDate,
        day: NaiveDate,
        tick: &crate::engines::Tick,
    ) -> bool {
        if !self.fires_on(anchor, day) {
            return false;
        }
        match self {
            Cadence::Daily
            | Cadence::Weekly
            | Cadence::Biweekly
            | Cadence::Monthly
            | Cadence::Quarterly
            | Cadence::Annually => tick.is_first_in_day,
            Cadence::Hourly => {
                // Fires on hour 0, 1, 2, ..., 23 each day. For a
                // tick covering `[start, start+dur)`, fire iff
                // the smallest integer hour ≥ start lies within
                // the window.
                let start = tick.start_hour_of_day;
                let end = start + tick.duration_hours;
                let first_hour = start.ceil();
                first_hour < end
            }
            Cadence::EveryNMinutes(n) => {
                // Fires every `n` minutes from midnight. Tick
                // covers `[start, start+dur)` hours; fire iff
                // any multiple of `(n/60)` hours falls in the
                // window. The smallest multiple ≥ start is
                // `ceil(start / period) * period`.
                let n = (*n as f64).max(1.0); // guard against zero
                let period_hours = n / 60.0;
                let start = tick.start_hour_of_day;
                let end = start + tick.duration_hours;
                let next_fire = (start / period_hours).ceil() * period_hours;
                next_fire < end
            }
        }
    }

    /// Sparse cadences where a single dropped fire would lose a whole
    /// period — so the engine postpones them to the next business day
    /// instead of skipping. Dense cadences (daily/weekly/biweekly/
    /// sub-day) skip a non-business day, because "every Tuesday" means
    /// Tuesday, not "Tuesday or the next business day".
    pub fn is_coarse(&self) -> bool {
        matches!(
            self,
            Cadence::Monthly | Cadence::Quarterly | Cadence::Annually
        )
    }
}

/// Last calendar day of `(year, month)` — 28–31 depending on the month
/// (29 in a leap February).
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first_of_next| first_of_next.pred_opt())
        .map(|last| last.day())
        .unwrap_or(28)
}

/// The anchor's day-of-month, clamped into a month that may be shorter:
/// a day-31 anchor lands on the 30th in a 30-day month and the 28th/29th
/// in February. Without this a month-end anchor on a monthly/quarterly
/// cadence silently never matches the short months — a day-31 anchor
/// never fires in April — losing those periods entirely.
fn clamp_anchor_day(anchor_day: u32, year: i32, month: u32) -> u32 {
    anchor_day.min(last_day_of_month(year, month))
}

/// How many days a sparse-cadence fire may be carried forward to reach a
/// business day. Covers any realistic weekend + holiday closure while
/// staying well under the ~28-day minimum gap between monthly fires, so
/// the look-back in [`coarse_fires_today`] can never reach the previous
/// period's nominal day.
const MAX_POSTPONE_DAYS: u64 = 10;

/// Fire decision for sparse cadences (monthly/quarterly/annually) with
/// business-day POSTPONEMENT rather than dropping. The nominal (clamped)
/// fire day is carried forward to the next business day on or after it;
/// `day` fires iff it is that carried-forward day for some nominal fire
/// within the postpone window. A nominal day that is itself a business
/// day fires on the day itself (`back == 0`).
fn coarse_fires_today(
    cadence: &Cadence,
    anchor: NaiveDate,
    day: NaiveDate,
    cal: &dyn BusinessCalendar,
) -> bool {
    if !cal.is_business_day(day) {
        return false;
    }
    for back in 0..=MAX_POSTPONE_DAYS {
        let Some(nominal) = day.checked_sub_days(Days::new(back)) else {
            break;
        };
        if cadence.fires_on(anchor, nominal) && cal.next_business_day(nominal) == day {
            return true;
        }
    }
    false
}

/// What a periodic spec does when it fires.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeriodicAction {
    /// Open a Job of `job_kind`. The engine emits a
    /// `periodic.job_requested` event carrying the kind + subject,
    /// which the live runner routes to `POST /api/jobs`.
    OpenJob {
        job_kind: String,
        #[serde(default)]
        subject_kind: Option<String>,
        #[serde(default)]
        subject_id: Option<String>,
    },
    /// Publish an event onto the bus and forward it to SimOutput.
    EmitEvent { topic: String, payload: Value },
}

/// One periodic spec.
#[derive(Debug, Clone, Deserialize)]
pub struct PeriodicSpec {
    pub name: String,
    pub cadence: Cadence,
    pub anchor_date: NaiveDate,
    #[serde(default)]
    pub business_calendar: Option<String>,
    pub action: PeriodicAction,
}

/// Engine. Holds the spec list + calendar registry; no internal
/// queue (firing is decided per-day from the spec + day).
pub struct PeriodicEngine {
    specs: Vec<PeriodicSpec>,
    calendars: CalendarRegistry,
}

impl PeriodicEngine {
    pub fn new(specs: Vec<PeriodicSpec>, calendars: CalendarRegistry) -> Self {
        Self { specs, calendars }
    }

    pub fn specs(&self) -> &[PeriodicSpec] {
        &self.specs
    }
}

/// Mutate `payload` to include `_day: "YYYY-MM-DD"`. If `payload` is
/// not an object, it is replaced with `{"_day": "...", "_value":
/// <original>}` so the caller's data isn't dropped. LiveApi's path
/// templates read this key when substituting `{_day}` into the
/// route URL.
fn inject_day(payload: &mut Value, day: NaiveDate) {
    let day_str = day.format("%Y-%m-%d").to_string();
    if let Value::Object(map) = payload {
        map.insert("_day".to_string(), Value::String(day_str));
        return;
    }
    let original = std::mem::replace(payload, Value::Null);
    let mut map = serde_json::Map::new();
    map.insert("_day".to_string(), Value::String(day_str));
    if !original.is_null() {
        map.insert("_value".to_string(), original);
    }
    *payload = Value::Object(map);
}

impl SimEngine for PeriodicEngine {
    fn name(&self) -> &str {
        "periodic"
    }

    fn step(&mut self, ctx: &mut DayContext<'_>) -> anyhow::Result<()> {
        for spec in &self.specs {
            let cal = self.calendars.get(spec.business_calendar.as_deref());
            // Tick-aware fire decision — preserves day-anchored gating
            // (day cadences only fire on `is_first_in_day`) AND lights up
            // sub-day cadences (Hourly / EveryNMinutes) on the right tick.
            let fires = if spec.cadence.is_coarse() {
                // Sparse cadences (monthly/quarterly/annually): a single
                // dropped fire loses a whole period, so a non-business
                // nominal day is POSTPONED to the next business day rather
                // than skipped, and a month-end anchor is clamped to the
                // real last day (a day-31 anchor fires Apr 30, not never).
                ctx.tick.is_first_in_day
                    && coarse_fires_today(&spec.cadence, spec.anchor_date, ctx.day, cal)
            } else {
                // Dense cadences (daily/weekly/biweekly/sub-day): a
                // non-business day SKIPS — "every Tuesday" means Tuesday,
                // not "Tuesday or the next business day after".
                spec.cadence
                    .fires_on_tick(spec.anchor_date, ctx.day, &ctx.tick)
                    && cal.is_business_day(ctx.day)
            };
            if !fires {
                continue;
            }
            match &spec.action {
                PeriodicAction::OpenJob {
                    job_kind,
                    subject_kind,
                    subject_id,
                } => {
                    let mut payload = serde_json::json!({
                        "job_kind": job_kind,
                        "subject_kind": subject_kind,
                        "subject_id": subject_id,
                        "spec": spec.name,
                    });
                    inject_day(&mut payload, ctx.day);
                    ctx.bus.publish(
                        "periodic.job_requested",
                        format!("periodic:{}", spec.name),
                        payload.clone(),
                    );
                    ctx.output.emit_event("periodic.job_requested", &payload)?;
                }
                PeriodicAction::EmitEvent { topic, payload } => {
                    let mut p = payload.clone();
                    inject_day(&mut p, ctx.day);
                    ctx.bus
                        .publish(topic.clone(), format!("periodic:{}", spec.name), p.clone());
                    ctx.output.emit_event(topic, &p)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::SimEventBus;
    use crate::output::InMemoryOutput;
    use crate::rng::Rng;
    use crate::shape_driven::ShapeDrivenState;
    use chrono::Weekday;
    use serde_json::json;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn step_engine(engine: &mut PeriodicEngine, day: NaiveDate) -> InMemoryOutput {
        let mut state = ShapeDrivenState::new();
        let mut bus = SimEventBus::new();
        let mut output = InMemoryOutput::default();
        let mut rng = Rng::new(0);
        let mut ctx = DayContext {
            tick: crate::engines::Tick::day(),
            day,
            rng: &mut rng,
            state: &mut state,
            output: &mut output,
            bus: &mut bus,
        };
        engine.step(&mut ctx).unwrap();
        output
    }

    #[test]
    fn daily_fires_every_day_at_or_after_anchor() {
        let anchor = d(2026, 4, 27);
        for offset in -3..30 {
            let day = anchor + chrono::Duration::days(offset);
            assert_eq!(
                Cadence::Daily.fires_on(anchor, day),
                offset >= 0,
                "Daily firing wrong on offset {offset}"
            );
        }
    }

    #[test]
    fn weekly_fires_only_on_anchor_weekday() {
        let anchor = d(2026, 4, 27); // Monday
        for offset in 0..21 {
            let day = anchor + chrono::Duration::days(offset);
            let expected = day.weekday() == Weekday::Mon;
            assert_eq!(Cadence::Weekly.fires_on(anchor, day), expected);
        }
    }

    #[test]
    fn biweekly_fires_every_other_anchor_weekday() {
        let anchor = d(2026, 4, 27); // Monday
        // Week 0 Mon: yes. Week 1 Mon: no. Week 2 Mon: yes.
        assert!(Cadence::Biweekly.fires_on(anchor, anchor));
        assert!(!Cadence::Biweekly.fires_on(anchor, anchor + chrono::Duration::days(7)));
        assert!(Cadence::Biweekly.fires_on(anchor, anchor + chrono::Duration::days(14)));
        assert!(!Cadence::Biweekly.fires_on(anchor, anchor + chrono::Duration::days(21)));
    }

    #[test]
    fn monthly_fires_on_day_of_month() {
        let anchor = d(2026, 4, 15);
        assert!(Cadence::Monthly.fires_on(anchor, d(2026, 4, 15)));
        assert!(Cadence::Monthly.fires_on(anchor, d(2026, 5, 15)));
        assert!(Cadence::Monthly.fires_on(anchor, d(2026, 12, 15)));
        assert!(!Cadence::Monthly.fires_on(anchor, d(2026, 5, 14)));
        assert!(!Cadence::Monthly.fires_on(anchor, d(2026, 5, 16)));
    }

    #[test]
    fn quarterly_fires_every_three_months() {
        let anchor = d(2026, 1, 31);
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 1, 31)));
        // April has no 31st, so a day-31 anchor CLAMPS to Apr 30 — the
        // quarter still fires (it used to be lost every year).
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 4, 30)));
        assert!(!Cadence::Quarterly.fires_on(anchor, d(2026, 4, 29)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 7, 31)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 10, 31)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2027, 1, 31)));
        // Off-quarter month.
        assert!(!Cadence::Quarterly.fires_on(anchor, d(2026, 5, 31)));
    }

    #[test]
    fn annually_fires_on_anniversary() {
        let anchor = d(2024, 7, 4);
        assert!(Cadence::Annually.fires_on(anchor, d(2024, 7, 4)));
        assert!(Cadence::Annually.fires_on(anchor, d(2025, 7, 4)));
        assert!(Cadence::Annually.fires_on(anchor, d(2030, 7, 4)));
        assert!(!Cadence::Annually.fires_on(anchor, d(2025, 7, 5)));
    }

    #[test]
    fn clamp_anchor_day_handles_short_months() {
        assert_eq!(clamp_anchor_day(31, 2025, 4), 30); // April → 30
        assert_eq!(clamp_anchor_day(31, 2025, 2), 28); // Feb, non-leap
        assert_eq!(clamp_anchor_day(31, 2024, 2), 29); // Feb, leap
        assert_eq!(clamp_anchor_day(31, 2025, 1), 31); // Jan, unchanged
        assert_eq!(clamp_anchor_day(15, 2025, 2), 15); // below month length
    }

    #[test]
    fn is_coarse_classifies_sparse_cadences() {
        assert!(Cadence::Monthly.is_coarse());
        assert!(Cadence::Quarterly.is_coarse());
        assert!(Cadence::Annually.is_coarse());
        assert!(!Cadence::Daily.is_coarse());
        assert!(!Cadence::Weekly.is_coarse());
        assert!(!Cadence::Biweekly.is_coarse());
        assert!(!Cadence::Hourly.is_coarse());
    }

    #[test]
    fn coarse_quarterly_clamps_april_and_postpones_weekends() {
        // Day-31 quarterly on a weekday-only calendar. 2026-01-31 is a
        // Saturday, so the Jan-quarter fire is carried forward to Monday
        // 2026-02-02 rather than dropped for the whole quarter.
        let reg = CalendarRegistry::with_builtins();
        let cal = reg.get(Some("weekdays-only"));
        let anchor = d(2024, 1, 31);
        let q = Cadence::Quarterly;
        assert!(!coarse_fires_today(&q, anchor, d(2026, 1, 31), cal)); // Sat
        assert!(!coarse_fires_today(&q, anchor, d(2026, 2, 1), cal)); // Sun
        assert!(coarse_fires_today(&q, anchor, d(2026, 2, 2), cal)); // Mon — postponed
        assert!(!coarse_fires_today(&q, anchor, d(2026, 2, 3), cal)); // Tue — no double-fire
        // A weekday nominal fires on the day itself (2025-07-31 is a Thu).
        assert!(coarse_fires_today(&q, anchor, d(2025, 7, 31), cal));
        // The clamped April nominal fires (2025-04-30 is a Wed).
        assert!(coarse_fires_today(&q, anchor, d(2025, 4, 30), cal));
        // A plain non-nominal business day never fires.
        assert!(!coarse_fires_today(&q, anchor, d(2025, 6, 16), cal));
    }

    #[test]
    fn open_job_action_publishes_request() {
        let mut engine = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "monthly-preventive-maintenance".into(),
                cadence: Cadence::Monthly,
                anchor_date: d(2026, 4, 15),
                business_calendar: None,
                action: PeriodicAction::OpenJob {
                    job_kind: "equipment-preventive-maintenance".into(),
                    subject_kind: Some("location".into()),
                    subject_id: Some("loc-brewery-brewhouse".into()),
                },
            }],
            CalendarRegistry::with_builtins(),
        );
        let out = step_engine(&mut engine, d(2026, 4, 15));
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].0, "periodic.job_requested");
        assert_eq!(
            out.events[0].1["job_kind"],
            "equipment-preventive-maintenance"
        );
        assert_eq!(out.events[0].1["spec"], "monthly-preventive-maintenance");
    }

    #[test]
    fn emit_event_action_publishes_topic_with_payload() {
        let mut engine = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "daily-bank-sweep".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: Some("us-banking".into()),
                action: PeriodicAction::EmitEvent {
                    topic: "ledger.bank_sweep_request".into(),
                    payload: json!({"as_of": "today"}),
                },
            }],
            CalendarRegistry::with_builtins(),
        );
        // Monday: fires.
        let out = step_engine(&mut engine, d(2026, 4, 27));
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].0, "ledger.bank_sweep_request");
        assert_eq!(out.events[0].1["as_of"], "today");
    }

    #[test]
    fn business_calendar_gates_non_business_days() {
        let mut engine = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "biz-only".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 4, 27),
                business_calendar: Some("us-banking".into()),
                action: PeriodicAction::EmitEvent {
                    topic: "x.y".into(),
                    payload: Value::Null,
                },
            }],
            CalendarRegistry::with_builtins(),
        );
        // Saturday: no fire.
        let out = step_engine(&mut engine, d(2026, 5, 2));
        assert_eq!(out.events.len(), 0);
        // Following Monday: fires.
        let out = step_engine(&mut engine, d(2026, 5, 4));
        assert_eq!(out.events.len(), 1);
    }

    #[test]
    fn before_anchor_does_not_fire() {
        let mut engine = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "future".into(),
                cadence: Cadence::Daily,
                anchor_date: d(2026, 6, 1),
                business_calendar: None,
                action: PeriodicAction::EmitEvent {
                    topic: "x.y".into(),
                    payload: Value::Null,
                },
            }],
            CalendarRegistry::with_builtins(),
        );
        let out = step_engine(&mut engine, d(2026, 5, 31));
        assert_eq!(out.events.len(), 0);
        let out = step_engine(&mut engine, d(2026, 6, 1));
        assert_eq!(out.events.len(), 1);
    }

    /// `Cadence::Hourly` fires on every tick covering an integer
    /// hour.
    #[test]
    fn cadence_hourly_fires_per_tick_at_hour_boundaries() {
        use crate::engines::Tick;
        let anchor = d(2026, 1, 1);
        let day = d(2026, 1, 5);
        let c = Cadence::Hourly;
        // Day-tick covers [0, 24) → fires once per day.
        assert!(c.fires_on_tick(anchor, day, &Tick::day()));
        // Hourly tick at hour H covers [H, H+1) → fires every tick.
        for h in 0..24 {
            assert!(c.fires_on_tick(anchor, day, &Tick::hour(h as f64, h == 0)));
        }
        // 15-min tick: only the tick that covers an integer hour
        // boundary fires. Tick at H=5.0 covers [5.0, 5.25) → fires.
        // Tick at H=5.25 covers [5.25, 5.5) → does NOT fire (next
        // integer hour after 5.25 is 6, not in [5.25, 5.5)).
        assert!(c.fires_on_tick(anchor, day, &Tick::new(0.25, 5.0, false)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::new(0.25, 5.25, false)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::new(0.25, 5.5, false)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::new(0.25, 5.75, false)));
        // Pre-anchor → never.
        assert!(!c.fires_on_tick(anchor, d(2025, 12, 31), &Tick::day()));
    }

    /// `Cadence::EveryNMinutes` fires on every tick covering a
    /// multiple of N minutes.
    #[test]
    fn cadence_every_n_minutes_fires_at_multiples() {
        use crate::engines::Tick;
        let anchor = d(2026, 1, 1);
        let day = d(2026, 1, 5);
        let c = Cadence::EveryNMinutes(15);
        // Day-tick: degenerates to once per day.
        assert!(c.fires_on_tick(anchor, day, &Tick::day()));
        // Hourly tick: each hour contains 4 fires (00, 15, 30, 45)
        // but the hourly tick window [H, H+1) covers all of them
        // → fires once per tick.
        for h in 0..24 {
            assert!(c.fires_on_tick(anchor, day, &Tick::hour(h as f64, h == 0)));
        }
        // 15-min tick: each tick covers exactly one multiple →
        // every tick fires.
        for k in 0..96 {
            let start = (k as f64) * 0.25;
            assert!(c.fires_on_tick(anchor, day, &Tick::new(0.25, start, k == 0)));
        }
        // 5-min tick: only the tick covering a 15-min boundary
        // fires (1-in-3). Tick at H=0.0 covers [0, 1/12) → fires.
        // Tick at H=1/12 covers [1/12, 2/12) — next 15-min
        // multiple is 0.25, not in window → does NOT fire.
        let p = 1.0 / 12.0; // 5 min in hours
        assert!(c.fires_on_tick(anchor, day, &Tick::new(p, 0.0, true)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::new(p, p, false)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::new(p, 2.0 * p, false)));
        // 4th tick (start=0.25) IS a 15-min boundary → fires.
        assert!(c.fires_on_tick(anchor, day, &Tick::new(p, 0.25, false)));
    }

    /// Day-anchored cadences stay gated on `is_first_in_day`;
    /// sub-day cadences IGNORE is_first_in_day.
    #[test]
    fn cadence_daily_only_fires_on_first_in_day_tick() {
        use crate::engines::Tick;
        let anchor = d(2026, 1, 1);
        let day = d(2026, 1, 5);
        let c = Cadence::Daily;
        assert!(c.fires_on_tick(anchor, day, &Tick::day())); // is_first=true
        assert!(c.fires_on_tick(anchor, day, &Tick::hour(0.0, true)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::hour(5.0, false)));
        assert!(!c.fires_on_tick(anchor, day, &Tick::hour(23.0, false)));
    }

    /// PeriodicEngine sub-day cadence: hourly fires emit 24
    /// events per sim-day at hourly tick granularity, 1 event
    /// per sim-day at day-tick.
    #[test]
    fn periodic_engine_hourly_cadence_fires_per_tick() {
        let mut engine = PeriodicEngine::new(
            vec![PeriodicSpec {
                name: "hourly-tick".into(),
                cadence: Cadence::Hourly,
                anchor_date: d(2026, 1, 1),
                business_calendar: None,
                action: PeriodicAction::EmitEvent {
                    topic: "internal.heartbeat".into(),
                    payload: Value::Null,
                },
            }],
            CalendarRegistry::with_builtins(),
        );
        // 24 hourly ticks over one sim-day → 24 fires.
        let mut state = ShapeDrivenState::new();
        let mut bus = SimEventBus::new();
        let mut output = InMemoryOutput::default();
        let mut rng = Rng::new(0);
        for h in 0..24u32 {
            let mut ctx = DayContext {
                day: d(2026, 1, 5),
                tick: crate::engines::Tick::hour(h as f64, h == 0),
                rng: &mut rng,
                state: &mut state,
                output: &mut output,
                bus: &mut bus,
            };
            engine.step(&mut ctx).unwrap();
        }
        assert_eq!(
            output.events.len(),
            24,
            "hourly cadence × 24 ticks = 24 fires"
        );
    }
}
