//! Rule registry — D2 of dispatcher-as-event-router.
//!
//! A `Rule` is one TOML row: a name, the topic it listens for, an optional
//! `when` predicate, and an ordered list of side-effect invocations (`do`).
//! The registry holds the parsed rules and answers "which rules match
//! this incoming event."
//!
//! The matching layer is pure: given `(topic, payload)` and a helper
//! table, it returns the rules whose `on_event` topic matches AND whose
//! `when` predicate evaluates true, with each rule's `do` args
//! pre-evaluated to concrete `Value`s. No I/O, no NATS, no handler
//! dispatch — those live in the next pass.

use super::expr::{self, Expr, HelperResolver, Value};
use boss_core::calendar::BusinessCalendar;
use chrono::{Datelike, Days, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Raw TOML shape — deserializes directly from the registry file
// ---------------------------------------------------------------------------

/// Top-level TOML document: `[[rule]] ...` blocks.
#[derive(Debug, Serialize, Deserialize)]
pub struct RawRegistry {
    #[serde(default, rename = "rule")]
    pub rules: Vec<RawRule>,
}

/// One rule as written in TOML. Strings get parsed into Expr at
/// registry-load time so authoring errors surface up front.
///
/// A rule is triggered EITHER by an incoming event (`on_event`) OR by
/// a schedule (`schedule`) — exactly one of the two. `from_raw`
/// enforces the XOR; a rule with both or neither is a load error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRule {
    pub name: String,
    /// The NATS topic this rule listens for. Mutually exclusive with
    /// `schedule`. Optional so a schedule-triggered rule can omit it;
    /// `from_raw` requires exactly one of `on_event` / `schedule`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_event: Option<String>,
    /// Clock-driven trigger. Mutually exclusive with `on_event`. When
    /// set, the rule fires on sim-day boundaries per the cadence
    /// instead of on a NATS event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<RawSchedule>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default, rename = "do")]
    pub do_steps: Vec<RawDoStep>,
    #[serde(default)]
    pub delay: Option<String>,
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

/// A clock-driven trigger as written in TOML / stored in the DB. The
/// dispatcher fires the rule's `do_steps` on each sim-DAY the cadence
/// selects (postponed onto a business day when a calendar is given).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawSchedule {
    pub cadence: Cadence,
    pub anchor_date: NaiveDate,
    /// Optional business-calendar code (`us-banking`, `us-tax`, …). When
    /// set, the day-firing decision postpones/skips non-business days
    /// per the cadence's density (see [`schedule_fires_on`]). Absent →
    /// every day is a business day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_calendar: Option<String>,
}

/// Built-in cadence shapes for schedule-triggered rules. Day-granularity
/// only — the dispatcher fires on sim-DAY boundaries, so there are no
/// sub-day cadences here (unlike the simulator's `PeriodicEngine`, which
/// also models `Hourly` / `EveryNMinutes`). Firing math is
/// [`Cadence::fires_on`], ported from that engine.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Cadence {
    Daily,
    Weekly,
    Biweekly,
    Monthly,
    Quarterly,
    Annually,
}

impl Cadence {
    /// Does this cadence fire on `day`, given its `anchor`? Pure
    /// calendar math — business-calendar postponement is applied
    /// *outside* this function by [`schedule_fires_on`]. Ported from
    /// `boss-sim`'s `engines::periodic::Cadence::fires_on` (same
    /// semantics; the dispatcher is Tier-1 and can't depend on the sim).
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
        }
    }

    /// Sparse cadences where a single dropped fire would lose a whole
    /// period — so the firing logic postpones them onto the next
    /// business day instead of skipping. Dense cadences
    /// (daily/weekly/biweekly) skip a non-business day, because "every
    /// Tuesday" means Tuesday, not "Tuesday or the next business day".
    pub fn is_coarse(&self) -> bool {
        matches!(
            self,
            Cadence::Monthly | Cadence::Quarterly | Cadence::Annually
        )
    }

    /// String form stored in the `schedule_cadence` DB column /
    /// authored in TOML (kebab-case). Round-trips with [`Cadence::parse`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Cadence::Daily => "daily",
            Cadence::Weekly => "weekly",
            Cadence::Biweekly => "biweekly",
            Cadence::Monthly => "monthly",
            Cadence::Quarterly => "quarterly",
            Cadence::Annually => "annually",
        }
    }

    /// Parse the `schedule_cadence` DB value. Case-insensitive; accepts
    /// the kebab spelling used in TOML.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "daily" => Some(Cadence::Daily),
            "weekly" => Some(Cadence::Weekly),
            "biweekly" => Some(Cadence::Biweekly),
            "monthly" => Some(Cadence::Monthly),
            "quarterly" => Some(Cadence::Quarterly),
            "annually" => Some(Cadence::Annually),
            _ => None,
        }
    }
}

/// Last calendar day of `(year, month)` — 28–31 (29 in a leap February).
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

