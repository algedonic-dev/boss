//! Static validation for `StationSpec` rows — the station **viability
//! lint**, the counterpart to [`crate::workflow_lint`].
//!
//! A Workflow is a program and its lint proves the program can finish.
//! A station is a *queue declaration*, and the failure mode is not a
//! program that cannot finish — it is a queue that silently holds
//! nothing, or that behaves differently from what the row says. Both
//! are invisible: an empty queue and a correctly-empty queue render
//! identically, so a station that can never match is indistinguishable
//! from a quiet day. That is the whole reason this file exists.
//!
//! Every rule below refuses a row that is wrong *on its own terms* —
//! provable from the spec alone, with no reference to what packets
//! happen to exist. A station holding nothing today is not a defect; a
//! station that could not hold anything on any day is.
//!
//! - **Contradictory metadata.** A key in both `metadata_present` and
//!   `metadata_absent`. No packet can satisfy both. The step clause has
//!   the same shape: a key demanded both `metadata_unmarked` and
//!   `metadata_equals` a marker value.
//! - **Terminal status with no window.** `status: closed|cancelled`
//!   while `terminal_window_days` is unset. The evaluation universe is
//!   in-flight packets (`http/stations.rs` filters `JobStatus::Open`
//!   unless a window is declared), so the clause can never match.
//! - **Inert window.** The mirror image: a `terminal_window_days`
//!   retention rule on a predicate pinned to a non-terminal status.
//!   The window is dead configuration — it reads as "departed packets
//!   linger here" and nothing ever will.
//! - **The `@me` biconditional.** `kind: actor` means the queue
//!   depends on who is asking, and [`SELF`] is the only thing that
//!   makes it so. An `actor` row without `@me` shows every executor
//!   the same queue while claiming to be personal; a shared row *with*
//!   `@me` varies per viewer while claiming to be shared — which is
//!   what made the census read a per-actor station as depth 0 and
//!   overstate orphaned packets at 92% when the real figure was 75%.
//! - **Self-parented rollup.** `rollup_parent == name` is a
//!   one-element cycle; a renderer walking parents does not return.
//! - **Equipment declares its automaton and bays, and only equipment
//!   does** (factory taxonomy M2, item 0a78f8f8). An `equipment` row
//!   with no automaton is shared equipment nobody operates, and with no
//!   bays a capacity nobody can read; a generic row naming either says
//!   it is equipment while its kind says it is not, and no reader would
//!   honour the field there.
//! - **An edge named twice.** `edges` lists the routes out of a station;
//!   the same target twice is one route written twice, and the floor's
//!   reader once refused the whole floor over it (backlog 1bb8c479).
//! - **A tree row that cannot stand in the tree** (T2 of design 4b27a65a,
//!   backlog 3f58c823). A tree field — `parent`, `mode`, `capacity`,
//!   `machines`, `allocation` — on a row with no `id` places nothing; an
//!   id or parent that is blank names nothing; a row that is its own
//!   parent is a one-element cycle; a tree row declares its mode; seats
//!   are at least one, and on equipment they are its bays (the two
//!   fields say one thing until `bays` retires); a machine is an
//!   automation, named once. Whether the parent exists, the tree has one
//!   root and the allocations fit needs the other rows, and is
//!   [`crate::station_tree::validate_station_tree`]'s question.
//! - **A placement that cannot be drawn** (backlog e3a927c3). An empty
//!   box, one past its parent floor's edge, or one on a row with no
//!   parent floor to stand on. Whether it overlaps a sibling needs the
//!   other rows and is
//!   [`crate::stations::validate_station_placements`]'s question.
//!
//! Runs at author time (`POST /api/stations/_validate`) and at publish
//! time (inside `StationRegistry::publish`, in BOTH adapters, against
//! the row the transaction actually promotes). One definition, two
//! call sites — because the 2026-08-13 outage was precisely a
//! `_validate` that could name the problem and a publish path that
//! never asked it.
//!
//! **Not yet covered: boot.** A row INSERTed straight into `stations`
//! by a SQL seed never passes through `publish`, so it bypasses this
//! file entirely — the same hole `workflow_quarantine` closes for
//! Workflows. Every row live today is viable (checked against the
//! deployed registry when this landed), so the gap is latent rather
//! than active; closing it is tracked separately.

use crate::station_queue::{SELF, StationPredicate};
use crate::stations::{StationKind, StationSpec};
use boss_core::job::JobStatus;
use serde_json::Value;

/// One viability failure. `field` is the offending spec field (empty
/// for whole-station failures), mirroring
/// [`crate::workflow_lint::WorkflowLintError`]'s `step`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationLintError {
    pub station: String,
    pub field: String,
    pub reason: String,
}

impl std::fmt::Display for StationLintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(f, "[{}] {}", self.station, self.reason)
        } else {
            write!(f, "[{}] `{}`: {}", self.station, self.field, self.reason)
        }
    }
}

