//! A step that requires presence declares the fields it signs.
//!
//! THE RULE (design 1ce67f7e, added by David on question `completer`,
//! 2026-10-07). A presence stamp binds the step's title and metadata —
//! `step_shape_hash` — and nothing else on the packet; hashing the whole
//! job was rejected, because routine writes elsewhere on it would void
//! approvals and bring the redundant ceremony back. Since that design a
//! step's live presence stamps carry its completion, sent by any actor
//! that may update the step. So everything an approval depends on has to
//! be ON the step being signed, the way ops-request renders its plan and
//! the plan's hash onto `approve` — and the workflow lint refuses a
//! presence step that declares no field but the signer's own decision.
//!
//! WHERE IT RUNS. `workflow_lint::validate_workflow`, phase 10 — so the
//! author-time dry run, every publish and the boot-time quarantine read
//! the one definition. The second pin here is the bundle the tree ships.
//!
//! MEASURED WHEN WRITTEN (2026-10-07): the eight presence steps of the
//! platform bundle all pass, and so do the same eight on the live
//! registry (ops-request v10 approve, publish-to-github v10 approve,
//! break-glass-enrolment v3 authorise, passkey-promotion v2 authorise,
//! prepare-a-deposit-key v1 enroll, prepare-the-machine-token-deposit-key
//! v1 enroll, confirm-recovery-sheet-kept v2 confirm,
//! reprint-recovery-sheet v2 print). No row was edited for this rule.

use boss_core::job::{Assurance, StepField};
use boss_jobs::registry::{StepSpec, WorkflowSpec, seedable_platform_workflows};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::workflow_lint::{SIGNERS_OWN_KEYS, validate_workflow};

fn approval(assurance: Option<Assurance>, fields: &[&str]) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "presence-fixture",
        "Presence fixture",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "approve".into(),
            kind: "sign-off".into(),
            ready_when: "true".into(),
            sign_offs_required: vec!["platform-admin".into()],
            assurance_required: assurance,
            fields: fields
                .iter()
                .map(|name| StepField::new(*name, "string"))
                .collect(),
            ..Default::default()
        }],
    )
}

fn refusals(spec: &WorkflowSpec) -> Vec<String> {
    validate_workflow(spec, &StepRegistry::v1())
        .into_iter()
        .filter(|e| e.reason.contains("declares no field it signs"))
        .map(|e| e.to_string())
        .collect()
}

#[test]
fn a_presence_step_that_declares_nothing_it_signs_is_refused() {
    // No field at all, and only the signer's own answer: both sign a
    // title and a yes.
    for fields in [&[][..], &["decision"][..], &SIGNERS_OWN_KEYS[..]] {
        let said = refusals(&approval(Some(Assurance::Presence), fields));
        assert_eq!(
            said.len(),
            1,
            "fields {fields:?} on a presence step must be refused once: {said:?}"
        );
        assert!(said[0].contains("approve"), "names the step: {said:?}");
    }
}

#[test]
fn a_presence_step_that_declares_a_signed_field_passes() {
    for fields in [&["plan"][..], &["decision", "source_sha"][..]] {
        assert!(
            refusals(&approval(Some(Assurance::Presence), fields)).is_empty(),
            "fields {fields:?} name what is signed"
        );
    }
}

/// THE CONTROL: the rule speaks only to presence. A session sign-off
/// that declares nothing but a decision is every ordinary approval.
#[test]
fn a_session_step_is_not_asked() {
    for assurance in [None, Some(Assurance::Session)] {
        assert!(refusals(&approval(assurance, &["decision"])).is_empty());
    }
}

/// The bundle the tree ships, read the way the sibling pin
/// (a_presence_step_declares_who_must_stamp_it.rs) reads it.
#[test]
fn every_presence_step_in_the_bundle_declares_what_it_signs() {
    let registry = StepRegistry::v1();
    let mut checked = Vec::new();
    let mut offenders = Vec::new();
    for wf in seedable_platform_workflows() {
        for step in &wf.steps {
            if step.assurance_required.unwrap_or_default() <= Assurance::Session {
                continue;
            }
            checked.push(format!("{}.{}", wf.kind, step.title));
        }
        offenders.extend(
            validate_workflow(&wf, &registry)
                .into_iter()
                .filter(|e| e.reason.contains("declares no field it signs"))
                .map(|e| e.to_string()),
        );
    }
    assert!(
        checked.len() >= 8,
        "the bundle held eight presence steps when this was written; found {checked:?} — \
         either the field was renamed or steps were lost, and this pin would otherwise pass \
         while checking nothing"
    );
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// The signer's own keys are spelled twice — here and in the sign-off
/// surface, which leaves them out of the document it draws as signed.
/// Held equal (CLAUDE.md §9a).
#[test]
fn the_signers_own_keys_are_the_sign_off_surfaces() {
    let js =
        std::fs::read_to_string(boss_testing::repo_root().join("infra/step-plugins/sign-off.js"))
            .unwrap();
    let line = js
        .lines()
        .find(|l| l.trim_start().starts_with("const TRIO = ["))
        .expect("sign-off.js declares TRIO on one line");
    let mut theirs: Vec<String> = line
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let mut ours: Vec<String> = SIGNERS_OWN_KEYS.iter().map(|s| s.to_string()).collect();
    theirs.sort();
    ours.sort();
    assert_eq!(ours, theirs, "sign-off.js: {line}");
}
