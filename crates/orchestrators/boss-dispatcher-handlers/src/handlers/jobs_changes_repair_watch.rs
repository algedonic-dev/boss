//! `jobs.changes_repair_watch` — every hour, each HELD car whose review
//! stands at a CHANGES verdict is judged again: its item's build is
//! returned to its station by the same judgement the verdict's own rule
//! makes, and a car that has gone the rule's bound with no repair
//! gate-run since the verdict files one alarm naming it (backlog
//! 205ce78a).
//!
//! THE CASE. Measured 2026-11-21 16:14-16:30Z (triage run 85bad576, every
//! car read whole): of 15 held cars standing at CHANGES, 11 were stranded.
//! Seven had been returned to their station by
//! `a-changes-verdict-returns-the-build-to-its-station` and never pulled —
//! ready, nobody's, standard, behind 1365 open items for 2.1 to 3.5 days.
//! Four stood ACTIVE on a closed run: the rule fires only on the review
//! run's own writes, and it leaves a build whose run edge names an open
//! run (the builder's run still unreported at verdict time, by design).
//! When that run closed later, or a repair run ended refused, no write to
//! the review run followed, so nothing judged the build again. The oldest,
//! d59e29b5, was eight days old; each held a built, gated car off the
//! train, and nothing alarmed.
//!
//! THE RETURN IS THE VERDICT RULE'S, re-run on age instead of on a write:
//! `changes_of` reads the verdict off the car's reviewer of record,
//! `owed_item` asks the repair door's facts of the car, and
//! `return_build` releases a build whose run edge names no open run and
//! leaves the `returned_for_changes` note — the same functions, so the
//! two rules cannot judge one car two ways. One widening, in
//! [`sweep_plan`]: a build carrying THIS verdict's note and active again
//! is a repair someone took after the return. The event rule leaves it as
//! the holder's; the sweep releases it when its run edge names a run that
//! has CLOSED (b007e3e3: repair run 7cb3a88a closed refused, the build
//! still active on agent-codex). A noted build with no run edge is a
//! hand's claim, and is left.
//!
//! THE ALARM IS THE RECORD OF THE REPAIR, not of the build. A car is
//! repaired by a gate-run on its branch: one opened AFTER the verdict
//! (`metadata={"branch": …}`, the containment the jobs API answers) ends
//! the wait, whatever the build step says. Past `alarm_hours` with none,
//! the car files ONE alarm keyed by the car, through `alarm_item` (an
//! open alarm holds the next tick; closing it re-arms), naming the review
//! run, the item and what the sweep found the build doing. The alarm does
//! not depend on the return: a car the repair door refuses — held at a
//! head other than the one reviewed (df0ec606) — is still unrepaired.
//!
//! IT TELLS THE OWNER, as `jobs.unheld_review_watch` does: an alarm
//! assigned to nobody pages nobody (backlog 35dabc3d), so a filing is
//! followed by a `direct` message to the platform owner, re-sent under
//! the same collapsing id while the alarm is open.

use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::car_release;
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::alarm_item::{self, Occurrence, Raised};
use super::common::{
    api_client, jobs_where_slim, open_jobs_of_kind, owner_for_filing, tell_owner, with_lane,
};
use super::jobs_changes_return_the_build::{
    Changes, EDGE_KEY, Plan, Returned, build_step, changes_of, note_body, noted, owed_item, plan,
    read_job, return_build, short, text,
};
use super::jobs_release_on_review::held_review;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.changes_repair_watch";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "changes-unrepaired";

/// The alarm-metadata key naming the car whose repair is late — the
/// dedup key, read over OPEN backlog items only.
pub const CAR_KEY: &str = "changes_unrepaired_car";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick.
const TICK_AT: &str = "_at";

/// The rule's declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// The packet kind whose open board holds the cars.
    pub kind: String,
    /// How long a CHANGES verdict may stand with no repair gate-run
    /// before its car is alarmed.
    pub alarm_hours: i64,
}

