//! The station tree and the pools it holds from — T2+T5 of design
//! 4b27a65a (backlog 3f58c823), carried as one bundle car by David's
//! `bundle_decision_20261106_1944Z`, whose acceptance is pinned here:
//!
//! 1. **The paper reorganization is row edits alone.** Handoff §10: "drop
//!    Documentation, add Operations, move the back half of pr-train" is
//!    one station row deleted, four added and three `parent` edits — done
//!    here to the bundle's own TOML files, published through the seed,
//!    and judged by the tree rules, with no code between. So is
//!    re-parenting one station: one field and its version bump.
//! 2. **An old build reads the new rows.** The rollback pin, in the shape
//!    backlog fb893bba set for pools: the readers of the build before
//!    this car, frozen below, read every new station and pool row.
//! 3. **The tree read serves the real hosts** from the estate registry —
//!    every node infra/estate/estate.toml declares, joined to its pool.
//!
//! And the pins that keep the bundle one tree: one root, every row in it,
//! every machine an automation row, every estate node its host pool.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::actor::ActorId;
use boss_core::port::EventBus;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::pools::{InMemoryPools, PoolSpec, load_pools, platform_pools_path, seed_pools};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_stations;
use boss_jobs::station_seed::{platform_stations_path, seed_stations};
use boss_jobs::station_tree::{
    address_of, host_of, protocol_owner, subtree, validate_automation_stations,
    validate_protocol_owners, validate_protocol_subtree, validate_station_tree,
};
use boss_jobs::{InMemoryJobs, InMemoryStations, JobsRepository, StationRegistry, StationSpec};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The seed's root station, by its row's NAME. Addresses here are built
/// from it, never spelled whole: the address lives in the rows (handoff
/// §8.1), and these tests check that the rows derive the one asserted.
const ROOT: &str = "algedonic";

fn stations() -> Vec<StationSpec> {
    load_stations(platform_stations_path()).expect("the platform station bundle parses")
}

fn pools() -> Vec<PoolSpec> {
    load_pools(platform_pools_path()).expect("the platform pool bundle parses")
}

fn named<'a>(rows: &'a [StationSpec], name: &str) -> &'a StationSpec {
    rows.iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("the bundle declares `{name}`"))
}

fn id_of(rows: &[StationSpec], name: &str) -> String {
    named(rows, name).id.clone().expect("a tree row has an id")
}

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_762_000_000 + secs, 0).expect("fixed instant")
}

// -- every protocol names its owner (T6, backlog baac0b65) -------------------

/// Design c0ae096f §1 (David, 2026-11-10 ~19:40Z; car 2 of that design,
/// backlog baac0b65): the owner of every platform protocol, by station
/// NAME so the ids stay in rows. It supersedes the first assignment
/// (docs/design/station-tree.md §2, landed by car c5894471) on twelve
/// rows; the two maintenance families the table does not name follow
/// its prose: every `maintenance-*` row but the ledger two is
/// Operations', as is every `prepare-*-deposit-key` row.
const DECIDED_OWNERS: [(&str, &[&str]); 4] = [
    (
        "engineering",
        &[
            "ship-a-change",
            "gate-run",
            "pr-train",
            "rerail-a-car",
            "park-a-job",
            "emergency-merge",
            "flight-a-change",
            "station-hold",
            "agent-run",
            "agent-execution",
            "enroll-a-managed-agent",
            "design-doc",
            "design-doc-review",
            "workflow-design",
            "protocol-experiment",
            "protocol-retro",
            "publish-a-protocol",
            "publish-to-github",
            "page-audit",
            "doc-flatten",
            "browser-renderer-observation",
        ],
    ),
    (
        "operations",
        &[
            "node-maintenance-window",
            "join-a-node",
            "replace-a-disk",
            "regenerate-deployment",
            "roll-the-dev-pod",
            "ops-request",
            "diagnostic-request",
            "diagnostic-recovery-assessment",
            "incident",
            "dns-zone-observation",
            "inspect-a-github-installation",
            "rotate-a-credential",
            "confirm-recovery-sheet-kept",
            "reprint-recovery-sheet",
            "break-glass-enrolment",
            "runtime-roster-capture",
        ],
    ),
    (
        ROOT,
        &[
            "backlog-item",
            "user-feedback",
            "approval",
            "publish-request",
            "passkey-promotion",
            "work-session",
            "department-retro",
            "correct-the-record",
            "learning",
            // Not placed by c0ae096f, which predates it: the envelope a
            // delayed rule's child waits in (backlog 84a010cb), and any
            // station's rule may declare one, so it is the root's like
            // `approval`.
            "delayed-admission",
            // Not placed by c0ae096f, which predates it: a dated release
            // milestone on a person's calendar (backlog fe199985, design
            // e1dba350) is the company's plan, not engineering's work,
            // and its one step stands on no queue but a calendar.
            "release-milestone",
        ],
    ),
    (
        "finance",
        &["maintenance-ledger-recognize", "maintenance-ledger-replay"],
    ),
];

/// The decided owner's station name for `kind`, if c0ae096f places it.
fn decided_owner(kind: &str) -> Option<&'static str> {
    let listed = DECIDED_OWNERS
        .iter()
        .find(|(_, kinds)| kinds.contains(&kind))
        .map(|(station, _)| *station);
    let family = (kind.starts_with("maintenance-")
        || (kind.starts_with("prepare-") && kind.ends_with("-deposit-key")))
    .then_some("operations");
    listed.or(family)
}

/// A HELD row (infra/platform/workflow-holds/) whose move the decided
/// table makes but no car makes for it: the row goes live only at the
/// platform owner's deliberate publish, so its move rides with his
/// next edit of it. Each names the owner it still carries, so the
/// exception cannot drift in silence and is deleted when the row moves.
const HELD_AWAITING_THE_OWNERS_PUBLISH: [(&str, &str); 1] = [("publish-a-protocol", ROOT)];

#[test]
fn every_platform_protocol_names_a_standing_owner_station() {
    let rows = stations();
    let workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses");
    let failures = validate_protocol_owners(&workflows, &rows);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let misplaced: Vec<String> = workflows
        .iter()
        .filter_map(|w| {
            let Some(decided) = decided_owner(&w.kind) else {
                return Some(format!("{}: design c0ae096f places no owner", w.kind));
            };
            let station = HELD_AWAITING_THE_OWNERS_PUBLISH
                .iter()
                .find(|(kind, _)| *kind == w.kind)
                .map_or(decided, |(_, carried)| *carried);
            (protocol_owner(w) != Some(id_of(&rows, station).as_str())).then(|| {
                format!(
                    "{}: owner {:?}, but design c0ae096f places it at {station}",
                    w.kind,
                    protocol_owner(w)
                )
            })
        })
        .collect();
    assert!(misplaced.is_empty(), "{}", misplaced.join("\n"));
}

/// The twelve rows sweep 1 of design c0ae096f moves to their decided
/// owner (car db099d49, backlog baac0b65).
const MOVED_BY_SWEEP_1: [&str; 12] = [
    "incident",
    "break-glass-enrolment",
    "workflow-design",
    "protocol-experiment",
    "protocol-retro",
    "enroll-a-managed-agent",
    "correct-the-record",
    "work-session",
    "maintenance-codebase-metrics",
    "maintenance-protocol-drift",
    "maintenance-ledger-recognize",
    "maintenance-ledger-replay",
];

