//! The platform station bundle — `infra/platform/stations/`, one
//! `<name>.toml` per station — is well-formed, viable and complete, read
//! with no database.
//!
//! WHY (backlog 393d3234, consolidation H4, car 1 of 4). Until
//! 2026-09-18 seven migrations were the only place a platform station
//! was declared. The bundle is now the home: the seed publishes it
//! insert-if-missing by (name, version) at every start, and
//! `infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
//! stations` in any migration newer than the cutover. The equality pin
//! against the migrations' own rows is
//! `the_stations_bundle_is_the_migrations_pg.rs`; this file holds the
//! rules that need no database and range over every file.
//!
//! The `[[station]]` shape is authored, not generated — `StationSpec`
//! does not serialize to TOML (TOML has no null) — and the loader
//! refuses a file named for a station it does not hold, a file holding
//! two, a status other than `active`, and a row the viability lint
//! rejects. The five names below are the ones the migrations seeded;
//! a station added later is a file dropped in and a line here.

use boss_jobs::seed_loader::{bundle_files, load_stations};
use boss_jobs::station_seed::platform_stations_path;
use boss_jobs::{StationRegistry, StationSpec};
use std::path::Path;

fn bundle() -> Vec<StationSpec> {
    load_stations(platform_stations_path()).expect("the platform station bundle parses")
}

#[test]
fn every_platform_station_uses_automatic_route_placement() {
    let rows = bundle();
    assert!(!rows.is_empty(), "the bundle holds stations");
    for station in rows {
        assert!(
            station.floor.is_none(),
            "automatic route placement remains the default"
        );
    }
}

/// One station per file, and the file is named for it, so `ls` answers
/// "which stations does a deployment seed".
#[test]
fn the_bundle_is_one_file_per_station() {
    let dir = Path::new(platform_stations_path());
    let files = bundle_files(dir).expect("the bundle directory lists its station files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a station file has a UTF-8 stem");
        let rows = load_stations(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let names: Vec<&str> = rows.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [stem],
            "{} holds exactly its own station",
            file.display()
        );
    }
    let mut names: Vec<String> = bundle().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "algedonic",
            "build",
            "converge",
            "deploy",
            "design",
            "design-decided",
            "design-review",
            "edge",
            "engineering",
            "estate",
            "finance",
            "forge",
            "forge-ci",
            "gates",
            "landed-review",
            "loading-dock",
            "maintenance",
            "me",
            "my-watchlist",
            "operations",
            "probe-shed",
            "q.platform-admin.task",
            "repair",
            "review",
            "vault",
        ],
        "the five platform stations the migrations seeded, plus `design-decided` \
         (declared here only, 2026-09-24, backlog 08372fdb), `landed-review` \
         (2026-10-09, backlog bd331d38), `gates` (2026-11-03, item 0a78f8f8) and \
         the converge's three, `deploy`, `converge` and `probe-shed` (2026-11-05, \
         backlog 364b27a3), `forge-ci` (2026-11-05, backlog c9b9bacc), the station \
         tree's root, departments and standing rooms (2026-11-06, backlog 3f58c823), \
         the `build` station under engineering (2026-11-09, backlog b5c815e4), \
         `finance` and the stage stations `design`, `review` and `maintenance` \
         (2026-11-10, design c0ae096f, backlog 599fec3e), less `executive`, \
         dissolved into the root (design 2910ec3d, backlog 878fd1e7), `me`, the \
         person's own station (design 01fa5bdf, backlog 5cb56bb3), and no other"
    );
}

// ---------------------------------------------------------------------------
// Equipment — the factory taxonomy's M2 (item 0a78f8f8, design 4348b857)
// ---------------------------------------------------------------------------

/// The equipment stations, as decided. The first two on the item
/// (`decided_2026_11_03`): the dock, operated by the train conductor
/// with ONE bay because one train boards at a time (Q1: the batch kind
/// became equipment), and the gates, operated by the gate runner with
/// the bays that are the gate capacity. Then the converge's three
/// (design ccfe87ff, question converge-stations, 2026-11-05; backlog
/// 364b27a3), each with one bay and its reason in its own file: the
/// deploy, the converge and the probe shed. Then the forge's CI runner
/// (backlog c9b9bacc), one bay because the forge has one runner slot,
/// the station whose stock disk:ci-workspaces is. Every other platform
/// station is generic and declares neither field.
#[test]
fn the_dock_the_gates_and_the_converges_three_are_equipment_and_every_other_station_is_generic() {
    let mut equipment: Vec<(String, Option<String>, Option<i32>)> = bundle()
        .into_iter()
        .filter(|s| s.kind == boss_jobs::StationKind::Equipment)
        .map(|s| (s.name, s.automaton, s.bays))
        .collect();
    equipment.sort();
    let row = |name: &str, automaton: &str, bays: i32| {
        (name.to_string(), Some(automaton.to_string()), Some(bays))
    };
    assert_eq!(
        equipment,
        vec![
            row("converge", "automation:train-conductor", 1),
            row("deploy", "automation:cluster-deploy-runner", 1),
            row("forge-ci", "automation:forge-actions-runner", 1),
            row("gates", "automation:gate-runner", 3),
            row("loading-dock", "automation:train-conductor", 1),
            row("probe-shed", "automation:ops-runner", 1),
        ]
    );
    for s in bundle() {
        if s.kind != boss_jobs::StationKind::Equipment {
            assert_eq!((&s.automaton, s.bays), (&None, None), "{}", s.name);
        }
    }
}

