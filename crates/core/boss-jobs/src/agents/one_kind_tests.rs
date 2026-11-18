//! The false-fourth-kind check (factory taxonomy M1, design 4348b857;
//! backlog 3ff2ef60, T1): every executor id the tree's record names
//! resolves to EXACTLY ONE `ExecutorKind`.
//!
//! WHY. An executor is a person, a robot or an automaton, and there is
//! no fourth kind (the design's rule 3). An id that two sources read as
//! two kinds — an employee row whose id the actor parse reads as an
//! automation, a login alias that is also a staff id, an automation row
//! spelled without its `automation:` prefix (which the parse reads as a
//! person) — is that fourth kind in disguise: whichever reader asks
//! first decides what the executor was. An id that resolves to no kind
//! (a blank) is the same failure from the other side.
//!
//! WHAT IS READ. The registries the tree declares, each for the kind it
//! implies, beside the `ActorId` parse of every canonical id:
//!   - `infra/platform/automations/` — the `automation_actors` bundle,
//!     each id and each `signs_for` family → automaton;
//!   - the tenant's `seeds/agents.toml` — each row → robot (the
//!     `agents.executor_kind` column's default; the declaration carries
//!     no kind of its own yet), and each login alias → its row's kind,
//!     resolved, never parsed: an address is a login, not an actor
//!     (`ActorId::RegisteredAgent`'s doc);
//!   - every `employees.json` seed → human (written `person`, the word every build reads).
//!
//! The claims history's half — every holder a recorded claim names
//! resolves to one kind — is `every_claim_holder_resolves_to_exactly_
//! one_executor_kind` in `claims_console_history.rs`. A unit module,
//! not a `tests/` file, so the pin links no new test binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use boss_core::actor::{ActorId, ExecutorKind};

use super::automations::load_automations_dir;
use super::seed::load_agents_toml;

