//! `jobs.urgent_item_watch` — an URGENT backlog item that is claimed
//! with nothing executing it is released, and one that runs for days
//! without converging asks for a finish line (backlog 9788e22d).
//!
//! THE CASE. Measured 2026-10-18 at David's prompt ("it has taken weeks
//! now to actually fix these urgent issues"): of 263 urgent items
//! delivered in 45 days the median took 0.32 days and the p90 3.6, but
//! four open ones were 12 to 23 days old. Three had run many builder
//! runs and landed several cars each without closing (2710c8fc ~89
//! runs and 7 cars, b3071bf4 ~46 runs and 5 landed, e1943ef0 ~28 runs
//! and 2 landed), because nothing checkable said when they were done.
//! One (d4c0cd2d) sat claimed for twelve days with no car and no run.
//! Two rules sat beside the gap and neither covered it, each on
//! purpose: `agent-held-real-work-is-watched-hourly` leaves
//! `backlog-item` undeclared so ~200 standard builds do not alarm, and
//! `an-abandoned-step-is-reclaimed-when-its-run-died` frees a step only
//! when the run NAMED on it was recorded `died` — a claim that never
//! had a run, or whose run ended `refused`, is never freed.
//!
//! SCOPED BY PRIORITY, AND THE SCOPE IS THE RULE'S ARGS. `priorities`
//! names which items are read (the shipped rule: `urgent,emergency`);
//! everything else is not read at all, so a standard build is never
//! released and never alarmed — the flat list the overdue alarm stands
//! apart from stays unshouted. The three bounds ride beside it as
//! registry data (`release_after_hours`, `rescope_after_days`,
//! `rescope_runs_over`), editable through `/api/dispatcher/rules` with
//! no deploy.
//!
//! HALF ONE — A CLAIM NOTHING EXECUTES IS RELEASED. The item's `build`
//! step is `active` on an id the agents registry names, NO agent-run
//! for the item is open, NO car naming it is in flight (open and not
//! landed — a run that ended `gated` waits on its train, honestly, and
//! is the commonest shape: 13 of 16 runless urgent claims measured
//! 2026-10-18), and nothing about the item, its runs or its cars has
//! moved for longer than `release_after_hours`. The step goes back to
//! `ready` and nobody's — the state a claim takes it from — through the
//! same two writes the death rule's reclaim uses (`release_patch`, then
//! `release_put`): the stale `agent_run` edge comes off, and the
//! evidence lands under [`RELEASE_KEY`] naming the claim it freed and
//! why. A claim stamps no instant, so the idle time is measured from
//! the newest movement the record holds; for a claim that never had a
//! run that is the item's own last step, which can make the release
//! early for a step claimed long after its triage. The bound is six
//! hours of nothing, so an executor that is working leaves a run moving
//! well inside it.
//!
//! HALF TWO — A LONG ONE ASKS FOR A FINISH LINE. An item open longer
//! than `rescope_after_days` that has landed at least one car, or that
//! carries more than `rescope_runs_over` agent runs, files ONE alarm
//! naming the item, its age and its run and car counts, and asking for
//! `finish_line` metadata — a checkable statement of done — instead of
//! another run. An item that already states one is not asked. The dedup
//! read takes EVERY status, so an item is asked once, ever: an open
//! alarm is not refiled, and a person's close is final (the
//! `jobs.agent_step_overdue` rule for the same reason — no tick nags).
//! The alarm is itself an urgent backlog-item, and carries
//! [`ITEM_KEY`], which keeps it out of its own watch.
//!
//! COUNTS ARE READ, NEVER TRUSTED. Runs and cars are read per item by
//! metadata containment (`metadata={"packet": <id>}`), and every row is
//! re-checked against the item: a server that ignored the filter
//! answers the unfiltered page, and counting it would alarm on every
//! item — so a row naming another packet refuses the tick by name.
//! Cars are read under `backlog_item` and `partial_item`; a car that
//! answers the item only through `also_answers` is seen IN FLIGHT (the
//! open-car read covers all three keys) but not counted as landed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    api_client, empty_roster_refusal, get_json, jobs_where_slim, owner_for_filing, post_json,
    row_or_refuse, rows_or_refuse, with_lane, write_json,
};
use super::jobs_age_out_step::last_moved;
use super::jobs_agent_step_overdue::agent_identities;
use super::jobs_reclaim_abandoned_step::{release_patch, release_put};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.urgent_item_watch";

/// The packet kind watched, and the one step of it a release frees.
const ITEM_KIND: &str = "backlog-item";
const BUILD: &str = "build";

/// The run kind and the key on it naming the packet it executes
/// (infra/platform/workflows/agent-run.toml: `packet`, written by
/// `boss dispatch`).
const RUN_KIND: &str = "agent-run";
const RUN_PACKET: &str = "packet";

/// The car kind.
const CAR_KIND: &str = "ship-a-change";

/// The run edge `boss dispatch` writes on the step it claims
/// (dd6d44b7); a release clears it, as the reclaim does.
const LINK: &str = "agent_run";

/// Where a release's evidence lands on the freed step — not the death
/// rule's `reclaimed` and not the hand verb's `released`, so a reader
/// can tell which door freed it.
pub const RELEASE_KEY: &str = "released_unrun";

