//! A gate-run whose purpose is gone before it ran has a word for that.
//!
//! THE DEFECT (backlog 8d7d0a2b, with b1c82a82), measured 2026-09-28 on
//! gate-run 8c2f644a. An earlier gate on the same branch had already gone
//! green on the same head and parked car 59e1f436, so the queued run was
//! redundant. Every terminal the protocol offered — green, failed, lost,
//! refused — was reached through `record-verdict`'s verdict, so the only
//! way to close it was to record a verdict that did not happen. It was
//! closed `lost` by the conductor's orphan settle at 00:40:22Z: a
//! deliberate stand-down on the record as a dead runner.
//!
//! `withdrawn` is that word. Reached only before any runner pod exists
//! (the door refuses once one does — `boss gate --withdraw`), so a
//! withdrawn run never judged anything and never began to.

use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;

fn gate_run() -> boss_jobs::registry::WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "gate-run")
        .expect("the gate-run protocol is in the platform bundle")
}

fn verdict_enum() -> String {
    gate_run()
        .steps
        .iter()
        .find(|s| s.title == "record-verdict")
        .expect("record-verdict step")
        .fields
        .iter()
        .find(|f| f.name == "verdict")
        .expect("the verdict field")
        .field_type
        .clone()
}

/// The protocol declares the word, and a terminal claims it — with the
/// `withdrawn` outcome, never `completed` (nothing was proven) and never
/// `failed` (nothing was judged against the branch).
#[test]
fn the_protocol_has_a_withdrawn_terminal_its_verdict_reaches() {
    let ty = verdict_enum();
    assert!(
        ty.split('|').map(str::trim).any(|v| v == "withdrawn"),
        "the verdict enum must carry `withdrawn`: it is {ty:?}"
    );
    let wf = gate_run();
    let t = wf
        .steps
        .iter()
        .find(|s| s.title == "withdrawn")
        .expect("a `withdrawn` terminal step");
    assert_eq!(t.kind, "outcome");
    assert!(
        t.ready_when.contains("verdict = \"withdrawn\""),
        "the terminal is reached by the verdict: {:?}",
        t.ready_when
    );
    assert_eq!(
        t.terminal.as_ref().map(|x| x.outcome.as_str()),
        Some("withdrawn"),
        "a withdrawal is its own outcome, not a green and not a red"
    );
    assert!(
        t.title_template.contains("before it ran") || t.title_template.contains("never ran"),
        "the title says the run never began, so no reader takes it for a dead runner: {:?}",
        t.title_template
    );
}

/// The first validator the step API runs is the StepType's own enum
/// (ff5b9634) — the door's write must pass it.
#[test]
fn a_withdrawn_verdict_passes_the_validator_the_step_api_runs() {
    let md = serde_json::json!({ "verdict": "withdrawn", "receipt": "{}" });
    StepRegistry::v1()
        .validate_metadata("gate-verdict", &md)
        .unwrap_or_else(|e| panic!("the step registry refused a withdrawal: {e:?}"));
}
