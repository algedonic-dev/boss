//! THE ROUTES AS THE LIVE READERS JUDGE THEM: [`crate::routes`], plus
//! two facts its derivation leaves out (backlog df62a1eb).
//!
//! 1. AN AGENT STEP IS DISPATCHED FROM EVERY REGION IT IS READY IN. The
//!    walk keeps where a step FIRST stands while ready (`Walk::at`), and
//!    `verb:dispatch` origins were resolved from that one region. A held
//!    car stands in the garage with `review` still ready — the walk
//!    reaches that state through the hold marker — and its review run is
//!    dispatched from there, so every such dispatch read as an undeclared
//!    garage -> shop-floor move and troubled the shop floor (live,
//!    2026-10-17, and 2026-10-19 08:03Z-09:09Z, three rows). The same
//!    held for a merged car at the landed-review station, which the live
//!    partition stands in MARSHALLING and the one-packet placement placed
//!    nowhere, because it judged the stations only for the inbound and
//!    stationed families (backlog 15418f03; live 2026-10-20 and
//!    2026-10-22, every landed-review dispatch an undeclared
//!    marshalling -> shop-floor move): the ready regions are read with
//!    [`place_alone_at_every_station`].
//! 2. A DISPATCH OFF A PACKET PINNED TO A RETIRED VERSION is judged
//!    against the version it runs under. In-flight packets stay pinned to
//!    the version they were admitted under; only active rows were walked,
//!    so the judge answered unknown for every dispatch naming an older
//!    one. The readers hand the registry the versions their dispatches
//!    name ([`pinned_versions`]), and each retired row handed over is
//!    walked for its dispatch origins only — it draws no protocol line,
//!    because the map is today's protocols.
//!
//! WHY A SEPARATE MODULE, NOT AN EDIT TO `routes.rs`. That file and
//! `infra/platform/yard/handoffs.toml` are inputs of the move-diagnosis
//! model's content address (`move_diagnosis::model`,
//! `registered-agent-handoff-v2`), pinned byte-for-byte by
//! `tests/handoff_model_source.rs` and
//! `tests/move_admission_observation_http.rs`. The diagnosis keeps the
//! derivation it was deployed with, so its identifier and its answers
//! stay one fact; the live readers (`http/routes.rs::derived` — the
//! routes, regions and moves reads and the mover's judge) take this one.
//! Cars 50ded63d and 3976779c made the same call.
//!
//! No second walk: the protocol walk is `routes::walk` itself, and the
//! regions a step is ready in are read off the placements it asks for —
//! every state it reaches is placed through the closure handed to it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use boss_core::job::StepStatus;

use crate::moves::{DispatchContext, RouteCount};
use crate::regions::{
    Instant, Placed, Stations, family_of, inbound_kinds, place_alone, place_alone_at_every_station,
};
use crate::registry::{WorkflowSpec, WorkflowStatus};
use crate::routes::{
    HANDOFFS_TOML, Route, RouteMap, Source, Walk, Walked, derive, hand_offs, markers_for,
    seeds_for, stands,
};

/// Every region the packet stands in while each step is ready, by slug.
pub type ReadyIn = BTreeMap<String, BTreeSet<&'static str>>;

/// One row, walked by [`crate::routes::walk`] and placed by the one
/// placement function — and, read off those same placements, every
/// region each step is ready in.
pub fn walk_ready(
    spec: &WorkflowSpec,
    stations: &Stations,
    inbound: &[String],
    now: Instant,
) -> (Walk, ReadyIn) {
    let family = family_of(&spec.kind, inbound);
    let ready: RefCell<ReadyIn> = RefCell::new(BTreeMap::new());
    let walked = crate::routes::walk(
        spec,
        &seeds_for(&spec.kind, stations),
        &markers_for(&spec.kind, stations),
        now,
        crate::regions::DEFAULT_WINDOW_HOURS,
        |job, steps, at| {
            let placed = place_alone(
                &(job.clone(), steps.to_vec()),
                family,
                stations,
                at,
                crate::regions::DEFAULT_WINDOW_HOURS,
            );
            // Only a state placed NOW: the walk also places a closed
            // packet one window later, to find its off-ramp. The region
            // a step is READY in is read as the live partition reads it,
            // with the stations judged over every family (backlog
            // 15418f03: a car at the landed-review station stands in
            // marshalling); the walk itself keeps the frozen placement,
            // so its protocol lines stay the derivation's.
            let live = (at == now).then(|| {
                place_alone_at_every_station(
                    &(job.clone(), steps.to_vec()),
                    family,
                    stations,
                    at,
                    crate::regions::DEFAULT_WINDOW_HOURS,
                )
            });
            if let Some(Placed::At(r)) = &live {
                let mut m = ready.borrow_mut();
                for slug in steps
                    .iter()
                    .filter(|s| s.status == StepStatus::Ready)
                    .filter_map(|s| s.spec_slug.clone())
                {
                    m.entry(slug).or_default().insert(*r);
                }
            }
            placed
        },
    );
    (walked, ready.into_inner())
}

