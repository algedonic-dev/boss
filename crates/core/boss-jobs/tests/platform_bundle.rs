//! The platform Workflow bundle says exactly what the code says.
//!
//! This is protocols-as-data Q4 made mechanical. David's answer: "moving
//! `user-feedback` v10 from code to bundle must produce a row identical
//! to the live v10 — not v11. If the loader publishes a new version
//! instead of recognising the existing one, every in-flight packet keeps
//! its old spec and the board grows a second lineage."
//!
//! The comparison has to run in this direction. `WorkflowSpec` does NOT
//! serialize to TOML — TOML has no null, so the first `None` field fails
//! with `UnsupportedType(unit)` — so the bundle cannot be generated from
//! the code and diffed. It is authored, and this test is what makes that
//! safe: it parses the bundle with the same reader the tenant bundles
//! use and asserts each row equals the spec it is replacing, field for
//! field, including every step.

use boss_jobs::registry::{WorkflowSpec, platform_workflows};
use boss_jobs::seed_loader::load_workflows;

const BUNDLE: &str = "../../../infra/platform/workflows.toml";

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(BUNDLE).expect("the platform bundle parses")
}

#[test]
fn platform_bundle_matches_the_shipped_spec() {
    let coded: Vec<WorkflowSpec> = platform_workflows();
    let mut checked = 0;
    for row in bundle() {
        let want = coded
            .iter()
            .find(|w| w.kind == row.kind)
            .unwrap_or_else(|| {
                panic!(
                    "bundle carries `{}`, which no shipped spec matches",
                    row.kind
                )
            });

        // Compared field by field rather than with one assert_eq, so a
        // mismatch names the field instead of printing two large specs
        // and leaving the reader to diff them by eye.
        assert_eq!(row.label, want.label, "{}: label", row.kind);
        assert_eq!(row.category, want.category, "{}: category", row.kind);
        assert_eq!(
            row.subject_kinds, want.subject_kinds,
            "{}: subject_kinds",
            row.kind
        );
        assert_eq!(
            row.owning_team, want.owning_team,
            "{}: owning_team",
            row.kind
        );
        assert_eq!(
            row.description, want.description,
            "{}: description",
            row.kind
        );
        assert_eq!(row.metadata, want.metadata, "{}: metadata", row.kind);
        assert_eq!(
            row.metadata_schema, want.metadata_schema,
            "{}: metadata_schema",
            row.kind
        );
        assert_eq!(
            row.entitlements, want.entitlements,
            "{}: entitlements",
            row.kind
        );
        assert_eq!(
            row.on_complete_create, want.on_complete_create,
            "{}: on_complete_create",
            row.kind
        );

        assert_eq!(
            row.steps.len(),
            want.steps.len(),
            "{}: step count — a missing step is a branch the protocol loses",
            row.kind
        );
        for (got, exp) in row.steps.iter().zip(want.steps.iter()) {
            let at = format!("{}/{}", row.kind, exp.title);
            assert_eq!(
                got.title, exp.title,
                "{at}: title (step order matters — it is the DAG's reading order)"
            );
            assert_eq!(got.kind, exp.kind, "{at}: kind");
            assert_eq!(
                got.ready_when, exp.ready_when,
                "{at}: ready_when — this IS the DAG edge"
            );
            assert_eq!(
                got.title_template, exp.title_template,
                "{at}: title_template"
            );
            assert_eq!(
                got.authority_role, exp.authority_role,
                "{at}: authority_role — the human gate"
            );
            assert_eq!(
                got.sign_offs_required, exp.sign_offs_required,
                "{at}: sign_offs_required"
            );
            assert_eq!(
                got.fields, exp.fields,
                "{at}: fields — required-at-done contract"
            );
            assert_eq!(
                got.metadata_defaults, exp.metadata_defaults,
                "{at}: metadata_defaults"
            );
            assert_eq!(got.terminal, exp.terminal, "{at}: terminal");
        }
        checked += 1;
    }
    assert!(checked > 0, "the bundle is empty — nothing was proven");
}

/// Every row in the bundle passes the same viability gate a publish
/// runs, so a malformed bundle fails here rather than at boot on the
/// deployment that loaded it.
#[test]
fn every_bundled_workflow_is_viable() {
    let reg = boss_jobs::step_registry::StepRegistry::v1();
    for row in bundle() {
        let problems = boss_jobs::workflow_lint::validate_workflow(&row, &reg);
        assert!(
            problems.is_empty(),
            "{} is not viable: {problems:?}",
            row.kind
        );
    }
}
