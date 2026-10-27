use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::{human_only, human_presentation::HumanPresentation, registry, seed_loader};

#[test]
fn human_presentations_survive_real_bundle_loading_and_materialization() {
    let workflows = seed_loader::load_workflows(registry::platform_bundle_path()).unwrap();
    let mut count = 0;
    for workflow in workflows {
        let steps = registry::materialize_steps(
            &workflow,
            &Subject::new("custom", "presentation-fixture"),
            JobId::new(),
            &serde_json::json!({}),
            StepId::new,
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
    assert_eq!(count, 19);
}