/// Parse the rule's args; each refuses by name.
pub fn declaration(args: &[(String, Value)]) -> Result<Declaration, String> {
    let kind = match arg(args, "kind") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(other) => {
            return Err(format!(
                "`kind` must be a non-empty string, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`kind` is not declared on the rule".into()),
    };
    let alarm_hours = match arg(args, "alarm_hours") {
        Some(Value::Int(n)) if *n > 0 => *n,
        Some(other) => {
            return Err(format!(
                "`alarm_hours` must be a positive whole number, got a {} ({other:?})",
                other.kind()
            ));
        }
        None => return Err("`alarm_hours` is not declared on the rule".into()),
    };
    Ok(Declaration { kind, alarm_hours })
}

/// PURE: each open car on the board whose review step is HELD, with the
/// run its review stands on (`car_release::reviewer_of_record`, the run
/// the repair door reads). A held car with no reviewer has no verdict.
pub fn held_cars(packets: &[Json]) -> Vec<(&Json, &str)> {
    packets
        .iter()
        .filter(|car| text(car, "/status") == "open" && held_review(car).is_some())
        .filter_map(|car| car_release::reviewer_of_record(car).map(|run| (car, run)))
        .collect()
}

/// PURE: when the verdict was recorded (`review.at`, written by `boss
/// review`), or `None` when it carries no readable stamp.
pub fn verdict_at(run: &Json) -> Option<DateTime<Utc>> {
    let at = text(run, &format!("/metadata/{}/at", car_release::REVIEW_KEY));
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// PURE: the first gate-run among `gates` opened AFTER `since` — the
/// repair the car is waiting for. An undated gate-run is not one: the
/// alarm errs loud.
pub fn repaired_since(gates: &[Json], since: DateTime<Utc>) -> Option<&str> {
    gates
        .iter()
        .filter(|g| DateTime::parse_from_rfc3339(text(g, "/opened_at")).is_ok_and(|t| t > since))
        .map(|g| text(g, "/id"))
        .find(|id| !id.is_empty())
}

/// PURE: the `/api/jobs` filter for the gate-runs filed on `branch`:
/// metadata containment, its value percent-encoded.
pub fn gate_runs_filter(branch: &str) -> String {
    let containment = json!({ "branch": branch }).to_string();
    let encoded: String = containment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("kind=gate-run&metadata={encoded}")
}

/// PURE: the verdict rule's plan for the build ([`plan`]), widened by one
/// case (module doc): a build this verdict already returned and someone
/// took again is released when its run edge names a run — the release
/// then judges that run, and leaves it unless it has closed.
pub(crate) fn sweep_plan<'a>(item: &'a Json, changes: &Changes) -> Plan<'a> {
    match plan(item, changes) {
        Plan::Leave(why) => match build_step(item) {
            Some(step)
                if text(item, "/status") == "open"
                    && text(step, "/status") == "active"
                    && noted(step, changes) =>
            {
                let edge = text(step, &format!("/metadata/{EDGE_KEY}"));
                if edge.is_empty() {
                    Plan::Leave(format!("{why}, on no run edge: a hand's claim"))
                } else {
                    Plan::Release {
                        step,
                        edge: Some(edge),
                    }
                }
            }
            _ => Plan::Leave(why),
        },
        other => other,
    }
}

/// PURE: the build as the sweep read it, for the alarm's reader.
pub fn build_state(item: &Json) -> String {
    match build_step(item) {
        None => format!("item {} has no build step", short(text(item, "/id"))),
        Some(step) => {
            let holder = Some(text(step, "/assignee_id")).filter(|h| !h.is_empty());
            let edge = text(step, &format!("/metadata/{EDGE_KEY}"));
            format!(
                "build {}, held by {}, run edge {}",
                text(step, "/status"),
                holder.unwrap_or("nobody"),
                if edge.is_empty() { "none" } else { short(edge) }
            )
        }
    }
}

