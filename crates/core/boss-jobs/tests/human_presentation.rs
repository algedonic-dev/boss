use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::{human_only, human_presentation::HumanPresentation, registry, seed_loader};

#[test]
fn human_presentations_survive_real_bundle_loading_and_materialization() {
    let workflows = seed_loader::load_workflows(registry::platform_bundle_path()).unwrap();
    let expected = workflows
        .iter()
        .flat_map(|w| &w.steps)
        .filter(|s| human_only::declared(&s.metadata_defaults))
        .count();
    assert!(expected > 0, "the real bundle must include human work");
    let mut count = 0;
    for workflow in workflows {
        let steps = registry::materialize_steps(
            &workflow,
            &Subject::new("custom", "presentation-fixture"),
            JobId::new(),
            &serde_json::json!({}),
            StepId::new,
        );
        assert_eq!(
            steps.len(),
            workflow.steps.len(),
            "{} materialization",
            workflow.kind
        );
        for (spec, step) in workflow.steps.iter().zip(steps) {
            if human_only::declared(&spec.metadata_defaults) {
                count += 1;
                let declared = HumanPresentation::read(&spec.metadata_defaults).unwrap();
                let admitted = HumanPresentation::read(&step.metadata).unwrap();
                assert_eq!(declared, admitted, "{}/{}", workflow.kind, spec.title);
            }
        }
    }
    assert_eq!(
        count, expected,
        "every declared human step was materialized and checked"
    );
}