/// THE VERSIONS a set of dispatches was registered under, by kind: what
/// a reader hands the registry so each pinned version is walked beside
/// the active row. A refused or absent context names none.
pub fn pinned_versions<'a>(
    contexts: impl IntoIterator<Item = Option<&'a DispatchContext>>,
) -> BTreeSet<(String, i32)> {
    contexts
        .into_iter()
        .flatten()
        .filter_map(|c| match c {
            DispatchContext::Registered {
                source_kind,
                source_version,
                ..
            } => Some((source_kind.clone(), *source_version)),
            DispatchContext::Refused { .. } => None,
        })
        .collect()
}

/// [`crate::routes::derive_from`], with every agent step's dispatch
/// declared from each region it is ready in, and the RETIRED platform
/// rows in `specs` walked for their dispatch origins (see the module
/// note).
pub fn derive_from(
    specs: &[WorkflowSpec],
    stations: &[crate::stations::StationSpec],
    observed: Option<&[RouteCount]>,
    now: Instant,
) -> RouteMap {
    let stations = Stations::of(stations);
    let active: Vec<WorkflowSpec> = specs
        .iter()
        .filter(|s| s.status == WorkflowStatus::Active)
        .cloned()
        .collect();
    let inbound = inbound_kinds(&active);
    let walk_rows = |status: WorkflowStatus| {
        let mut out: Vec<(Walk, ReadyIn)> = specs
            .iter()
            .filter(|s| s.status == status && s.category == "platform")
            .map(|s| walk_ready(s, &stations, &inbound, now))
            .collect();
        out.sort_by(|(a, _), (b, _)| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
        out
    };
    let current = walk_rows(WorkflowStatus::Active);
    let pinned = walk_rows(WorkflowStatus::Retired);
    let walks: Vec<Walk> = current.iter().map(|(w, _)| w.clone()).collect();
    let (mut map, declared) = match hand_offs(HANDOFFS_TOML) {
        Ok(h) => (derive(&walks, &h, observed), h),
        Err(e) => {
            let mut map = derive(&walks, &[], observed);
            map.refused.push(e);
            (map, Vec::new())
        }
    };
    for h in declared
        .iter()
        .filter(|h| h.from_agent_steps && h.by == "verb:dispatch" && h.from.is_none())
    {
        // An unresolvable destination is already refused, by name, in
        // `map.refused`.
        let Some(to) = h.to.as_ref().and_then(|at| stands(&walks, at).ok()) else {
            continue;
        };
        for (w, ready) in current.iter().chain(&pinned) {
            for step in &w.agent_steps {
                for from in ready.get(step).into_iter().flatten() {
                    if *from == to {
                        continue;
                    }
                    declare(
                        &mut map,
                        from,
                        to,
                        Source::HandOff {
                            by: h.by.clone(),
                            from: Some(format!("{}@{step}", w.kind)),
                            to: h.to.clone(),
                            why: h.why.clone(),
                            terminal: None,
                            agent_version: Some(w.version),
                        },
                    );
                }
            }
        }
    }
    map.walked.extend(pinned.iter().map(|(w, _)| Walked {
        kind: w.kind.clone(),
        version: w.version,
        states: w.states,
        truncated: w.truncated,
    }));
    map
}

/// Add one declaring source to the route `from -> to`, once, keeping the
/// routes in the (from, to) order `derive` serves them in.
fn declare(map: &mut RouteMap, from: &str, to: &str, source: Source) {
    let key = (Some(from.to_string()), Some(to.to_string()));
    match map
        .routes
        .binary_search_by(|r| (&r.from, &r.to).cmp(&(&key.0, &key.1)))
    {
        Ok(i) => {
            let r = &mut map.routes[i];
            if !r.sources.contains(&source) {
                r.sources.push(source);
            }
            r.declared = true;
        }
        Err(i) => map.routes.insert(
            i,
            Route {
                from: key.0,
                to: key.1,
                declared: true,
                sources: vec![source],
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        chrono::DateTime::parse_from_rfc3339("2026-09-26T05:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// The tree's protocols and stations, as `routes::tests::tree` reads
    /// them.
    fn tree() -> (Vec<WorkflowSpec>, Vec<crate::stations::StationSpec>) {
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("the platform Workflow bundle parses");
        let mut stations =
            crate::seed_loader::load_stations(crate::station_seed::platform_stations_path())
                .expect("the platform station bundle parses");
        let names: Vec<String> = stations.iter().map(|s| s.name.clone()).collect();
        stations.extend(crate::station_projection::derived_stations(
            &specs,
            &names,
            now(),
        ));
        (specs, stations)
    }

    fn route<'a>(map: &'a RouteMap, from: Option<&str>, to: Option<&str>) -> Option<&'a Route> {
        map.routes
            .iter()
            .find(|r| r.from.as_deref() == from && r.to.as_deref() == to)
    }

    /// A registered review dispatch off `ship-a-change@review` at
    /// `version`, as the mover records its context.
    fn review_dispatch(version: i32) -> DispatchContext {
        dispatch_off(crate::car::REVIEW_SLUG, version)
    }

    /// A registered dispatch off `ship-a-change@<step>` at `version`.
    fn dispatch_off(step: &str, version: i32) -> DispatchContext {
        DispatchContext::Registered {
            run: "run".into(),
            parent: "car".into(),
            step_id: format!("{step}-step"),
            source_kind: crate::regions::CAR_KIND.into(),
            source_version: version,
            source_step: step.into(),
        }
    }

    fn active_version(specs: &[WorkflowSpec], kind: &str) -> i32 {
        specs
            .iter()
            .find(|s| s.kind == kind && s.status == WorkflowStatus::Active)
            .map(|s| s.version)
            .expect("the tree has an active row of the kind")
    }

    fn judge(map: &RouteMap, from: &str, version: i32) -> Option<bool> {
        judge_step(map, from, crate::car::REVIEW_SLUG, version)
    }

    fn judge_step(map: &RouteMap, from: &str, step: &str, version: i32) -> Option<bool> {
        map.judges_context(
            Some(from),
            Some("shop-floor"),
            Some(crate::agent_budget::RUN_KIND),
            None,
            Some("packet"),
            Some(&dispatch_off(step, version)),
        )
    }

    /// A LANDED-REVIEW DISPATCHED OFF A CAR IN MARSHALLING IS DECLARED
    /// (backlog 15418f03). A merged car whose `landed-review` is ready
    /// stands at the landed-review station and nothing else claims it,
    /// so the live partition puts it in marshalling — and its review run
    /// is dispatched from there. The one-packet walk judged the stations
    /// only for the inbound and stationed families, never placed a car
    /// in marshalling, and so declared no origin there: live 2026-10-20
    /// and again 2026-10-22, every such dispatch read as an undeclared
    /// marshalling -> shop-floor move and troubled the shop floor.
    #[test]
    fn a_landed_review_dispatched_from_marshalling_is_declared() {
        let (specs, stations) = tree();
        let map = derive_from(&specs, &stations, None, now());
        let version = active_version(&specs, crate::regions::CAR_KIND);
        assert_eq!(
            judge_step(&map, "marshalling", crate::car::LANDED_REVIEW_SLUG, version),
            Some(true),
            "a landed-review dispatched off a car standing in marshalling is declared"
        );
        let r = route(&map, Some("marshalling"), Some("shop-floor"))
            .expect("the landed-review dispatch is drawn from marshalling");
        assert!(r.sources.iter().any(|s| matches!(s,
            Source::HandOff { by, from: Some(from), .. }
            if by == "verb:dispatch" && from == "ship-a-change@landed-review"
        )));
        // A car is a pipeline kind, never inbound: receiving cannot hold
        // one, so no origin is declared there — a dispatch off a car the
        // map placed in receiving would still be trouble.
        assert_eq!(
            judge_step(&map, "receiving", crate::car::LANDED_REVIEW_SLUG, version),
            Some(false)
        );
    }

    /// A REVIEW DISPATCHED FROM THE GARAGE IS DECLARED (backlog
    /// df62a1eb): a held car stands there with `review` ready.
    #[test]
    fn a_review_dispatched_from_the_garage_is_declared() {
        let (specs, stations) = tree();
        let map = derive_from(&specs, &stations, None, now());
        let r = route(&map, Some("garage"), Some("shop-floor"))
            .expect("the review dispatch is drawn from the garage");
        assert!(r.declared);
        assert!(r.sources.iter().any(|s| matches!(s,
            Source::HandOff { by, from: Some(from), to: Some(to), .. }
            if by == "verb:dispatch" && from == "ship-a-change@review"
                && to == "agent-run@briefed"
        )));
        let version = active_version(&specs, crate::regions::CAR_KIND);
        for from in ["dock", "garage"] {
            assert_eq!(
                judge(&map, from, version),
                Some(true),
                "a review dispatched off a car in the {from} is declared"
            );
        }
        // The routes are still served in (from, to) order, each once.
        let keys: Vec<_> = map.routes.iter().map(|r| (&r.from, &r.to)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(keys, sorted);
    }

    /// A DISPATCH PINNED TO A RETIRED VERSION IS JUDGED AGAINST IT
    /// (backlog df62a1eb); a version never handed over stays unknown,
    /// never a region-pair permission, and a pinned walk draws no
    /// protocol line.
    #[test]
    fn a_dispatch_pinned_to_a_superseded_version_is_judged_against_it() {
        let (mut specs, stations) = tree();
        let active = active_version(&specs, crate::regions::CAR_KIND);
        let mut older = specs
            .iter()
            .find(|s| s.kind == crate::regions::CAR_KIND && s.version == active)
            .cloned()
            .unwrap();
        older.version = active - 1;
        older.status = WorkflowStatus::Retired;
        specs.push(older);
        let map = derive_from(&specs, &stations, None, now());
        assert_eq!(judge(&map, "dock", active - 1), Some(true));
        assert_eq!(judge(&map, "garage", active - 1), Some(true));
        assert_eq!(judge(&map, "dock", active - 2), None, "never read: unknown");
        assert!(
            map.walked
                .iter()
                .any(|w| w.kind == crate::regions::CAR_KIND && w.version == active - 1)
        );
        let protocol_lines = |m: &RouteMap| {
            m.routes
                .iter()
                .flat_map(|r| &r.sources)
                .filter(|s| matches!(s, Source::Workflow { .. }))
                .count()
        };
        let today = crate::routes::derive_from(&specs, &stations, None, now());
        assert_eq!(protocol_lines(&map), protocol_lines(&today));
    }

    /// Every route the frozen derivation declares, this one declares
    /// with the same sources first: it only ADDS dispatch origins.
    #[test]
    fn it_only_adds_dispatch_origins_to_the_frozen_derivation() {
        let (specs, stations) = tree();
        let frozen = crate::routes::derive_from(&specs, &stations, None, now());
        let live = derive_from(&specs, &stations, None, now());
        for r in &frozen.routes {
            let l = route(&live, r.from.as_deref(), r.to.as_deref()).expect("kept");
            assert_eq!(l.declared, r.declared || l.declared);
            assert_eq!(&l.sources[..r.sources.len()], &r.sources[..]);
            assert!(l.sources[r.sources.len()..].iter().all(|s| matches!(s,
                Source::HandOff { by, agent_version: Some(_), .. } if by == "verb:dispatch")));
        }
        assert_eq!(live.refused, frozen.refused);
    }

    #[test]
    fn pinned_versions_name_registered_dispatches_only() {
        let refused = DispatchContext::Refused { why: "x".into() };
        let got = pinned_versions([Some(&review_dispatch(9)), Some(&refused), None]);
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            [(crate::regions::CAR_KIND.to_string(), 9)]
        );
    }
}