/// An equipment station's automaton is a REGISTERED one: the id it
/// names is a row of `infra/platform/automations/`, the registry of
/// what may sign as an automation. A parse alone would admit a typo
/// that operates nothing. Asked through the one function the publish
/// door and the platform seed ask (backlog d0cf2cdc), not a copy of it.
#[test]
fn every_equipment_automaton_is_a_registered_automation() {
    let registered: Vec<String> = boss_jobs::agents::automations::load_automations_dir(Path::new(
        boss_jobs::agents::automations::platform_automations_path(),
    ))
    .expect("the platform automations bundle loads")
    .into_iter()
    .map(|a| a.id)
    .collect();
    let rows = bundle();
    assert!(
        rows.iter().any(|s| s.automaton.is_some()),
        "the bundle declares equipment, so the pin judges something"
    );
    let problems = boss_jobs::stations::validate_station_automatons(&rows, &registered);
    assert!(
        problems.is_empty(),
        "{}",
        problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// THE GATE CAPACITY HAS ONE HOME (CLAUDE.md §9a). It is the `gates`
/// row's `bays`, and the delivery policy that held it as
/// `gate_max_concurrent` until v11 declares no such key any more — the
/// loader refuses an unknown key, so a file that kept it would not load.
#[test]
fn the_gate_capacity_is_the_gates_rows_bays() {
    let gates = station(boss_jobs::stations::GATES_STATION);
    assert_eq!(gates.kind, boss_jobs::StationKind::Equipment);
    // Three since 2026-11-07 (backlog a082371d, design 121f44a1 D2):
    // the Micron gate volume took the I/O bound away.
    assert_eq!(gates.bays, Some(3));
    assert_eq!(gates.predicate.kind.as_deref(), Some("gate-run"));
    assert_eq!(gates.predicate.status, Some(JobStatus::Open));
    let policy = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infra/platform/delivery-policy/train-conductor.toml"),
    )
    .expect("the delivery policy bundle reads");
    assert!(
        !policy
            .lines()
            .any(|l| l.trim_start().starts_with("gate_max_concurrent")),
        "the delivery policy no longer declares the gate capacity — it is bays:gates"
    );
}

/// The one compiled fallback a reader uses when the gates row cannot be
/// read is the row's own number — a fallback, never a second policy.
#[test]
fn the_compiled_gate_bays_are_the_gates_rows() {
    assert_eq!(
        station(boss_jobs::stations::GATES_STATION).bays,
        Some(boss_jobs::stations::COMPILED_GATE_BAYS),
        "infra/platform/stations/gates.toml `bays` and \
         boss_jobs::stations::COMPILED_GATE_BAYS drifted: move them together"
    );
}

/// Pass item 4 of design ccfe87ff (backlog 1bb8c479): the pin. Every
/// edge a platform station declares — a route a packet takes next —
/// names an active row of this bundle, judged by the same check the seed
/// runs before any publish. And at least one station declares one, so
/// the floor's layout has a route to read and this pin is not vacuous.
#[test]
fn every_edge_a_platform_station_declares_names_a_declared_station() {
    let stations = bundle();
    let failures = boss_jobs::stations::validate_station_edges(&stations, &stations);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(
        stations.iter().any(|s| !s.edges.is_empty()),
        "no platform station declares where its packets go next"
    );
}

/// The refusal names the station that declares the edge and the station
/// it could not find, and a retired row is not a station an edge may
/// name.
#[test]
fn an_edge_to_a_station_no_active_row_declares_is_refused_naming_both() {
    let with_edge = |to: &str| {
        let mut stations = bundle();
        let gates = stations
            .iter_mut()
            .find(|s| s.name == "gates")
            .expect("the gates row");
        gates.edges = vec![to.to_string()];
        stations
    };
    let judged = |stations: &[StationSpec]| -> Vec<String> {
        boss_jobs::stations::validate_station_edges(stations, stations)
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert!(
        judged(&with_edge("loading-dock")).is_empty(),
        "loading-dock is a declared station"
    );
    let failures = judged(&with_edge("no-such-station"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("[gates]") && failures[0].contains("`no-such-station`"),
        "{}",
        failures[0]
    );
    let retired: Vec<StationSpec> = with_edge("loading-dock")
        .into_iter()
        .map(|mut s| {
            if s.name == "loading-dock" {
                s.status = boss_jobs::registry::WorkflowStatus::Retired;
            }
            s
        })
        .collect();
    assert!(
        judged(&retired)
            .iter()
            .any(|f| f.contains("[gates]") && f.contains("`loading-dock`")),
        "a retired station row does not count"
    );
}

/// Every bundled row passes the same viability gate an API publish
/// passes (`station_lint::gate_active`) and can be published as-is
/// into an empty registry at its declared version.
#[tokio::test]
async fn every_bundled_station_is_viable_and_publishable() {
    let registry = boss_jobs::stations::InMemoryStations::new();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    for spec in bundle() {
        boss_jobs::station_lint::gate_active(&spec)
            .unwrap_or_else(|problems| panic!("{} is not viable: {problems:?}", spec.name));
        let declared = spec.version;
        let published = registry
            .publish_declared(spec, &actor, now)
            .await
            .expect("a bundle row publishes");
        assert_eq!(published.version, declared, "the declared version lands");
    }
}

/// A file that declares anything but an active row, or a version below
/// 1, is refused by the loader with the station named — a bundle row is
/// what a fresh deployment gets, and that is an active row.
#[test]
fn a_bundle_row_must_be_active_at_a_real_version() {
    let retired = "\
[[station]]
name = \"x\"
version = 1
status = \"retired\"
title = \"X\"
kind = \"group\"
[station.predicate]
kind = \"ship-a-change\"
";
    let err = boss_jobs::seed_loader::parse_stations(retired, "x.toml")
        .expect_err("a retired declaration is refused");
    assert!(err.to_string().contains("`x`"), "{err}");
    assert!(err.to_string().contains("retired"), "{err}");

    let err = boss_jobs::seed_loader::parse_stations(
        &retired
            .replace("version = 1", "version = 0")
            .replace("retired", "active"),
        "x.toml",
    )
    .expect_err("version 0 is refused");
    assert!(err.to_string().contains("version"), "{err}");

    // The deleted kind (item 0a78f8f8) is no declaration at all, and an
    // equipment row without its automaton and bays is not viable.
    let active = retired.replace("retired", "active");
    let err = boss_jobs::seed_loader::parse_stations(
        &active.replace("kind = \"group\"", "kind = \"batch\""),
        "x.toml",
    )
    .expect_err("batch is no station kind");
    assert!(err.to_string().contains("batch"), "{err}");
    let err = boss_jobs::seed_loader::parse_stations(
        &active.replace("kind = \"group\"", "kind = \"equipment\""),
        "x.toml",
    )
    .expect_err("an equipment row names its automaton and bays");
    assert!(
        err.to_string().contains("automaton") && err.to_string().contains("bays"),
        "{err}"
    );
}

/// A bundle row declares no upstream: the walk upstream is DERIVED from
/// the tree, as the parent station's page (backlog a1c0a0ff, design
/// deba3000). Seven rows hand-typed `href = "/system/yard"` and the dock
/// `/system/feedback`, both routes the IT consolidation retired on
/// 2026-08-31, so the pointer led nowhere for ten weeks and nothing read
/// it against the router. The loader refuses the key by name and says
/// where the link comes from instead.
#[test]
fn a_bundle_row_declares_no_upstream_because_the_tree_derives_it() {
    let declared = "\
[[station]]
name = \"x\"
version = 1
status = \"active\"
title = \"X\"
kind = \"group\"
[station.predicate]
kind = \"ship-a-change\"
[station.upstream]
label = \"THE YARD\"
href = \"/system/yard\"
";
    let err = boss_jobs::seed_loader::parse_stations(declared, "x.toml")
        .expect_err("a declared upstream is refused");
    let why = err.to_string();
    assert!(why.contains("`x`"), "{why}");
    assert!(why.contains("upstream") && why.contains("parent"), "{why}");

    // Every row the bundle holds parses, and so none declares one: the
    // loader refuses the whole bundle on the first that does, and
    // StationSpec has no field left to carry one (backlog d2cb38af).
    boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
        .expect("the platform station bundle parses");
}

// ---------------------------------------------------------------------------
// /it/design — IN, WORKING and OUT, as the two design stations declare them
// ---------------------------------------------------------------------------
//
// WHY (backlog 08372fdb, from page audit 79db48ae, 2026-09-23). /it/design
// rendered one station, `design-review`, whose lens asked for no steps, so
// three things were invisible: a review saved but not completed looked
// exactly like one never touched (the review step stays `ready` through a
// Save — only a `pending` step is flipped — so its answers are the only
// trace, and they live on the step); a design decided and waiting at
// `fold` vanished the moment its review completed; and nothing settled
// was shown at all — 34 designs closed on 2026-09-23 alone. These pin
// what the DATA must hand the page for each of the three, evaluated the
// way `GET /api/stations/{name}/queue` evaluates it, with no database.

use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use chrono::NaiveDate;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("a real September day")
}

fn station(name: &str) -> StationSpec {
    bundle()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("the bundle declares `{name}`"))
}

/// A design-doc packet opened on `day(1)` with its review and fold steps
/// in the given states — the two steps the stations read.
fn design(review: StepStatus, fold: StepStatus) -> (Job, Vec<Step>) {
    let mut job = Job::new(
        "design-doc",
        Subject::new("custom", "boss-platform"),
        "a design",
        "agent-claude",
        Priority::Standard,
        day(1),
    );
    job.status = JobStatus::Open;
    let mut r = Step::new(job.id, "review-design", "review", 1);
    r.spec_slug = Some("review".into());
    r.status = review;
    let mut f = Step::new(job.id, "task", "fold", 2);
    f.spec_slug = Some("fold".into());
    f.status = fold;
    (job, vec![r, f])
}

fn closed_on(mut packet: (Job, Vec<Step>), on: u32) -> (Job, Vec<Step>) {
    packet.0.status = JobStatus::Closed;
    packet.0.closed_on = Some(day(on));
    packet
}

/// IN — the review queue carries its members' steps, so a row can say
/// "saved, 2 of 3 answered" from the review step's own `resolutions`
/// instead of rendering a half-made decision as an untouched one.
#[test]
fn the_review_queue_carries_the_review_step_it_is_waiting_on() {
    let spec = station("design-review");
    let lens = spec.lens.as_ref().expect("design-review declares a lens");
    assert!(
        lens.with_steps,
        "design-review must carry steps: a saved review is visible only on its step"
    );
    assert!(
        lens.panels.iter().any(|p| p == "queue") && lens.panels.iter().any(|p| p == "decided"),
        "the page renders the queue AND the decided designs: {:?}",
        lens.panels
    );

    let waiting = design(StepStatus::Ready, StepStatus::Pending);
    let id = waiting.0.id.to_string();
    let q = boss_jobs::evaluate_station(&spec, vec![waiting], day(24));
    assert_eq!(q.total, 1);
    let steps = q
        .steps
        .get(&id)
        .expect("the member's steps ride the envelope");
    assert!(
        steps
            .iter()
            .any(|s| s.spec_slug.as_deref() == Some("review")),
        "the review step is among them"
    );
}

/// WORKING and OUT — `design-decided` holds every design whose review
/// completed: open ones are at `fold` (decided, not yet landed), closed
/// ones stay for the station's terminal window and then leave. A design
/// still under review is not decided and is not held here.
#[test]
fn the_decided_station_holds_folds_in_flight_and_recently_settled_designs() {
    let spec = station("design-decided");
    let window = spec
        .terminal_window_days
        .expect("design-decided declares a terminal window — OUT is its reason to exist");
    assert!(
        spec.lens.as_ref().is_some_and(|l| l.with_steps),
        "the fold step's status and `folded_into` are what the rows show"
    );

    let under_review = design(StepStatus::Ready, StepStatus::Pending);
    let at_fold = design(StepStatus::Completed, StepStatus::Active);
    let settled = closed_on(design(StepStatus::Completed, StepStatus::Completed), 20);
    let long_gone = closed_on(
        design(StepStatus::Completed, StepStatus::Completed),
        20 - window - 1,
    );
    let (fold_id, settled_id) = (at_fold.0.id.to_string(), settled.0.id.to_string());

    let q = boss_jobs::evaluate_station(
        &spec,
        vec![under_review, at_fold, settled, long_gone],
        day(20),
    );
    let mut held: Vec<String> = q.data.iter().map(|j| j.id.to_string()).collect();
    held.sort();
    let mut want = vec![fold_id.clone(), settled_id.clone()];
    want.sort();
    assert_eq!(
        held,
        want,
        "decided = the fold in flight and the design settled inside the window; \
         not the one still under review, not the one settled {} days ago",
        window + 1
    );
    assert!(q.steps.contains_key(&fold_id) && q.steps.contains_key(&settled_id));
}
