//! WHO MOVED THE WORK, per region (backlog 9ca41c08, car 1 of design
//! 393bdef6, answered by David 2026-10-16).
//!
//! WHY THIS EXISTS. Downstream of the shop floor every step records
//! `completed_by` — the actor the API signed the completing write with,
//! which [`ActorId`] already separates into an automation, an agent and
//! a person — and nothing aggregated it. The rails' machine/person split
//! is written per border in `crate::borders` (`flow_of`), so the map
//! said who SHOULD move a packet and never who DID. This is the reading
//! that says who did: for each region downstream of the shop floor, the
//! completions in the window of the steps whose completion moves a packet
//! OUT of it, by the class of the actor that completed each.
//!
//! WHICH STEPS LEAVE A REGION is not a second list: it is the route
//! map's own crossings ([`completion_exits`]), the protocol walks that
//! already draw every rail. A crossing made by a job key, a marker or an
//! aged-out window is no step completion, and moves nobody this reading
//! could name — those are the hand acts that need the actor on the moves
//! record's cause (a later car).
//!
//! WHY THE EXITS ARE READ HERE AND NOT A FIELD ON THE CROSSING. The
//! first build of this car added `completed` to `routes::Crossing` and
//! `routes::Source::Workflow`, and gate-run 20724417 went red on two
//! pins: `routes.rs` is one of the source files the v2 move-diagnosis
//! model hashes into its content address (`move_diagnosis::model`), and
//! that address is immutable model data. So `routes.rs` stays
//! byte-identical, and the exit step is read off the crossing's own
//! `step` and `via` ([`completed_step`]), in the shapes the walk writes.
//!
//! WHO MOVED A PACKET A TERMINAL CLOSED is read off the packet, not the
//! walk (backlog 8bc32104). The walk names ONE step behind a terminal —
//! the one whose completion it tried last on its own path, `closed, after
//! proven completed` for ship-a-change's `merged` — while `merged` is
//! ready when `proven` AND a `landed-review` are done, so the step that
//! closed a real car is whichever of them completed LAST. Crediting the
//! walk's step read 13 of the last 60 closed cars on 2026-11-13 as moved
//! by `automation:ops-runner` when an agent's landed-review had closed
//! them. So a closing exit credits the latest-completed of the steps the
//! terminal's `ready_when` names — the terminal row's own `blocked_by`,
//! written at materialisation — and counts at the terminal's completion,
//! the moment the packet left. `routes.rs` stays byte-identical, for the
//! reason above.
//!
//! NO TARGET RATIO, by decision: the reading has a direction (hand-moved
//! downstream goes down) and no goal number, so it is a measure beside
//! the state and never a band that judges it.

use std::collections::{BTreeSet, HashSet};

use boss_core::actor::ActorId;
use boss_core::job::{Job, Step, StepStatus};

use super::judging::Windows;
use super::{Measure, Regions, measure_said, number_text};

/// The regions the reading is taken for: downstream of the shop floor,
/// where the work is a car on its way to main and out of it (design
/// 393bdef6). Upstream regions (receiving, marshalling, the shop floor)
/// are where people and agents are SUPPOSED to do the work.
pub const AUTOMATION_REGIONS: [&str; 7] = [
    "gates", "dock", "track", "shed", "arrivals", "garage", "publish",
];

/// The measure's name, so a reader can find it among a region's KPI.
pub const MACHINE_MOVED: &str = "machine-moved";

/// How many exit-step completions each class of actor made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MovedBy {
    /// An `automation:` actor — a rule, the conductor, a runner.
    pub machine: usize,
    /// An LLM session, in either spelling of the class.
    pub agent: usize,
    /// A person.
    pub human: usize,
    /// A completed step with no `completed_by` — one that predates the
    /// stamp. Counted apart, never folded into a class.
    pub unknown: usize,
}

impl MovedBy {
    /// Moved by an agent or a person: by hand, as the design names it.
    pub fn hand(&self) -> usize {
        self.agent + self.human
    }