/// The alarm's dedup key: the item it asks about.
pub const ITEM_KEY: &str = "rescope_item";

/// The item metadata key the alarm asks for, and whose presence answers
/// it: a checkable statement of when the item is done.
pub const FINISH_LINE_KEY: &str = "finish_line";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "rescope-urgent-item";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick (`schedule_runner::tick`).
const TICK_AT: &str = "_at";

/// Is `word` a job priority, as the jobs API spells one? Read off
/// `boss_core::job::Priority`'s own serde form rather than a copy of
/// its words (CLAUDE.md §9a). A `priorities` word that is not one would
/// match nothing — a watch that is not running — so it refuses by name.
fn is_priority(word: &str) -> bool {
    serde_json::from_value::<boss_core::job::Priority>(json!(word)).is_ok()
}

/// The rule's declaration: which priorities are read, and the three
/// bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounds {
    pub priorities: BTreeSet<String>,
    pub release_after_hours: i64,
    pub rescope_after_days: i64,
    pub rescope_runs_over: i64,
}

/// Parse the rule's args. Every one is required — a bound left out is a
/// half of the watch that is not running — and a malformed one refuses
/// by name rather than vanishing.
pub fn bounds(args: &[(String, Value)]) -> Result<Bounds, String> {
    let count = |name: &str| match arg(args, name) {
        Some(Value::Int(n)) if *n > 0 => Ok(*n),
        Some(other) => Err(format!(
            "`{name}` must be a positive whole number, got a {} ({other:?})",
            other.kind()
        )),
        None => Err(format!(
            "`{name}` is not declared on the rule — every bound is the rule's to state"
        )),
    };
    let priorities: BTreeSet<String> = match arg(args, "priorities") {
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        Some(other) => {
            return Err(format!(
                "`priorities` must be a comma-separated string of job priorities, got a {} \
                 ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`priorities` is not declared on the rule".into()),
    };
    if priorities.is_empty() {
        return Err("`priorities` names no priority — the watch would read nothing".into());
    }
    if let Some(bad) = priorities.iter().find(|p| !is_priority(p)) {
        return Err(format!(
            "`priorities` names `{bad}`, which is not a job priority — it would match no item"
        ));
    }
    Ok(Bounds {
        priorities,
        release_after_hours: count("release_after_hours")?,
        rescope_after_days: count("rescope_after_days")?,
        rescope_runs_over: count("rescope_runs_over")?,
    })
}

/// Is this listed packet one the watch reads: of a declared priority,
/// and not one of this handler's own alarms.
fn watched(item: &Json, b: &Bounds) -> bool {
    item.get("priority")
        .and_then(Json::as_str)
        .is_some_and(|p| b.priorities.contains(p))
        && item.pointer(&format!("/metadata/{ITEM_KEY}")).is_none()
}

fn text<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}

fn build_step(item: &Json) -> Option<&Json> {
    item.get("steps")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .find(|s| text(s, "spec_slug") == BUILD)
}

/// The item's `build` step, when it is CLAIMED by an agent.
fn agent_claim<'a>(item: &'a Json, agents: &BTreeSet<String>) -> Option<&'a Json> {
    build_step(item)
        .filter(|s| text(s, "status") == "active" && agents.contains(text(s, "assignee_id")))
}

fn finish_line_stated(item: &Json) -> bool {
    item.pointer(&format!("/metadata/{FINISH_LINE_KEY}"))
        .is_some_and(|v| match v {
            Json::Null => false,
            Json::String(s) => !s.trim().is_empty(),
            _ => true,
        })
}

fn opened(item: &Json) -> Option<DateTime<Utc>> {
    [item.pointer("/metadata/opened_at"), item.get("opened_at")]
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .find_map(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn hours(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_seconds() as f64 / 3600.0
}

/// Past the age bound with no finish line stated — an item whose run
/// and car counts are worth reading.
fn aged(item: &Json, b: &Bounds, now: DateTime<Utc>) -> bool {
    !finish_line_stated(item)
        && opened(item).is_some_and(|t| hours(t, now) > (b.rescope_after_days * 24) as f64)
}

/// The car keys that name an item. `also_answers` is a list, the
/// other two one id each (`boss_jobs::car`).
fn names_item(car: &Json, item: &str) -> bool {
    let md = car.get("metadata");
    [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM]
        .iter()
        .any(|k| md.and_then(|m| m.get(*k)).and_then(Json::as_str) == Some(item))
        || md
            .and_then(|m| m.get(boss_jobs::car::ALSO_ANSWERS))
            .and_then(Json::as_array)
            .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(item)))
}

/// Every item an OPEN car that has not landed names — the cars still
/// in flight. A landed car stays open in the shed for its proof and
/// review, and is not executing anything for the item.
pub fn in_flight(open_cars: &[Json]) -> BTreeSet<String> {
    let md_ids = |car: &Json| -> Vec<String> {
        let md = car.get("metadata");
        let single = [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM]
            .into_iter()
            .filter_map(|k| md.and_then(|m| m.get(k)).and_then(Json::as_str));
        let listed = md
            .and_then(|m| m.get(boss_jobs::car::ALSO_ANSWERS))
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str);
        single.chain(listed).map(str::to_string).collect()
    };
    open_cars
        .iter()
        .filter(|c| text(c, "status") == "open" && !boss_jobs::car::is_landed(c))
        .flat_map(md_ids)
        .filter(|id| !id.is_empty())
        .collect()
}

