//! `infra/platform/workflows/rerail-a-car.toml` keeps its decided shape
//! (design b35456ac, answering user-feedback ff4ae3d2). One pin file per
//! kind file (see `platform_bundle.rs`), so a new protocol touches no
//! shared line.
//!
//! The protocol is the repair the machine files for a car the conductor
//! has left behind on a merge conflict three trains running: one builder
//! step that runs `boss rerail`, resolves the hunk keeping both sides'
//! intent, and carries the fresh green onto the car — or refuses, naming
//! the hunk, and a person decides.

use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "rerail-a-car")
        .expect("rerail-a-car ships in the platform bundle")
}

fn step<'a>(w: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    w.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("a `{title}` step"))
}

/// The three answers the one step can give, in the order the design
/// names its terminals.
const RESULTS: [&str; 3] = ["rerailed", "refused", "overtaken"];

/// The work step is a BUILDER's, at the setting the design decided
/// (decision 3): a scoped resolve-and-regate, not a design, so medium
/// effort and $3, with a one-hour duration that gives the silence rule a
/// two-hour bound over the 30-40 minutes each repair took by hand.
#[test]
fn the_rerail_step_is_a_builders_at_the_decided_setting() {
    let w = bundled();
    assert_eq!(
        w.subject_kinds,
        vec!["custom".to_string()],
        "the car's subject"
    );
    let rerail = step(&w, "rerail");
    assert_eq!(rerail.kind, "task");
    let agent = rerail
        .agent
        .as_ref()
        .expect("the rerail step declares its agent");
    assert_eq!(agent.profile, "builder", "it ships a car");
    assert_eq!(agent.model, "opus-5[1m]");
    assert_eq!(agent.budget_usd, 3.0);
    assert_eq!(agent.effort.as_str(), "medium");
    assert_eq!(rerail.duration_hours, Some(1.0));
    assert_eq!(rerail.authority_role.as_deref(), Some("platform-admin"));
}

/// The procedure is the verb's own sequence, so a builder handed the
/// step runs the door rather than rebuilding the path by hand.
#[test]
fn the_procedure_runs_the_rerail_door_and_carries_the_green_onto_the_car() {
    let w = bundled();
    let procedure = step(&w, "rerail")
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("the rerail step carries its runbook as `procedure`");
    for needle in [
        "boss rerail",
        "--finish",
        "BOSS_AGENT_RUN",
        "infra/gate.sh --lint",
        "refused",
    ] {
        assert!(
            procedure.contains(needle),
            "the procedure names `{needle}`: {procedure}"
        );
    }
}

/// `result` is an ENUM, so the fork over it is total: every value a
/// builder or the close rule can write reaches exactly one terminal, and
/// the answer is required alongside the evidence behind it.
#[test]
fn every_result_reaches_its_own_terminal() {
    let w = bundled();
    let rerail = step(&w, "rerail");
    let result = rerail
        .fields
        .iter()
        .find(|f| f.name == "result")
        .expect("a result field");
    assert!(result.required);
    assert_eq!(result.field_type, RESULTS.join("|"));
    let evidence = rerail
        .fields
        .iter()
        .find(|f| f.name == "evidence")
        .expect("an evidence field");
    assert!(evidence.required, "no evidence is not a pass");

    for r in RESULTS {
        let terminal = step(&w, r);
        assert_eq!(terminal.kind, "outcome");
        assert_eq!(
            terminal.terminal.as_ref().map(|t| t.outcome.as_str()),
            Some(r)
        );
        assert!(
            terminal
                .ready_when
                .contains(&format!("steps.rerail.metadata.result = \"{r}\"")),
            "`{r}` is reached by its own answer: {}",
            terminal.ready_when
        );
    }
}

/// The two rules that file and close the repair name this protocol's
/// kind, step and answers — read from their files, so a renamed step or
/// value reds here rather than dead-lettering in production.
#[test]
fn the_rules_that_file_and_close_it_name_this_protocol() {
    let dir = boss_testing::dispatcher_rules_dir();
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(format!("{name}.toml")))
            .unwrap_or_else(|e| panic!("read rule {name}: {e}"))
    };
    let trigger = read("rerail-a-car-left-behind-on-a-conflict");
    assert!(
        trigger.contains(r#"kind = "\"rerail-a-car\"""#),
        "{trigger}"
    );
    let close = read("rerail-a-car-is-overtaken-when-its-car-closes");
    assert!(close.contains(r#"kind = "\"rerail-a-car\"""#), "{close}");
    assert!(close.contains(r#"step = "\"rerail\"""#), "{close}");
    assert!(close.contains(r#"\"result\": \"overtaken\""#), "{close}");
    assert!(
        trigger.contains(r#""metadata.car" = "id""#)
            && close.contains(r#"match_field = "\"car\"""#),
        "the key the trigger files the car under is the key the close matches on"
    );
}