    fn with(self, by: Option<&ActorId>) -> Self {
        match by {
            Some(ActorId::Automation(_)) => MovedBy {
                machine: self.machine + 1,
                ..self
            },
            Some(a) if a.is_agent() => MovedBy {
                agent: self.agent + 1,
                ..self
            },
            Some(_) => MovedBy {
                human: self.human + 1,
                ..self
            },
            None => MovedBy {
                unknown: self.unknown + 1,
                ..self
            },
        }
    }
}

/// The step whose COMPLETION made a crossing the walk recorded as
/// `step` by `via`, or `None` where no step completed to make it. The
/// shapes are `routes::walk`'s own: a completion is `completed` or
/// `completed, k=v, …` on the completing step; one that closed the
/// packet is named by its terminal and says `closed, after <slug>
/// <how>`, so the slug is the step; `admitted`, a job key
/// (`job.metadata.…`), a marker (`marker … set`) and the aged-out
/// window ([`crate::routes::AGED_OUT`]) complete no step.
pub fn completed_step<'a>(step: &'a str, via: &'a str) -> Option<&'a str> {
    let by_completion = |how: &str| how == "completed" || how.starts_with("completed, ");
    match via.strip_prefix("closed, after ") {
        Some(rest) => rest
            .split_once(' ')
            .filter(|(_, how)| by_completion(how))
            .map(|(slug, _)| slug),
        None => by_completion(via).then_some(step),
    }
}

/// One way a step completion moves a packet out of a region, read off a
/// crossing the walk recorded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Exit {
    pub workflow: String,
    /// The step whose completion the walk saw make the crossing.
    pub step: String,
    /// The terminal that completion readied and closed the packet on,
    /// when it is not `step` itself. Such an exit is credited to the
    /// step the terminal names that completed last on the packet, which
    /// need not be `step` (backlog 8bc32104).
    pub closes: Option<String>,
}

impl Exit {
    /// A step whose own completion moves the packet out.
    pub fn by(workflow: &str, step: &str) -> Self {
        Exit {
            workflow: workflow.into(),
            step: step.into(),
            closes: None,
        }
    }

    /// The slug of the step row whose completion IS the exit: the
    /// terminal for a closing exit, else the step.
    fn leaves_on(&self) -> &str {
        self.closes.as_deref().unwrap_or(&self.step)
    }
}

/// THE STEP COMPLETIONS THAT MOVE A PACKET OUT OF `region`: every
/// protocol crossing on a route leaving it that a step completion made.
/// Read off the walks that draw the rails, so the reading and the rails
/// cannot disagree about which crossings leave a region. A declared
/// hand-off names no step.
pub fn completion_exits(map: &crate::routes::RouteMap, region: &str) -> BTreeSet<Exit> {
    map.routes
        .iter()
        .filter(|r| r.from.as_deref() == Some(region) && r.to.as_deref() != Some(region))
        .flat_map(|r| &r.sources)
        .filter_map(|s| match s {
            crate::routes::Source::Workflow {
                workflow,
                step,
                via,
                ..
            } => completed_step(step, via).map(|done| Exit {
                closes: (done != step).then(|| step.clone()),
                ..Exit::by(workflow, done)
            }),
            _ => None,
        })
        .collect()
}

/// The exits in the CURRENT window, by who made each: a step's own
/// completion by its completer, a terminal's by the completer of the
/// step behind it that completed last ([`mover`]). A step read twice —
/// the same packet carried by two of the handler's reads — counts once.
pub fn moved_by(exits: &BTreeSet<Exit>, rows: &[&(Job, Vec<Step>)], w: &Windows) -> MovedBy {
    let mut seen = HashSet::new();
    rows.iter()
        .flat_map(|(job, steps)| {
            steps
                .iter()
                .filter_map(move |s| mover(exits, job, steps, s, w))
        })
        .filter(|s| seen.insert(s.id))
        .fold(MovedBy::default(), |m, s| m.with(s.completed_by.as_ref()))
}