/// Every authored dispatcher rule, in the shape `GET /api/dispatcher/rules`
/// answers: one JSON object per `[[rule]]` of infra/dispatcher/rules/.
fn shipped_rules() -> Vec<serde_json::Value> {
    let dir = boss_testing::repo_root().join("infra/dispatcher/rules");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the rules directory reads")
        .map(|e| e.expect("a rules entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).expect("a rule file reads");
            let doc: toml::Table = text
                .parse()
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let rules = doc
                .get("rule")
                .and_then(toml::Value::as_array)
                .cloned()
                .unwrap_or_default();
            rules
                .into_iter()
                .map(|r| serde_json::to_value(r).expect("a rule is JSON"))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A MOVED ROW THE SERVER PROTECTS GOES LIVE AT A SIGNED PUBLISH (review
/// 6254feec of car db099d49, B1). The publish door refuses an unsigned
/// publish over a passkey-protected live row (`publish_passkey`: a
/// presence step, or an edge to a protected kind, closed transitively),
/// but the unattended drift publish's dry run does not judge the passkey:
/// an unheld protected row reads "would publish", the real run leaves a
/// refused draft, and that draft refuses every later check for every
/// kind, while `publish-drift --file-held` files a signing packet only
/// for a HELD kind. So each moved row the server protects carries a file
/// under infra/platform/workflow-holds/, and a moved row it does not
/// protect carries none, so an agent's publish is not held for nothing.
#[test]
fn every_moved_row_the_server_protects_waits_for_its_signed_publish() {
    let workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses");
    let rules = shipped_rules();
    let edges = boss_jobs::publish_passkey::edges(&rules, &workflows);
    let protected = boss_jobs::publish_passkey::protected_kinds(&workflows, &edges);
    // Control: a presence row, and one protected only through an edge,
    // so an empty rule read cannot pass this pin by protecting nothing.
    for kind in ["break-glass-enrolment", "workflow-design"] {
        assert!(
            protected.contains_key(kind),
            "{kind} is protected on the shipped rows and rules: {protected:?}"
        );
    }
    let holds = boss_testing::repo_root().join("infra/platform/workflow-holds");
    let failures: Vec<String> = MOVED_BY_SWEEP_1
        .iter()
        .filter_map(|kind| {
            let held = holds.join(format!("{kind}.toml")).is_file();
            match (protected.get(*kind), held) {
                (Some(why), false) => Some(format!(
                    "{kind} is passkey-protected ({why}) and carries no \
                     infra/platform/workflow-holds/{kind}.toml"
                )),
                (None, true) => Some(format!(
                    "{kind} is not passkey-protected; a hold keeps an agent's publish of it waiting for nothing"
                )),
                _ => None,
            }
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// An owner names a station OF THE TREE: an ACTIVE row with that id
/// (tree_rows), the set the parent validation reads. A retired row kept
/// for its history is no owner, and a retired version listed before its
/// active successor does not mask the successor (Codex review of car
/// c5894471, R1).
/// A step's `at` names an ACTIVE row of the tree, the same set an owner
/// resolves in: a retired station is no place to stand, and a retired
/// version listed first does not mask its active successor.
#[test]
fn a_step_at_a_retired_station_is_named_and_a_retired_version_masks_nothing() {
    let rows = stations();
    let workflows: Vec<_> =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses")
            .into_iter()
            .filter(|w| w.kind == "gate-run")
            .collect();
    assert_eq!(
        validate_protocol_subtree(&workflows, &rows),
        Vec::<String>::new()
    );
    let gates = id_of(&rows, "gates");
    let retired: Vec<StationSpec> = rows
        .iter()
        .cloned()
        .map(|mut s| {
            if s.name == "gates" {
                s.status = WorkflowStatus::Retired;
            }
            s
        })
        .collect();
    let failures = validate_protocol_subtree(&workflows, &retired);
    assert!(
        !failures.is_empty()
            && failures
                .iter()
                .all(|f| f.contains(&format!("at {gates} is no active station of the tree"))),
        "{failures:?}"
    );
    let mut history = vec![named(&retired, "gates").clone()];
    history.extend(rows.iter().cloned());
    assert_eq!(
        validate_protocol_subtree(&workflows, &history),
        Vec::<String>::new()
    );
}

#[test]
fn a_retired_owner_is_no_owner_and_cannot_mask_its_active_successor() {
    let rows = stations();
    let mut workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses");
    workflows.truncate(1);
    let engineering = id_of(&rows, "engineering");
    workflows[0].metadata["owner"] = serde_json::json!(engineering);
    assert_eq!(
        validate_protocol_owners(&workflows, &rows),
        Vec::<String>::new()
    );

    let retired: Vec<StationSpec> = rows
        .iter()
        .cloned()
        .map(|mut s| {
            if s.name == "engineering" {
                s.status = WorkflowStatus::Retired;
            }
            s
        })
        .collect();
    let failures = validate_protocol_owners(&workflows, &retired);
    assert_eq!(
        failures,
        vec![format!(
            "{}: owner {engineering} is no active station of the tree",
            workflows[0].kind
        )],
    );

    let mut history = vec![named(&retired, "engineering").clone()];
    history.extend(rows.iter().cloned());
    assert_eq!(
        validate_protocol_owners(&workflows, &history),
        Vec::<String>::new(),
        "a retired version listed first masks no active successor"
    );
}

#[test]
fn an_owner_that_is_no_standing_station_is_named() {
    let rows = stations();
    let mut workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses");
    workflows.truncate(3);
    workflows[0].metadata["owner"] = serde_json::json!("00000000-0000-4000-8000-000000000000");
    workflows[1]
        .metadata
        .as_object_mut()
        .expect("metadata is a table")
        .remove("owner");
    let ad_hoc = rows
        .iter()
        .find(|s| s.mode == Some(boss_jobs::stations::StationMode::AdHoc))
        .and_then(|s| s.id.clone());
    if let Some(id) = &ad_hoc {
        workflows[2].metadata["owner"] = serde_json::json!(id);
    }
    let failures = validate_protocol_owners(&workflows, &rows);
    assert!(
        failures
            .iter()
            .any(|f| f.starts_with(&format!("{}: owner ", workflows[0].kind))
                && f.contains("00000000-0000-4000-8000-000000000000")),
        "{failures:?}"
    );
    assert!(
        failures.iter().any(|f| f
            == &format!(
                "{}: names no owner station (metadata.owner)",
                workflows[1].kind
            )),
        "{failures:?}"
    );
    assert_eq!(
        failures.len(),
        2 + usize::from(ad_hoc.is_some()),
        "{failures:?}"
    );
}

/// The bundle's workflows, as the seed reads them.
fn workflows() -> Vec<boss_jobs::registry::WorkflowSpec> {
    boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the platform workflow bundle parses")
}

fn step_mut<'a>(
    rows: &'a mut [boss_jobs::registry::WorkflowSpec],
    kind: &str,
    title: &str,
) -> &'a mut boss_jobs::registry::StepSpec {
    rows.iter_mut()
        .find(|w| w.kind == kind)
        .and_then(|w| w.steps.iter_mut().find(|s| s.title == title))
        .unwrap_or_else(|| panic!("the bundle declares `{kind}` step `{title}`"))
}

fn set(step: &mut boss_jobs::registry::StepSpec, key: &str, value: &str) {
    if !step.metadata_defaults.is_object() {
        step.metadata_defaults = serde_json::json!({});
    }
    step.metadata_defaults[key] = serde_json::json!(value);
}

/// station-tree.md §1: "A protocol names only stations in its owner's
/// subtree. Work that crosses a boundary is a hand-off, never a
/// reference." The invariant a-protocol-names-only-its-owners-subtree,
/// enforced here on the bundle.
#[test]
fn every_platform_step_stands_in_its_owners_subtree() {
    let rows = stations();
    let flows = workflows();
    let failures = validate_protocol_subtree(&flows, &rows);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The first station the bundle names: a gate's verdict is recorded
    // at the gates, inside engineering, which owns gate-run.
    let verdict = flows
        .iter()
        .find(|w| w.kind == "gate-run")
        .and_then(|w| w.steps.iter().find(|s| s.title == "record-verdict"))
        .expect("gate-run records a verdict");
    assert_eq!(
        verdict.metadata_defaults.get("at").and_then(|v| v.as_str()),
        Some(id_of(&rows, "gates").as_str())
    );
}

#[test]
fn a_step_outside_its_owners_subtree_or_a_hand_off_to_nothing_is_named() {
    let rows = stations();
    let mut flows = workflows();
    // ship-a-change is engineering's; the estate is operations'.
    set(
        step_mut(&mut flows, "ship-a-change", "scope"),
        "at",
        &id_of(&rows, "estate"),
    );
    set(
        step_mut(&mut flows, "pr-train", "scheduled"),
        "at",
        "00000000-0000-4000-8000-000000000000",
    );
    set(
        step_mut(&mut flows, "incident", "raised"),
        "fulfilled_by",
        "no-such-protocol",
    );
    let both = step_mut(&mut flows, "backlog-item", "triage");
    set(both, "at", &id_of(&rows, "engineering"));
    set(both, "fulfilled_by", "ship-a-change");
    // Inside the owner's subtree and a real hand-off pass.
    set(
        step_mut(&mut flows, "pr-train", "collect"),
        "at",
        &id_of(&rows, "loading-dock"),
    );
    set(
        step_mut(&mut flows, "incident", "diagnose"),
        "fulfilled_by",
        "ops-request",
    );
    let failures = validate_protocol_subtree(&flows, &rows);
    let named = |needle: &str| failures.iter().any(|f| f.contains(needle));
    assert!(
        named("ship-a-change step `scope`: at") && named("outside"),
        "{failures:?}"
    );
    assert!(
        named(
            "pr-train step `scheduled`: at 00000000-0000-4000-8000-000000000000 is no active station"
        ),
        "{failures:?}"
    );
    assert!(
        named("incident step `raised`: fulfilled_by no-such-protocol is no protocol"),
        "{failures:?}"
    );
    assert!(
        named("backlog-item step `triage`: names both"),
        "{failures:?}"
    );
    assert_eq!(failures.len(), 4, "{failures:?}");
}

/// A PRESENT placement that is no id is named, never read as absent
/// (Codex review of T6 car B, 2026-11-08). gate-run's record-verdict
/// declares `at`; read through the seed loader with that value made a
/// list, a number or a blank string, the loader accepts the row and the
/// subtree check used to see no placement at all, so the bundle
/// invariant passed on a declaration it had dropped. The same holds for
/// `fulfilled_by`, and for both keys at once.
#[test]
fn a_present_placement_that_is_no_id_is_named() {
    let rows = stations();
    let path = Path::new(boss_jobs::registry::platform_bundle_path()).join("gate-run.toml");
    let text = std::fs::read_to_string(&path).expect("gate-run.toml reads");
    let gates = id_of(&rows, "gates");
    let declared = format!("at = \"{gates}\"");
    assert!(
        text.contains(&declared),
        "record-verdict stands at the gates"
    );
    let validate = |replacement: &str| {
        let flows = boss_jobs::seed_loader::parse_workflows(
            &text.replace(&declared, replacement),
            "platform",
            "gate-run.toml",
        )
        .expect("the loader accepts the row; the shape is the subtree check's to name");
        validate_protocol_subtree(&flows, &rows)
    };
    // The control: the shipped declaration passes.
    assert_eq!(validate(&declared), Vec::<String>::new());
    for (replacement, key) in [
        ("at = [\"outside-the-owner-subtree\"]", "at"),
        ("at = 7", "at"),
        ("at = \"  \"", "at"),
        ("fulfilled_by = [\"ops-request\"]", "fulfilled_by"),
        ("fulfilled_by = \"\"", "fulfilled_by"),
    ] {
        let failures = validate(replacement);
        assert_eq!(failures.len(), 1, "{replacement}: {failures:?}");
        assert!(
            failures[0].contains("gate-run step `record-verdict`")
                && failures[0].contains(&format!("{key} must name")),
            "{replacement}: {failures:?}"
        );
    }
    // Both keys, one malformed: still a step that names both.
    let both = validate(&format!("{declared}, fulfilled_by = 7"));
    assert!(
        both.len() == 1 && both[0].contains("names both"),
        "{both:?}"
    );
}

// -- every engineering step names where it runs (sweep 2, c0ae096f) ----------

/// Where a step stands: a station of the tree, by NAME so the ids stay in
/// rows, or the protocol whose packet fulfils it.
#[derive(Debug, Clone, Copy)]
enum Placed {
    At(&'static str),
    By(&'static str),
}

/// Sweep 2 of design c0ae096f car 2 (backlog baac0b65; David's data
/// sequence of 2026-11-12, step 1): every step of engineering's protocols
/// names the station it runs at, so the engineering page's StationLine
/// (design 08d1530e) draws each stop where it stands instead of in an
/// unplaced tray. Read by station NAME, so the ids stay in rows.
///
/// The station each step stands at, and why:
/// - `build` holds every robot slot (build.toml), so the work a robot
///   does stands there: a car scoped and built, a run briefed, building
///   and reporting, a rerail, a native execution observed.
/// - `review` is engineering's review stage: a car's `review` step stays
///   ready while the car is held for its review and while it rides its
///   train, so it stands there. Never at `loading-dock`: a queue is its
///   predicate OR its tree address, so an `at` of the dock would put
///   held and train-borne cars in the queue whose predicate keeps them
///   out; a boardable car stands at the dock through that predicate
///   (review a3a25eb2, B1).
/// - `loading-dock`'s one bay is the train boarding: a train collects
///   and assembles there.
/// - `forge` holds the repository: a train's PR is opened and merged on
///   it; `forge-ci` is its runner, already the queue of a train at `ci`.
/// - `probe-shed` and `landed-review` are the queues of a landed car's
///   two debts, `proven` and `landed-review`.
/// - `design-review` is the queue of a design's open questions, and
///   `design` the stage where decided designs fold into current truth.
/// - A step that WAITS on another packet names it as a hand-off: a car's
///   gate and a run's verdict are a gate-run's, which stands at `gates`.
const ENGINEERING_PLACED: [(&str, &str, Placed); 26] = [
    ("ship-a-change", "scope", Placed::At("build")),
    ("ship-a-change", "build", Placed::At("build")),
    ("ship-a-change", "gate", Placed::By("gate-run")),
    ("ship-a-change", "review", Placed::At("review")),
    ("ship-a-change", "proven", Placed::At("probe-shed")),
    (
        "ship-a-change",
        "landed-review",
        Placed::At("landed-review"),
    ),
    ("gate-run", "record-verdict", Placed::At("gates")),
    ("pr-train", "collect", Placed::At("loading-dock")),
    ("pr-train", "assemble", Placed::At("loading-dock")),
    ("pr-train", "pr", Placed::At("forge")),
    ("pr-train", "ci", Placed::At("forge-ci")),
    ("pr-train", "merged", Placed::At("forge")),
    ("design-doc", "review", Placed::At("design-review")),
    ("design-doc", "fold", Placed::At("design")),
    ("agent-run", "briefed", Placed::At("build")),
    ("agent-run", "building", Placed::At("build")),
    ("agent-run", "verdict", Placed::By("gate-run")),
    ("agent-run", "reported", Placed::At("build")),
    ("rerail-a-car", "rerail", Placed::At("build")),
    ("agent-execution", "observing", Placed::At("build")),
    ("design-doc-review", "review", Placed::At("design-review")),
    ("doc-flatten", "survey", Placed::At("design")),
    ("doc-flatten", "fold", Placed::At("design")),
    ("doc-flatten", "curate", Placed::At("design")),
    ("doc-flatten", "verify", Placed::At("design")),
    ("doc-flatten", "review", Placed::At("design")),
];

/// The engineering steps that name no station, each with why. A step
/// leaves this list in the car that places it: the pin refuses an entry
/// whose step has since been placed, so the list cannot outlive its
/// reason.
const ENGINEERING_UNPLACED: [(&str, &str, &str); 57] = [
    ("pr-train", "deployed", PR_TRAIN_PAST_THE_IMAGE),
    ("pr-train", "converged", PR_TRAIN_PAST_THE_IMAGE),
    (
        "browser-renderer-observation",
        "observe",
        "a signed-in person's own browser, which no station of the tree holds",
    ),
    (
        "browser-renderer-observation",
        "qualify",
        "an independent qualification of attended evidence; the review stage queues car reviews, not this",
    ),
    ("correct-the-record", "evidence", CORRECTS_ANY_RECORD),
    ("correct-the-record", "review", CORRECTS_ANY_RECORD),
    ("correct-the-record", "applied", CORRECTS_ANY_RECORD),
    ("emergency-merge", "undo-first", RUNS_WHEN_THE_LINE_IS_DOWN),
    (
        "emergency-merge",
        "gate-locally",
        RUNS_WHEN_THE_LINE_IS_DOWN,
    ),
    ("emergency-merge", "approve", RUNS_WHEN_THE_LINE_IS_DOWN),
    ("emergency-merge", "merge", RUNS_WHEN_THE_LINE_IS_DOWN),
    ("emergency-merge", "converged", RUNS_WHEN_THE_LINE_IS_DOWN),
    ("emergency-merge", "retro", RUNS_WHEN_THE_LINE_IS_DOWN),
    ("flight-a-change", "ship", A_FLIGHT_IS_WATCHED_IN_PRODUCTION),
    (
        "flight-a-change",
        "turn-on",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "flight-a-change",
        "observe",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "flight-a-change",
        "widen",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "flight-a-change",
        "extend",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "flight-a-change",
        "reobserve",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    ("flight-a-change", "pull", A_FLIGHT_IS_WATCHED_IN_PRODUCTION),
    (
        "flight-a-change",
        "decide",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "flight-a-change",
        "cleanup",
        A_FLIGHT_IS_WATCHED_IN_PRODUCTION,
    ),
    (
        "maintenance-codebase-metrics",
        "run",
        RUN_BY_AN_OPERATIONS_AUTOMATON,
    ),
    (
        "maintenance-protocol-drift",
        "run",
        RUN_BY_AN_OPERATIONS_AUTOMATON,
    ),
    ("page-audit", "measure", A_PAGE_OF_ANY_DEPARTMENT),
    ("page-audit", "file", A_PAGE_OF_ANY_DEPARTMENT),
    ("page-audit", "test", A_PAGE_OF_ANY_DEPARTMENT),
    ("page-audit", "review", A_PAGE_OF_ANY_DEPARTMENT),
    ("page-audit", "revise", A_PAGE_OF_ANY_DEPARTMENT),
    (
        "park-a-job",
        "park",
        "an operation on any packets, which stand wherever they stand",
    ),
    (
        "park-a-job",
        "unblock",
        "an operation on any packets, which stand wherever they stand",
    ),
    ("publish-to-github", "measure", THE_MIRROR_IS_OFF_THE_TREE),
    ("publish-to-github", "review", THE_MIRROR_IS_OFF_THE_TREE),
    ("publish-to-github", "approve", THE_MIRROR_IS_OFF_THE_TREE),
    ("publish-to-github", "open-pr", THE_MIRROR_IS_OFF_THE_TREE),
    (
        "publish-to-github",
        "read-checks",
        THE_MIRROR_IS_OFF_THE_TREE,
    ),
    (
        "publish-to-github",
        "judge-checks",
        THE_MIRROR_IS_OFF_THE_TREE,
    ),
    ("publish-to-github", "merge", THE_MIRROR_IS_OFF_THE_TREE),
    (
        "station-hold",
        "release",
        "a hold names its station per packet (metadata.station), which one row cannot",
    ),
    (
        "work-session",
        "active",
        "an operator's session; a person's seat is no station of the tree yet",
    ),
    ("workflow-design", "author", MOVED_IN_BY_SWEEP_1),
    ("workflow-design", "validate", MOVED_IN_BY_SWEEP_1),
    ("workflow-design", "approve", MOVED_IN_BY_SWEEP_1),
    ("workflow-design", "publish", MOVED_IN_BY_SWEEP_1),
    ("protocol-experiment", "state", MOVED_IN_BY_SWEEP_1),
    ("protocol-experiment", "run", MOVED_IN_BY_SWEEP_1),
    ("protocol-experiment", "measure", MOVED_IN_BY_SWEEP_1),
    ("protocol-experiment", "decide", MOVED_IN_BY_SWEEP_1),
    ("protocol-retro", "collect", MOVED_IN_BY_SWEEP_1),
    ("protocol-retro", "analyze", MOVED_IN_BY_SWEEP_1),
    ("protocol-retro", "gaps", MOVED_IN_BY_SWEEP_1),
    ("protocol-retro", "report", MOVED_IN_BY_SWEEP_1),
    ("protocol-retro", "review", MOVED_IN_BY_SWEEP_1),
    ("enroll-a-managed-agent", "scope", MOVED_IN_BY_SWEEP_1),
    ("enroll-a-managed-agent", "enroll", MOVED_IN_BY_SWEEP_1),
    ("enroll-a-managed-agent", "install", MOVED_IN_BY_SWEEP_1),
    ("enroll-a-managed-agent", "verify", MOVED_IN_BY_SWEEP_1),
];

/// Past the merge a train deploys and converges at operations' `deploy`
/// and `converge`, outside engineering's subtree, and `converged`
/// already stands at `converge` by its `equipment:` executor. Splitting
/// pr-train at the image (a roll-a-train protocol operations owns, T6
/// part D) makes them a hand-off; a `fulfilled_by` here before that
/// protocol exists would take the train off the converge queue.
const PR_TRAIN_PAST_THE_IMAGE: &str = "deploy and converge are operations' stations, outside engineering's subtree (T6 part D splits pr-train at the image)";
const CORRECTS_ANY_RECORD: &str =
    "a correction of any completed step in the record, whose station is the corrected packet's";
const RUNS_WHEN_THE_LINE_IS_DOWN: &str =
    "the lane for when the pipeline's own stations are down, so its steps run outside them";
const A_FLIGHT_IS_WATCHED_IN_PRODUCTION: &str =
    "a flight is watched in production for an audience, and no station of the tree holds flights";
const RUN_BY_AN_OPERATIONS_AUTOMATON: &str = "run by automation:boss-step, whose host station is operations' estate, outside engineering's subtree";
const A_PAGE_OF_ANY_DEPARTMENT: &str =
    "a page of any department's surface, whose station is that department's, read off the packet";
const THE_MIRROR_IS_OFF_THE_TREE: &str =
    "the public GitHub mirror is outside the company, and no station of the tree holds it";
/// Sweep 1 of design c0ae096f (car db099d49, parked beside this one)
/// moves four rows INTO engineering: workflow-design, protocol-experiment,
/// protocol-retro and enroll-a-managed-agent. Their steps were measured by
/// neither sweep, and without an entry here the tree the two cars make
/// together refuses this pin, whichever lands first. An excuse needs only
/// its step to exist, so on a tree without that move these entries are
/// inert; the next engineering sweep places them. The rows that car moves
/// OUT of engineering (correct-the-record, work-session and the two
/// maintenance rows) keep their entries until then for the same reason.
const MOVED_IN_BY_SWEEP_1: &str = "moved into engineering by car db099d49 (design c0ae096f sweep 1) after this sweep was measured; the next engineering sweep places it";

#[test]
fn every_engineering_work_step_names_where_it_runs_or_why_not() {
    let rows = stations();
    let engineering = id_of(&rows, "engineering");
    let registry = boss_jobs::step_registry::StepRegistry::v1();
    let flows = workflows();
    let declared = |step: &boss_jobs::registry::StepSpec, key: &str| {
        step.metadata_defaults
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let step_of = |kind: &str, title: &str| {
        flows
            .iter()
            .find(|w| w.kind == kind)
            .and_then(|w| w.steps.iter().find(|s| s.title == title))
            .unwrap_or_else(|| panic!("the bundle declares `{kind}` step `{title}`"))
    };
    let mut failures = Vec::new();
    for (kind, title, placed) in ENGINEERING_PLACED {
        let step = step_of(kind, title);
        let (key, want) = match placed {
            Placed::At(station) => ("at", id_of(&rows, station)),
            Placed::By(protocol) => ("fulfilled_by", protocol.to_string()),
        };
        if declared(step, key).as_deref() != Some(want.as_str()) {
            failures.push(format!(
                "{kind} step `{title}`: {key} should name {placed:?} ({want}), names {:?}",
                declared(step, key)
            ));
        }
    }
    // A marker (a trigger, an outcome) is state the machine reaches on
    // its own, completed the instant it is ready, so it never stands
    // anywhere. Every other step of an engineering protocol is placed,
    // or named unplaced with its reason.
    for w in flows
        .iter()
        .filter(|w| protocol_owner(w) == Some(engineering.as_str()))
    {
        for step in &w.steps {
            if registry.get(&step.kind).is_some_and(|t| t.is_marker()) {
                continue;
            }
            let placed = declared(step, "at").is_some() || declared(step, "fulfilled_by").is_some();
            let excused = ENGINEERING_UNPLACED
                .iter()
                .any(|(k, s, _)| *k == w.kind && *s == step.title);
            match (placed, excused) {
                (false, false) => failures.push(format!(
                    "{} step `{}` names no station and is not named unplaced",
                    w.kind, step.title
                )),
                (true, true) => failures.push(format!(
                    "{} step `{}` is placed; drop it from ENGINEERING_UNPLACED",
                    w.kind, step.title
                )),
                _ => {}
            }
        }
    }
    // Every excused step exists, so a renamed step cannot leave a stale
    // excuse behind.
    for (kind, title, why) in ENGINEERING_UNPLACED {
        assert!(!why.trim().is_empty(), "{kind} `{title}` names its reason");
        step_of(kind, title);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// -- the bundle is one tree --------------------------------------------------

#[test]
fn the_platform_stations_make_one_tree_with_every_row_in_it() {
    let rows = stations();
    let failures = validate_station_tree(&rows, &pools());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let outside: Vec<&str> = rows
        .iter()
        .filter(|s| s.id.is_none())
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        outside.is_empty(),
        "platform rows outside the tree: {outside:?}"
    );
    let roots: Vec<&str> = rows
        .iter()
        .filter(|s| s.parent.is_none())
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(roots, vec![ROOT], "the company is the one root");
    for name in ["engineering", "operations", "finance"] {
        assert_eq!(
            address_of(&rows, name),
            Some(format!("/{ROOT}/{name}")),
            "the departments of handoff section 1 stand under the root"
        );
    }
    // Every row reaches the root, so every row has an address.
    for row in &rows {
        assert!(
            address_of(&rows, &row.name).is_some(),
            "`{}` has no address",
            row.name
        );
    }
}

#[test]
fn every_machine_a_station_names_is_an_automation_row() {
    let automations: BTreeSet<String> = boss_jobs::agents::automations::load_automations_dir(
        Path::new(boss_jobs::agents::automations::platform_automations_path()),
    )
    .expect("the automation bundle parses")
    .into_iter()
    .map(|a| a.id)
    .collect();
    let rows = stations();
    let unknown: Vec<String> = rows
        .iter()
        .flat_map(|s| s.machines.iter().map(move |m| (s, m)))
        .filter(|(_, m)| !automations.contains(*m))
        .map(|(s, m)| format!("{} names {m}", s.name))
        .collect();
    assert!(unknown.is_empty(), "{unknown:?}");
    assert!(
        rows.iter().any(|s| !s.machines.is_empty()),
        "no station names a machine"
    );
}

/// Design c4854fd4 (backlog 85cb2103): a card's rail is the line its
/// nearest row up the tree names, so the root and every department under
/// it name one and each station below inherits it by walking parents. The
/// code is a Design token, declared once in the web stylesheet, so this
/// holds the two together (CLAUDE.md §9a): a row naming a code the
/// stylesheet does not declare would draw night ink and say nothing.
///
/// A department is a child of the root WITH a line of its own. Since the
/// executive desk dissolved into the company (design 2910ec3d car 1,
/// backlog 878fd1e7) the root also hosts stations of its own that are no
/// department and draw the company's line (since backlog 5cb56bb3, the
/// person's own station `me`); they are named in
/// [`HOSTED_AT_THE_COMPANY`], so a new department that forgets its line
/// is refused by name rather than read as one of them.
#[test]
fn every_top_level_station_names_a_line_the_stylesheet_declares() {
    let rows = stations();
    let root = id_of(&rows, ROOT);
    let css = std::fs::read_to_string(boss_testing::repo_root().join("apps/web/src/styles.css"))
        .expect("read the web stylesheet");
    let mut lineless: Vec<&str> = rows
        .iter()
        .filter(|s| s.parent.as_deref() == Some(root.as_str()) && s.line.is_none())
        .map(|s| s.name.as_str())
        .collect();
    lineless.sort_unstable();
    assert_eq!(
        lineless, HOSTED_AT_THE_COMPANY,
        "a child of the root names a line unless it is a queue the company hosts itself"
    );
    let top: Vec<&StationSpec> = rows
        .iter()
        .filter(|s| s.parent.is_none() || s.parent.as_deref() == Some(root.as_str()))
        .filter(|s| !HOSTED_AT_THE_COMPANY.contains(&s.name.as_str()))
        .collect();
    assert!(top.len() >= 4, "the root and its departments: {top:?}");
    let failures: Vec<String> = top
        .iter()
        .filter_map(|s| match s.line.as_deref() {
            None => Some(format!("`{}` names no line", s.name)),
            Some(code) if !css.contains(&format!("--map-line-{code}:")) => Some(format!(
                "`{}` names line `{code}`, which apps/web/src/styles.css does not declare",
                s.name
            )),
            Some(_) => None,
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The lines the design decided from the company frame.
    for (name, line) in [
        // Design 08d1530e Q2 (backlog 950ad972): its own line, at
        // today's IT blue, because IT is retiring (df0ecd8b).
        ("engineering", "engineering"),
        ("operations", "hosting"),
        // Design c0ae096f (backlog 599fec3e): Finance returns on its own line.
        ("finance", "finance"),
    ] {
        assert_eq!(named(&rows, name).line.as_deref(), Some(line), "{name}");
    }
}

/// Backlog e3a927c3 (David 2026-11-19, the Plan artboard of
/// https://claude.ai/artifact/DriP1eEZq8zSxj1oTRuYs8): the company's
/// children and Engineering's stand at fixed places on their parent's
/// floor, in that artboard's own 1440 x 900 units. Engineering's boxes are
/// the artboard's, copied; the prototype's `train` is the forge, where
/// trains run (forge-ci stands in it); `design`, which the artboard does
/// not draw, takes the free bay under build; the company floor, which no
/// artboard draws yet, puts Engineering beside Operations, the station
/// its outbound addresses, with Finance under Operations. Executive is
/// not placed: it retires into the root (backlog 878fd1e7), and the
/// queues it leaves at the root carry no placement, so the view's
/// fallback grid draws them. Every active child of Engineering is
/// placed, and the placements pass the seed's own overlap check.
#[test]
fn the_company_and_engineering_floors_place_every_child_where_the_plan_does() {
    let rows = stations();
    let expected: [(&str, [u32; 4]); 10] = [
        ("engineering", [120, 120, 600, 420]),
        ("operations", [780, 120, 540, 420]),
        ("finance", [780, 600, 540, 200]),
        ("build", [300, 270, 230, 300]),
        ("gates", [590, 120, 230, 170]),
        ("review", [590, 590, 230, 190]),
        ("loading-dock", [880, 360, 140, 140]),
        ("forge", [880, 120, 140, 140]),
        ("probe-shed", [880, 590, 230, 190]),
        ("design", [300, 600, 230, 190]),
    ];
    for (name, [x, y, w, h]) in expected {
        assert_eq!(
            named(&rows, name).placement,
            Some(boss_jobs::stations::StationPlacement { x, y, w, h }),
            "{name}"
        );
    }
    let engineering = id_of(&rows, "engineering");
    let unplaced: Vec<&str> = rows
        .iter()
        .filter(|s| s.parent.as_deref() == Some(engineering.as_str()))
        .filter(|s| s.placement.is_none())
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        unplaced.is_empty(),
        "engineering's floor leaves {unplaced:?} unplaced"
    );
    assert_eq!(
        boss_jobs::stations::validate_station_placements(&rows, &rows),
        Vec::new()
    );
    assert!(
        named(&rows, ROOT).placement.is_none(),
        "the root stands on no floor"
    );
}

/// The stations the company station hosts itself, by name, sorted: each
/// is a child of the root and no department, so it declares no line and
/// draws the company's. The person's two queues stood here when the
/// executive desk dissolved (design 2910ec3d car 1, backlog 878fd1e7),
/// until design 01fa5bdf step 3 gave them `me`, the person's own station
/// (backlog 5cb56bb3), which is the one row left here.
const HOSTED_AT_THE_COMPANY: [&str; 1] = ["me"];

/// DESIGN 2910ec3d, car 1 (backlog 878fd1e7; David, 2026-11-10: "the
/// Executive station is actually the same as the Algedonic station in
/// theory"), re-checked against design 01fa5bdf (David, 2026-11-17).
/// The executive desk was allocated nothing and held no executor the root
/// does not, so its row is gone and nothing stands in it. Its rows MOVED,
/// they were not rebuilt: each keeps its stable `id`, and a move is a
/// `parent` edit that gains no capability. The person's queues stood at
/// the company until `me`, the person's own station, existed to take them
/// (design 01fa5bdf step 3, backlog 5cb56bb3); the decided-designs board, the OUT of
/// the design station's page, stands beside `design-review` in
/// engineering's design station, where 01fa5bdf places design.
#[test]
fn the_executive_desk_dissolved_into_the_company_and_its_rows_kept_their_identity() {
    let rows = stations();
    assert!(
        !rows.iter().any(|s| s.name == "executive"),
        "the executive station row is retired"
    );
    // The desk's id, spelled once because it is now history.
    const DESK: &str = "5f922a2a-31f5-4f07-acf1-41dc140687a4";
    let in_desk: Vec<&str> = rows
        .iter()
        .filter(|s| s.parent.as_deref() == Some(DESK) || s.id.as_deref() == Some(DESK))
        .map(|s| s.name.as_str())
        .collect();
    assert!(in_desk.is_empty(), "rows still at the desk: {in_desk:?}");
    for (name, under, id) in [
        (
            "my-watchlist",
            "/me",
            "4feca45a-de46-4649-8569-f5a7262b8017",
        ),
        (
            "q.platform-admin.task",
            "/me",
            "21bf19d3-94dc-4fcc-ac7c-4a19923c726f",
        ),
        (
            "design-decided",
            "/engineering/design",
            "ebbae18b-5bee-468e-8be7-53aa54a91e87",
        ),
    ] {
        assert_eq!(
            address_of(&rows, name),
            Some(format!("/{ROOT}{under}/{name}")),
            "`{name}` left the desk for its new parent"
        );
        assert_eq!(
            named(&rows, name).id.as_deref(),
            Some(id),
            "`{name}` kept its id"
        );
        assert_eq!(
            named(&rows, name).mode,
            Some(boss_jobs::stations::StationMode::Standing),
            "`{name}` is still a standing station"
        );
    }
    // Their new parent is the person's own station: one row for every
    // person, at the root, no department (no line of its own) and no
    // capability, so the second move gained no one a claim either.
    let me = named(&rows, "me");
    assert_eq!(address_of(&rows, "me"), Some(format!("/{ROOT}/me")));
    assert_eq!(
        me.id.as_deref(),
        Some("bbfea3e2-d865-42c1-858b-85d45d29f350")
    );
    assert!(me.line.is_none(), "`me` is no department: {me:?}");
    assert!(me.capability.is_none(), "`me` admits no one: {me:?}");
    // No capability gained in the move: the operator's task station still
    // admits exactly the one role, and the two views admit no one by role.
    let task = named(&rows, "q.platform-admin.task")
        .capability
        .as_ref()
        .expect("the operator's task station keeps its capability");
    assert_eq!(task.roles, vec!["platform-admin".to_string()]);
    assert!(task.models.is_empty(), "no model admitted: {task:?}");
    for view in ["my-watchlist", "design-decided"] {
        assert!(
            named(&rows, view).capability.is_none(),
            "`{view}` gained a capability"
        );
    }
    // The automation hosted at the desk is hosted at the company.
    let automations = boss_jobs::agents::automations::load_automations_dir(Path::new(
        boss_jobs::agents::automations::platform_automations_path(),
    ))
    .expect("the platform automation bundle parses");
    let usage = automations
        .iter()
        .find(|a| a.id == "automation:surface-usage")
        .expect("the surface-usage automation row");
    assert_eq!(usage.station, Some(id_of(&rows, ROOT)));
}

/// CLAUDE.md §9a: a node's vCPUs live in the estate declaration and in its
/// host pool's capacity, so the two are held equal, both ways, here.
#[test]
fn every_estate_node_has_its_host_pool_and_no_other() {
    let estate = boss_jobs::estate_seed::load_estate_toml(
        &boss_testing::repo_root().join("infra/estate/estate.toml"),
    )
    .expect("the estate declaration parses");
    let declared: BTreeMap<String, Option<i32>> =
        estate.iter().map(|n| (n.id.clone(), n.cpu)).collect();
    let hosts: BTreeMap<String, String> = pools()
        .into_iter()
        .filter_map(|p| {
            let node = host_of(&p.name)?.to_string();
            let amount = match &p.capacity {
                boss_jobs::pools::PoolCapacity::Configured { amount } => amount.to_string(),
                boss_jobs::pools::PoolCapacity::Unknown {} => "unknown".into(),
            };
            Some((node, amount))
        })
        .collect();
    assert_eq!(
        hosts.keys().collect::<Vec<_>>(),
        declared.keys().collect::<Vec<_>>(),
        "one host pool per estate node, and none for a box the estate does not declare"
    );
    for (node, cpu) in &declared {
        let cpu = cpu.unwrap_or_else(|| panic!("estate node `{node}` declares no cpu"));
        assert_eq!(
            hosts[node],
            cpu.to_string(),
            "host:{node} capacity is its cpu"
        );
    }
    for real in ["w-1", "w-2", "cp-1", "cp-2", "cp-3", "forge", "boss-gcp"] {
        assert!(hosts.contains_key(real), "the estate's `{real}` has a pool");
    }
}

/// A Kubernetes CPU quantity in millicores: `"4"`, `4`, `250m`, `"2.5"`.
fn cpu_millis(q: &str) -> u64 {
    let q = q.trim().trim_matches('"');
    match q.strip_suffix('m') {
        Some(m) => m
            .parse()
            .unwrap_or_else(|_| panic!("a millicore count: {q}")),
        None => {
            let cores: f64 = q.parse().unwrap_or_else(|_| panic!("a CPU count: {q}"));
            // A quantity in tenths or thousandths, never a fraction of a
            // millicore, so the round is exact.
            (cores * 1000.0).round() as u64
        }
    }
}

/// Every `<kind>: {cpu: <q>, …}` line in `path` that `marker` picks out,
/// in millicores. `expect` is how many the caller knows are there, so a
/// container growing the same shape is a red here rather than a silent
/// wrong read.
fn manifest_cpus(path: &str, kind: &str, marker: &str, expect: usize) -> Vec<u64> {
    let text = std::fs::read_to_string(boss_testing::repo_root().join(path)).expect("read");
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(&format!("{kind}: {{cpu: ")) && l.contains(marker))
        .collect();
    assert_eq!(
        lines.len(),
        expect,
        "{path}: {expect} `{kind}` line(s) with `{marker}`: {lines:?}"
    );
    lines
        .iter()
        .map(|l| {
            let q = l
                .split("cpu: ")
                .nth(1)
                .and_then(|rest| rest.split([',', '}']).next())
                .unwrap_or_else(|| panic!("{path}: a CPU quantity in {l}"));
            cpu_millis(q)
        })
        .collect()
}

/// CLAUDE.md §9a again: the allocations the bundle declares are read off
/// the manifests that decide them, so they are held to those manifests.
///
/// REWRITTEN for backlog 2b7c78f8 (2026-11-21, David: tune resource
/// allocations for throughput). Its first form held the gates to the gate
/// CONTAINER's 4 CPU and Engineering to that plus the dev pod's 16-CPU
/// LIMIT, and both were wrong in the direction that over-admits. The
/// scheduler places a gate POD, whose effective request is the gate's 4
/// plus the postgres native sidecar's 250m beside it (4.25, measured on
/// the live pods 16:52Z); and the dev pod's limit is not what it takes of
/// w-1 — its REQUEST is (4 + 250m + 50m = 4.3), and since isolation the
/// builders run in dev-build workers that are pods of their own, billed
/// 2.5 each at the claim. So the record charged 28 + the dev pod's 4.3
/// against a 32-vCPU box before Longhorn's guaranteed CPU or a DaemonSet,
/// and admitted 3 gates + 6 workers when w-1 places 3 gates + 3.
///
/// Held here, each from the file that decides it:
///   * gates = ceil(seats x the gate pod's effective CPU request);
///   * Engineering's float (its allocation less what the gates reserve) is
///     a whole number of worker claims at agent-run's billed vCPU — the
///     number of builders the host:w-1 line admits, so a builder w-1
///     cannot place waits in that line instead of in the scheduler;
///   * Engineering plus the dev pod's request fits host:w-1's capacity.
///     The rest of the box — Longhorn's guaranteed instance-manager CPU,
///     the DaemonSets, and the playground stack while it stands on w-1 —
///     is named in engineering.toml as read by node-pods, not pinned: no
///     manifest in this tree decides it.
#[test]
fn the_engineering_allocations_are_the_manifests_numbers() {
    let rows = stations();
    let held = |name: &str| -> u64 {
        let n: u64 = named(&rows, name).allocation["host:w-1"]
            .to_string()
            .parse()
            .expect("a whole vCPU count");
        n * 1000
    };
    let gate_pod: u64 = [
        manifest_cpus(
            "infra/gate-runner/gate-runner.yaml",
            "requests",
            "ephemeral-storage",
            1,
        ),
        manifest_cpus(
            "infra/gate-runner/gate-runner.yaml",
            "requests",
            "memory: 3Gi",
            1,
        ),
    ]
    .concat()
    .iter()
    .sum();
    assert_eq!(
        gate_pod, 4250,
        "the gate pod: the gate 4 + the sidecar 250m"
    );
    let dev_pod: u64 = manifest_cpus("infra/cluster/manifests/boss-dev.yaml", "requests", "", 3)
        .iter()
        .sum();
    let seats = u64::try_from(named(&rows, "gates").capacity.expect("seats")).expect("positive");
    assert_eq!(
        held("gates"),
        (seats * gate_pod).div_ceil(1000) * 1000,
        "the gates' share of the build node: {seats} gate pods of {gate_pod}m"
    );
    let bundle =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform bundle parses");
    let worker = bundle
        .iter()
        .find(|w| w.kind == "agent-run")
        .and_then(|w| w.steps.iter().find(|s| s.title == "building"))
        .and_then(|s| s.resources.as_ref())
        .and_then(|r| r.vcpu.as_ref())
        .map(|q| cpu_millis(&q.to_string()))
        .expect("agent-run's building bills a worker's vCPU");
    let float = held("engineering")
        .checked_sub(held("gates"))
        .expect("engineering holds the gates' share");
    assert!(
        float > 0 && float % worker == 0,
        "engineering's float {float}m is a whole number of {worker}m worker claims"
    );
    let box_cpu = pools()
        .into_iter()
        .find(|p| p.name == "host:w-1")
        .and_then(|p| match p.capacity {
            boss_jobs::pools::PoolCapacity::Configured { amount } => {
                Some(cpu_millis(&amount.to_string()))
            }
            boss_jobs::pools::PoolCapacity::Unknown {} => None,
        })
        .expect("host:w-1 has a configured capacity");
    assert!(
        held("engineering") + dev_pod <= box_cpu,
        "engineering's {}m plus the dev pod's {dev_pod}m request is more than w-1's {box_cpu}m: \
         the record admits work the scheduler cannot place",
        held("engineering")
    );
}

/// THE BUILD STATION (backlog b5c815e4, design 52613d21 row 1): where a
/// change is built. Measured 2026-11-09 19:22Z, every robots:opus and
/// robots:sol builder claim stood at `a.platform-admin.opus-5-5`, a
/// per-executor address outside the tree, and no station row allocated
/// the robot slots — so nothing could read how many builders the factory
/// seats, or whether they starve. The row stands under engineering and
/// names every robot pool the bundle declares (by NAME). Its queue is the
/// default admission queue, priority then age, over the backlog items
/// waiting at their `build` step.
///
/// SINCE BACKLOG 30c1efe8 a station RESERVES what its allocation names
/// and BORROWS the rest up its line, and a builder's claim is judged
/// here once the claim door writes version-3 claims (the writer car after
/// the reader car that brought this row; readers go first, review
/// 6fdc1391). Build reserves NONE of each robot pool (David sets the
/// numbers later): every robot claim, wherever it stands, borrows its
/// slot from the root, first come first served as before — a reservation
/// here would leave a reviewer's or an analyst's claim the root's
/// unreserved remainder. So build seats the slots it can borrow, the
/// robot pools' own capacities, and the slot counts stay the pool rows'
/// tunables.
///
/// It reserves NONE of a host pool either: its builders' vCPU floats in
/// engineering's share of the build node, all of which is the dev pod's
/// (`a_run_floats_in_its_owners_share_of_the_build_node`).
#[test]
fn the_build_station_stands_under_engineering_and_seats_the_robot_slots_it_holds() {
    use boss_jobs::resources::ROBOTS_PREFIX;
    let rows = stations();
    let build = named(&rows, "build");
    assert_eq!(
        address_of(&rows, "build"),
        Some(format!("/{ROOT}/engineering/build"))
    );
    assert_eq!(build.mode, Some(boss_jobs::stations::StationMode::Standing));
    assert_eq!(build.status, WorkflowStatus::Active);
    let robot_pools: Vec<PoolSpec> = pools()
        .into_iter()
        .filter(|p| p.name.starts_with(ROBOTS_PREFIX))
        .collect();
    let robots: BTreeSet<String> = robot_pools.iter().map(|p| p.name.clone()).collect();
    let held: BTreeSet<String> = build
        .allocation
        .keys()
        .filter(|k| k.starts_with(ROBOTS_PREFIX))
        .cloned()
        .collect();
    assert_eq!(held, robots, "build names every robot pool");
    for pool in &robots {
        assert_eq!(
            build.allocation[pool].to_string(),
            "0",
            "build reserves none of {pool}: its robots borrow from the root, first come \
             first served"
        );
    }
    let slots: u64 = robot_pools
        .iter()
        .map(|p| match &p.capacity {
            boss_jobs::pools::PoolCapacity::Configured { amount } => amount
                .to_string()
                .parse::<u64>()
                .expect("a whole slot count"),
            boss_jobs::pools::PoolCapacity::Unknown {} => panic!("{} declares its slots", p.name),
        })
        .sum();
    let seats = u64::try_from(build.capacity.expect("build declares its seats")).expect("positive");
    assert_eq!(seats, slots, "build seats the robot slots it can borrow");
    let hosts: Vec<&String> = build
        .allocation
        .keys()
        .filter(|pool| host_of(pool).is_some())
        .collect();
    assert!(
        hosts.is_empty(),
        "build reserves {hosts:?}, which engineering floats for every builder's claim"
    );
    assert_eq!(
        build.discipline,
        vec![
            boss_jobs::DisciplineKey::Priority,
            boss_jobs::DisciplineKey::Age
        ],
        "the default admission queue"
    );
    assert_eq!(build.predicate.kind.as_deref(), Some("backlog-item"));
    let step = build.predicate.step.as_ref().expect("a step clause");
    assert_eq!(step.slug.as_deref(), Some("build"));
    let workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform workflow bundle parses");
    let item = workflows
        .iter()
        .find(|w| w.kind == "backlog-item")
        .expect("the bundle declares backlog-item");
    assert!(
        item.steps.iter().any(|s| s.title == "build"),
        "the queue names a step backlog-item declares"
    );
}

/// DESIGN c0ae096f, car 1 (backlog 599fec3e; David, 2026-11-10). Finance
/// returns as a department — a direct child of the root — because the
/// company now holds real sponsorship money and BOSS tracks its budget.
/// Engineering gains its design and review stages, and Operations its
/// maintenance stage, as stations INSIDE them rather than sibling
/// departments, and the two queues of those stages leave executive for
/// them: design-review for engineering/design, landed-review (which agents
/// now pay) for engineering/review.
///
/// Read by ADDRESS, the chain of names derived from the rows' `parent`
/// ids, so no id is spelled here. That every row's parent names a station
/// row of the bundle is the tree's own rule, judged on the whole bundle by
/// `the_platform_stations_make_one_tree_with_every_row_in_it` above
/// (validate_station_tree: "names parent …, which no active tree row
/// declares as its id") and by the platform seed before it publishes.
#[test]
fn finance_is_a_department_and_the_stage_stations_stand_in_their_departments() {
    let rows = stations();
    for (name, under) in [
        ("finance", ""),
        ("design", "/engineering"),
        ("review", "/engineering"),
        ("maintenance", "/operations"),
        ("design-review", "/engineering/design"),
        ("landed-review", "/engineering/review"),
    ] {
        assert_eq!(
            address_of(&rows, name),
            Some(format!("/{ROOT}{under}/{name}")),
            "design c0ae096f places `{name}`"
        );
        assert_eq!(
            named(&rows, name).mode,
            Some(boss_jobs::stations::StationMode::Standing),
            "`{name}` is a standing station"
        );
    }
}

#[test]
fn the_tree_pools_carry_their_shape_and_the_roster_its_class() {
    let pools = pools();
    let shape = |name: &str| {
        pools
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("the bundle declares `{name}`"))
            .shape
    };
    use boss_jobs::pools::PoolShape;
    assert_eq!(shape("host:w-1"), Some(PoolShape::Capacity));
    assert_eq!(shape("budget:robots"), Some(PoolShape::Rate));
    let people = pools
        .iter()
        .find(|p| p.name == "people:platform-admin")
        .expect("the roster pool");
    let roster = people
        .roster
        .as_ref()
        .expect("its capacity is a roster read");
    assert_eq!(
        roster.eligibility[0].codes,
        vec!["platform-admin".to_string()]
    );
    assert_eq!(people.capacity, boss_jobs::pools::PoolCapacity::Unknown {});
}

/// The robot pools follow the tree (backlog 84f570a1; station-tree.md §3,
/// "`model-slot:<class>` until T5 renames it"): one `robots:<model>` row
/// per model family the agents registry's robots declare — `opus` for
/// agent-claude and agent-claude-b, `sol` for agent-codex, read live on
/// 2026-11-07 — and no `model-slot:` row is declared any longer. The
/// <model> is the family a run's model id belongs to, the token an agent
/// row's `model_class` already holds, so the rename keys nothing new.
#[test]
fn the_robot_pools_are_robots_by_model_and_no_model_slot_row_is_declared() {
    use boss_jobs::pools::{PoolKind, PoolShape};
    let pools = pools();
    let robots: BTreeSet<&str> = pools
        .iter()
        .filter_map(|p| p.name.strip_prefix(boss_jobs::resources::ROBOTS_PREFIX))
        .collect();
    assert_eq!(robots, BTreeSet::from(["opus", "sol"]));
    for model in robots {
        let pool = pools
            .iter()
            .find(|p| p.name == boss_jobs::resources::robot_pool(model))
            .expect("the robot pool");
        // A claim reserves concurrency 1 of it (resources.rs `pools`).
        assert_eq!(
            (pool.kind, pool.shape, pool.unit.as_str()),
            (PoolKind::Flow, Some(PoolShape::Capacity), "slot"),
            "{}",
            pool.name
        );
    }
    let left: Vec<&str> = pools
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| n.starts_with("model-slot:"))
        .collect();
    assert!(left.is_empty(), "model-slot rows still declared: {left:?}");
}

/// Every pool row says where its capacity number comes from (T5; the
/// station-tree.md §3 table's last column), so the console can show the
/// source beside the number — and beside an `unknown`, the state most
/// rows are in and the one that most needs its reason said.
#[test]
fn every_pool_row_says_where_its_capacity_comes_from() {
    let silent: Vec<String> = pools()
        .into_iter()
        .filter(|p| {
            p.capacity_from
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
        })
        .map(|p| p.name)
        .collect();
    assert!(
        silent.is_empty(),
        "pool rows with no capacity_from: {silent:?}"
    );
}

// -- acceptance 1: reorganizing is row edits alone ---------------------------

/// The bundle's files, copied into a scratch directory of this test's own,
/// so a test can edit rows as an operator would.
fn bundle_copy(label: &str) -> PathBuf {
    let dir = boss_testing::scratch_dir(label);
    for entry in std::fs::read_dir(platform_stations_path()).expect("read the bundle") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|e| e == "toml") {
            std::fs::copy(&path, dir.join(path.file_name().expect("a name"))).expect("copy");
        }
    }
    dir
}

/// One row edit: the file's `parent = ...` line set to `parent`, and its
/// `version = N` line to `version`.
fn edit_row(dir: &Path, name: &str, parent: &str, version: i32) {
    let file = dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&file).expect("read the row");
    let edited: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("parent = ") {
                format!("parent = \"{parent}\"")
            } else if line.starts_with("version = ") {
                format!("version = {version}")
            } else {
                line.to_string()
            }
        })
        .collect();
    std::fs::write(&file, edited.join("\n") + "\n").expect("write the row");
}

fn version_of(rows: &[StationSpec], name: &str) -> i32 {
    named(rows, name).version
}

/// A Documentation department, as the prototype's first seed had it.
fn documentation_row(root_id: &str) -> String {
    format!(
        "[[station]]\nname = \"documentation\"\nversion = 1\nstatus = \"active\"\n\
         title = \"Documentation\"\nkind = \"group\"\n\
         id = \"0d0c0d0c-0000-4000-8000-00000000d0c5\"\nparent = \"{root_id}\"\n\
         mode = \"standing\"\n\n[station.predicate]\ntags_any = [\"station:documentation\"]\n"
    )
}

const OPERATIONS_ROWS: [&str; 4] = ["operations", "estate", "vault", "edge"];
const BACK_HALF: [&str; 3] = ["converge", "deploy", "repair"];

fn addresses(rows: &[StationSpec]) -> BTreeMap<String, String> {
    rows.iter()
        .filter_map(|s| Some((s.name.clone(), address_of(rows, &s.name)?)))
        .collect()
}

/// Handoff §10, the test it sets itself: "the reorganization done on paper
/// today (drop Documentation, add Operations, move the back half of
/// pr-train) is one station row deleted, four added, three `parent` edits
/// ... If any of that needs a code change, that is the defect to hunt."
/// The station half of it, as edits to the bundle's rows and nothing else,
/// published through the seed and judged by the tree's rules on both
/// sides. (The protocol half is T6's, baac0b65: each workflow names its
/// owner station by id, pinned by
/// `every_platform_protocol_names_a_standing_owner_station`; the
/// `fulfilled_by` hand-off lands with step station specs.)
#[tokio::test]
async fn the_paper_reorganization_is_row_edits_alone() {
    let shipped = stations();
    let pools = pools();
    let root = id_of(&shipped, ROOT);
    let engineering = id_of(&shipped, "engineering");
    let operations = id_of(&shipped, "operations");

    // BEFORE: Documentation stands under the company, there is no
    // Operations, and the back half of pr-train is Engineering's.
    let dir = bundle_copy("paper-reorganization");
    // Operations and every row standing in it but the back half, READ OFF
    // the shipped tree: the paper's four (OPERATIONS_ROWS) plus any station
    // added under operations since, which the paper adds with them. A
    // listed copy broke the day `maintenance` joined operations (design
    // c0ae096f, backlog 599fec3e).
    let under = format!("/{ROOT}/operations");
    let operations_rows: Vec<String> = shipped
        .iter()
        .filter(|s| !BACK_HALF.contains(&s.name.as_str()))
        .filter(|s| {
            address_of(&shipped, &s.name)
                .is_some_and(|a| a == under || a.starts_with(&format!("{under}/")))
        })
        .map(|s| s.name.clone())
        .collect();
    for name in OPERATIONS_ROWS {
        assert!(
            operations_rows.iter().any(|r| r == name),
            "the paper adds `{name}`: {operations_rows:?}"
        );
    }
    let kept: BTreeMap<String, String> = operations_rows
        .iter()
        .map(|name| {
            let file = dir.join(format!("{name}.toml"));
            let text = std::fs::read_to_string(&file).expect("read");
            std::fs::remove_file(&file).expect("remove");
            (name.to_string(), text)
        })
        .collect();
    std::fs::write(dir.join("documentation.toml"), documentation_row(&root)).expect("write");
    for name in BACK_HALF {
        edit_row(&dir, name, &engineering, version_of(&shipped, name));
    }
    let before = load_stations(&dir).expect("the paper's before parses");
    assert_eq!(validate_station_tree(&before, &pools), Vec::<String>::new());
    assert_eq!(
        address_of(&before, "deploy"),
        Some(format!("/{ROOT}/engineering/deploy"))
    );
    assert_eq!(
        address_of(&before, "documentation"),
        Some(format!("/{ROOT}/documentation"))
    );

    let registry = InMemoryStations::new();
    seed_stations(&registry, &before, &actor(), at(0), false)
        .await
        .expect("the before tree publishes");

    // THE REORGANIZATION, as row edits: one deleted, four added, three
    // parents (each its version bump, the bundle's edit path).
    std::fs::remove_file(dir.join("documentation.toml")).expect("delete one row");
    for (name, text) in &kept {
        std::fs::write(dir.join(format!("{name}.toml")), text).expect("add a row");
    }
    for name in BACK_HALF {
        edit_row(&dir, name, &operations, version_of(&shipped, name) + 1);
    }
    let after = load_stations(&dir).expect("the paper's after parses");
    assert_eq!(validate_station_tree(&after, &pools), Vec::<String>::new());
    // The edited bundle is the shipped tree, address for address.
    assert_eq!(addresses(&after), addresses(&shipped));

    // Published through the seed; the deleted row is retired by the live
    // verb, as a removed bundle row is. The live tree is the after tree.
    let report = seed_stations(&registry, &after, &actor(), at(60), false)
        .await
        .expect("the reorganization publishes");
    assert!(!report.to_string().is_empty());
    registry
        .retire("documentation", &actor(), at(61))
        .await
        .expect("retire the deleted row");
    let live = registry.list_active().await.expect("the live registry");
    assert_eq!(validate_station_tree(&live, &pools), Vec::<String>::new());
    assert_eq!(addresses(&live), addresses(&shipped));
    for name in BACK_HALF {
        assert_eq!(
            address_of(&live, name),
            Some(format!("/{ROOT}/operations/{name}")),
            "the back half of pr-train moved by its parent alone"
        );
    }
    assert!(live.iter().all(|s| s.name != "documentation"));
}

/// Re-parenting one station is ONE field (and the version bump every
/// bundle edit carries); the address beneath follows, nothing else moves.
/// Moving a station that holds an allocation without moving what its new
/// parent holds is refused, naming the constraint.
#[tokio::test]
async fn re_parenting_a_station_is_one_field() {
    let shipped = stations();
    let pools = pools();
    let operations = id_of(&shipped, "operations");
    let registry = InMemoryStations::new();
    seed_stations(&registry, &shipped, &actor(), at(0), false)
        .await
        .expect("the shipped tree publishes");

    let dir = bundle_copy("re-parent-one-station");
    let file = dir.join("probe-shed.toml");
    let text_before = std::fs::read_to_string(&file).expect("read");
    edit_row(
        &dir,
        "probe-shed",
        &operations,
        version_of(&shipped, "probe-shed") + 1,
    );
    let text_after = std::fs::read_to_string(&file).expect("read");
    let changed: Vec<(&str, &str)> = text_before
        .lines()
        .zip(text_after.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(
        changed.len(),
        2,
        "the parent and the version, nothing else: {changed:?}"
    );

    let edited = load_stations(&dir).expect("parses");
    assert_eq!(validate_station_tree(&edited, &pools), Vec::<String>::new());
    seed_stations(&registry, &edited, &actor(), at(60), false)
        .await
        .expect("the re-parent publishes");
    let live = registry.list_active().await.expect("live");
    assert_eq!(
        address_of(&live, "probe-shed"),
        Some(format!("/{ROOT}/operations/probe-shed"))
    );
    let moved: Vec<&str> = live
        .iter()
        .filter(|s| s.version != version_of(&shipped, &s.name))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(moved, vec!["probe-shed"], "one row changed");
    assert_eq!(
        named(&live, "probe-shed").id,
        named(&shipped, "probe-shed").id,
        "the id is stable"
    );

    // The gates hold 12 vCPU of host:w-1 from Engineering; Operations holds
    // none, so moving them there alone is refused by the allocation rule.
    edit_row(
        &dir,
        "gates",
        &operations,
        version_of(&shipped, "gates") + 1,
    );
    let refused = validate_station_tree(&load_stations(&dir).expect("parses"), &pools);
    assert!(
        refused
            .iter()
            .any(|r| r.contains("`operations`") && r.contains("`host:w-1`")),
        "{refused:?}"
    );
}

// -- acceptance 2: an old build reads the new rows ---------------------------

/// The station row as the build before this car reads it, field for field
/// and attribute for attribute (crates/core/boss-jobs/src/stations.rs at
/// origin/main 18d8525c5): FROZEN, because it stands for the named
/// last-converged build a watchdog rollback lands on. Like that build's
/// StationSpec it denies no unknown field. Never edit it to match a later
/// StationSpec — that is the point. One field is left out: v1's capacity
/// count, an `Option<i32>` under `serde(default)`, whose name the factory
/// vocabulary lint retires in new code. This car changes no row's value
/// for it, and the mirror denies no unknown key, so leaving it out changes
/// no answer this test gives.
#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct PreTreeStation {
    name: String,
    version: i32,
    status: WorkflowStatus,
    title: String,
    kind: boss_jobs::StationKind,
    #[serde(default)]
    automaton: Option<String>,
    predicate: boss_jobs::StationPredicate,
    #[serde(default = "boss_jobs::station_queue::default_discipline")]
    discipline: Vec<boss_jobs::DisciplineKey>,
    #[serde(default)]
    terminal_window_days: Option<u32>,
    #[serde(default)]
    capability: Option<boss_jobs::StationCapability>,
    #[serde(default)]
    rollup_parent: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    floor: Option<boss_jobs::stations::FloorPlacement>,
    #[serde(default)]
    edges: Vec<String>,
    #[serde(default)]
    lens: Option<boss_jobs::StationLens>,
    created_at: DateTime<Utc>,
}

/// The pool row as the build before this car reads it: PoolSpec at
/// origin/main a113f3b6e, open to unknown keys (backlog fb893bba). FROZEN.
#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct PreShapePool {
    name: String,
    version: i32,
    status: WorkflowStatus,
    unit: String,
    kind: boss_jobs::pools::PoolKind,
    capacity: boss_jobs::pools::PoolCapacity,
    #[serde(default)]
    roster: Option<boss_jobs::pools::RosterDeclaration>,
    #[serde(default)]
    reclaimer: Option<String>,
    #[serde(default)]
    reclaimer_kind: Option<String>,
    #[serde(default)]
    program: Option<String>,
    #[serde(default)]
    return_verifier: Option<String>,
    #[serde(default)]
    identity_pin_unenforced: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    station: Option<String>,
    #[serde(default)]
    created_at: DateTime<Utc>,
}

/// THE ROLLBACK PIN. Every station and pool row this car ships, in the
/// wire shape a stored row has (the published fact's payload, the claim
/// path's `to_jsonb`, the API body), is read by the build before it: the
/// tree keys and `shape` are skipped, and the kind, predicate and capacity
/// are values that build already knows. A row it could not read would
/// take every station or pool read down with it on a rollback.
#[test]
fn an_old_build_reads_every_new_station_and_pool_row() {
    let mut tree_keys = 0;
    for row in stations() {
        let wire = serde_json::to_value(&row).expect("a station serializes");
        tree_keys += ["id", "parent", "mode", "capacity", "machines", "allocation"]
            .iter()
            .filter(|k| wire.get(**k).is_some())
            .count();
        let old: PreTreeStation = serde_json::from_value(wire.clone())
            .unwrap_or_else(|e| panic!("the old build refuses `{}`: {e}\n{wire}", row.name));
        assert_eq!(
            (old.name.as_str(), old.version),
            (row.name.as_str(), row.version)
        );
        assert_eq!(old.predicate, row.predicate, "the predicate reads the same");
    }
    assert!(tree_keys > 0, "the pin reads rows that carry tree keys");

    let mut shaped = 0;
    for pool in pools() {
        let wire = serde_json::to_value(&pool).expect("a pool serializes");
        shaped += usize::from(wire.get("shape").is_some());
        let old: PreShapePool = serde_json::from_value(wire.clone())
            .unwrap_or_else(|e| panic!("the old build refuses `{}`: {e}\n{wire}", pool.name));
        // (The stored-row reader of that build names `shape` as a key it
        // read past and reads on: pools.rs's
        // a_stored_pool_row_carrying_a_key_this_build_does_not_know_still_reads.)
        assert_eq!(old.capacity, pool.capacity, "the capacity reads the same");
    }
    assert!(shaped > 0, "the pin reads pools that carry a shape");
}

// -- acceptance 3: the tree read serves the real hosts -----------------------

fn user_header() -> String {
    serde_json::to_string(&User {
        id: "emp-operator".to_string(),
        role: "operator".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User serialises")
}

async fn tree_app() -> axum::Router {
    let registry = Arc::new(InMemoryStations::new());
    for row in stations() {
        registry.seed(row).expect("seed a station");
    }
    let pool_registry = Arc::new(InMemoryPools::default());
    seed_pools(pool_registry.as_ref(), &pools(), &actor(), at(0), false)
        .await
        .expect("seed the pools");
    let jobs = Arc::new(InMemoryJobs::for_selection_fixture().with_resource_pools(pool_registry));
    let estate = boss_jobs::estate_seed::load_estate_toml(
        &boss_testing::repo_root().join("infra/estate/estate.toml"),
    )
    .expect("the estate declaration parses");
    jobs.declare_estate_nodes(
        &estate,
        &EventStamp::new("jobs", ActorId::Automation("estate-seed".into())),
    )
    .await
    .expect("declare the estate");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        stations: Some(registry as Arc<dyn StationRegistry>),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header("x-boss-user", user_header())
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn the_tree_read_serves_the_real_hosts_from_the_estate_registry() {
    let app = tree_app().await;
    let (status, body) = get(&app, &format!("/api/stations?under={ROOT}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().expect("a subtree");
    assert_eq!(body["total"].as_u64(), Some(rows.len() as u64));
    let root_name = rows[0]["station"]["name"]
        .as_str()
        .expect("the root has a name");
    let root = format!("/{root_name}");
    assert_eq!(
        rows[0]["address"],
        root.as_str(),
        "the root's address is its own name"
    );
    let gates = rows
        .iter()
        .find(|r| r["station"]["name"] == "gates")
        .expect("the gates stand in the company");
    assert_eq!(
        gates["address"],
        format!("{root}/engineering/gates").as_str()
    );
    assert_eq!(gates["depth"], 2);
    assert_eq!(
        rows.len(),
        stations().len(),
        "the root's subtree is every tree row"
    );

    let pools = body["pools"].as_array().expect("pools beside the tree");
    let mut served: Vec<(String, i64)> = pools
        .iter()
        .filter(|p| p.get("host").is_some())
        .map(|p| {
            assert_eq!(
                p["pool"]["name"],
                format!("host:{}", p["host"]["id"].as_str().unwrap())
            );
            (
                p["host"]["id"].as_str().unwrap().to_string(),
                p["host"]["cpu"].as_i64().unwrap(),
            )
        })
        .collect();
    served.sort();
    let expected: Vec<(String, i64)> = [
        ("boss-gcp", 4),
        ("cp-1", 8),
        ("cp-2", 12),
        ("cp-3", 8),
        ("forge", 16),
        ("w-1", 32),
        ("w-2", 4),
        ("w-3", 8),
    ]
    .into_iter()
    .map(|(n, c)| (n.to_string(), c))
    .collect();
    assert_eq!(
        served, expected,
        "every real host, read from the estate registry"
    );
    assert!(
        pools.iter().all(|p| p.get("host_unavailable").is_none()),
        "{pools:?}"
    );
    assert_eq!(body["hosts_without_pool"], serde_json::json!([]));
    assert!(
        body["pools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pool"]["name"] == "budget:robots")
    );
    // The seed lands the robot pools under the tree's names, each with
    // where its capacity comes from on the wire (backlog 84f570a1): the
    // SET limit allocation item 5 landed (design 012822ea Q1), never one
    // derived from max_concurrent_runs. The SHAPE is asserted, never the
    // NUMBER: a slot count is a tunable that lives in its row
    // (infra/platform/pools/robots:<model>.toml) and nowhere else
    // (CLAUDE.md 9a). Until backlog f399458f this loop pinned 6 and 2, so
    // raising robots:opus (item 89b17d21, 4 -> 6) rode a code car, a gate
    // and a train to keep it green, and a publish through the live pools
    // door would have left main red for a change that carried no code.
    let declared = load_pools(platform_pools_path()).expect("the platform pool bundle parses");
    let robots: Vec<&PoolSpec> = declared
        .iter()
        .filter(|p| p.name.starts_with(boss_jobs::resources::ROBOTS_PREFIX))
        .collect();
    assert!(!robots.is_empty(), "the tree declares robot pools");
    for spec in robots {
        let name = spec.name.as_str();
        let row = pools
            .iter()
            .find(|p| p["pool"]["name"] == name)
            .unwrap_or_else(|| panic!("the seed lands {name}: {pools:?}"));
        // A robot pool is a root pool: it names no station of its own
        // (station-tree.md section 3), so it is served under the root.
        assert_eq!(spec.station, None, "{name} is held at the root");
        assert_eq!(row["pool"]["capacity"]["state"], "configured", "{row}");
        let amount = row["pool"]["capacity"]["amount"]
            .as_str()
            .and_then(|a| a.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("{name}: the amount on the wire is an integer: {row}"));
        assert!(amount > 0, "{name}: a configured limit is positive: {row}");
        let models: Vec<&str> = row["pool"]["models"]
            .as_array()
            .map(|m| m.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(
            !models.is_empty(),
            "{name} meters at least one model: {row}"
        );
        assert_eq!(models, spec.models, "{name}: the models the row declares");
        assert!(
            row["pool"]["capacity_from"]
                .as_str()
                .is_some_and(|s| s.contains("SET") && s.contains("012822ea")),
            "{row}"
        );
    }
    assert!(
        pools.iter().all(|p| !p["pool"]["name"]
            .as_str()
            .unwrap()
            .starts_with("model-slot:")),
        "{pools:?}"
    );

    // A subtree serves its own rows and the pools they hold from.
    let (status, body) = get(&app, "/api/stations?under=engineering").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: BTreeSet<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["station"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains("gates") && names.contains("forge-ci") && !names.contains("deploy"));
    let held: Vec<&str> = body["pools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pool"]["name"].as_str().unwrap())
        .collect();
    // Engineering names the robot pools for its build station (backlog
    // b5c815e4; reserving none of them since 30c1efe8, so every robot
    // claim borrows from the root) beside its share of the build node.
    assert_eq!(held, vec!["host:w-1", "robots:opus", "robots:sol"]);

    let (status, _) = get(&app, "/api/stations?under=no-such-station").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The flat list is unchanged without `under`.
    let (status, body) = get(&app, "/api/stations").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"][0].get("address").is_none(),
        "{}",
        body["data"][0]
    );
}

/// Backlog e3a927c3: the station view reads a station's placement off
/// GET /api/stations, the flat list and the tree read alike, and a row
/// with none carries no `placement` key, which is the view's cue to lay
/// it out on its fallback grid.
#[tokio::test]
async fn the_station_reads_carry_each_placement_and_omit_a_missing_one() {
    let app = tree_app().await;
    let gates = serde_json::json!({"x": 590, "y": 120, "w": 230, "h": 170});
    let (status, body) = get(&app, "/api/stations").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = |name: &str| {
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("`{name}` in the list"))
            .clone()
    };
    assert_eq!(row("gates")["placement"], gates);
    assert!(
        row("converge").get("placement").is_none(),
        "{}",
        row("converge")
    );

    let (status, body) = get(&app, "/api/stations?under=engineering").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let placed = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"]["name"] == "gates")
        .expect("gates under engineering");
    assert_eq!(placed["station"]["placement"], gates);
}

#[test]
fn a_subtree_read_is_bounded_by_the_rows_it_holds() {
    let rows = stations();
    let nodes = subtree(&rows, ROOT).expect("the root");
    let unique: BTreeSet<&str> = nodes.iter().map(|n| n.station.name.as_str()).collect();
    assert_eq!(unique.len(), nodes.len(), "every station once");
}

/// Automations get a host station (docs/design/station-tree.md section 6:
/// "station = <its station id>"; backlog 6ecb5480): every automation row
/// names a STANDING station of the tree, resolved among the active rows,
/// and the dispatcher is three rows of one binary, one at the root, one
/// in engineering and one in operations.
#[test]
fn every_automation_names_a_standing_station_of_the_tree() {
    let rows = stations();
    let automations = boss_jobs::agents::automations::load_automations_dir(std::path::Path::new(
        boss_jobs::agents::automations::platform_automations_path(),
    ))
    .expect("the platform automation bundle parses");
    assert_eq!(
        validate_automation_stations(&automations, &rows),
        Vec::<String>::new()
    );
    let mut dispatchers: Vec<&str> = automations
        .iter()
        .filter(|a| a.id.starts_with("automation:dispatcher"))
        .filter_map(|a| a.station.as_deref())
        .filter_map(|id| rows.iter().find(|s| s.id.as_deref() == Some(id)))
        .map(|s| s.name.as_str())
        .collect();
    dispatchers.sort_unstable();
    assert_eq!(dispatchers, ["algedonic", "engineering", "operations"]);
}

/// The failures name the row and what it named: no station, an id no
/// ACTIVE row of the tree carries (a retired row is none), and a station
/// that is not standing.
#[test]
fn an_automation_that_stands_nowhere_is_named() {
    let rows = stations();
    let mut automations = boss_jobs::agents::automations::load_automations_dir(
        std::path::Path::new(boss_jobs::agents::automations::platform_automations_path()),
    )
    .expect("the platform automation bundle parses");
    automations.truncate(3);
    automations[0].station = None;
    automations[1].station = Some("00000000-0000-4000-8000-000000000000".into());
    let gates = id_of(&rows, "gates");
    automations[2].station = Some(gates.clone());
    let retired: Vec<StationSpec> = rows
        .iter()
        .cloned()
        .map(|mut s| {
            if s.name == "gates" {
                s.status = WorkflowStatus::Retired;
            }
            s
        })
        .collect();
    let failures = validate_automation_stations(&automations, &retired);
    assert_eq!(
        failures,
        vec![
            format!("{}: names no host station (station)", automations[0].id),
            format!(
                "{}: station 00000000-0000-4000-8000-000000000000 is no active station of the tree",
                automations[1].id
            ),
            format!(
                "{}: station {gates} is no active station of the tree",
                automations[2].id
            ),
        ],
    );
}