/// `metadata={"<key>":"<id>"}` percent-encoded for a list query — the
/// containment door (`crate::metadata_containment` in boss-jobs). The
/// id is a packet id; anything outside `[A-Za-z0-9-]` is refused rather
/// than encoded, because nothing else is one.
fn containment(key: &str, id: &str) -> Result<String, HandlerError> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(HandlerError::Downstream(format!(
            "the open backlog listed a packet whose id {id:?} is not a packet id"
        )));
    }
    Ok(format!("metadata=%7B%22{key}%22%3A%22{id}%22%7D"))
}

/// One claim the record says nothing is executing, past the bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub item: String,
    pub step_id: String,
    pub assignee: String,
    pub since: DateTime<Utc>,
    pub idle_hours: f64,
    pub bound_hours: i64,
}

/// One item past its age bound that has not converged.
#[derive(Debug, Clone, PartialEq)]
pub struct Rescope {
    pub item: String,
    pub title: String,
    pub priority: String,
    pub opened: DateTime<Utc>,
    pub age_days: f64,
    pub runs: usize,
    pub landed_cars: usize,
    pub bound_days: i64,
    pub bound_runs: i64,
}

/// What the record holds about one item's executions: its agent runs
/// and the cars that name it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct History {
    pub runs: usize,
    pub open_runs: usize,
    pub run_ids: BTreeSet<String>,
    pub landed_cars: usize,
    /// The newest movement across every run and car — a step completed,
    /// a packet closed, a packet opened.
    pub last_moved: Option<DateTime<Utc>>,
}

/// Fold one item's run rows and car rows into its [`History`].
///
/// Every row is re-checked against the item — a run must name it as its
/// `packet`, a car under one of its item keys — and one that does not
/// is an error naming the row: the containment filter was not applied,
/// and the page in hand is somebody else's. A car read under two keys
/// can return one car twice; it is counted once.
pub fn history(item: &str, runs: &[Json], cars: &[Json]) -> Result<History, String> {
    let mut h = History::default();
    for run in runs {
        let packet = run
            .pointer(&format!("/metadata/{RUN_PACKET}"))
            .and_then(Json::as_str)
            .unwrap_or("");
        if packet != item {
            return Err(format!(
                "the agent-run read for item {item} answered run {} of packet {packet:?} — the \
                 metadata filter was not applied, and counting the page would judge every item \
                 by another's runs",
                text(run, "id")
            ));
        }
        if h.run_ids.insert(text(run, "id").to_string()) {
            h.runs += 1;
            if text(run, "status") == "open" {
                h.open_runs += 1;
            }
            h.last_moved = h.last_moved.max(last_moved(run, Some("closed_at")));
        }
    }
    let mut seen = BTreeSet::new();
    for car in cars {
        if !names_item(car, item) {
            return Err(format!(
                "the car read for item {item} answered car {} that names another item — the \
                 metadata filter was not applied",
                text(car, "id")
            ));
        }
        if seen.insert(text(car, "id").to_string()) {
            if boss_jobs::car::is_landed(car) {
                h.landed_cars += 1;
            }
            h.last_moved = h.last_moved.max(last_moved(car, Some("closed_at")));
        }
    }
    Ok(h)
}

/// The release one item is due, if any. Pure. `in_flight` is every item
/// an open, unlanded car names ([`in_flight`]).
pub fn release_due(
    item: &Json,
    h: &History,
    agents: &BTreeSet<String>,
    in_flight: &BTreeSet<String>,
    b: &Bounds,
    now: DateTime<Utc>,
) -> Option<Release> {
    let id = text(item, "id");
    let step = agent_claim(item, agents)?;
    if h.open_runs > 0 || in_flight.contains(id) {
        return None;
    }
    let since = last_moved(item, None).max(h.last_moved)?;
    let idle_hours = hours(since, now);
    if idle_hours <= b.release_after_hours as f64 {
        return None;
    }
    Some(Release {
        item: id.to_string(),
        step_id: text(step, "id").to_string(),
        assignee: text(step, "assignee_id").to_string(),
        since,
        idle_hours: (idle_hours * 10.0).round() / 10.0,
        bound_hours: b.release_after_hours,
    })
}

/// The rescope alarm one item is due, if any. Pure: past the age bound,
/// no finish line stated, and either a landed car or more runs than the
/// bound.
pub fn rescope_due(item: &Json, h: &History, b: &Bounds, now: DateTime<Utc>) -> Option<Rescope> {
    if !aged(item, b, now) {
        return None;
    }
    if h.landed_cars == 0 && h.runs as i64 <= b.rescope_runs_over {
        return None;
    }
    let opened = opened(item)?;
    Some(Rescope {
        item: text(item, "id").to_string(),
        title: text(item, "title").to_string(),
        priority: text(item, "priority").to_string(),
        opened,
        age_days: (hours(opened, now) / 24.0 * 10.0).round() / 10.0,
        runs: h.runs,
        landed_cars: h.landed_cars,
        bound_days: b.rescope_after_days,
        bound_runs: b.rescope_runs_over,
    })
}