/// The step whose completion moved `job` out where `s` is an exit
/// completed in the current window, else `None`. For a terminal that
/// closed it, that is the latest-completed, no later than the terminal,
/// of the steps its `ready_when` names (the row's `blocked_by`) and the
/// step the walk named, which stands in for a row with no `blocked_by`.
/// A terminal with none of them completed is credited to itself.
fn mover<'a>(
    exits: &BTreeSet<Exit>,
    job: &Job,
    steps: &'a [Step],
    s: &'a Step,
    w: &Windows,
) -> Option<&'a Step> {
    let at = s
        .completed_at
        .filter(|at| s.status == StepStatus::Completed && w.current(*at))?;
    let slug = s.spec_slug.as_deref()?;
    let hits: Vec<&Exit> = exits
        .iter()
        .filter(|e| e.workflow == job.kind && e.leaves_on() == slug)
        .collect();
    if hits.is_empty() {
        return None;
    }
    if hits.iter().all(|e| e.closes.is_none()) {
        return Some(s);
    }
    let named = |x: &Step| {
        s.blocked_by.contains(&x.id)
            || x.spec_slug
                .as_deref()
                .is_some_and(|name| hits.iter().any(|e| e.closes.is_some() && e.step == name))
    };
    steps
        .iter()
        .filter(|x| x.id != s.id && x.status == StepStatus::Completed && named(x))
        .filter_map(|x| {
            x.completed_at
                .filter(|done| *done <= at)
                .map(|done| (done, x))
        })
        .max_by_key(|(done, _)| *done)
        .map(|(_, x)| x)
        .or(Some(s))
}

/// The reading as the region's KPI carries it: `machine-moved 80% · 2
/// hand-moved/day`. The value is the machine share of the ATTRIBUTED
/// completions, `None` where there were none — a share of nothing is
/// not zero — and the sentence says which of the reasons it is.
pub fn automation_measure(exits: Option<&BTreeSet<Exit>>, moved: MovedBy, w: &Windows) -> Measure {
    let none =
        |why: &str| measure_said(MACHINE_MOVED, None, "%", format!("{MACHINE_MOVED}: {why}"));
    let Some(exits) = exits else {
        return none("no reading — the routes could not be derived");
    };
    if exits.is_empty() {
        return none("no step completion leaves this region");
    }
    let unknown = if moved.unknown > 0 {
        format!(" · {} unattributed", moved.unknown)
    } else {
        String::new()
    };
    let attributed = moved.machine + moved.hand();
    if attributed == 0 {
        return none(&format!("no step left this region in the window{unknown}"));
    }
    #[allow(clippy::cast_precision_loss)]
    let share = moved.machine as f64 * 100.0 / attributed as f64;
    measure_said(
        MACHINE_MOVED,
        Some(share),
        "%",
        format!(
            "{MACHINE_MOVED} {}% · {} hand-moved/day{unknown}",
            number_text(share),
            number_text(w.per_day(moved.hand())),
        ),
    )
}