/// The anchor's day-of-month clamped into a (possibly shorter) month: a
/// day-31 anchor lands on the 30th in a 30-day month and the 28th/29th
/// in February. Without this a month-end anchor silently never matches
/// the short months (a day-31 anchor never fires in April), losing
/// those periods entirely.
fn clamp_anchor_day(anchor_day: u32, year: i32, month: u32) -> u32 {
    anchor_day.min(last_day_of_month(year, month))
}

/// How many days a sparse-cadence fire may be carried forward to reach a
/// business day. Covers any realistic weekend + holiday closure while
/// staying well under the ~28-day minimum gap between monthly fires, so
/// the look-back in [`schedule_fires_on`] can never reach the previous
/// period's nominal day. Mirrors `boss-sim`'s `MAX_POSTPONE_DAYS`.
const MAX_POSTPONE_DAYS: u64 = 10;

/// THE day-firing decision: does the schedule `sched` fire on sim-day
/// `day`, given its (optional, already-resolved) business calendar?
///
/// Ported from `boss-sim`'s `PeriodicEngine` business-day logic:
/// - **Dense** cadences (Daily/Weekly/Biweekly) fire iff
///   `cadence.fires_on(anchor, day)` AND (no calendar OR
///   `cal.is_business_day(day)`) — a non-business day is *skipped*.
/// - **Sparse** cadences (Monthly/Quarterly/Annually) fire on `day` iff
///   the nominal (clamped) fire date *postpones* to `day` via
///   `business_day_on_or_after` (walking back up to `MAX_POSTPONE_DAYS`)
///   — a non-business nominal day is *carried forward*, not dropped, so
///   a whole period is never lost. With no calendar, a sparse cadence
///   fires exactly on its nominal day.
///
/// Pure: no I/O, deterministic in `(sched, cal, day)`.
pub fn schedule_fires_on(
    sched: &RawSchedule,
    cal: Option<&BusinessCalendar>,
    day: NaiveDate,
) -> bool {
    let anchor = sched.anchor_date;
    if sched.cadence.is_coarse() {
        // Sparse: postpone the nominal fire onto a business day.
        match cal {
            None => sched.cadence.fires_on(anchor, day),
            Some(cal) => {
                if !cal.is_business_day(day) {
                    return false;
                }
                for back in 0..=MAX_POSTPONE_DAYS {
                    let Some(nominal) = day.checked_sub_days(Days::new(back)) else {
                        break;
                    };
                    // `business_day_on_or_after` (not `next_business_day`):
                    // a nominal day that is itself a business day must map
                    // to itself (back==0 fires on the day); a non-business
                    // nominal carries forward to the first business day on
                    // or after it.
                    if sched.cadence.fires_on(anchor, nominal)
                        && cal.business_day_on_or_after(nominal) == day
                    {
                        return true;
                    }
                }
                false
            }
        }
    } else {
        // Dense: a non-business day SKIPS — "every Tuesday" means Tuesday.
        sched.cadence.fires_on(anchor, day) && cal.map(|c| c.is_business_day(day)).unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawDoStep {
    pub handler: String,
    #[serde(default)]
    pub args: HashMap<String, String>,
}

/// Parse the raw TOML rule registry WITHOUT building the runtime Expr
/// trees — the read-only `/api/dispatcher/rules` surface serves the
/// rules verbatim (name, on_event, when, do/args) for the cascade
/// visualization. The runtime dispatch path uses `Registry::from_toml`,
/// which additionally compiles the predicates.
pub fn parse_raw(src: &str) -> Result<RawRegistry, RegistryError> {
    toml::from_str(src).map_err(|e| RegistryError::Toml(e.to_string()))
}

/// Load the ACTIVE dispatcher rules from the `dispatcher_rules` registry
/// table into the raw shape. The dispatcher reads this at startup
/// (replacing the legacy rules.toml file read) and `/api/dispatcher/rules`
/// serves it. `do_steps` is stored as JSONB matching `RawDoStep`.
pub async fn load_active_rules(pool: &sqlx::PgPool) -> Result<RawRegistry, RegistryError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        name: String,
        // Nullable now: a schedule-triggered rule has no `on_event`.
        on_event: Option<String>,
        when_expr: Option<String>,
        do_steps: serde_json::Value,
        delay: Option<String>,
        version: i32,
        // Clock-trigger columns (all-or-nothing): cadence + anchor are
        // both set for a scheduled rule, both NULL for an event rule.
        schedule_cadence: Option<String>,
        schedule_anchor: Option<NaiveDate>,
        schedule_calendar: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT name, on_event, when_expr, do_steps, delay, version, \
                schedule_cadence, schedule_anchor, schedule_calendar \
         FROM dispatcher_rules WHERE status = 'active' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| RegistryError::Storage(e.to_string()))?;

    let mut rules = Vec::with_capacity(rows.len());
    for r in rows {
        let do_steps: Vec<RawDoStep> = serde_json::from_value(r.do_steps)
            .map_err(|e| RegistryError::Storage(format!("do_steps parse: {e}")))?;
        // Reassemble the schedule from its columns. cadence + anchor must
        // both be present; an unknown cadence string is a storage error.
        let schedule = match (r.schedule_cadence, r.schedule_anchor) {
            (Some(cadence), Some(anchor_date)) => {
                let cadence = Cadence::parse(&cadence).ok_or_else(|| {
                    RegistryError::Storage(format!(
                        "rule {:?}: unknown schedule_cadence {cadence:?}",
                        r.name
                    ))
                })?;
                Some(RawSchedule {
                    cadence,
                    anchor_date,
                    business_calendar: r.schedule_calendar,
                })
            }
            (None, None) => None,
            _ => {
                return Err(RegistryError::Storage(format!(
                    "rule {:?}: schedule_cadence and schedule_anchor must both be set or both NULL",
                    r.name
                )));
            }
        };
        rules.push(RawRule {
            name: r.name,
            on_event: r.on_event,
            schedule,
            when: r.when_expr,
            do_steps,
            delay: r.delay,
            version: r.version as u32,
        });
    }
    Ok(RawRegistry { rules })
}