fn err(spec: &StationSpec, field: &str, reason: impl Into<String>) -> StationLintError {
    StationLintError {
        station: spec.name.clone(),
        field: field.to_string(),
        reason: reason.into(),
    }
}

/// Whether a job status is terminal — a packet in this status has
/// left the network, so an in-flight queue can never hold it.
fn is_terminal(status: JobStatus) -> bool {
    matches!(status, JobStatus::Closed | JobStatus::Cancelled)
}

/// Validate a single StationSpec. Returns every violation found; an
/// empty Vec means the station is viable.
pub fn validate_station(spec: &StationSpec) -> Vec<StationLintError> {
    let mut errs = Vec::new();
    check_predicate(spec, &spec.predicate, &mut errs);
    check_retention(spec, &mut errs);
    check_self_binding(spec, &mut errs);
    check_rollup(spec, &mut errs);
    check_derived_namespace(spec, &mut errs);
    check_retired_kind(spec, &mut errs);
    check_equipment(spec, &mut errs);
    check_edges(spec, &mut errs);
    check_tree_fields(spec, &mut errs);
    check_placement(spec, &mut errs);
    errs
}

/// Validate every spec in a list. One call, every error reported —
/// for the seed bundles and the boot check.
pub fn validate_all(specs: &[StationSpec]) -> Vec<StationLintError> {
    specs.iter().flat_map(validate_station).collect()
}