/// Hang the reading on each of [`AUTOMATION_REGIONS`], after its own
/// KPI (the primary measure stays first). `routes` is `None` when the
/// route map could not be derived, which every such region then says.
pub fn with_automation(
    map: Regions,
    routes: Option<&crate::routes::RouteMap>,
    rows: &[&(Job, Vec<Step>)],
    w: &Windows,
) -> Regions {
    let regions = map
        .regions
        .into_iter()
        .map(|mut r| {
            if AUTOMATION_REGIONS.contains(&r.name.as_str()) {
                let exits = routes.map(|m| completion_exits(m, &r.name));
                let moved = exits
                    .as_ref()
                    .map(|e| moved_by(e, rows, w))
                    .unwrap_or_default();
                r.kpi.push(automation_measure(exits.as_ref(), moved, w));
            }
            r
        })
        .collect();
    Regions { regions, ..map }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    fn done_by(job: &Job, slug: &str, at: &str, by: Option<&str>) -> Step {
        let mut s = step(job, slug, StepStatus::Completed, Some(at));
        s.completed_by = by.map(|a| a.parse().unwrap());
        s
    }

    fn exits() -> BTreeSet<Exit> {
        [("pr-train", "merged"), ("gate-run", "record-verdict")]
            .into_iter()
            .map(|(k, s)| Exit::by(k, s))
            .collect()
    }

    /// The route map a live instance seeded from the tree derives: the
    /// Workflow bundle, the station bundle and the stations the
    /// protocols project (as `routes`' own tests build it).
    fn tree_routes() -> crate::routes::RouteMap {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-26T05:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("the platform Workflow bundle parses");
        let mut stations =
            crate::seed_loader::load_stations(crate::station_seed::platform_stations_path())
                .expect("the platform station bundle parses");
        let names: Vec<String> = stations.iter().map(|s| s.name.clone()).collect();
        stations.extend(crate::station_projection::derived_stations(
            &specs, &names, now,
        ));
        crate::routes::derive_from(&specs, &stations, None, now)
    }

    /// EVERY `via` SHAPE THE WALK WRITES, read for the step it completed:
    /// a completion (bare or with its branch keys) names its own step; a
    /// completion that closed the packet names the step behind the
    /// terminal; admission, a job key, a marker, a closing marker and the
    /// aged-out window name none.
    #[test]
    fn a_crossings_completed_step_is_read_off_its_via() {
        assert_eq!(completed_step("pr", "completed"), Some("pr"));
        assert_eq!(
            completed_step("triage", "completed, disposition=build"),
            Some("triage")
        );
        assert_eq!(
            completed_step(
                "green",
                "closed, after record-verdict completed, verdict=green"
            ),
            Some("record-verdict")
        );
        assert_eq!(
            completed_step("merged", "closed, after merged completed"),
            Some("merged")
        );
        for (step, via) in [
            ("filed", "admitted"),
            ("pr", "job.metadata.lane=x"),
            ("review", "marker hold set"),
            ("review", "marker hold released"),
            ("off", "closed, after review marker hold released"),
            ("off", "closed, after pr job.metadata.lane=x"),
            ("arrived", crate::routes::AGED_OUT),
        ] {
            assert_eq!(completed_step(step, via), None, "{step} by {via}");
        }
    }

    /// THE STEPS WHOSE COMPLETION LEAVES A REGION, on the tree's own
    /// walks: a completion counts, and so does one that closed the
    /// packet (named by the step that closed it, with the terminal it
    /// closed on); a marker, a job key and an aged-out window name no one.
    #[test]
    fn a_regions_completion_exits_are_the_steps_its_crossings_complete() {
        let map = tree_routes();
        let closes = |k: &str, s: &str, t: &str| Exit {
            closes: Some(t.into()),
            ..Exit::by(k, s)
        };
        let gates = completion_exits(&map, "gates");
        assert!(
            gates.contains(&closes("gate-run", "record-verdict", "green")),
            "{gates:?}"
        );
        assert!(gates.contains(&Exit::by("pr-train", "merged")), "{gates:?}");
        assert!(
            !gates
                .iter()
                .any(|e| e.workflow == "gate-run" && e.step == "green"),
            "a terminal is not the step: {gates:?}"
        );
        let dock = completion_exits(&map, "dock");
        assert!(dock.contains(&Exit::by("pr-train", "pr")), "{dock:?}");
        assert!(
            !dock
                .iter()
                .any(|e| e.workflow == "ship-a-change" && e.step == "review"),
            "a marker moves nobody: {dock:?}"
        );
        assert!(completion_exits(&map, "shed").contains(&closes(
            "ship-a-change",
            "proven",
            "merged"
        )));
        assert!(
            completion_exits(&map, "garage").is_empty(),
            "the garage is left by markers only"
        );
        assert!(
            completion_exits(&map, "arrivals").is_empty(),
            "arrivals is left by aging out"
        );
    }

    /// THE ARITHMETIC, over a fixture of steps with mixed `completed_by`:
    /// only exit steps, only completed, only inside the window, each
    /// actor in its class, a step read twice counted once.
    #[test]
    fn the_reading_counts_exit_completions_in_the_window_by_class() {
        let train = job("pr-train", "t", JobStatus::Closed, json!({}));
        let gate = job("gate-run", "g", JobStatus::Closed, json!({}));
        let train_row = (
            train.clone(),
            vec![
                // In the window, by the conductor: machine.
                done_by(
                    &train,
                    "merged",
                    "2026-09-19T10:00:00Z",
                    Some("automation:train-conductor"),
                ),
                // Not an exit step: not counted, whoever did it.
                done_by(&train, "pr", "2026-09-19T10:00:00Z", Some("emp-david")),
                // An exit step still ready: not a move.
                step(&train, "merged", StepStatus::Ready, None),
            ],
        );
        let gate_row = (
            gate.clone(),
            vec![
                done_by(
                    &gate,
                    "record-verdict",
                    "2026-09-19T09:00:00Z",
                    Some("automation:gate-runner"),
                ),
                done_by(
                    &gate,
                    "record-verdict",
                    "2026-09-19T08:00:00Z",
                    Some("agent-claude"),
                ),
                done_by(
                    &gate,
                    "record-verdict",
                    "2026-09-19T07:00:00Z",
                    Some("emp-david"),
                ),
                done_by(&gate, "record-verdict", "2026-09-19T06:00:00Z", None),
                // The previous window: outside this reading.
                done_by(
                    &gate,
                    "record-verdict",
                    "2026-09-18T06:00:00Z",
                    Some("emp-david"),
                ),
            ],
        );
        let w = Windows::of(t(NOW), 24);
        let rows = [&train_row, &gate_row, &gate_row];
        let moved = moved_by(&exits(), &rows, &w);
        assert_eq!(
            moved,
            MovedBy {
                machine: 2,
                agent: 1,
                human: 1,
                unknown: 1
            }
        );
        let m = automation_measure(Some(&exits()), moved, &w);
        assert_eq!(m.name, MACHINE_MOVED);
        assert_eq!(m.unit, "%");
        assert_eq!(m.value, Some(50.0));
        assert_eq!(
            m.text,
            "machine-moved 50% · 2 hand-moved/day · 1 unattributed"
        );
        // A wider window spreads the same hand moves over more days.
        let week = Windows::of(t(NOW), 48);
        let m = automation_measure(Some(&exits()), moved_by(&exits(), &rows, &week), &week);
        assert_eq!(
            m.text,
            "machine-moved 40% · 1.5 hand-moved/day · 1 unattributed"
        );
    }

    /// A ship-a-change car as the record holds it: `done` steps as
    /// `(slug, completed_at, completed_by)`, the rest at their status, and
    /// `merged`'s `blocked_by` the steps its ready_when names, as
    /// materialisation writes it (`registry::predicate_step_refs`).
    fn car(done: &[(&str, &str, &str)], rest: &[(&str, StepStatus)]) -> (Job, Vec<Step>) {
        let status = if rest.is_empty() {
            JobStatus::Closed
        } else {
            JobStatus::Open
        };
        let car = job("ship-a-change", "car", status, json!({}));
        let mut steps: Vec<Step> = done
            .iter()
            .map(|(slug, at, by)| done_by(&car, slug, at, Some(by)))
            .chain(rest.iter().map(|(slug, st)| step(&car, slug, *st, None)))
            .collect();
        let named: Vec<StepId> = steps
            .iter()
            .filter(|s| {
                matches!(
                    s.spec_slug.as_deref(),
                    Some("landed-review" | "proven" | "review")
                )
            })
            .map(|s| s.id)
            .collect();
        if let Some(m) = steps
            .iter_mut()
            .find(|s| s.spec_slug.as_deref() == Some("merged"))
        {
            m.blocked_by = named;
        }
        (car, steps)
    }

    /// A CONJUNCTIVE TERMINAL'S EXIT IS THE ACT OF WHICHEVER STEP IT
    /// CONJOINS COMPLETED LAST (backlog 8bc32104). ship-a-change's
    /// `merged` is ready when `steps.proven.done AND
    /// (steps.landed-review.done OR …)`, and the tree's walk names the
    /// shed's crossing `closed, after proven completed` — one order the
    /// walk tried, not the order the packet lived. Measured 2026-11-13 on
    /// the last 60 closed cars: 13 had `proven` completed by
    /// automation:ops-runner hours before an agent's `landed-review`, and
    /// `merged` closed in that second (d1e01362: proven 03:02:02 by
    /// ops-runner, landed-review 11:06:51 by agent-codex, merged 11:06:52
    /// by the marker rule), while the live shed read `machine-moved 100% ·
    /// 0 hand-moved/day`. Where `proven` IS last the machine moved it; a
    /// car whose `merged` has not closed has not left the shed at all.
    #[test]
    fn a_conjunctive_terminal_is_credited_to_the_step_that_completed_last() {
        let shed = completion_exits(&tree_routes(), "shed");
        // The measured case: the agent's landed-review closed it.
        let reviewed_last = car(
            &[
                ("review", "2026-09-19T00:30:00Z", "agent-claude"),
                ("proven", "2026-09-19T03:02:02Z", "automation:ops-runner"),
                ("landed-review", "2026-09-19T11:06:51Z", "agent-codex"),
                (
                    "merged",
                    "2026-09-19T11:06:52Z",
                    "automation:rule:complete-marker-on-step-ready",
                ),
            ],
            &[],
        );
        // The control: the landed-review came first, the proof closed it.
        let proven_last = car(
            &[
                ("review", "2026-09-19T01:00:00Z", "agent-claude"),
                ("landed-review", "2026-09-19T02:00:00Z", "agent-claude"),
                ("proven", "2026-09-19T05:00:00Z", "automation:ops-runner"),
                (
                    "merged",
                    "2026-09-19T05:00:00Z",
                    "automation:rule:complete-marker-on-step-ready",
                ),
            ],
            &[],
        );
        // Proven, still waiting on its landed-review: still in the shed.
        let still_in_the_shed = car(
            &[
                ("review", "2026-09-19T01:00:00Z", "agent-claude"),
                ("proven", "2026-09-19T06:00:00Z", "automation:ops-runner"),
            ],
            &[
                ("landed-review", StepStatus::Ready),
                ("merged", StepStatus::Pending),
            ],
        );
        let w = Windows::of(t(NOW), 24);
        let rows = [&reviewed_last, &proven_last, &still_in_the_shed];
        assert_eq!(
            moved_by(&shed, &rows, &w),
            MovedBy {
                machine: 1,
                agent: 1,
                human: 0,
                unknown: 0
            },
            "exits read: {shed:?}"
        );
    }

    /// A share of nothing is not zero, and each kind of nothing says
    /// which it is.
    #[test]
    fn no_completion_no_exit_and_no_routes_are_three_sentences_never_a_zero() {
        let w = Windows::of(t(NOW), 24);
        let quiet = automation_measure(Some(&exits()), MovedBy::default(), &w);
        assert_eq!(quiet.value, None);
        assert_eq!(
            quiet.text,
            "machine-moved: no step left this region in the window"
        );
        let none = automation_measure(Some(&BTreeSet::new()), MovedBy::default(), &w);
        assert_eq!(
            none.text,
            "machine-moved: no step completion leaves this region"
        );
        let unread = automation_measure(None, MovedBy::default(), &w);
        assert_eq!(unread.value, None);
        assert!(
            unread.text.contains("could not be derived"),
            "{}",
            unread.text
        );
    }

    /// Only the downstream regions carry it, after their own KPI.
    #[test]
    fn the_reading_rides_the_downstream_regions_after_their_own_kpi() {
        let status = empty_status();
        let map =
            crate::regions::regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let before: Vec<usize> = map.regions.iter().map(|r| r.kpi.len()).collect();
        let w = Windows::of(t(NOW), 24);
        let out = with_automation(map, None, &[], &w);
        for (r, n) in out.regions.iter().zip(before) {
            if AUTOMATION_REGIONS.contains(&r.name.as_str()) {
                assert_eq!(r.kpi.len(), n + 1, "{}", r.name);
                assert_eq!(r.kpi.last().unwrap().name, MACHINE_MOVED, "{}", r.name);
            } else {
                assert_eq!(r.kpi.len(), n, "{}", r.name);
            }
        }
    }
}