/// The alarm one rescope files.
pub fn alarm_body(r: &Rescope, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let id8 = &r.item[..r.item.len().min(8)];
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 9788e22d) at {now}: {priority} item \
         {item} ({title}) has been open {age} days, since {opened}, past the {days} days this \
         rule declares; it has landed {cars} car(s) and carried {runs} agent run(s) (the bound \
         is more than {bound_runs}) without closing. Another run is not the answer — nothing \
         checkable says when this item is done. Write its finish line: `boss job patch {item} \
         <file.json>` with {{\"{FINISH_LINE_KEY}\": \"<the observable fact that closes it — a \
         test that passes, a probe that prints, a count that reaches zero>\"}}, then rescope or \
         split what remains. This alarm is filed once per item; it refuses nothing and \
         dispatches nothing.",
        now = now.to_rfc3339(),
        priority = r.priority,
        item = r.item,
        title = r.title,
        age = r.age_days,
        opened = r.opened.to_rfc3339(),
        days = r.bound_days,
        cars = r.landed_cars,
        runs = r.runs,
        bound_runs = r.bound_runs,
    );
    let metadata = json!({
        "area": "delivery",
        "scope": SCOPE,
        ITEM_KEY: r.item,
        "item_title": r.title,
        "item_priority": r.priority,
        "age_days": r.age_days,
        "runs": r.runs,
        "landed_cars": r.landed_cars,
        "bound_days": r.bound_days,
        "bound_runs": r.bound_runs,
        "asks": FINISH_LINE_KEY,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "RESCOPE: {} item {id8} open {}d, {} runs, {} cars landed — state its finish line",
            r.priority, r.age_days, r.runs, r.landed_cars
        ),
        "subject": {"subject_kind": "custom", "id": r.item},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// Every alarm this handler has filed, keyed by the item it names.
fn alarms_by_item(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter_map(|row| {
            let item = row
                .pointer(&format!("/metadata/{ITEM_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((item.to_string(), row.clone()))
        })
        .collect()
}

pub struct JobsUrgentItemWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsUrgentItemWatch {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// One item's runs and the cars that name it, by containment.
    async fn read_history(&self, item: &str, rule: &str) -> Result<History, HandlerError> {
        let runs = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind={RUN_KIND}&{}", containment(RUN_PACKET, item)?),
            rule,
        )
        .await?;
        let mut cars = Vec::new();
        for key in [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM] {
            cars.extend(
                jobs_where_slim(
                    &self.client,
                    self.base(),
                    &format!("kind={CAR_KIND}&{}", containment(key, item)?),
                    rule,
                )
                .await?,
            );
        }
        history(item, &runs, &cars).map_err(HandlerError::Downstream)
    }

    /// Free one claim: re-read the item whole (the listing was slim),
    /// confirm the claim is the one judged, then the edge off and the
    /// evidence on through the merge door, then the status and assignee
    /// alone — the reclaim's two writes, in the reclaim's order
    /// (ce3a4b16).
    async fn release(
        &self,
        r: &Release,
        h: &History,
        priority: &str,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        let row = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), r.item),
            rule,
        )
        .await?;
        let row = row_or_refuse(row, &format!("GET /api/jobs/{}", r.item))
            .map_err(HandlerError::Downstream)?;
        let Some(step) = build_step(&row).filter(|s| {
            text(s, "id") == r.step_id
                && text(s, "status") == "active"
                && text(s, "assignee_id") == r.assignee
        }) else {
            tracing::info!(rule, item = %r.item, "{HANDLER}: the claim moved since the listing; left");
            return Ok(false);
        };
        let edge = step
            .pointer(&format!("/metadata/{LINK}"))
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty());
        // An edge naming a run this item's history does not hold is a
        // claim this handler cannot judge — a run of some other packet,
        // or a read that missed one. Left, and said.
        if let Some(e) = edge
            && !h.run_ids.contains(e)
        {
            tracing::warn!(
                rule,
                item = %r.item,
                run = e,
                "{HANDLER}: the claim names a run this item's agent-run read does not hold; left"
            );
            return Ok(false);
        }
        let evidence = json!({
            "from_assignee": r.assignee,
            "from_run": edge,
            "runs": h.runs,
            "idle_since": r.since.to_rfc3339(),
            "idle_hours": r.idle_hours,
            "after_hours": r.bound_hours,
            "why": format!(
                "claimed by {} with no open agent-run and no car in flight, and nothing about \
                 the item, its runs or its cars has moved for {}h — past the {}h this rule \
                 declares for a {priority} item. A claim nothing executes holds the item out \
                 of every queue (backlog 9788e22d); it is back at `ready`, nobody's, for the \
                 next dispatch.",
                r.assignee, r.idle_hours, r.bound_hours
            ),
            "at": now.to_rfc3339(),
            "rule": rule,
        });
        let url = format!("{}/api/jobs/{}/steps/{}", self.base(), r.item, r.step_id);
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{url}/metadata"),
            &release_patch(LINK, RELEASE_KEY, evidence),
            rule,
        )
        .await?;
        write_json(
            &self.client,
            reqwest::Method::PUT,
            &url,
            &release_put(),
            rule,
        )
        .await?;
        Ok(true)
    }
}