// ---------------------------------------------------------------------------
// Parsed shape — what the matcher walks at runtime
// ---------------------------------------------------------------------------

/// What fires a rule: an incoming event matching a topic pattern, or a
/// clock-driven schedule. Exactly one per rule (the XOR is enforced in
/// `Rule::from_raw`).
#[derive(Debug, Clone)]
pub enum Trigger {
    /// NATS event whose subject matches this pattern (today's path).
    Event(TopicPattern),
    /// Sim-day cadence (the new clock-driven path).
    Schedule(Schedule),
}

/// A validated clock-driven trigger — the parsed counterpart of
/// [`RawSchedule`]. `Cadence` already round-trips, so this is just the
/// raw shape carried into the runtime registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub cadence: Cadence,
    pub anchor_date: NaiveDate,
    pub business_calendar: Option<String>,
}

impl Schedule {
    /// The day-firing decision for this schedule (see
    /// [`schedule_fires_on`]).
    pub fn fires_on(&self, cal: Option<&BusinessCalendar>, day: NaiveDate) -> bool {
        schedule_fires_on(
            &RawSchedule {
                cadence: self.cadence,
                anchor_date: self.anchor_date,
                business_calendar: self.business_calendar.clone(),
            },
            cal,
            day,
        )
    }
}

/// A rule with `when` / `args` already parsed into Expr trees. Built once at
/// registry load; runtime cost is just walking the trees.
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    /// What fires this rule — an event topic or a schedule.
    pub trigger: Trigger,
    pub when: Option<Expr>,
    pub do_steps: Vec<DoStep>,
    pub delay: Option<String>,
    pub version: u32,
}

impl Rule {
    /// The event topic pattern this rule listens for, or `None` if it is
    /// schedule-triggered. Lets the runner collect NATS subscriptions
    /// without matching on the trigger enum at every call site.
    pub fn event_pattern(&self) -> Option<&TopicPattern> {
        match &self.trigger {
            Trigger::Event(p) => Some(p),
            Trigger::Schedule(_) => None,
        }
    }

