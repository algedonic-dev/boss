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

/// THE STEPS WHOSE COMPLETION MOVES A PACKET OUT OF `region`, as
/// `(workflow, step slug)`: every protocol crossing on a route leaving
/// it that a step completion made. Read off the walks that draw the
/// rails, so the reading and the rails cannot disagree about which
/// steps leave a region. A declared hand-off names no step.
pub fn completion_exits(map: &crate::routes::RouteMap, region: &str) -> BTreeSet<(String, String)> {
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
            } => completed_step(step, via).map(|done| (workflow.clone(), done.to_string())),
            _ => None,
        })
        .collect()
}

/// The completions in the CURRENT window of the steps named in `exits`
/// (`(workflow, step slug)`), by who completed them. A step read twice
/// — the same packet carried by two of the handler's reads — counts
/// once.
pub fn moved_by(
    exits: &BTreeSet<(String, String)>,
    rows: &[&(Job, Vec<Step>)],
    w: &Windows,
) -> MovedBy {
    let mut seen = HashSet::new();
    rows.iter()
        .flat_map(|(job, steps)| steps.iter().map(move |s| (job, s)))
        .filter(|(job, s)| {
            s.status == StepStatus::Completed
                && s.spec_slug
                    .as_ref()
                    .is_some_and(|slug| exits.contains(&(job.kind.clone(), slug.clone())))
                && s.completed_at.is_some_and(|at| w.current(at))
        })
        .filter(|(_, s)| seen.insert(s.id))
        .fold(MovedBy::default(), |m, (_, s)| {
            m.with(s.completed_by.as_ref())
        })
}

/// The reading as the region's KPI carries it: `machine-moved 80% · 2
/// hand-moved/day`. The value is the machine share of the ATTRIBUTED
/// completions, `None` where there were none — a share of nothing is
/// not zero — and the sentence says which of the reasons it is.
pub fn automation_measure(
    exits: Option<&BTreeSet<(String, String)>>,
    moved: MovedBy,
    w: &Windows,
) -> Measure {
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

    fn exits() -> BTreeSet<(String, String)> {
        [("pr-train", "merged"), ("gate-run", "record-verdict")]
            .into_iter()
            .map(|(k, s)| (k.to_string(), s.to_string()))
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
    /// packet (named by the step that closed it, not by the terminal); a
    /// marker, a job key and an aged-out window name no one.
    #[test]
    fn a_regions_completion_exits_are_the_steps_its_crossings_complete() {
        let map = tree_routes();
        let pair = |k: &str, s: &str| (k.to_string(), s.to_string());
        let gates = completion_exits(&map, "gates");
        assert!(
            gates.contains(&pair("gate-run", "record-verdict")),
            "{gates:?}"
        );
        assert!(gates.contains(&pair("pr-train", "merged")), "{gates:?}");
        assert!(
            !gates.contains(&pair("gate-run", "green")),
            "a terminal is not the step: {gates:?}"
        );
        let dock = completion_exits(&map, "dock");
        assert!(dock.contains(&pair("pr-train", "pr")), "{dock:?}");
        assert!(
            !dock.contains(&pair("ship-a-change", "review")),
            "a marker moves nobody: {dock:?}"
        );
        assert!(completion_exits(&map, "shed").contains(&pair("ship-a-change", "proven")));
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
