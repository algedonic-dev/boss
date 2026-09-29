//! Every platform protocol step declares the fields its KIND requires.
//!
//! Two copies of one fact (CLAUDE.md §9a). The StepType registry
//! (`crates/core/boss-jobs/seeds/step_types.toml`) says which metadata a
//! kind demands at completion, and the completion door enforces it
//! (`validate_metadata`). The Workflow row's step says which fields the
//! step DECLARES, and the declared list is what the step page renders as
//! its form (`apps/web/src/steps/GenericSurface.svelte` reads
//! `step.fields`) and what a brief prints as "required at done". When the
//! row declares less than the kind demands, the person holding the step
//! is shown a form that cannot complete it.
//!
//! Measured 2026-09-28 05:38Z (backlog a14f04b3): rotate-a-credential's
//! `scope` step (kind `credential-rotation`) declared only `old_token`
//! and `old_token_last_eight`, while the kind requires `credential`,
//! `reason`, `locations` and `consumers`. David opened the step and saw
//! two optional boxes; the earlier rotations had been filed by
//! automation with the keys already on the step, so nobody had met the
//! form before.
//!
//! A required kind field counts as provided when the step either
//! declares it or sets it in `metadata_defaults` — a trigger's
//! `trigger_kind`, an outcome's `outcome_kind` are filled at
//! materialisation and never asked of anyone.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;

/// Every `(workflow, step, kind, field)` whose kind requires the field
/// and whose step neither declares it REQUIRED nor defaults it. A
/// declared-but-optional copy is the same defect in a quieter shape:
/// the form offers the box, lets the person leave it empty, and the
/// completion is refused for it.
fn undeclared_required(rows: &[WorkflowSpec], reg: &StepRegistry) -> Vec<String> {
    rows.iter()
        .flat_map(|w| w.steps.iter().map(move |s| (w, s)))
        .flat_map(|(w, s)| {
            reg.get(&s.kind)
                .map(|kind| {
                    kind.fields
                        .iter()
                        .filter(|f| f.required)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .filter(|f| {
                    !s.fields.iter().any(|d| d.name == f.name && d.required)
                        && s.metadata_defaults.get(f.name).is_none()
                })
                .map(move |f| format!("{} / {} (kind {}): {}", w.kind, s.title, s.kind, f.name))
        })
        .collect()
}

/// The rule is not vacuous: a step of a kind with required fields that
/// declares none of them is named, field by field, and a step that
/// declares them (or defaults one) is not.
#[test]
fn the_rule_names_each_field_a_step_leaves_out() {
    // Built from the bundled row rather than a TOML fixture: the loader
    // lints a row whole (terminals, reachability), and the shape under
    // test is two variants of one real step, not a viable protocol.
    let rows = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    let mut row = rows
        .into_iter()
        .find(|w| w.kind == "rotate-a-credential")
        .expect("rotate-a-credential ships in the platform bundle");
    let scope = row
        .steps
        .iter()
        .find(|s| s.title == "scope")
        .cloned()
        .expect("a scope step");
    let declared = |name: &str| {
        scope
            .fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("the scope step declares `{name}`"))
    };

    // `bare`: an optional copy of one required field and nothing else —
    // the 3df2dbd3 shape, one field worse.
    let mut bare = scope.clone();
    bare.title = "bare".into();
    bare.fields = vec![boss_core::job::StepField {
        required: false,
        ..declared("credential")
    }];
    // `whole`: three declared, the fourth supplied by a default.
    let mut whole = scope.clone();
    whole.title = "whole".into();
    whole.fields = vec![
        declared("credential"),
        declared("locations"),
        declared("consumers"),
    ];
    whole.metadata_defaults = serde_json::json!({ "reason": "fixture" });
    row.steps = vec![bare, whole];

    assert_eq!(
        undeclared_required(&[row], &StepRegistry::v1()),
        vec![
            "rotate-a-credential / bare (kind credential-rotation): credential",
            "rotate-a-credential / bare (kind credential-rotation): reason",
            "rotate-a-credential / bare (kind credential-rotation): locations",
            "rotate-a-credential / bare (kind credential-rotation): consumers",
        ],
        "an optional copy counts as missing; the whole step is silent"
    );
}

#[test]
fn every_platform_step_declares_the_fields_its_kind_requires() {
    let rows = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    assert!(!rows.is_empty(), "an empty bundle would prove nothing");
    let missing = undeclared_required(&rows, &StepRegistry::v1());
    assert!(
        missing.is_empty(),
        "a step's form is built from the fields it declares, so a field its kind \
         requires at completion and the step does not declare cannot be supplied by \
         the person holding it — declare each (description copied from the kind in \
         seeds/step_types.toml):\n  {}",
        missing.join("\n  ")
    );
}
