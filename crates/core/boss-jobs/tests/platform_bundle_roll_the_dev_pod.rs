//! `infra/platform/workflows/roll-the-dev-pod.toml` — the protocol for a
//! dev pod roll and for BOSS bringing the agent sessions back up by
//! itself afterwards (backlog e505b3dc, David 2026-10-29; the design is
//! the packet's `decision` key). One pin file per kind file (see
//! `platform_bundle.rs`), so this pin touches no shared line.
//!
//! What is pinned is what the decision fixed: the order, the two paths
//! (a planned roll quiesces first; an emergency one — the pod already
//! gone — starts at `rolled`), the evidence each step holds as FIELDS
//! rather than prose, and who does each step: the conductor
//! (`agent-claude`), the factory (`agent-codex`), and never David.

use boss_core::job::{FilledBy, JobId, StepId, StepStatus, Subject};
use boss_jobs::audience::Audience;
use boss_jobs::registry::{
    StepSpec, WorkflowSpec, materialize_steps, materialize_steps_at, missing_filer_fields,
    platform_bundle_path,
};
use boss_jobs::seed_loader::load_workflows;

const KIND: &str = "roll-the-dev-pod";
const CONDUCTOR: &str = "agent-claude";
const FACTORY: &str = "agent-codex";

fn roll() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == KIND)
        .expect("roll-the-dev-pod ships in the platform bundle")
}

fn step<'a>(wf: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    wf.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("{KIND} has a `{title}` step"))
}

/// Admit a packet the way the jobs API does — with the step registry,
/// so the trigger fires at open — and read back each step's status
/// after the open-time readiness pass.
fn admitted(job_metadata: serde_json::Value) -> Vec<(String, StepStatus)> {
    let wf = roll();
    let subject = Subject::new(wf.subject_kinds[0].as_str(), "boss-dev");
    let registry = boss_jobs::step_registry::StepRegistry::v1();
    let steps = materialize_steps_at(
        &wf,
        &subject,
        JobId::new(),
        &job_metadata,
        StepId::new,
        None,
        Some(&registry),
    );
    wf.steps
        .iter()
        .zip(steps)
        .map(|(spec, step)| (spec.title.clone(), step.status))
        .collect()
}

fn status(of: &[(String, StepStatus)], title: &str) -> StepStatus {
    of.iter()
        .find(|(t, _)| t == title)
        .unwrap_or_else(|| panic!("`{title}` was materialised"))
        .1
}

fn raise(planned: &str) -> serde_json::Value {
    serde_json::json!({
        "why": "car 4e2ec7d2 edits boss-dev.yaml, and its landing rolls the pod",
        "planned": planned,
        "carrying": "car 4e2ec7d2",
    })
}

/// The decided order. The decision names `quiet` and `resumed` as one
/// step each, done by "each agent session"; a confirmation is evidence
/// only from the session it is about, so each is split into the
/// conductor's half and the factory's half, and the step after waits on
/// both.
#[test]
fn the_roll_runs_in_the_decided_order() {
    let titles: Vec<String> = roll().steps.into_iter().map(|s| s.title).collect();
    assert_eq!(
        titles,
        [
            "raised",
            "quiesce",
            "quiet-conductor",
            "quiet-factory",
            "roll",
            "rolled",
            "conductor-up",
            "factory-up",
            "resumed-conductor",
            "resumed-factory",
            "review",
            "closed",
        ]
    );
    let wf = roll();
    let ready = |t: &str| step(&wf, t).ready_when.clone();
    assert_eq!(
        ready("quiesce"),
        "steps.raised.done AND steps.raised.metadata.planned = \"true\""
    );
    assert_eq!(ready("quiet-conductor"), "steps.quiesce.done");
    assert_eq!(ready("quiet-factory"), "steps.quiesce.done");
    assert_eq!(
        ready("roll"),
        "steps.quiet-conductor.done AND steps.quiet-factory.done"
    );
    assert_eq!(
        ready("rolled"),
        "steps.roll.done OR (steps.raised.done AND steps.raised.metadata.planned = \"false\")"
    );
    assert_eq!(ready("conductor-up"), "steps.rolled.done");
    assert_eq!(ready("factory-up"), "steps.rolled.done");
    assert_eq!(ready("resumed-conductor"), "steps.conductor-up.done");
    assert_eq!(ready("resumed-factory"), "steps.factory-up.done");
    assert_eq!(
        ready("review"),
        "steps.resumed-conductor.done AND steps.resumed-factory.done"
    );
    assert_eq!(ready("closed"), "steps.review.done");
    assert!(
        step(&wf, "closed").terminal.is_some(),
        "closed is the terminal"
    );
}