    /// The schedule this rule fires on, or `None` if it is
    /// event-triggered.
    pub fn schedule(&self) -> Option<&Schedule> {
        match &self.trigger {
            Trigger::Schedule(s) => Some(s),
            Trigger::Event(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DoStep {
    pub handler: String,
    /// Args evaluate at match time. Each entry's value is an Expr tree;
    /// the matcher returns the evaluated values keyed by the same name.
    pub args: Vec<(String, Expr)>,
}

// ---------------------------------------------------------------------------
// Topic patterns
// ---------------------------------------------------------------------------

/// NATS-style hierarchical topic match. Segments separated by `.`:
/// - `*` matches exactly one segment
/// - `>` matches one or more trailing segments (only allowed at the end)
/// - bareword matches that segment literally
///
/// Example: `step.done.*` matches `step.done.procurement` but not
/// `step.done.procurement.detail`. `step.done.>` matches both.
#[derive(Debug, Clone)]
pub struct TopicPattern {
    raw: String,
    segments: Vec<TopicSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TopicSegment {
    Literal(String),
    SingleWildcard,
    TrailingWildcard,
}

impl TopicPattern {
    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let segments: Result<Vec<TopicSegment>, RegistryError> = raw
            .split('.')
            .enumerate()
            .map(|(i, seg)| match seg {
                "*" => Ok(TopicSegment::SingleWildcard),
                ">" => Ok(TopicSegment::TrailingWildcard),
                "" => Err(RegistryError::InvalidTopic {
                    topic: raw.to_string(),
                    reason: "empty segment".into(),
                }),
                lit => {
                    // `>` is only legal as the last segment. Catch a `>` in
                    // a literal slot too — `foo.>.bar` isn't allowed.
                    if lit.contains('>') {
                        return Err(RegistryError::InvalidTopic {
                            topic: raw.to_string(),
                            reason: format!("'>' in non-final segment {i}"),
                        });
                    }
                    Ok(TopicSegment::Literal(lit.to_string()))
                }
            })
            .collect();
        let segments = segments?;
        for (i, seg) in segments.iter().enumerate() {
            if seg == &TopicSegment::TrailingWildcard && i != segments.len() - 1 {
                return Err(RegistryError::InvalidTopic {
                    topic: raw.to_string(),
                    reason: "'>' must be the final segment".into(),
                });
            }
        }
        Ok(TopicPattern {
            raw: raw.to_string(),
            segments,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// True iff `topic` matches this pattern.
    pub fn matches(&self, topic: &str) -> bool {
        let parts: Vec<&str> = topic.split('.').collect();
        let mut i = 0;
        let mut j = 0;
        while i < self.segments.len() {
            match &self.segments[i] {
                TopicSegment::TrailingWildcard => {
                    // Must have at least one remaining segment to match `>`.
                    return j < parts.len();
                }
                TopicSegment::SingleWildcard => {
                    if j >= parts.len() {
                        return false;
                    }
                    i += 1;
                    j += 1;
                }
                TopicSegment::Literal(lit) => {
                    if j >= parts.len() || parts[j] != lit {
                        return false;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        // Pattern fully consumed — exact match only if topic is too.
        j == parts.len()
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Registry {
    rules: Vec<Rule>,
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_toml(src: &str) -> Result<Self, RegistryError> {
        let raw: RawRegistry =
            toml::from_str(src).map_err(|e| RegistryError::Toml(e.to_string()))?;
        Self::from_raw(raw)
    }

    pub fn from_raw(raw: RawRegistry) -> Result<Self, RegistryError> {
        let mut rules = Vec::with_capacity(raw.rules.len());
        for r in raw.rules {
            rules.push(Rule::from_raw(r)?);
        }
        // Per-name uniqueness — same rule shouldn't be authored twice.
        let mut seen = std::collections::HashSet::new();
        for r in &rules {
            if !seen.insert(r.name.clone()) {
                return Err(RegistryError::DuplicateName(r.name.clone()));
            }
        }
        Ok(Registry { rules })
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

impl Rule {
    pub fn from_raw(r: RawRule) -> Result<Self, RegistryError> {
        // A rule is triggered by EXACTLY ONE of `on_event` / `schedule`.
        let trigger = match (&r.on_event, &r.schedule) {
            (Some(topic), None) => Trigger::Event(TopicPattern::parse(topic)?),
            (None, Some(sched)) => Trigger::Schedule(Schedule {
                cadence: sched.cadence,
                anchor_date: sched.anchor_date,
                business_calendar: sched.business_calendar.clone(),
            }),
            (Some(_), Some(_)) => {
                return Err(RegistryError::AmbiguousTrigger(r.name.clone()));
            }
            (None, None) => {
                return Err(RegistryError::MissingTrigger(r.name.clone()));
            }
        };
        let when = match r.when {
            Some(src) => Some(expr::parse(&src).map_err(|e| RegistryError::BadExpr {
                rule: r.name.clone(),
                location: "when".into(),
                src,
                err: e.to_string(),
            })?),
            None => None,
        };
        let mut do_steps = Vec::with_capacity(r.do_steps.len());
        for ds in r.do_steps {
            let mut parsed_args = Vec::with_capacity(ds.args.len());
            // Sort so the evaluation order is deterministic across runs
            // (HashMap iteration order isn't, and downstream assertions
            // expect stable layout).
            let mut arg_pairs: Vec<(String, String)> = ds.args.into_iter().collect();
            arg_pairs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in arg_pairs {
                let e = expr::parse(&v).map_err(|e| RegistryError::BadExpr {
                    rule: r.name.clone(),
                    location: format!("do[].args.{k}"),
                    src: v.clone(),
                    err: e.to_string(),
                })?;
                parsed_args.push((k, e));
            }
            do_steps.push(DoStep {
                handler: ds.handler,
                args: parsed_args,
            });
        }
        Ok(Rule {
            name: r.name,
            trigger,
            when,
            do_steps,
            delay: r.delay,
            version: r.version,
        })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("toml parse: {0}")]
    Toml(String),
    #[error("registry storage: {0}")]
    Storage(String),
    #[error("invalid topic {topic:?}: {reason}")]
    InvalidTopic { topic: String, reason: String },
    #[error("rule {rule:?} {location} could not parse {src:?}: {err}")]
    BadExpr {
        rule: String,
        location: String,
        src: String,
        err: String,
    },
    #[error("duplicate rule name {0:?}")]
    DuplicateName(String),
    #[error("rule {0:?} has neither on_event nor schedule (exactly one required)")]
    MissingTrigger(String),
    #[error("rule {0:?} has both on_event and schedule (exactly one allowed)")]
    AmbiguousTrigger(String),
}

// ---------------------------------------------------------------------------
// Matcher
// ---------------------------------------------------------------------------

/// A rule that matched an incoming event, with its handler invocations'
/// args resolved to concrete Values. What the side-effect handler dispatch
/// layer consumes (in the next pass).
#[derive(Debug, Clone)]
pub struct MatchedRule {
    pub rule_name: String,
    pub invocations: Vec<MatchedInvocation>,
}

#[derive(Debug, Clone)]
pub struct MatchedInvocation {
    pub handler: String,
    pub args: Vec<(String, Value)>,
}

/// Walk the registry against an incoming event and return all matched
/// rules with their args evaluated.
///
/// Pure: same inputs → same outputs. Rule iteration order is registry
/// declaration order so audit_log + tests have a stable layout.
pub fn match_event(
    registry: &Registry,
    topic: &str,
    payload: &serde_json::Value,
    helpers: &dyn HelperResolver,
) -> Result<Vec<MatchedRule>, MatchError> {
    let ctx = expr::Context { payload, helpers };
    let mut out = Vec::new();
    for rule in registry.rules() {
        // Schedule-triggered rules never match an incoming event; the
        // clock-stream task fires them. Skip them here.
        let Some(pattern) = rule.event_pattern() else {
            continue;
        };
        if !pattern.matches(topic) {
            continue;
        }
        if let Some(when) = &rule.when {
            let v = expr::eval(when, &ctx).map_err(|err| MatchError::PredicateFailed {
                rule: rule.name.clone(),
                err,
            })?;
            match v.as_bool() {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    return Err(MatchError::PredicateNotBool {
                        rule: rule.name.clone(),
                        kind: v.kind(),
                    });
                }
            }
        }
        let mut invocations = Vec::with_capacity(rule.do_steps.len());
        for ds in &rule.do_steps {
            let mut args = Vec::with_capacity(ds.args.len());
            for (k, expr) in &ds.args {
                let val = expr::eval(expr, &ctx).map_err(|err| MatchError::ArgFailed {
                    rule: rule.name.clone(),
                    handler: ds.handler.clone(),
                    arg: k.clone(),
                    err,
                })?;
                args.push((k.clone(), val));
            }
            invocations.push(MatchedInvocation {
                handler: ds.handler.clone(),
                args,
            });
        }
        out.push(MatchedRule {
            rule_name: rule.name.clone(),
            invocations,
        });
    }
    Ok(out)
}

#[derive(Debug, Error)]
pub enum MatchError {
    #[error("rule {rule:?} predicate evaluation failed: {err}")]
    PredicateFailed { rule: String, err: expr::EvalError },
    #[error("rule {rule:?} predicate returned non-bool {kind}")]
    PredicateNotBool { rule: String, kind: &'static str },
    #[error("rule {rule:?} handler {handler:?} arg {arg:?} failed: {err}")]
    ArgFailed {
        rule: String,
        handler: String,
        arg: String,
        err: expr::EvalError,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::{HelperResolver, NoHelpers, Value};
    use super::*;
    use serde_json::json;

    /// Date constructor used across the schedule/cadence tests.
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // ----- topic patterns -----

    #[test]
    fn topic_literal_match() {
        let p = TopicPattern::parse("step.done.procurement").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(!p.matches("step.done.billing"));
        assert!(!p.matches("step.done.procurement.extra"));
    }

    #[test]
    fn topic_single_wildcard() {
        let p = TopicPattern::parse("step.done.*").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(p.matches("step.done.billing"));
        assert!(!p.matches("step.done"));
        assert!(!p.matches("step.done.procurement.extra"));
    }

    #[test]
    fn topic_trailing_wildcard() {
        let p = TopicPattern::parse("step.done.>").unwrap();
        assert!(p.matches("step.done.procurement"));
        assert!(p.matches("step.done.procurement.extra"));
        assert!(!p.matches("step.done")); // > requires >=1 trailing
    }

    #[test]
    fn topic_rejects_trailing_in_middle() {
        assert!(matches!(
            TopicPattern::parse("step.>.done"),
            Err(RegistryError::InvalidTopic { .. })
        ));
    }

    #[test]
    fn topic_rejects_empty_segment() {
        assert!(matches!(
            TopicPattern::parse("step..done"),
            Err(RegistryError::InvalidTopic { .. })
        ));
    }

    // ----- registry load -----

    const RESTOCK_RULE_TOML: &str = r#"
[[rule]]
name = "spawn-restock-on-low-inventory"
on_event = "inventory.parts.consumed"
when = "on_hand <= reorder_point AND NOT open_po_exists(part_sku)"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"ingredient-restock\"", subject = "vendor_for(part_sku)" }
"#;

    #[test]
    fn loads_design_doc_canonical_rule() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        assert_eq!(reg.rules().len(), 1);
        let r = &reg.rules()[0];
        assert_eq!(r.name, "spawn-restock-on-low-inventory");
        assert_eq!(
            r.event_pattern().expect("event-triggered").raw(),
            "inventory.parts.consumed"
        );
        assert!(r.when.is_some());
        assert_eq!(r.do_steps.len(), 1);
        assert_eq!(r.do_steps[0].handler, "jobs.spawn");
        assert_eq!(r.do_steps[0].args.len(), 2);
    }

    #[test]
    fn empty_registry_is_legal() {
        let reg = Registry::from_toml("").unwrap();
        assert_eq!(reg.rules().len(), 0);
    }

    #[test]
    fn rejects_malformed_when() {
        let toml = r#"
[[rule]]
name = "bad"
on_event = "x"
when = "a AND"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::BadExpr { location, .. }) if location == "when"
        ));
    }

    #[test]
    fn rejects_duplicate_name() {
        let toml = r#"
[[rule]]
name = "dupe"
on_event = "x"
[[rule]]
name = "dupe"
on_event = "y"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::DuplicateName(_))
        ));
    }

    #[test]
    fn args_order_is_deterministic_across_loads() {
        // Same TOML loaded twice — the (alphabetically) sorted arg order
        // must be stable. Catches HashMap iteration drift.
        let toml = r#"
[[rule]]
name = "r"
on_event = "x"
[[rule.do]]
handler = "h"
args = { zebra = "\"z\"", alpha = "\"a\"", mango = "\"m\"" }
"#;
        let r1 = Registry::from_toml(toml).unwrap();
        let r2 = Registry::from_toml(toml).unwrap();
        let keys1: Vec<&String> = r1.rules()[0].do_steps[0]
            .args
            .iter()
            .map(|(k, _)| k)
            .collect();
        let keys2: Vec<&String> = r2.rules()[0].do_steps[0]
            .args
            .iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys1, keys2);
        assert_eq!(
            keys1,
            vec![
                &"alpha".to_string(),
                &"mango".to_string(),
                &"zebra".to_string()
            ]
        );
    }

    // ----- matcher -----

    struct MockHelpers;

    impl HelperResolver for MockHelpers {
        fn call(&self, name: &str, args: &[Value]) -> Result<Value, expr::EvalError> {
            match name {
                "open_po_exists" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::Bool(sku == "PKG-WITH-PO")),
                    _ => Err(expr::EvalError::TypeError {
                        expected: "string",
                        got: "other",
                    }),
                },
                "vendor_for" => match args.first() {
                    Some(Value::String(sku)) => Ok(Value::String(format!("vnd-for-{sku}"))),
                    _ => Err(expr::EvalError::TypeError {
                        expected: "string",
                        got: "other",
                    }),
                },
                _ => Err(expr::EvalError::UnknownHelper(name.to_string())),
            }
        }
    }

    #[test]
    fn matches_canonical_rule_when_predicate_true() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched =
            match_event(&reg, "inventory.parts.consumed", &payload, &MockHelpers).unwrap();
        assert_eq!(matched.len(), 1);
        let m = &matched[0];
        assert_eq!(m.rule_name, "spawn-restock-on-low-inventory");
        assert_eq!(m.invocations.len(), 1);
        let inv = &m.invocations[0];
        assert_eq!(inv.handler, "jobs.spawn");
        // Args are sorted alphabetically: kind, subject.
        let args: Vec<(&str, &Value)> = inv.args.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(
            args,
            vec![
                ("kind", &Value::String("ingredient-restock".into())),
                ("subject", &Value::String("vnd-for-PKG-CO2-50LB".into())),
            ]
        );
    }

    #[test]
    fn does_not_match_when_predicate_false() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-WITH-PO",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched =
            match_event(&reg, "inventory.parts.consumed", &payload, &MockHelpers).unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn does_not_match_different_topic() {
        let reg = Registry::from_toml(RESTOCK_RULE_TOML).unwrap();
        let payload = json!({
            "part_sku": "PKG-CO2-50LB",
            "on_hand": 10,
            "reorder_point": 20,
        });
        let matched = match_event(&reg, "step.done.procurement", &payload, &MockHelpers).unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn matches_wildcard_topic() {
        let toml = r#"
[[rule]]
name = "log-every-step-done"
on_event = "step.done.*"
[[rule.do]]
handler = "audit.log"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        assert_eq!(
            match_event(&reg, "step.done.procurement", &payload, &NoHelpers)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            match_event(&reg, "step.done.billing", &payload, &NoHelpers)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            match_event(&reg, "step.opened", &payload, &NoHelpers)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn multiple_rules_match_independently() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r2"
on_event = "step.done.>"
[[rule.do]]
handler = "h2"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let payload = json!({});
        let matched = match_event(&reg, "step.done.procurement", &payload, &NoHelpers).unwrap();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].rule_name, "r1");
        assert_eq!(matched[1].rule_name, "r2");
    }

    #[test]
    fn iteration_order_is_registry_declaration_order() {
        let toml = r#"
[[rule]]
name = "second"
on_event = "x"
[[rule.do]]
handler = "h"
[[rule]]
name = "first"
on_event = "x"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let matched = match_event(&reg, "x", &json!({}), &NoHelpers).unwrap();
        // Declaration order: "second" then "first".
        assert_eq!(matched[0].rule_name, "second");
        assert_eq!(matched[1].rule_name, "first");
    }

    #[test]
    fn predicate_non_bool_is_match_error() {
        let toml = r#"
[[rule]]
name = "weird"
on_event = "x"
when = "42"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        assert!(matches!(
            match_event(&reg, "x", &json!({}), &NoHelpers),
            Err(MatchError::PredicateNotBool { .. })
        ));
    }

    // ----- schedule trigger: load + XOR validation -----

    #[test]
    fn loads_schedule_rule_from_toml() {
        let toml = r#"
[[rule]]
name = "daily-bank-sweep"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-04-27"
business_calendar = "us-banking"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"bank-sweep\"", subject_kind = "\"account\"", subject = "\"acct-ops\"" }
"#;
        let reg = Registry::from_toml(toml).unwrap();
        assert_eq!(reg.rules().len(), 1);
        let r = &reg.rules()[0];
        assert!(r.event_pattern().is_none(), "schedule rule has no topic");
        let sched = r.schedule().expect("schedule-triggered");
        assert_eq!(sched.cadence, Cadence::Daily);
        assert_eq!(sched.anchor_date, d(2026, 4, 27));
        assert_eq!(sched.business_calendar.as_deref(), Some("us-banking"));
    }

    #[test]
    fn schedule_rule_without_calendar_loads() {
        let toml = r#"
[[rule]]
name = "monthly-close"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-31"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let sched = reg.rules()[0].schedule().unwrap();
        assert_eq!(sched.cadence, Cadence::Monthly);
        assert_eq!(sched.business_calendar, None);
    }

    #[test]
    fn rejects_rule_with_both_on_event_and_schedule() {
        let toml = r#"
[[rule]]
name = "ambiguous"
on_event = "x"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::AmbiguousTrigger(_))
        ));
    }