/// Each id, and every (kind, source) the record gives it.
type Record = BTreeMap<String, BTreeSet<(ExecutorKind, &'static str)>>;

/// Record that `source` reads `id` as `kind`. A blank id is recorded
/// with no kind at all: it names no executor.
fn declare(record: &mut Record, id: &str, kind: ExecutorKind, source: &'static str) {
    let kinds = record.entry(id.to_string()).or_default();
    if !id.trim().is_empty() {
        kinds.insert((kind, source));
    }
}

/// Record a canonical id under its registry's kind AND under the kind
/// the actor parse gives it — the two must agree.
fn canonical(record: &mut Record, id: &str, kind: ExecutorKind, source: &'static str) {
    declare(record, id, kind, source);
    let parsed = match id.parse::<ActorId>() {
        Ok(actor) => actor.executor_kind(),
        Err(never) => match never {},
    };
    declare(record, id, parsed, "ActorId parse");
}

/// Every id that resolves to zero kinds or to more than one, named with
/// the sources that disagree.
fn not_one_kind(record: &Record) -> Vec<String> {
    record
        .iter()
        .filter_map(|(id, kinds)| {
            let distinct: BTreeSet<ExecutorKind> = kinds.iter().map(|(k, _)| *k).collect();
            (distinct.len() != 1).then(|| format!("{id:?} resolves to {kinds:?}"))
        })
        .collect()
}

fn employee_ids(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    rows.iter()
        .map(|row| {
            row["id"]
                .as_str()
                .unwrap_or_else(|| panic!("{}: an employee row has no id", path.display()))
                .to_string()
        })
        .collect()
}

fn tree_record() -> Record {
    let root = boss_testing::repo_root();
    let mut record = Record::new();
    let automations = load_automations_dir(&root.join("infra/platform/automations")).unwrap();
    assert!(
        !automations.is_empty(),
        "the automations bundle read as empty"
    );
    for row in &automations {
        canonical(
            &mut record,
            &row.id,
            ExecutorKind::Automaton,
            "automation_actors",
        );
        if let Some(family) = &row.signs_for {
            canonical(
                &mut record,
                family,
                ExecutorKind::Automaton,
                "automation_actors.signs_for",
            );
        }
    }
    let agents =
        load_agents_toml(&root.join(
            "crates/orchestrators/boss-cli/tests/fixtures/tenant-algedonic/seeds/agents.toml",
        ))
        .unwrap();
    assert!(!agents.is_empty(), "the tenant's agents read as empty");
    for row in &agents {
        canonical(&mut record, &row.id, ExecutorKind::Robot, "agents");
        for alias in &row.aliases {
            declare(&mut record, alias, ExecutorKind::Robot, "actor_aliases");
        }
    }
    for seed in [
        "examples/brewery/seeds/employees.json",
        "crates/orchestrators/boss-cli/tests/fixtures/tenant-algedonic/seeds/employees.json",
    ] {
        let ids = employee_ids(&root.join(seed));
        assert!(!ids.is_empty(), "{seed} read as empty");
        for id in ids {
            canonical(&mut record, &id, ExecutorKind::Human, "employees");
        }
    }
    record
}

#[test]
fn every_executor_id_the_tree_declares_resolves_to_exactly_one_kind() {
    let record = tree_record();
    let wrong = not_one_kind(&record);
    assert!(
        wrong.is_empty(),
        "{} executor id(s) do not resolve to exactly one kind — a fourth kind in disguise:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    // Each kind is present, so the check above judged all three.
    let seen: BTreeSet<ExecutorKind> = record
        .values()
        .flat_map(|kinds| kinds.iter().map(|(k, _)| *k))
        .collect();
    assert_eq!(seen, ExecutorKind::ALL.into_iter().collect());
}

/// The `agents.executor_kind` column lives twice (CLAUDE.md §9a): its
/// SQL default and CHECK in the migration, and the kind the actor parse
/// gives a registered agent's id. The record above reads every agents
/// row as robot BECAUSE the column's default is the parse's answer; this
/// holds the two equal, and holds the CHECK to words the enum knows and
/// that are not `person` (an agents row is never staff).
#[test]
fn the_agents_executor_kind_column_defaults_to_what_the_parse_says_an_agent_is() {
    let dir = boss_testing::repo_root().join("infra/postgres/schema");
    let declaring: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.ok()?.path()).ok()?;
            text.contains("ADD COLUMN IF NOT EXISTS executor_kind")
                .then_some(text)
        })
        .collect();
    assert_eq!(
        declaring.len(),
        1,
        "one migration declares agents.executor_kind"
    );
    let sql = &declaring[0];
    let quoted = |after: &str| -> Vec<String> {
        let tail = &sql[sql
            .find(after)
            .unwrap_or_else(|| panic!("no {after} in the migration"))
            + after.len()..];
        let clause = &tail[..tail.find(')').unwrap_or(tail.len())];
        clause
            .split('\'')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    };
    let default: ExecutorKind = quoted("DEFAULT")[0].parse().unwrap();
    let agent: ActorId = "agent-claude".parse().unwrap();
    assert_eq!(default, agent.executor_kind());
    let admitted: Vec<ExecutorKind> = quoted("CHECK (executor_kind IN (")
        .iter()
        .map(|w| w.parse().unwrap())
        .collect();
    assert_eq!(admitted, [ExecutorKind::Robot, ExecutorKind::Automaton]);
}

/// The control: the check above is strong enough to name each shape of
/// a fourth kind, so its silence on the tree means something.
#[test]
fn the_one_kind_check_names_each_shape_of_a_fourth_kind() {
    let mut planted = Record::new();
    // An employee row whose id the parse reads as an automation.
    canonical(
        &mut planted,
        "automation:intruder",
        ExecutorKind::Human,
        "employees",
    );
    // An automation row spelled without its prefix: the parse says person.
    canonical(
        &mut planted,
        "nightly-sweep",
        ExecutorKind::Automaton,
        "automation_actors",
    );
    // A login alias that is also a staff id.
    canonical(&mut planted, "emp-shared", ExecutorKind::Human, "employees");
    declare(
        &mut planted,
        "emp-shared",
        ExecutorKind::Robot,
        "actor_aliases",
    );
    // An id that names no executor.
    canonical(&mut planted, "", ExecutorKind::Human, "employees");
    // And one that is fine, which must not be named.
    canonical(&mut planted, "emp-032", ExecutorKind::Human, "employees");
    let named = not_one_kind(&planted).join("\n");
    for id in [
        "\"automation:intruder\"",
        "\"nightly-sweep\"",
        "\"emp-shared\"",
        "\"\"",
    ] {
        assert!(named.contains(id), "{id} was not named:\n{named}");
    }
    assert!(
        !named.contains("emp-032"),
        "a one-kind id was named:\n{named}"
    );
}