/// A planned roll asks the sessions to stop before anything rolls:
/// at admission `quiesce` is the only step ready and `rolled` waits on
/// the roll itself.
#[test]
fn a_planned_roll_quiesces_before_it_rolls() {
    let s = admitted(raise("true"));
    assert_eq!(status(&s, "raised"), StepStatus::Completed);
    assert_eq!(status(&s, "quiesce"), StepStatus::Ready);
    for later in [
        "quiet-conductor",
        "quiet-factory",
        "roll",
        "rolled",
        "review",
    ] {
        assert_eq!(status(&s, later), StepStatus::Pending, "`{later}` waits");
    }
}

/// An emergency roll has nothing to quiesce — the pod is already gone —
/// so `quiesce`, both `quiet` halves and `roll` are SKIPPED at admission
/// (the branch not taken, provably) and the packet starts at `rolled`.
#[test]
fn an_emergency_roll_starts_at_rolled() {
    let s = admitted(raise("false"));
    for skipped in ["quiesce", "quiet-conductor", "quiet-factory", "roll"] {
        assert_eq!(
            status(&s, skipped),
            StepStatus::Skipped,
            "`{skipped}` is skipped when the roll was not planned"
        );
    }
    assert_eq!(status(&s, "rolled"), StepStatus::Ready);
    assert_eq!(status(&s, "conductor-up"), StepStatus::Pending);
}

/// The trigger's facts are the filer's, refused at admission when
/// absent: a roll packet with no reason, no path and no carrying car
/// cannot be opened, because `planned` decides which path it runs.
#[test]
fn a_roll_is_refused_at_admission_without_its_facts() {
    let wf = roll();
    let raised = step(&wf, "raised");
    for name in ["why", "planned", "carrying"] {
        let f = raised
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("raised declares `{name}`"));
        assert!(
            f.required && f.filled_by == FilledBy::Filer,
            "raised.{name}"
        );
    }
    assert_eq!(
        raised
            .fields
            .iter()
            .find(|f| f.name == "planned")
            .unwrap()
            .field_type,
        "true|false"
    );
    let subject = Subject::new(wf.subject_kinds[0].as_str(), "boss-dev");
    let steps = materialize_steps(
        &wf,
        &subject,
        JobId::new(),
        &serde_json::json!({}),
        StepId::new,
    );
    let missing = missing_filer_fields(&steps);
    for name in ["why", "planned", "carrying"] {
        assert!(
            missing.contains(&("raised".to_string(), name.to_string())),
            "an empty raise is refused naming raised.{name}: {missing:?}"
        );
    }
}