#[async_trait]
impl Handler for JobsUrgentItemWatch {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let b = bounds(args).map_err(HandlerError::Permanent)?;
        // The tick's own instant. Absent on a daily firing — a bound of
        // hours judged once a day is the same silence one level up.
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} judges hours and needs a \
                     sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        let listing = get_json(&self.client, &format!("{}/api/agents", self.base()), rule).await?;
        let agents: Vec<Json> =
            rows_or_refuse(&listing, "GET /api/agents").map_err(HandlerError::Downstream)?;
        let agents = agent_identities(&agents);
        if agents.is_empty() {
            return Err(empty_roster_refusal(
                HANDLER,
                "agents",
                "every instance runs at least one registered agent, and an empty read is a dark \
                 or narrowed registry — judging it would read every claim as a person's",
            ));
        }

        let items: Vec<Json> = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind={ITEM_KIND}&status=open"),
            rule,
        )
        .await?
        .into_iter()
        .filter(|it| watched(it, &b))
        .collect();
        let flying = in_flight(
            &jobs_where_slim(
                &self.client,
                self.base(),
                &format!("kind={CAR_KIND}&status=open"),
                rule,
            )
            .await?,
        );
        // EVERY status: an item asked once is never asked again.
        let asked = alarms_by_item(
            &jobs_where_slim(
                &self.client,
                self.base(),
                &format!("kind={ITEM_KIND}&metadata_has={ITEM_KEY}"),
                rule,
            )
            .await?,
        );

        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let (mut released, mut raised) = (0usize, 0usize);
        for item in &items {
            let id = text(item, "id");
            let claimed = agent_claim(item, &agents).is_some() && !flying.contains(id);
            let long = !asked.contains_key(id) && aged(item, &b, now);
            if !claimed && !long {
                continue;
            }
            let h = self.read_history(id, rule).await?;
            if claimed
                && let Some(r) = release_due(item, &h, &agents, &flying, &b, now)
                && self
                    .release(&r, &h, text(item, "priority"), now, rule)
                    .await?
            {
                released += 1;
            }
            if long && let Some(r) = rescope_due(item, &h, &b, now) {
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &alarm_body(&r, &owner, now, rule),
                    rule,
                )
                .await?;
                raised += 1;
            }
        }
        tracing::info!(
            rule,
            watched = items.len(),
            released,
            raised,
            "{HANDLER}: read the {} item(s) of priority {:?}",
            items.len(),
            b.priorities
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-10-18T18:00:00+00:00";
    const ITEM: &str = "item-1";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "an-urgent-item-is-released-when-unrun-and-rescoped-when-long".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-10-18", "_at": NOW}),
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            (
                "priorities".to_string(),
                Value::String("urgent,emergency".into()),
            ),
            ("release_after_hours".to_string(), Value::Int(6)),
            ("rescope_after_days".to_string(), Value::Int(3)),
            ("rescope_runs_over".to_string(), Value::Int(10)),
        ]
    }

    fn b() -> Bounds {
        bounds(&args()).unwrap()
    }

    fn agents() -> BTreeSet<String> {
        agent_identities(&[json!({"id": "agent-claude-b", "aliases": ["claude-b@algedonic.dev"]})])
    }

    /// A backlog item, triaged to build at `opened_at`, its `build` step
    /// in `build_status` on `assignee`, at `priority`.
    fn item(priority: &str, opened_at: &str, build_status: &str, assignee: &str) -> Json {
        json!({
            "id": ITEM, "kind": "backlog-item", "status": "open", "priority": priority,
            "title": "An item that will not converge",
            "metadata": {"opened_at": opened_at},
            "steps": [
                {"id": format!("{ITEM}-triage"), "spec_slug": "triage", "status": "completed",
                 "completed_at": opened_at},
                {"id": format!("{ITEM}-build"), "spec_slug": "build", "status": build_status,
                 "assignee_id": assignee},
            ],
        })
    }

    /// An agent-run of the item, closed at `closed_at` with `result`, or
    /// open when `closed_at` is `None`.
    fn run(id: &str, opened_at: &str, closed_at: Option<&str>, result: &str) -> Json {
        let building = match closed_at {
            Some(t) => json!({"id": format!("{id}-building"), "spec_slug": "building",
                              "status": "completed", "completed_at": t}),
            None => json!({"id": format!("{id}-building"), "spec_slug": "building",
                           "status": "active"}),
        };
        json!({
            "id": id, "kind": "agent-run",
            "status": if closed_at.is_some() { "closed" } else { "open" },
            "metadata": {"packet": ITEM, "step": "build", "agent": "agent-claude-b",
                         "opened_at": opened_at, "outcome": result},
            "steps": [building],
        })
    }

    /// A car naming the item under `key`, landed or not, open or not.
    fn car(id: &str, key: &str, status: &str, merged: bool) -> Json {
        json!({
            "id": id, "kind": "ship-a-change", "status": status,
            "metadata": {key: ITEM, "branch": format!("fix/{id}"), "opened_at": "2026-10-10T00:00:00Z",
                         "merged": if merged { "true" } else { "false" }},
            "steps": [],
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    /// `metadata={"<key>":"<ITEM>"}`, encoded as the handler sends it.
    fn contains(kind: &str, key: &str) -> &'static str {
        Box::leak(
            format!("/api/jobs?kind={kind}&metadata=%7B%22{key}%22%3A%22{ITEM}%22%7D")
                .into_boxed_str(),
        )
    }

    // -- the declaration -------------------------------------------------

    #[test]
    fn the_declaration_is_a_priority_scope_and_three_positive_bounds() {
        let ok = b();
        assert_eq!(
            ok.priorities,
            BTreeSet::from(["emergency".to_string(), "urgent".to_string()])
        );
        assert_eq!(
            (
                ok.release_after_hours,
                ok.rescope_after_days,
                ok.rescope_runs_over
            ),
            (6, 3, 10)
        );
        for (i, name) in [
            "release_after_hours",
            "rescope_after_days",
            "rescope_runs_over",
        ]
        .iter()
        .enumerate()
        {
            for bad in [Value::Int(0), Value::Int(-1), Value::String("6".into())] {
                let mut a = args();
                a[i + 1].1 = bad.clone();
                let err = bounds(&a).unwrap_err();
                assert!(err.contains(name), "{bad:?}: {err}");
            }
            let missing: Vec<_> = args().into_iter().filter(|(k, _)| k != name).collect();
            assert!(bounds(&missing).unwrap_err().contains(name));
        }
        for bad in ["", " , ", "urgnet"] {
            let mut a = args();
            a[0].1 = Value::String(bad.into());
            assert!(
                bounds(&a).unwrap_err().contains("priorities"),
                "{bad:?} names no priority the watch can read"
            );
        }
    }

    // -- half one: the release -------------------------------------------

    /// THE CASE d4c0cd2d: claimed, its one run refused eight hours ago,
    /// no car — the claim is released, and the step records which claim
    /// it freed and why.
    #[test]
    fn a_claim_with_no_open_run_and_no_car_past_the_bound_is_due_a_release() {
        let it = item("urgent", "2026-10-06T08:54:00Z", "active", "agent-claude-b");
        let h = history(
            ITEM,
            &[run(
                "r1",
                "2026-10-18T09:00:00Z",
                Some("2026-10-18T10:00:00Z"),
                "refused",
            )],
            &[],
        )
        .unwrap();
        let r = release_due(&it, &h, &agents(), &BTreeSet::new(), &b(), at(NOW)).unwrap();
        assert_eq!(r.step_id, "item-1-build");
        assert_eq!(r.assignee, "agent-claude-b");
        assert_eq!(
            r.since,
            at("2026-10-18T10:00:00Z"),
            "the run's end, not the triage"
        );
        assert_eq!(r.idle_hours, 8.0);
    }

    /// A live claim is untouched: an open run, a car in flight, a run
    /// that ended inside the bound, a step on a person, a step not
    /// claimed.
    #[test]
    fn a_live_claim_is_never_due_a_release() {
        let it = |status: &str, who: &str| item("urgent", "2026-10-06T08:54:00Z", status, who);
        let ended = |t: &str| {
            history(
                ITEM,
                &[run("r1", "2026-10-18T09:00:00Z", Some(t), "gated")],
                &[],
            )
            .unwrap()
        };
        let open = history(ITEM, &[run("r2", "2026-10-18T09:00:00Z", None, "")], &[]).unwrap();
        let none = BTreeSet::new();
        let flying = BTreeSet::from([ITEM.to_string()]);
        let cases = [
            ("an open run", it("active", "agent-claude-b"), open, &none),
            (
                "a car in flight",
                it("active", "agent-claude-b"),
                ended("2026-10-17T00:00:00Z"),
                &flying,
            ),
            (
                "a run that ended inside the bound",
                it("active", "agent-claude-b"),
                ended("2026-10-18T14:00:00Z"),
                &none,
            ),
            (
                "a step on a person",
                it("active", "emp-david"),
                ended("2026-10-17T00:00:00Z"),
                &none,
            ),
            (
                "a step nobody claimed",
                it("ready", "agent-claude-b"),
                ended("2026-10-17T00:00:00Z"),
                &none,
            ),
        ];
        for (why, it, h, flight) in cases {
            assert_eq!(
                release_due(&it, &h, &agents(), flight, &b(), at(NOW)),
                None,
                "{why}"
            );
        }
    }

    // -- half two: the rescope -------------------------------------------

    #[test]
    fn a_long_item_with_a_landed_car_or_many_runs_is_due_a_rescope() {
        let it = item("urgent", "2026-09-28T00:30:00Z", "ready", "agent-claude");
        let runs = |n: usize| -> Vec<Json> {
            (0..n)
                .map(|i| {
                    run(
                        &format!("r{i}"),
                        "2026-10-01T00:00:00Z",
                        Some("2026-10-01T01:00:00Z"),
                        "gated",
                    )
                })
                .collect()
        };
        let landed = [car("c1", "partial_item", "closed", true)];
        let r = rescope_due(
            &it,
            &history(ITEM, &runs(2), &landed).unwrap(),
            &b(),
            at(NOW),
        )
        .expect("a landed car past three days");
        assert_eq!((r.runs, r.landed_cars), (2, 1));
        assert!(r.age_days > 20.0, "{r:?}");
        assert!(
            rescope_due(&it, &history(ITEM, &runs(11), &[]).unwrap(), &b(), at(NOW)).is_some(),
            "eleven runs is more than ten"
        );
        assert!(
            rescope_due(&it, &history(ITEM, &runs(10), &[]).unwrap(), &b(), at(NOW)).is_none(),
            "ten runs is not MORE than ten"
        );
        let young = item("urgent", "2026-10-17T00:00:00Z", "ready", "agent-claude");
        assert!(
            rescope_due(
                &young,
                &history(ITEM, &runs(30), &landed).unwrap(),
                &b(),
                at(NOW)
            )
            .is_none(),
            "inside three days nothing is asked"
        );
        let mut stated = it.clone();
        stated["metadata"][FINISH_LINE_KEY] = json!("the gate refuses X, pinned by test Y");
        assert!(
            rescope_due(
                &stated,
                &history(ITEM, &runs(30), &landed).unwrap(),
                &b(),
                at(NOW)
            )
            .is_none(),
            "an item that states its finish line has answered the alarm"
        );
    }

    /// A row the containment read should not have answered is a server
    /// that ignored the filter — refused, never counted.
    #[test]
    fn a_row_naming_another_packet_is_refused_not_counted() {
        let mut stray = run("r9", "2026-10-01T00:00:00Z", None, "");
        stray["metadata"]["packet"] = json!("someone-else");
        assert!(
            history(ITEM, &[stray], &[])
                .unwrap_err()
                .contains("someone-else")
        );
        let stray_car = json!({"id": "c9", "status": "open", "metadata": {"backlog_item": "x"}});
        assert!(history(ITEM, &[], &[stray_car]).unwrap_err().contains("c9"));
    }

    // -- end to end, against the stub jobs API ---------------------------

    const AGENTS: &str = "/api/agents";
    const ITEMS: &str = "/api/jobs?kind=backlog-item&status=open";
    const CARS_OPEN: &str = "/api/jobs?kind=ship-a-change&status=open";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&metadata_has=rescope_item";

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsUrgentItemWatch> {
        JobsUrgentItemWatch::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn agents_listing() -> Json {
        listing(vec![
            json!({"id": "agent-claude-b", "aliases": ["claude-b@algedonic.dev"]}),
        ])
    }

    /// The full row the release re-reads before it writes: the slim
    /// listing carries no step metadata, and the edge is there.
    fn full(mut it: Json, edge: Option<&str>) -> Json {
        if let Some(e) = edge {
            it["steps"][1]["metadata"] = json!({"agent_run": e, "agent_model": "opus-5-5"});
        }
        it
    }

    async fn serve(
        items: Vec<Json>,
        runs: Vec<Json>,
        cars: Vec<Json>,
        open_cars: Vec<Json>,
        alarms: Vec<Json>,
        full_row: Json,
    ) -> listing_stub::Stub {
        listing_stub::serve(vec![
            (AGENTS, agents_listing()),
            (contains("agent-run", "packet"), listing(runs)),
            (contains("ship-a-change", "backlog_item"), listing(cars)),
            (contains("ship-a-change", "partial_item"), listing(vec![])),
            (CARS_OPEN, listing(open_cars)),
            (ALARMS, listing(alarms)),
            (ITEMS, listing(items)),
            ("/api/jobs/item-1", full_row),
        ])
        .await
    }

    /// PINNED: released claim. The edge comes off and the evidence lands
    /// through the merge door FIRST, then the status and assignee alone.
    #[tokio::test]
    async fn a_released_claim_is_put_back_ready_and_says_what_it_freed() {
        let it = item("urgent", "2026-10-06T08:54:00Z", "active", "agent-claude-b");
        let stub = serve(
            vec![it.clone()],
            vec![run(
                "r1",
                "2026-10-18T09:00:00Z",
                Some("2026-10-18T10:00:00Z"),
                "refused",
            )],
            vec![],
            vec![],
            vec![],
            full(it, Some("r1")),
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(
            sent[0].0,
            "PATCH /api/jobs/item-1/steps/item-1-build/metadata"
        );
        let m = &sent[0].1;
        assert_eq!(m["agent_run"], Json::Null, "the stale edge comes off");
        let ev = &m[RELEASE_KEY];
        assert_eq!(ev["from_assignee"], "agent-claude-b");
        assert_eq!(ev["from_run"], "r1");
        assert_eq!(ev["idle_hours"], 8.0);
        assert_eq!(ev["after_hours"], 6);
        assert!(ev["why"].as_str().unwrap().contains("no open agent-run"));
        assert_eq!(sent[1].0, "PUT /api/jobs/item-1/steps/item-1-build");
        assert_eq!(sent[1].1, json!({"status": "ready", "assignee_id": null}));
    }

    /// PINNED: untouched live claim — a run still open, and a car in
    /// flight after a gated run.
    #[tokio::test]
    async fn a_live_claim_is_untouched() {
        let it = item("urgent", "2026-10-16T08:54:00Z", "active", "agent-claude-b");
        let stub = serve(
            vec![it.clone()],
            vec![run("r1", "2026-10-16T09:00:00Z", None, "")],
            vec![],
            vec![],
            vec![],
            full(it.clone(), Some("r1")),
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "open run: {:?}", stub.writes());

        let in_flight = car("c1", "backlog_item", "open", false);
        let stub = serve(
            vec![it.clone()],
            vec![run(
                "r1",
                "2026-10-16T09:00:00Z",
                Some("2026-10-16T10:00:00Z"),
                "gated",
            )],
            vec![in_flight.clone()],
            vec![in_flight],
            vec![],
            full(it, Some("r1")),
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(
            stub.writes().is_empty(),
            "car in flight: {:?}",
            stub.writes()
        );
    }

    /// PINNED: untouched standard item — claimed with no run for weeks
    /// AND past every rescope bound, it is neither released nor alarmed:
    /// a standard item is not read.
    #[tokio::test]
    async fn a_standard_item_is_never_released_and_never_alarms() {
        let it = item(
            "standard",
            "2026-09-01T00:00:00Z",
            "active",
            "agent-claude-b",
        );
        let runs: Vec<Json> = (0..30)
            .map(|i| {
                run(
                    &format!("r{i}"),
                    "2026-09-02T00:00:00Z",
                    Some("2026-09-02T01:00:00Z"),
                    "gated",
                )
            })
            .collect();
        let stub = serve(
            vec![it.clone()],
            runs,
            vec![car("c1", "backlog_item", "closed", true)],
            vec![],
            vec![],
            full(it, None),
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// PINNED: single alarm — one POST naming the item, its age, its run
    /// and car counts, and asking for a finish line.
    #[tokio::test]
    async fn a_long_item_files_one_rescope_alarm() {
        let it = item("urgent", "2026-09-28T00:30:00Z", "ready", "agent-claude");
        let stub = serve(
            vec![it.clone()],
            vec![run(
                "r1",
                "2026-10-01T00:00:00Z",
                Some("2026-10-01T01:00:00Z"),
                "gated",
            )],
            vec![
                car("c1", "backlog_item", "closed", true),
                car("c2", "backlog_item", "closed", true),
            ],
            vec![],
            vec![],
            full(it, None),
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let body = &sent[0].1;
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[ITEM_KEY], ITEM);
        assert_eq!(m["scope"], SCOPE);
        assert_eq!(m["runs"], 1);
        assert_eq!(m["landed_cars"], 2);
        assert_eq!(m["asks"], FINISH_LINE_KEY);
        assert_eq!(m["input_channel"], "telemetry/monitoring");
        let title = body["title"].as_str().unwrap();
        assert!(
            title.contains("item-1") && title.contains("finish line"),
            "{title}"
        );
        assert!(m["detail"].as_str().unwrap().contains(FINISH_LINE_KEY));
    }

    /// PINNED: no refire — an alarm already filed for the item, open or
    /// closed by a person, holds the next tick.
    #[tokio::test]
    async fn an_item_already_asked_is_not_asked_again() {
        for (status, triage) in [("open", "ready"), ("closed", "completed")] {
            let it = item("urgent", "2026-09-28T00:30:00Z", "ready", "agent-claude");
            let alarm = json!({
                "id": "al-1", "status": status,
                "metadata": {"scope": SCOPE, ITEM_KEY: ITEM},
                "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": triage}],
            });
            let stub = serve(
                vec![it.clone()],
                vec![],
                vec![car("c1", "backlog_item", "closed", true)],
                vec![],
                vec![alarm],
                full(it, None),
            )
            .await;
            handler(&stub).invoke(&args(), &ctx()).await.unwrap();
            assert!(stub.writes().is_empty(), "{status}: {:?}", stub.writes());
        }
    }

    #[tokio::test]
    async fn an_empty_agents_registry_is_refused_not_read_as_no_agents() {
        let stub = listing_stub::serve(vec![(AGENTS, listing_stub::empty_listing())]).await;
        let err = handler(&stub).invoke(&args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("agents")),
            "{err}"
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_daily_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-10-18"});
        let err = handler(&stub).invoke(&args(), &c).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at") && m.contains("hourly")),
            "{err}"
        );
    }

    /// The shipped rule's declaration parses, and it is PRIORITY-SCOPED:
    /// urgent and emergency only, never standard — the ~200 standard
    /// builds `agent-held-real-work-is-watched-hourly` refuses to
    /// declare stay unread here too (backlog 9788e22d).
    #[test]
    fn the_shipped_rule_watches_only_urgent_and_emergency_items() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                let v = v.trim();
                let value = match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    Some(s) => Value::String(s.to_string()),
                    None => Value::Int(v.parse().unwrap_or(-1)),
                };
                (k.clone(), value)
            })
            .collect();
        let declared = bounds(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(
            declared.priorities,
            BTreeSet::from(["emergency".to_string(), "urgent".to_string()]),
            "{}: a standard item is never watched",
            rule.name
        );
        assert_eq!(
            (
                declared.release_after_hours,
                declared.rescope_after_days,
                declared.rescope_runs_over
            ),
            (6, 3, 10),
            "the bounds the packet proposed"
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsUrgentItemWatch::new(
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