/// One car past its bound with no repair gate-run.
#[derive(Debug, Clone, PartialEq)]
pub struct Unrepaired {
    pub car: String,
    pub car_title: String,
    pub branch: String,
    pub review_run: String,
    pub item: String,
    pub head: String,
    pub verdict_at: DateTime<Utc>,
    pub waited_hours: f64,
    pub bound_hours: i64,
    /// What the sweep found, or did, with the item's build.
    pub build: String,
}

/// PURE: the hours from `since` to `now`, to a tenth.
pub fn waited_hours(since: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    ((now - since).num_seconds() as f64 / 360.0).round() / 10.0
}

/// The alarm one unrepaired car files.
pub fn alarm_body(u: &Unrepaired, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 205ce78a) at {now}: held car {car} ({title}) \
         has stood {h}h at a CHANGES verdict from review run {run}, recorded {at} at head {head}, \
         against a bound of {b}h, and no gate-run has been opened on its branch {branch} since. \
         Its item {item}: {build}. A returned build waits at its station until a builder pulls \
         it; `boss dispatch {item}` admits it as this car's REPAIR and briefs the findings. While \
         this alarm stays open the car is not alarmed again, and closing it re-arms the watch.",
        now = now.to_rfc3339(),
        car = u.car,
        title = u.car_title,
        h = u.waited_hours,
        run = u.review_run,
        at = u.verdict_at.to_rfc3339(),
        head = short(&u.head),
        b = u.bound_hours,
        branch = u.branch,
        item = if u.item.is_empty() {
            "(none named)"
        } else {
            u.item.as_str()
        },
        build = u.build,
    );
    let metadata = json!({
        "area": "jobs",
        "scope": SCOPE,
        CAR_KEY: u.car,
        "car": u.car,
        "branch": u.branch,
        "review_run": u.review_run,
        "item": u.item,
        "reviewed_sha": u.head,
        "verdict_at": u.verdict_at.to_rfc3339(),
        "waited_hours": u.waited_hours,
        "bound_hours": u.bound_hours,
        "build": u.build,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "UNREPAIRED CHANGES: car {} ({}) {}h at review {}'s CHANGES with no repair gate-run",
            short(&u.car),
            u.car_title,
            u.waited_hours,
            short(&u.review_run),
        ),
        "subject": {"subject_kind": "custom", "id": u.car},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

pub struct JobsChangesRepairWatch {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    messages_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsChangesRepairWatch {
    pub fn new(
        jobs_base: impl Into<String>,
        messages_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            messages_base: messages_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The verdict rule's return, judged again: what it did or found,
    /// for the alarm's reader.
    async fn return_the_build(
        &self,
        car: &Json,
        changes: &Changes,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<(String, String), HandlerError> {
        let item_id = match owed_item(car, changes) {
            Ok(item) => item,
            Err(why) => {
                // The car's own link, for the alarm, when the door refuses.
                let named = [boss_jobs::car::BACKLOG_ITEM, boss_jobs::car::PARTIAL_ITEM]
                    .into_iter()
                    .map(|key| text(car, &format!("/metadata/{key}")))
                    .find(|item| !item.is_empty())
                    .unwrap_or_default();
                return Ok((named.to_string(), format!("not returned: {why}")));
            }
        };
        let Some(item) = read_job(&self.client, self.base(), &item_id).await? else {
            return Ok((item_id, "the item is gone (404)".into()));
        };
        let state = build_state(&item);
        let at = now.to_rfc3339();
        let note = |released: Option<&str>| {
            note_body(car, &item_id, changes, released, Some(at.as_str()), rule)
        };
        let did = match return_build(
            &self.client,
            self.base(),
            rule,
            &item_id,
            sweep_plan(&item, changes),
            note,
        )
        .await?
        {
            Returned::Left(why) => format!("{state} — left: {why}"),
            Returned::Noted { released } => {
                tracing::info!(rule, car = %changes.car, item = %item_id,
                    released = released.as_deref().unwrap_or("nobody"),
                    "{HANDLER}: returned the item's build to its station for the car's repair");
                format!(
                    "returned to its station by this sweep (was {state}; released from {})",
                    released.as_deref().unwrap_or("nobody, it was waiting")
                )
            }
        };
        Ok((item_id, did))
    }
}

#[async_trait]
impl Handler for JobsChangesRepairWatch {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let decl = declaration(args).map_err(HandlerError::Permanent)?;
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

        let packets = open_jobs_of_kind(&self.client, self.base(), &decl.kind, rule).await?;
        let held = held_cars(&packets);
        let mut unrepaired: Vec<Unrepaired> = Vec::new();
        let mut undated = 0usize;
        for (car, reviewer) in &held {
            let Some(run) = read_job(&self.client, self.base(), reviewer).await? else {
                continue;
            };
            // A release, no verdict yet, or a review of another car.
            let Some(changes) = changes_of(&run).filter(|c| c.car == text(car, "/id")) else {
                continue;
            };
            let (item, build) = self.return_the_build(car, &changes, now, rule).await?;
            let Some(at) = verdict_at(&run) else {
                undated += 1;
                tracing::warn!(rule, car = %changes.car, review = %changes.run,
                    "{HANDLER}: the CHANGES verdict carries no readable `at`, so its age is unjudged");
                continue;
            };
            let waited = waited_hours(at, now);
            if waited <= decl.alarm_hours as f64 {
                continue;
            }
            let branch = text(car, "/metadata/branch").to_string();
            let gates =
                jobs_where_slim(&self.client, self.base(), &gate_runs_filter(&branch), rule)
                    .await?;
            if let Some(gate) = repaired_since(&gates, at) {
                tracing::info!(rule, car = %changes.car, gate,
                    "{HANDLER}: a gate-run since the verdict is the repair");
                continue;
            }
            unrepaired.push(Unrepaired {
                car: changes.car.clone(),
                car_title: text(car, "/title").to_string(),
                branch,
                review_run: changes.run.clone(),
                item,
                head: changes.head.clone(),
                verdict_at: at,
                waited_hours: waited,
                bound_hours: decl.alarm_hours,
                build,
            });
        }
        if unrepaired.is_empty() {
            tracing::info!(
                rule,
                held = held.len(),
                undated,
                "{HANDLER}: no unrepaired CHANGES"
            );
            return Ok(());
        }

        let open = jobs_where_slim(
            &self.client,
            self.base(),
            &format!("kind=backlog-item&status=open&metadata_has={CAR_KEY}"),
            rule,
        )
        .await?;
        // An open alarm is assigned to nobody, so its owner is told
        // directly; the re-send collapses on its id (backlog 35dabc3d).
        for alarm in open
            .iter()
            .filter(|a| a.pointer("/metadata/scope").and_then(Json::as_str) == Some(SCOPE))
        {
            let id = text(alarm, "/id");
            let to = text(alarm, "/owner_id");
            tell_owner(
                &self.client,
                &self.messages_base,
                SCOPE,
                id,
                alarm,
                to,
                rule,
            )
            .await?;
        }
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let mut filed = 0usize;
        for u in &unrepaired {
            let held = alarm_item::open_item(&open, CAR_KEY, &u.car);
            let occ = Occurrence::new(
                now.to_rfc3339(),
                format!(
                    "{}h at review {}'s CHANGES with no repair gate-run",
                    u.waited_hours,
                    short(&u.review_run)
                ),
            );
            let alarm = alarm_body(u, &owner, now, rule);
            let raised = alarm_item::raise_minted(
                &self.client,
                self.base(),
                held,
                alarm.clone(),
                &occ,
                Default::default(),
                rule,
            )
            .await?;
            if let Raised::Filed(Some(id)) = &raised {
                tell_owner(
                    &self.client,
                    &self.messages_base,
                    SCOPE,
                    id,
                    &alarm,
                    &owner,
                    rule,
                )
                .await?;
                filed += 1;
            }
        }
        tracing::info!(
            rule,
            held = held.len(),
            unrepaired = unrepaired.len(),
            undated,
            filed,
            "{HANDLER}: read {} open {} packet(s)",
            packets.len(),
            decl.kind
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::jobs_changes_return_the_build::NOTE_KEY;
    use super::super::listing_stub;
    use super::*;

    const RULE: &str = "a-changes-held-car-is-returned-and-alarmed-until-repaired";
    const NOW: &str = "2026-11-21T17:00:00+00:00";
    const CAR: &str = "d59e29b5-0000-4000-8000-000000000000";
    const ITEM: &str = "56b81861-0000-4000-8000-000000000000";
    const REVIEWER: &str = "ff17eca5-0000-4000-8000-000000000000";
    const BUILDER: &str = "39a3dbc6-0000-4000-8000-000000000000";
    const REPAIR: &str = "7cb3a88a-0000-4000-8000-000000000000";
    const BUILD_STEP: &str = "00000000-0000-4000-8000-000000000b1d";
    const H1: &str = "b9c34453b9c34453b9c34453b9c34453b9c34453";
    const H2: &str = "85ec71cd85ec71cd85ec71cd85ec71cd85ec71cd";
    const BRANCH: &str = "feat/job-convert-cohort-mode";

    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".into(), Value::String("ship-a-change".into())),
            ("alarm_hours".into(), Value::Int(12)),
        ]
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A held car in the shape the live board had: its review step held
    /// at `hold_sha`, dispatched to the reviewer run.
    fn held_car(hold_sha: &str) -> Json {
        json!({"id": CAR, "kind": "ship-a-change", "status": "open",
               "title": "boss job convert moves a cohort",
               "metadata": {"branch": BRANCH, "agent_run": BUILDER,
                            "built_by": "claude@algedonic.dev", "backlog_item": ITEM},
               "steps": [{"id": "00000000-0000-4000-8000-000000000003", "spec_slug": "review",
                          "title": boss_jobs::car::REVIEW, "status": "active",
                          "metadata": {"hold": "touches an ops verb", "hold_sha": hold_sha,
                                       "agent_run": REVIEWER}}]})
    }

    fn verdict(v: &str, sha: &str, at: &str) -> Json {
        json!({"id": REVIEWER, "kind": "agent-run", "status": "closed",
               "metadata": {"packet": CAR, "step": "review",
                   "review": {"car": CAR, "branch": BRANCH, "reviewed_sha": sha,
                              "verdict": v, "at": at}}})
    }

    fn item(status: &str, holder: Option<&str>, edge: Option<&str>, noted: bool) -> Json {
        let mut md = json!({"agent_profile": "builder"});
        if let Some(e) = edge {
            md[EDGE_KEY] = json!(e);
        }
        if noted {
            md[NOTE_KEY] = json!({"review": REVIEWER, "head": H1});
        }
        json!({"id": ITEM, "kind": "backlog-item", "status": "open",
               "steps": [{"id": BUILD_STEP, "spec_slug": "build", "status": status,
                          "assignee_id": holder, "metadata": md}]})
    }

    fn run(id: &str, status: &str) -> Json {
        json!({"id": id, "kind": "agent-run", "status": status})
    }

    fn gate(id: &str, opened_at: &str) -> Json {
        json!({"id": id, "kind": "gate-run", "status": "closed", "opened_at": opened_at,
               "metadata": {"branch": BRANCH}})
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    #[test]
    fn the_declaration_names_its_kind_and_its_bound() {
        let d = declaration(&args()).unwrap();
        assert_eq!(d.kind, "ship-a-change");
        assert_eq!(d.alarm_hours, 12);
        for missing in ["kind", "alarm_hours"] {
            let a: Vec<_> = args().into_iter().filter(|(k, _)| k != missing).collect();
            assert!(declaration(&a).unwrap_err().contains(missing));
        }
        let mut bad = args();
        bad[1].1 = Value::Int(0);
        assert!(declaration(&bad).unwrap_err().contains("alarm_hours"));
    }

    #[test]
    fn only_an_open_held_car_with_a_reviewer_is_read() {
        let mut unheld = held_car(H1);
        unheld["steps"][0]["metadata"]["hold"] = json!(false);
        let mut closed = held_car(H1);
        closed["status"] = json!("closed");
        let mut unreviewed = held_car(H1);
        unreviewed["steps"][0]["metadata"]["agent_run"] = Json::Null;
        let board = vec![held_car(H1), unheld, closed, unreviewed];
        let got: Vec<&str> = held_cars(&board).into_iter().map(|(_, r)| r).collect();
        assert_eq!(got, vec![REVIEWER]);
    }

    #[test]
    fn a_gate_run_opened_after_the_verdict_is_the_repair() {
        let since = at("2026-11-18T09:57:39Z");
        let before = gate("g-before", "2026-11-18T09:00:00Z");
        let after = gate("g-after", "2026-11-18T13:17:16Z");
        let undated = json!({"id": "g-undated", "kind": "gate-run"});
        assert_eq!(repaired_since(&[before.clone(), undated], since), None);
        assert_eq!(repaired_since(&[before, after], since), Some("g-after"));
    }

    #[test]
    fn the_gate_run_filter_is_the_branch_containment_encoded() {
        assert_eq!(
            gate_runs_filter("feat/x"),
            "kind=gate-run&metadata=%7B%22branch%22%3A%22feat%2Fx%22%7D"
        );
    }

    /// b007e3e3: the build this verdict returned was taken again by a
    /// repair run that ended refused. The verdict rule leaves a noted,
    /// active build as its holder's; the sweep releases it — the release
    /// then judges the run, so an OPEN repair run still keeps it. A noted
    /// build with no run edge is a hand's claim, and is left.
    #[test]
    fn a_noted_build_taken_again_is_released_only_on_a_run_edge() {
        let changes = changes_of(&verdict("changes", H1, "2026-11-16T04:04:08Z")).unwrap();
        let taken = item("active", Some("agent-codex"), Some(REPAIR), true);
        assert!(matches!(plan(&taken, &changes), Plan::Leave(_)));
        assert!(matches!(
            sweep_plan(&taken, &changes),
            Plan::Release {
                edge: Some(REPAIR),
                ..
            }
        ));
        let by_hand = item("active", Some("emp-david"), None, true);
        assert!(matches!(sweep_plan(&by_hand, &changes), Plan::Leave(_)));
        let waiting = item("ready", None, Some(BUILDER), true);
        assert!(matches!(sweep_plan(&waiting, &changes), Plan::Leave(_)));
    }

    // -- end to end, against the stub jobs API ---------------------------

    const OPEN: &str = "/api/jobs?kind=ship-a-change&status=open";
    const ALARMS: &str =
        "/api/jobs?kind=backlog-item&status=open&metadata_has=changes_unrepaired_car";
    const PUT_BUILD: &str = "PUT /api/jobs/56b81861-0000-4000-8000-000000000000/steps/00000000-0000-4000-8000-000000000b1d";
    const PATCH_BUILD: &str = "PATCH /api/jobs/56b81861-0000-4000-8000-000000000000/steps/00000000-0000-4000-8000-000000000b1d/metadata";

    fn gates_route() -> &'static str {
        Box::leak(format!("/api/jobs?{}", gate_runs_filter(BRANCH)).into_boxed_str())
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-11-21", "_at": NOW}),
        }
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsChangesRepairWatch> {
        JobsChangesRepairWatch::new(
            stub.base.clone(),
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn by_id(id: &str) -> &'static str {
        Box::leak(format!("/api/jobs/{id}").into_boxed_str())
    }

    /// The board, the reviewer's verdict, the item, the runs and the
    /// car's gate-runs as the stub answers them.
    async fn serve(
        car: Json,
        verdict: Json,
        item: Json,
        runs: Vec<Json>,
        gates: Vec<Json>,
    ) -> listing_stub::Stub {
        let mut answers = vec![
            (OPEN, listing(vec![car])),
            (by_id(REVIEWER), verdict),
            (by_id(ITEM), item),
            (gates_route(), listing(gates)),
            (ALARMS, listing_stub::empty_listing()),
        ];
        for r in runs {
            let id = text(&r, "/id").to_string();
            answers.push((by_id(&id), r));
        }
        listing_stub::serve(answers).await
    }

    /// THE CASE (d59e29b5, 2026-11-13): a CHANGES eight days old, the
    /// build active on a builder run that closed after the verdict, no
    /// gate-run since. One sweep returns the build to its station AND
    /// files one alarm naming the car.
    #[tokio::test]
    async fn an_eight_day_old_changes_on_a_closed_run_is_returned_and_alarmed() {
        let stub = serve(
            held_car(H1),
            verdict("changes", H1, "2026-11-13T04:34:24Z"),
            item("active", Some("agent-claude"), Some(BUILDER), false),
            vec![run(BUILDER, "closed")],
            vec![gate("g-1", "2026-11-12T01:11:36Z")],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let writes = stub.writes();
        let put = writes
            .iter()
            .position(|w| w == PUT_BUILD)
            .expect("released");
        let patch = writes.iter().position(|w| w == PATCH_BUILD).expect("noted");
        assert!(put < patch, "the release first, then the note: {writes:?}");
        let sent = stub.sent();
        let alarm = &sent
            .iter()
            .find(|s| s.0 == "POST /api/jobs")
            .expect("alarm")
            .1;
        assert_eq!(alarm["metadata"][CAR_KEY], CAR);
        assert_eq!(alarm["metadata"]["review_run"], REVIEWER);
        assert_eq!(alarm["metadata"]["item"], ITEM);
        assert_eq!(alarm["priority"], "urgent");
        let build = alarm["metadata"]["build"].as_str().unwrap();
        assert!(
            build.contains("returned to its station by this sweep"),
            "{build}"
        );
        assert!(build.contains("agent-claude"), "{build}");
        let title = alarm["title"].as_str().unwrap();
        assert!(
            title.starts_with("UNREPAIRED CHANGES: car d59e29b5"),
            "{title}"
        );
        assert!(title.contains("204.4h"), "{title}");
        let msg = sent
            .iter()
            .find(|s| s.0 == "POST /api/messages/send")
            .expect("told");
        assert_eq!(msg.1["recipient_id"], "emp-owner");
    }

    /// The seven routed and never pulled (b0eb9188 et al.): the build is
    /// ready with this verdict's note. Nothing is written to it again,
    /// and the car is alarmed.
    #[tokio::test]
    async fn a_returned_build_never_pulled_is_alarmed_and_not_rewritten() {
        let stub = serve(
            held_car(H1),
            verdict("changes", H1, "2026-11-19T13:45:33Z"),
            item("ready", None, Some(BUILDER), true),
            vec![],
            vec![gate("g-1", "2026-11-18T21:33:28Z")],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(
            stub.writes(),
            vec!["POST /api/jobs", "POST /api/messages/send"],
            "the alarm and its message, nothing on the build"
        );
    }

    /// df0ec606: the hold stands at another head than the one reviewed,
    /// so the repair door refuses the return; with no gate-run since, the
    /// car is still unrepaired and still alarmed.
    #[tokio::test]
    async fn a_car_the_repair_door_refuses_is_still_alarmed() {
        let stub = serve(
            held_car(H2),
            verdict("changes", H1, "2026-11-18T09:57:39Z"),
            item("active", Some("agent-claude"), Some(BUILDER), false),
            vec![run(BUILDER, "closed")],
            vec![],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(
            stub.writes(),
            vec!["POST /api/jobs", "POST /api/messages/send"]
        );
        let alarm = &stub.sent()[0].1;
        assert_eq!(alarm["metadata"]["item"], ITEM, "the car's own link");
        let build = alarm["metadata"]["build"].as_str().unwrap();
        assert!(build.starts_with("not returned:"), "{build}");
    }

    /// A gate-run on the branch since the verdict is the repair: the car
    /// is not alarmed (4bcc782d, df0ec606 measured so on 2026-11-21).
    #[tokio::test]
    async fn a_gate_run_since_the_verdict_holds_the_alarm() {
        let stub = serve(
            held_car(H1),
            verdict("changes", H1, "2026-11-18T09:57:39Z"),
            item("ready", None, Some(BUILDER), true),
            vec![],
            vec![gate("g-repair", "2026-11-18T13:17:16Z")],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// Inside the bound nothing is alarmed, though the return still runs;
    /// a RELEASE is not this rule's at all.
    #[tokio::test]
    async fn a_fresh_changes_or_a_release_files_nothing() {
        let stub = serve(
            held_car(H1),
            verdict("changes", H1, "2026-11-21T12:50:18Z"),
            item("active", Some("agent-claude"), Some(BUILDER), false),
            vec![run(BUILDER, "closed")],
            vec![],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(
            stub.writes(),
            vec![PUT_BUILD, PATCH_BUILD],
            "returned, not alarmed"
        );

        let stub = serve(
            held_car(H1),
            verdict("release", H1, "2026-11-13T04:34:24Z"),
            item("active", Some("agent-claude"), Some(BUILDER), false),
            vec![run(BUILDER, "closed")],
            vec![],
        )
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// An open repair run keeps the build (96257580 et al. are in flight),
    /// and a car whose alarm is open is not alarmed again — its owner's
    /// message is only re-sent under the collapsing id.
    #[tokio::test]
    async fn an_open_alarm_holds_and_an_open_repair_run_keeps_the_build() {
        let open_alarm = json!({"id": "al-1", "kind": "backlog-item", "status": "open",
            "owner_id": "emp-owner", "title": "UNREPAIRED CHANGES",
            "metadata": {"scope": SCOPE, CAR_KEY: CAR}});
        let stub = listing_stub::serve(vec![
            (OPEN, listing(vec![held_car(H1)])),
            (
                by_id(REVIEWER),
                verdict("changes", H1, "2026-11-13T04:34:24Z"),
            ),
            (
                by_id(ITEM),
                item("active", Some("agent-codex"), Some(REPAIR), true),
            ),
            (by_id(REPAIR), run(REPAIR, "open")),
            (gates_route(), listing(vec![])),
            (ALARMS, listing(vec![open_alarm])),
        ])
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(stub.writes(), vec!["POST /api/messages/send"]);
        assert_eq!(stub.sent()[0].1["id"], "changes-unrepaired:al-1:emp-owner");
    }

    #[tokio::test]
    async fn a_firing_with_no_instant_is_refused() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-11-21"});
        let err = handler(&stub).invoke(&args(), &c).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at")),
            "{err}"
        );
    }

    /// The shipped rule: hourly over the open ship-a-change board, with a
    /// twelve-hour bound (the operator's, 2026-11-21).
    #[test]
    fn the_shipped_rule_sweeps_the_cars_hourly_with_a_twelve_hour_bound() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        assert_eq!(rule.name, RULE);
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                let v = v.trim().trim_matches('"');
                let parsed = v
                    .parse::<i64>()
                    .map_or_else(|_| Value::String(v.into()), Value::Int);
                (k.clone(), parsed)
            })
            .collect();
        let d = declaration(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(d.kind, "ship-a-change");
        assert_eq!(d.alarm_hours, 12);
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsChangesRepairWatch::new(
            "http://unused",
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