/// The protocol holds EVIDENCE AS FIELDS, required at done, so a step
/// cannot close on a sentence: the handoff key and the open-runs read
/// before the roll, the act's id, the new pod's name and start time,
/// each session's first write with its time, what it resumed from, and
/// the measured minutes in the review.
#[test]
fn the_evidence_is_required_fields_not_prose() {
    let wf = roll();
    let required = |s: &str, name: &str| -> String {
        let f = step(&wf, s)
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("`{s}` declares `{name}`"));
        assert!(f.required, "`{s}.{name}` is required at done");
        f.field_type.clone()
    };
    assert_eq!(required("quiesce", "sessions_asked"), "array");
    required("quiesce", "asked_via");
    for half in ["quiet-conductor", "quiet-factory"] {
        required(half, "handoff_key");
        required(half, "open_runs");
        assert_eq!(required(half, "read_at"), "date-time");
    }
    assert_eq!(required("roll", "act"), "release|converge");
    required("roll", "act_ref");
    assert_eq!(required("roll", "acted_at"), "date-time");
    required("rolled", "new_pod");
    assert_eq!(required("rolled", "started_at"), "date-time");
    required("rolled", "read_via");
    for up in ["conductor-up", "factory-up"] {
        assert_eq!(required(up, "first_write_at"), "date-time");
        required(up, "first_write_ref");
        assert_eq!(required(up, "brought_up_by"), "boot|agent|person");
    }
    for half in ["resumed-conductor", "resumed-factory"] {
        required(half, "resumed_from");
        required(half, "first_work");
        assert_eq!(required(half, "first_work_at"), "date-time");
    }
    for minutes in [
        "roll_to_conductor_up_minutes",
        "roll_to_factory_up_minutes",
        "roll_to_resumed_minutes",
    ] {
        assert_eq!(required("review", minutes), "number");
    }
    required("review", "measured_from");
    required("review", "what_broke");
    required("review", "person_steps");
    assert_eq!(required("review", "fix_packets"), "array");
}

/// Who does each step. An `individual` audience is claim-for authority
/// and the bundle holds every one to an `automation:` principal
/// (`every_individual_executor_in_the_platform_bundle_is_an_automation`),
/// so no step names `agent-claude` or `agent-codex` as its audience:
/// every step is for the `platform-admin` role both agents hold, and the
/// session that writes it is named in its title and procedure. The
/// conductor's steps carry the analyst block, so no queue reads them as
/// a person's work. The factory's carry NO block, because every block in
/// the bundle names the opus family the effort definitions run
/// (`agent_spec::DEFINITION_MODEL`) and the factory does not run it.
/// No step is a person's: the trial asks whether David is needed.
#[test]
fn the_conductor_and_the_factory_do_every_step_and_david_none() {
    let wf = roll();
    let conductor = [
        "quiesce",
        "quiet-conductor",
        "roll",
        "rolled",
        "conductor-up",
        "resumed-conductor",
        "review",
    ];
    let factory = ["quiet-factory", "factory-up", "resumed-factory"];
    for t in conductor {
        let a = step(&wf, t)
            .agent
            .as_ref()
            .unwrap_or_else(|| panic!("`{t}` declares its agent block"));
        assert_eq!(a.profile, "analyst", "{t}");
        assert_eq!(a.model, "opus-5-5", "{t}");
    }
    for t in factory {
        let s = step(&wf, t);
        assert!(s.agent.is_none(), "`{t}` declares no opus block");
        assert!(
            s.title_template.starts_with(&format!("{FACTORY}: ")),
            "`{t}` names the factory as the session that writes it"
        );
    }
    for s in wf.steps.iter().filter(|s| s.kind == "task") {
        assert_eq!(
            s.audience,
            Some(Audience::Role("platform-admin".into())),
            "`{}` is for the role both agents hold",
            s.title
        );
        assert!(
            !boss_jobs::human_only::declared(&s.metadata_defaults),
            "`{}` must not require a person",
            s.title
        );
    }
    let review = step(&wf, "review");
    let procedure = review.metadata_defaults["procedure"]
        .as_str()
        .expect("review has a procedure");
    assert!(
        procedure.contains(CONDUCTOR) && procedure.contains(FACTORY),
        "the review checks each step was written by the session it names"
    );
}

/// And it is viable: the same gate a publish runs.
#[test]
fn the_roll_protocol_is_viable() {
    let reg = boss_jobs::step_registry::StepRegistry::v1();
    let problems = boss_jobs::workflow_lint::validate_workflow(&roll(), &reg);
    assert!(problems.is_empty(), "{KIND} is not viable: {problems:?}");
}
