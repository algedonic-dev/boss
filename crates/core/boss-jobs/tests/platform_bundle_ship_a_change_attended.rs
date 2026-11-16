//! The appended attended terminal can be adopted explicitly without rewriting proof.
use boss_core::job::StepStatus;

#[test]
fn appending_the_terminal_does_not_strand_an_old_landed_packet() {
    let mut rows = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap();
    let current = rows.remove(0);
    assert_eq!(current.steps.last().unwrap().title, "attended-qualified");
    let mut prior = current.clone();
    prior.steps.pop();
    let at = prior
        .steps
        .iter()
        .map(|step| {
            (
                step.title.clone(),
                if matches!(
                    step.title.as_str(),
                    "opened" | "scope" | "build" | "gate" | "review"
                ) {
                    StepStatus::Completed
                } else if step.title == "proven" {
                    StepStatus::Ready
                } else {
                    StepStatus::Pending
                },
            )
        })
        .collect();
    let result = boss_jobs::protocol_conversion::convertibility_for_packet(&prior, &current, &at);
    assert!(result.is_automatic(), "{result:?}");
    assert_eq!(
        prior.steps.iter().find(|step| step.title == "proven"),
        current.steps.iter().find(|step| step.title == "proven")
    );
    for step in &prior.steps {
        assert_eq!(
            Some(step),
            current
                .steps
                .iter()
                .find(|candidate| candidate.title == step.title),
            "existing protocol step changed: {}",
            step.title
        );
    }
}