    #[test]
    fn rejects_rule_with_neither_trigger() {
        let toml = r#"
[[rule]]
name = "untriggered"
[[rule.do]]
handler = "h"
"#;
        assert!(matches!(
            Registry::from_toml(toml),
            Err(RegistryError::MissingTrigger(_))
        ));
    }

    #[test]
    fn schedule_rule_serializes_without_on_event() {
        // The seed-matches-toml drift guard compares serialized RawRule
        // structurally — a schedule rule must omit `on_event` and carry
        // `schedule`, an event rule the reverse. Guard both shapes.
        let raw_sched = RawRule {
            name: "s".into(),
            on_event: None,
            schedule: Some(RawSchedule {
                cadence: Cadence::Weekly,
                anchor_date: d(2026, 4, 27),
                business_calendar: None,
            }),
            when: None,
            do_steps: vec![],
            delay: None,
            version: 1,
        };
        let v = serde_json::to_value(&raw_sched).unwrap();
        assert!(v.get("on_event").is_none(), "schedule rule omits on_event");
        assert_eq!(v["schedule"]["cadence"], "weekly");

        let raw_event = RawRule {
            name: "e".into(),
            on_event: Some("step.done.x".into()),
            schedule: None,
            when: None,
            do_steps: vec![],
            delay: None,
            version: 1,
        };
        let v = serde_json::to_value(&raw_event).unwrap();
        assert_eq!(v["on_event"], "step.done.x");
        assert!(v.get("schedule").is_none(), "event rule omits schedule");
    }