/// **The publish gate.** A spec may occupy the ACTIVE slot only if it
/// is viable; `Err` carries every problem in the order the lint found
/// them.
///
/// Called by every registry write that can set `status = active`, in
/// BOTH adapters. Draft writes deliberately do NOT call it: a draft is
/// work in progress and may be saved in any state — the same posture
/// as [`crate::workflow_lint::gate_active`].
pub fn gate_active(spec: &StationSpec) -> Result<(), Vec<StationLintError>> {
    let errs = validate_station(spec);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// The wire shape for a list of lint problems: `[{field, reason,
/// message}]`. One definition so the author-time dry run
/// (`_validate`, 200 + `ok:false`) and the publish refusal (422) hand
/// the editor the same JSON to render.
pub fn problems_json(errs: &[StationLintError]) -> Vec<Value> {
    errs.iter()
        .map(|e| {
            serde_json::json!({
                "field": e.field,
                "reason": e.reason,
                "message": e.to_string(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// A key demanded present and absent at once. Reported once per key,
/// in a deterministic order, so two runs of the lint agree.
fn check_predicate(spec: &StationSpec, p: &StationPredicate, errs: &mut Vec<StationLintError>) {
    let mut both: Vec<&String> = p
        .metadata_present
        .iter()
        .filter(|k| p.metadata_absent.contains(k))
        .collect();
    both.sort();
    both.dedup();
    for key in both {
        errs.push(err(
            spec,
            "predicate.metadata_present",
            format!(
                "metadata key `{key}` is required both present and absent; \
                 no packet can satisfy both, so this queue is always empty"
            ),
        ));
    }
    check_step_markers(spec, p, errs);
}

/// The step clause's version of the same contradiction: a key demanded
/// equal to a marker value AND demanded unmarked. `metadata_unmarked`
/// reads [`crate::stranded::marked`]'s rule, so the two clauses are only
/// contradictory when the demanded value IS a marker — `hold: ""` is a
/// released hold and agrees with `metadata_unmarked: ["hold"]`, which is
/// why this tests the value rather than the key alone.
fn check_step_markers(spec: &StationSpec, p: &StationPredicate, errs: &mut Vec<StationLintError>) {
    let Some(step) = &p.step else { return };
    let mut both: Vec<&String> = step
        .metadata_unmarked
        .iter()
        .filter(|k| {
            step.metadata_equals.get(*k).is_some_and(|want| {
                crate::stranded::marked(&serde_json::json!({ "v": want }), "v").is_some()
            })
        })
        .collect();
    both.sort();
    both.dedup();
    for key in both {
        errs.push(err(
            spec,
            "predicate.step.metadata_unmarked",
            format!(
                "step metadata key `{key}` is required unmarked and equal to a \
                 marker value at once; no step can satisfy both, so this queue \
                 is always empty"
            ),
        ));
    }
}

/// The two halves of the retention rule: a terminal status needs a
/// window to be reachable at all, and a window needs a terminal status
/// to do anything.
fn check_retention(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    match (spec.predicate.status, spec.terminal_window_days) {
        (Some(status), None) if is_terminal(status) => errs.push(err(
            spec,
            "predicate.status",
            format!(
                "status `{status:?}` is terminal but no `terminal_window_days` is declared; \
                 a station evaluates in-flight packets only, so this queue is always empty. \
                 Declare a retention window, or match an in-flight status."
            )
            .to_lowercase(),
        )),
        (Some(status), Some(days)) if !is_terminal(status) => errs.push(err(
            spec,
            "terminal_window_days",
            format!(
                "a {days}-day retention window is declared, but the predicate pins \
                 status `{status:?}`, which is never terminal; the window can never \
                 apply. Drop the window, or match a terminal status."
            )
            .to_lowercase(),
        )),
        _ => {}
    }
}

/// `kind: actor` ⟺ the predicate names `@me`. Both directions are
/// failures, and each is silent in its own way.
fn check_self_binding(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    let binds = spec.predicate.binds_self();
    match (spec.kind, binds) {
        (StationKind::Actor, false) => errs.push(err(
            spec,
            "predicate",
            format!(
                "an `actor` station serves one executor, but no clause names the \
                 `{SELF}` placeholder, so every executor sees the same queue. \
                 Bind a clause to `{SELF}`, or declare a different station kind."
            ),
        )),
        (kind, true) if kind != StationKind::Actor => errs.push(err(
            spec,
            "kind",
            format!(
                "the predicate names `{SELF}`, so this queue's contents depend on who \
                 is asking — but the station is declared `{}`, which is shared. A \
                 shared queue that varies per viewer cannot be reported on or handed \
                 off. Declare `actor`, or drop the `{SELF}` clause.",
                kind.as_str()
            ),
        )),
        _ => {}
    }
}
/// An authored station in the DERIVED namespace must cover the
/// constraint its name claims.
///
/// `q.<role>.<step-kind>` is the projection's namespace
/// ([`crate::station_projection::station_name`]), and the merge rule is
/// "authored wins": a projected row whose name is already authored is
/// DROPPED ([`crate::station_projection::derived_stations`]). That is
/// the right rule for a deliberate override — and a silent hole for an
/// accidental one: an authored row squatting on `q.bookkeeper.task`
/// with an unrelated predicate SHADOWS the constraint queue without
/// serving it, so steps for that (kind, role) pair reach no queue while
/// the station list still shows the name (a6306207's one residual —
/// the projection covers every declared pair by construction EXCEPT
/// through this shadow).
///
/// The check is on the station's own terms, like every rule here: a
/// name in the reserved namespace must carry the step clause the name
/// promises — `kind = <step-kind>` and `metadata_equals.authority_role
/// = <role>`, the exact predicate the projection would have written.
fn check_derived_namespace(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    let Some(rest) = spec.name.strip_prefix("q.") else {
        return;
    };
    // Roles and step kinds are kebab-case, so the dot split is
    // unambiguous (station_projection's own naming argument). A
    // `q.`-name that does not split is not the derived shape.
    let Some((role, kind)) = rest.split_once('.') else {
        return;
    };
    if role.is_empty() || kind.is_empty() {
        return;
    }
    let covers = spec.predicate.step.as_ref().is_some_and(|s| {
        s.kind.as_deref() == Some(kind)
            && s.metadata_equals.get("authority_role").map(String::as_str) == Some(role)
    });
    if !covers {
        errs.push(err(
            spec,
            "name",
            format!(
                "occupies the derived namespace `q.{role}.{kind}` — the projection \
                 yields to an authored name, so this station SHADOWS that constraint \
                 queue without serving it: steps for (`{kind}`, `{role}`) would reach \
                 no queue while the list still shows the name. Declare the covering \
                 step clause (kind = `{kind}`, metadata_equals authority_role = \
                 `{role}`), or name the station outside `q.`."
            ),
        ));
    }
}

/// The deleted `batch` kind (item 0a78f8f8) reads only so the record
/// still reads ([`StationKind::RetiredBatch`]); no row may be published
/// with it.
fn check_retired_kind(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    if spec.kind == StationKind::RetiredBatch {
        errs.push(err(
            spec,
            "kind",
            "the `batch` kind was deleted (factory taxonomy Q1): a station that \
             bundles packets for an automaton is `equipment` with its automaton and \
             bays, and one served by people is `group`"
                .to_string(),
        ));
    }
}

/// `kind: equipment` ⟺ the row names its automaton and its bays — the
/// register entry `an-equipment-station-declares-its-bays`.
///
/// The automaton is held to what an automaton IS in the executor
/// taxonomy: an id that parses as `ActorId::Automation`, the set an
/// automaton executor is drawn from (`ExecutorKind::Automaton`, car
/// eec11034, maps from exactly that variant). Two more halves are
/// checked because the parse alone admits them: the id must be spelled
/// the way it parses back — `system` parses as `automation:platform`
/// but names no row — and its slug must not be empty. That a REGISTERED
/// automation answers to it is a question of the automations registry,
/// not of the row alone, so [`crate::stations::validate_station_automatons`]
/// asks it at every publish (backlog d0cf2cdc).
fn check_equipment(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    if spec.kind != StationKind::Equipment {
        let kind = spec.kind.as_str();
        if let Some(automaton) = &spec.automaton {
            errs.push(err(
                spec,
                "automaton",
                format!(
                    "a `{kind}` station names automaton `{automaton}`, but only an \
                     `equipment` station is operated by an automaton. Declare \
                     `kind = \"equipment\"` with its bays, or drop the automaton."
                ),
            ));
        }
        if let Some(bays) = spec.bays {
            errs.push(err(
                spec,
                "bays",
                format!(
                    "a `{kind}` station declares {bays} bay(s), but bays bound only an \
                     `equipment` station; a generic station's bandwidth is its \
                     capability. Drop the bays."
                ),
            ));
        }
        return;
    }
    match spec.automaton.as_deref() {
        None => errs.push(err(
            spec,
            "automaton",
            "an `equipment` station names the automaton that operates it \
             (`automaton = \"automation:<slug>\"`); none is declared"
                .to_string(),
        )),
        Some(automaton) if !is_automation_id(automaton) => errs.push(err(
            spec,
            "automaton",
            format!(
                "automaton `{automaton}` is not an automation id: an equipment station \
                 is operated by an automaton, spelled `automation:<slug>` as \
                 `ActorId::Automation` writes it"
            ),
        )),
        Some(_) => {}
    }
    match spec.bays {
        None => errs.push(err(
            spec,
            "bays",
            "an `equipment` station declares its bays — how many packets it \
             serves at once; none is declared"
                .to_string(),
        )),
        Some(bays) if bays < 1 => errs.push(err(
            spec,
            "bays",
            format!(
                "an `equipment` station has at least one bay; {bays} would hold \
                 every packet in its queue forever"
            ),
        )),
        Some(_) => {}
    }
}

/// Whether `id` names an automation: it parses as `ActorId::Automation`
/// with a non-empty slug, and parses back to the same spelling.
pub(crate) fn is_automation_id(id: &str) -> bool {
    use boss_core::actor::ActorId;
    let Ok(parsed) = id.parse::<ActorId>();
    matches!(&parsed, ActorId::Automation(slug) if !slug.is_empty()) && parsed.to_string() == id
}

/// The tree fields a row can be judged on alone (T2 of design 4b27a65a,
/// backlog 3f58c823) — see the module list.
fn check_tree_fields(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    let blank = |s: &str| s.is_empty() || s.trim() != s || s.len() > 128;
    match spec.id.as_deref() {
        Some(id) if blank(id) => errs.push(err(
            spec,
            "id",
            "a station id is nonempty, at most 128 bytes, with no surrounding whitespace",
        )),
        Some(_) => {
            if spec.mode.is_none() {
                errs.push(err(
                    spec,
                    "mode",
                    "a tree row declares its mode: `standing` or `ad-hoc`",
                ));
            }
        }
        None => {
            let placed: Vec<&str> = [
                ("parent", spec.parent.is_some()),
                ("mode", spec.mode.is_some()),
                ("capacity", spec.capacity.is_some()),
                ("machines", !spec.machines.is_empty()),
                ("allocation", !spec.allocation.is_empty()),
            ]
            .into_iter()
            .filter_map(|(field, set)| set.then_some(field))
            .collect();
            for field in placed {
                errs.push(err(
                    spec,
                    field,
                    format!(
                        "declares `{field}` but no `id`; a row with no id stands outside the \
                         station tree, so the field places nothing"
                    ),
                ));
            }
        }
    }
    if let Some(parent) = spec.parent.as_deref() {
        if blank(parent) {
            errs.push(err(
                spec,
                "parent",
                "a parent names a station id; this one is blank",
            ));
        } else if spec.id.as_deref() == Some(parent) {
            errs.push(err(
                spec,
                "parent",
                "names its own id as its parent — a one-element cycle",
            ));
        }
    }
    if let Some(capacity) = spec.capacity {
        if capacity < 1 {
            errs.push(err(
                spec,
                "capacity",
                format!("declares capacity {capacity}; a station seats at least one claim"),
            ));
        }
        if let Some(bays) = spec.bays
            && bays != capacity
        {
            errs.push(err(
                spec,
                "capacity",
                format!(
                    "declares capacity {capacity} and bays {bays}; on equipment they are one \
                     number until `bays` retires"
                ),
            ));
        }
    }
    for (at, machine) in spec.machines.iter().enumerate() {
        if !is_automation_id(machine) {
            errs.push(err(
                spec,
                "machines",
                format!("`{machine}` is not an automation id (`automation:<slug>`)"),
            ));
        } else if spec.machines[..at].contains(machine) {
            errs.push(err(
                spec,
                "machines",
                format!("names machine `{machine}` more than once"),
            ));
        }
    }
    for pool in spec.allocation.keys() {
        if blank(pool) {
            errs.push(err(
                spec,
                "allocation",
                "an allocation names a pool; this one is blank",
            ));
        }
    }
}

/// A placement the row alone proves cannot be drawn (backlog e3a927c3):
/// an empty box, a box past its parent floor's edge, or a box on a row
/// with no parent, which has no floor to stand on (the root, or a row
/// outside the tree). Whether it overlaps a sibling needs the other rows
/// and is [`crate::stations::validate_station_placements`]'s question,
/// which the publish door and the seed ask beside this lint.
fn check_placement(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    use crate::stations::{FLOOR_HEIGHT, FLOOR_WIDTH};
    let Some(p) = spec.placement else {
        return;
    };
    if spec.parent.is_none() {
        errs.push(err(
            spec,
            "placement",
            "a placement is a place on the parent's floor, and this row names no parent",
        ));
    }
    if p.w == 0 || p.h == 0 {
        errs.push(err(
            spec,
            "placement",
            format!(
                "a {}x{} box is empty; a placed station has width and height",
                p.w, p.h
            ),
        ));
    } else if !p.inside_the_floor() {
        errs.push(err(
            spec,
            "placement",
            format!(
                "the box at ({}, {}) sized {}x{} leaves the parent's floor, which is \
                 {FLOOR_WIDTH}x{FLOOR_HEIGHT} units",
                p.x, p.y, p.w, p.h
            ),
        ));
    }
}

/// An edge named twice — one route written twice (Codex R1 P2 on car
/// 3eb6ecca, backlog 1bb8c479). Provable from the row alone, so it lives
/// here, where the seed's bundle load, both adapters' publish and the dry
/// run all ask; whether a target is a declared station needs the other
/// rows and is [`crate::stations::validate_station_edges`]'s question,
/// which the publish door asks beside this lint
/// ([`crate::stations::gate_publish`]).
fn check_edges(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    let repeated: std::collections::BTreeSet<&str> = spec
        .edges
        .iter()
        .enumerate()
        .filter(|(at, to)| spec.edges[..*at].contains(to))
        .map(|(_, to)| to.as_str())
        .collect();
    errs.extend(repeated.into_iter().map(|to| {
        err(
            spec,
            "edges",
            format!("declares the edge to `{to}` more than once; a route is declared once"),
        )
    }));
}

/// A station that rolls up into itself.
fn check_rollup(spec: &StationSpec, errs: &mut Vec<StationLintError>) {
    if spec.rollup_parent.as_deref() == Some(spec.name.as_str()) {
        errs.push(err(
            spec,
            "rollup_parent",
            "a station cannot roll up into itself; a renderer walking parents \
             does not terminate"
                .to_string(),
        ));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_queue::StepMatch;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
    }

    /// A viable generic station: the review queues' shape.
    fn viable() -> StationSpec {
        StationSpec::draft(
            "design-review",
            "Design review",
            StationKind::Group,
            StationPredicate {
                kind: Some("design-doc".into()),
                status: Some(JobStatus::Open),
                ..Default::default()
            },
            now(),
        )
    }

    /// A viable equipment station: the dock as it ships.
    fn equipment() -> StationSpec {
        let mut spec = StationSpec::draft(
            "loading-dock",
            "Loading dock",
            StationKind::Equipment,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                status: Some(JobStatus::Open),
                ..Default::default()
            },
            now(),
        );
        spec.automaton = Some("automation:train-conductor".into());
        spec.bays = Some(1);
        spec
    }

    fn reasons(errs: &[StationLintError]) -> Vec<&str> {
        errs.iter().map(|e| e.field.as_str()).collect()
    }

    // -- equipment: an automaton and bays, and only there (M2) ----------

    #[test]
    fn an_equipment_station_that_names_its_automaton_and_bays_is_viable() {
        assert_eq!(validate_station(&equipment()), Vec::new());
        assert!(gate_active(&equipment()).is_ok());
    }

    #[test]
    fn an_equipment_station_must_name_its_automaton_and_its_bays() {
        let mut spec = equipment();
        spec.automaton = None;
        spec.bays = None;
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["automaton", "bays"], "{errs:?}");
        assert!(errs[0].reason.contains("automaton"), "{}", errs[0].reason);
        assert!(errs[1].reason.contains("bays"), "{}", errs[1].reason);
    }

    /// The automaton is an AUTOMATON: an id that parses as
    /// `ActorId::Automation`, spelled the way it parses back, with a
    /// slug. A person, a registered agent, an LLM session and the bare
    /// `system` alias (which parses as `automation:platform` but is not
    /// that spelling) are each refused, naming the half that failed.
    #[test]
    fn an_equipment_automaton_must_be_an_automation_id() {
        for refused in [
            "emp-david",
            "agent-claude",
            "claude:opus-5",
            "system",
            "automation:",
            "",
        ] {
            let mut spec = equipment();
            spec.automaton = Some(refused.into());
            let errs = validate_station(&spec);
            assert_eq!(reasons(&errs), vec!["automaton"], "{refused}: {errs:?}");
            assert!(
                errs[0].reason.contains("automation:"),
                "{refused}: the refusal names the spelling an automaton takes: {}",
                errs[0].reason
            );
        }
        let mut spec = equipment();
        spec.automaton = Some("automation:gate-runner".into());
        assert_eq!(validate_station(&spec), Vec::new());
    }

    /// The record's `batch` spelling reads, and no publish accepts it.
    #[test]
    fn the_deleted_batch_kind_is_refused_at_publish() {
        let mut spec = viable();
        spec.kind = StationKind::RetiredBatch;
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["kind"], "{errs:?}");
        assert!(errs[0].reason.contains("batch"), "{}", errs[0].reason);
        assert!(gate_active(&spec).is_err());
    }

    #[test]
    fn an_equipment_station_has_at_least_one_bay() {
        for bays in [0, -1] {
            let mut spec = equipment();
            spec.bays = Some(bays);
            assert_eq!(reasons(&validate_station(&spec)), vec!["bays"], "{bays}");
        }
    }

    /// The other direction: a generic station that names an automaton
    /// or declares bays is claiming to be machinery while its kind says
    /// it is not, and no reader would honour either field there.
    #[test]
    fn a_generic_station_declares_no_automaton_and_no_bays() {
        for kind in [StationKind::Group, StationKind::Constraint] {
            let mut spec = viable();
            spec.kind = kind;
            spec.automaton = Some("automation:gate-runner".into());
            spec.bays = Some(2);
            let errs = validate_station(&spec);
            assert_eq!(reasons(&errs), vec!["automaton", "bays"], "{kind:?}");
            assert!(
                errs[0].reason.contains(kind.as_str()),
                "the refusal names the kind: {}",
                errs[0].reason
            );
        }
    }

    #[test]
    fn the_platform_seed_shape_is_viable() {
        assert_eq!(validate_station(&viable()), Vec::new());
        assert!(gate_active(&viable()).is_ok());
    }

    #[test]
    fn a_key_required_present_and_absent_can_never_match() {
        let mut spec = viable();
        spec.predicate.metadata_present = vec!["branch".into(), "train".into()];
        spec.predicate.metadata_absent = vec!["train".into()];
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate.metadata_present"]);
        assert!(errs[0].reason.contains("`train`"), "{}", errs[0].reason);
        // `branch` is present-only and must not be reported.
        assert!(!errs[0].reason.contains("branch"), "{}", errs[0].reason);
    }

    /// The step clause's contradiction, and the one case that is NOT
    /// one: `hold: ""` is a RELEASED hold, which `metadata_unmarked`
    /// reads as no marker, so demanding both is satisfiable and must not
    /// be refused. A lint that refused it would block the row this car
    /// exists to seed.
    #[test]
    fn a_step_key_required_unmarked_and_marked_can_never_match() {
        let mut spec = viable();
        spec.predicate.step = Some(StepMatch {
            slug: Some("review".into()),
            metadata_unmarked: vec!["hold".into()],
            metadata_equals: BTreeMap::from([("hold".to_string(), "a reason".to_string())]),
            ..Default::default()
        });
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate.step.metadata_unmarked"]);
        assert!(errs[0].reason.contains("`hold`"), "{}", errs[0].reason);

        // A released hold is no marker, so this pair is satisfiable.
        spec.predicate.step.as_mut().unwrap().metadata_equals =
            BTreeMap::from([("hold".to_string(), String::new())]);
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn a_terminal_status_without_a_window_is_an_always_empty_queue() {
        let mut spec = viable();
        spec.predicate.status = Some(JobStatus::Closed);
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate.status"]);
        assert!(errs[0].reason.contains("terminal_window_days"));
    }

    #[test]
    fn a_terminal_status_with_a_window_is_exactly_what_the_window_is_for() {
        let mut spec = viable();
        spec.predicate.status = Some(JobStatus::Closed);
        spec.terminal_window_days = Some(7);
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn a_window_on_a_non_terminal_predicate_is_dead_configuration() {
        let mut spec = viable();
        spec.terminal_window_days = Some(7); // predicate pins status: open
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["terminal_window_days"]);
    }

    #[test]
    fn a_window_with_no_status_clause_is_fine() {
        // No status clause = the universe opens up and the window
        // narrows it back down. That is the documented pairing.
        let mut spec = viable();
        spec.predicate.status = None;
        spec.terminal_window_days = Some(7);
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn an_actor_station_that_never_binds_self_is_not_personal_at_all() {
        let mut spec = viable();
        spec.kind = StationKind::Actor;
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate"]);
        assert!(errs[0].reason.contains("@me"));
    }

    #[test]
    fn a_shared_station_that_binds_self_varies_per_viewer() {
        let mut spec = viable();
        spec.predicate.metadata_equals =
            BTreeMap::from([("submitted_by".into(), SELF.to_string())]);
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["kind"]);
        assert!(errs[0].reason.contains("group"), "{}", errs[0].reason);
    }

    #[test]
    fn self_binding_through_the_step_clause_counts_the_same() {
        // binds_self() reads the step assignee too; the lint must not
        // have its own narrower idea of what "personal" means.
        let mut spec = viable();
        spec.kind = StationKind::Actor;
        spec.predicate.step = Some(StepMatch {
            assignee_id: Some(SELF.to_string()),
            ..Default::default()
        });
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn the_real_watchlist_row_is_viable() {
        // my-watchlist as actually declared: an actor station bound
        // through metadata_equals.
        let spec = StationSpec::draft(
            "my-watchlist",
            "My watchlist",
            StationKind::Actor,
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
                ..Default::default()
            },
            now(),
        );
        assert_eq!(validate_station(&spec), Vec::new());
    }

    #[test]
    fn a_station_cannot_roll_up_into_itself() {
        let mut spec = viable();
        spec.rollup_parent = Some(spec.name.clone());
        assert_eq!(reasons(&validate_station(&spec)), vec!["rollup_parent"]);

        let mut ok = viable();
        ok.rollup_parent = Some("engineering".into());
        assert_eq!(validate_station(&ok), Vec::new());
    }

    /// Codex R1 P2 on car 3eb6ecca (backlog 1bb8c479): an edge named
    /// twice is one route written twice. The floor's reader refused such
    /// a row and with it the whole department floor, so the registry
    /// refuses it first — naming the station and the repeated target.
    #[test]
    fn a_station_naming_one_edge_twice_is_refused_naming_the_target() {
        let mut spec = viable();
        spec.edges = vec![
            "loading-dock".into(),
            "repair".into(),
            "loading-dock".into(),
        ];
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["edges"]);
        assert_eq!(errs[0].station, "design-review");
        assert!(
            errs[0].reason.contains("`loading-dock`") && !errs[0].reason.contains("repair"),
            "the refusal names the repeated target and only it: {}",
            errs[0].reason
        );
        assert!(gate_active(&spec).is_err());

        let mut ok = viable();
        ok.edges = vec!["loading-dock".into(), "repair".into()];
        assert_eq!(validate_station(&ok), Vec::new());
    }

    // -- the station tree's row rules (T2 of design 4b27a65a) -----------

    fn tree_row() -> StationSpec {
        let mut spec = equipment();
        spec.id = Some("2d0f4c1e-0000-4000-8000-000000000001".into());
        spec.parent = Some("2d0f4c1e-0000-4000-8000-000000000000".into());
        spec.mode = Some(crate::stations::StationMode::Standing);
        spec.capacity = Some(1);
        spec.machines = vec!["automation:train-conductor".into()];
        spec.allocation
            .insert("host:w-1".into(), crate::resources::Quantity::from(1_u64));
        spec
    }

    #[test]
    fn a_tree_row_declaring_every_field_is_viable() {
        assert_eq!(validate_station(&tree_row()), Vec::new());
    }

    #[test]
    fn a_tree_field_on_a_row_with_no_id_is_refused_naming_the_field() {
        let mut spec = tree_row();
        spec.id = None;
        let errs = validate_station(&spec);
        let mut fields = reasons(&errs);
        fields.sort();
        assert_eq!(
            fields,
            vec!["allocation", "capacity", "machines", "mode", "parent"]
        );
        // No tree field and no id: outside the tree, and viable.
        assert_eq!(validate_station(&equipment()), Vec::new());
    }

    #[test]
    fn a_tree_row_that_cannot_stand_in_the_tree_is_refused() {
        let cases: Vec<(&str, Box<dyn Fn(&mut StationSpec)>)> = vec![
            ("id", Box::new(|s| s.id = Some(" padded".into()))),
            ("mode", Box::new(|s| s.mode = None)),
            ("parent", Box::new(|s| s.parent = Some(String::new()))),
            ("parent", Box::new(|s| s.parent = s.id.clone())),
            (
                "capacity",
                Box::new(|s| {
                    s.capacity = Some(0);
                    s.bays = Some(0);
                }),
            ),
            ("capacity", Box::new(|s| s.capacity = Some(2))),
            (
                "machines",
                Box::new(|s| s.machines = vec!["gate-runner".into()]),
            ),
            (
                "machines",
                Box::new(|s| s.machines = vec!["automation:a".into(), "automation:a".into()]),
            ),
        ];
        for (field, edit) in cases {
            let mut spec = tree_row();
            edit(&mut spec);
            let errs = validate_station(&spec);
            assert!(
                reasons(&errs).contains(&field),
                "expected a refusal on `{field}`: {errs:?}"
            );
            assert!(gate_active(&spec).is_err());
        }
    }

    /// Backlog e3a927c3: a placement is a box on the parent's floor. A row
    /// with none is viable (the view lays it out on its fallback grid); a
    /// box filling the floor exactly is on it; an empty box, one past
    /// either edge (including one whose far edge wraps a u32), and one on a
    /// row with no parent floor to stand on are refused, naming the field.
    #[test]
    fn a_placement_is_a_nonempty_box_on_its_parents_floor() {
        use crate::stations::{FLOOR_HEIGHT, FLOOR_WIDTH, StationPlacement};
        let placed = |x, y, w, h| {
            let mut spec = tree_row();
            spec.placement = Some(StationPlacement { x, y, w, h });
            spec
        };
        assert_eq!(validate_station(&tree_row()), Vec::new(), "no placement");
        assert_eq!(validate_station(&placed(590, 120, 230, 170)), Vec::new());
        assert_eq!(
            validate_station(&placed(0, 0, FLOOR_WIDTH, FLOOR_HEIGHT)),
            Vec::new(),
            "the whole floor is on the floor"
        );
        for (x, y, w, h) in [
            (10, 10, 0, 50),
            (10, 10, 50, 0),
            (FLOOR_WIDTH - 10, 0, 11, 10),
            (0, FLOOR_HEIGHT - 10, 10, 11),
            (u32::MAX, 0, 2, 10),
        ] {
            let spec = placed(x, y, w, h);
            let errs = validate_station(&spec);
            assert_eq!(
                reasons(&errs),
                vec!["placement"],
                "{x},{y} {w}x{h}: {errs:?}"
            );
            assert!(gate_active(&spec).is_err());
        }
        let mut root = placed(10, 10, 50, 50);
        root.parent = None;
        let errs = validate_station(&root);
        assert_eq!(reasons(&errs), vec!["placement"], "{errs:?}");
        assert!(errs[0].reason.contains("parent"), "{errs:?}");
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut spec = viable();
        spec.kind = StationKind::Actor; // no @me
        spec.rollup_parent = Some(spec.name.clone());
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["predicate", "rollup_parent"]);
        assert!(gate_active(&spec).is_err());
    }

    #[test]
    fn the_wire_shape_carries_field_reason_and_rendered_message() {
        let mut spec = viable();
        spec.rollup_parent = Some(spec.name.clone());
        let errs = validate_station(&spec);
        let json = problems_json(&errs);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["field"], "rollup_parent");
        assert_eq!(
            json[0]["message"].as_str().unwrap(),
            format!("[design-review] `rollup_parent`: {}", errs[0].reason)
        );
    }

    #[test]
    fn validate_all_reports_across_every_spec() {
        let mut bad = viable();
        bad.name = "broken".into();
        bad.rollup_parent = Some(bad.name.clone());
        let errs = validate_all(&[viable(), bad]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].station, "broken");
    }

    /// A `q.<role>.<kind>` name shadows the projection (authored wins),
    /// so an authored row there without the covering step clause is a
    /// constraint queue that exists in name only — steps for that pair
    /// reach no queue while the list still shows the name (a6306207).
    #[test]
    fn squatting_on_the_derived_namespace_without_covering_it_is_refused() {
        let mut spec = viable();
        spec.name = "q.bookkeeper.bill-approval".into();
        // viable()'s predicate matches ship-a-change jobs — nothing
        // about bookkeeper bill-approval steps.
        let errs = validate_station(&spec);
        assert_eq!(reasons(&errs), vec!["name"]);
        assert!(errs[0].reason.contains("shadows") || errs[0].reason.contains("SHADOWS"));
        assert!(
            errs[0].reason.contains("bill-approval"),
            "{}",
            errs[0].reason
        );
        assert!(errs[0].reason.contains("bookkeeper"), "{}", errs[0].reason);
    }

    /// ...and WITH the covering clause it is a legitimate override —
    /// the documented reason authored-wins exists at all (an operator
    /// wanting a custom discipline on a derived queue).
    #[test]
    fn a_deliberate_override_carrying_the_covering_clause_is_viable() {
        let mut spec = viable();
        spec.name = "q.bookkeeper.bill-approval".into();
        spec.predicate.step = Some(StepMatch {
            kind: Some("bill-approval".into()),
            metadata_equals: BTreeMap::from([("authority_role".into(), "bookkeeper".into())]),
            ..Default::default()
        });
        assert_eq!(validate_station(&spec), Vec::new());
    }

    /// A `q.`-prefixed name that is not the two-segment derived shape is
    /// not the namespace this guards.
    #[test]
    fn a_q_name_that_is_not_the_derived_shape_is_left_alone() {
        let mut spec = viable();
        spec.name = "q.holding".into();
        assert_eq!(validate_station(&spec), Vec::new());
    }

    /// THE EQUALITY TEST (§9a): every station the projection itself
    /// emits must pass this lint — the guard describes the projection's
    /// own shape, and if the two drift this catches which.
    #[test]
    fn every_projected_station_passes_its_own_lint() {
        use crate::registry::seedable_platform_workflows;
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap();
        let derived =
            crate::station_projection::derived_stations(&seedable_platform_workflows(), &[], now);
        assert!(!derived.is_empty(), "expected derived stations to test");
        let errs = validate_all(&derived);
        assert_eq!(errs, Vec::new(), "a projected station failed the lint");
    }
}