    #[test]
    fn schedule_rules_have_no_event_subscription() {
        // A schedule rule must not pull a NATS subscription — the runner
        // would otherwise subscribe to a bogus topic.
        let toml = r#"
[[rule]]
name = "sched"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        assert!(reg.rules()[0].event_pattern().is_none());
        // match_event ignores it entirely.
        assert!(
            match_event(&reg, "anything.at.all", &json!({}), &NoHelpers)
                .unwrap()
                .is_empty()
        );
    }

    // ----- Cadence::fires_on — ported from boss-sim periodic.rs -----

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
            let expected = day.weekday() == chrono::Weekday::Mon;
            assert_eq!(Cadence::Weekly.fires_on(anchor, day), expected);
        }
    }

    #[test]
    fn biweekly_fires_every_other_anchor_weekday() {
        let anchor = d(2026, 4, 27); // Monday
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
    fn quarterly_fires_every_three_months_with_month_end_clamp() {
        let anchor = d(2026, 1, 31);
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 1, 31)));
        // April has no 31st → a day-31 anchor CLAMPS to Apr 30.
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 4, 30)));
        assert!(!Cadence::Quarterly.fires_on(anchor, d(2026, 4, 29)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 7, 31)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2026, 10, 31)));
        assert!(Cadence::Quarterly.fires_on(anchor, d(2027, 1, 31)));
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
    }

    #[test]
    fn cadence_parse_round_trips() {
        for c in [
            Cadence::Daily,
            Cadence::Weekly,
            Cadence::Biweekly,
            Cadence::Monthly,
            Cadence::Quarterly,
            Cadence::Annually,
        ] {
            assert_eq!(Cadence::parse(c.as_str()), Some(c));
        }
        assert_eq!(Cadence::parse("MONTHLY"), Some(Cadence::Monthly));
        assert_eq!(Cadence::parse("  weekly "), Some(Cadence::Weekly));
        assert_eq!(Cadence::parse("never"), None);
    }

    // ----- schedule_fires_on — the day-firing decision -----

    /// A weekday-only calendar (Sat+Sun off) with a couple of holidays,
    /// mirroring the kind of calendar boss-calendar seeds.
    fn weekdays_cal() -> BusinessCalendar {
        BusinessCalendar::new("weekdays-only", "Weekdays Only").with_closed([
            d(2026, 5, 25), // a Monday "holiday"
            d(2026, 7, 3),  // a Friday "holiday"
        ])
    }

    fn sched(cadence: Cadence, anchor: NaiveDate, cal: Option<&str>) -> RawSchedule {
        RawSchedule {
            cadence,
            anchor_date: anchor,
            business_calendar: cal.map(String::from),
        }
    }

    #[test]
    fn dense_no_calendar_fires_on_nominal() {
        // Daily with no calendar fires every day on/after the anchor,
        // weekends included.
        let s = sched(Cadence::Daily, d(2026, 4, 27), None);
        assert!(schedule_fires_on(&s, None, d(2026, 4, 27)));
        assert!(schedule_fires_on(&s, None, d(2026, 5, 2))); // Saturday — fires
        assert!(!schedule_fires_on(&s, None, d(2026, 4, 26))); // before anchor
    }

    #[test]
    fn dense_with_calendar_skips_weekend_and_holiday() {
        let cal = weekdays_cal();
        let s = sched(Cadence::Daily, d(2026, 4, 27), Some("weekdays-only"));
        // Monday: fires.
        assert!(schedule_fires_on(&s, Some(&cal), d(2026, 4, 27)));
        // Saturday/Sunday: SKIPPED (not postponed) for dense cadences.
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 5, 2)));
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 5, 3)));
        // Holiday Monday 2026-05-25: skipped; no make-up on the 26th
        // (Daily already fires the 26th on its own).
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 5, 25)));
        assert!(schedule_fires_on(&s, Some(&cal), d(2026, 5, 26)));
    }

    #[test]
    fn dense_weekly_skips_when_anchor_weekday_is_holiday() {
        // Weekly anchored on Monday; the holiday Monday is skipped with
        // NO make-up (dense semantics) — "every Monday" means Monday.
        let cal = weekdays_cal();
        let s = sched(Cadence::Weekly, d(2026, 4, 27), Some("weekdays-only")); // Mon
        assert!(schedule_fires_on(&s, Some(&cal), d(2026, 5, 18))); // a normal Mon
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 5, 25))); // holiday Mon — skipped
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 5, 26))); // Tue — no make-up
    }

    #[test]
    fn sparse_no_calendar_fires_exactly_on_nominal() {
        // Monthly with no calendar: nominal day only, weekend or not.
        let s = sched(Cadence::Monthly, d(2026, 1, 31), None);
        assert!(schedule_fires_on(&s, None, d(2026, 1, 31)));
        assert!(schedule_fires_on(&s, None, d(2026, 4, 30))); // clamped
        assert!(!schedule_fires_on(&s, None, d(2026, 2, 2)));
    }

    #[test]
    fn sparse_postpones_off_weekend() {
        // Quarterly day-31 on a weekday-only calendar. 2026-01-31 is a
        // Saturday, so the Jan-quarter fire carries forward to Monday
        // 2026-02-02 — not dropped for the whole quarter.
        let cal = weekdays_cal();
        let s = sched(Cadence::Quarterly, d(2024, 1, 31), Some("weekdays-only"));
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 1, 31))); // Sat — nominal
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 2, 1))); // Sun
        assert!(schedule_fires_on(&s, Some(&cal), d(2026, 2, 2))); // Mon — postponed
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 2, 3))); // Tue — no double-fire
        // A weekday nominal fires on the day itself (2025-07-31 is a Thu).
        assert!(schedule_fires_on(&s, Some(&cal), d(2025, 7, 31)));
        // The clamped April nominal fires (2025-04-30 is a Wed).
        assert!(schedule_fires_on(&s, Some(&cal), d(2025, 4, 30)));
        // A plain non-nominal business day never fires.
        assert!(!schedule_fires_on(&s, Some(&cal), d(2025, 6, 16)));
    }

    #[test]
    fn sparse_postpones_off_holiday() {
        // Annually anchored on 2026-07-03 (a Friday) which IS a holiday
        // on the cal → postpones to the next business day, Monday 07-06
        // (Sat 07-04 + Sun 07-05 are weekend).
        let cal = weekdays_cal();
        let s = sched(Cadence::Annually, d(2026, 7, 3), Some("weekdays-only"));
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 7, 3))); // holiday Fri
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 7, 4))); // Sat
        assert!(!schedule_fires_on(&s, Some(&cal), d(2026, 7, 5))); // Sun
        assert!(schedule_fires_on(&s, Some(&cal), d(2026, 7, 6))); // Mon — postponed
    }

    #[test]
    fn schedule_method_matches_free_fn() {
        let cal = weekdays_cal();
        let s = Schedule {
            cadence: Cadence::Quarterly,
            anchor_date: d(2024, 1, 31),
            business_calendar: Some("weekdays-only".into()),
        };
        assert!(s.fires_on(Some(&cal), d(2026, 2, 2)));
        assert!(!s.fires_on(Some(&cal), d(2026, 1, 31)));
    }
}
