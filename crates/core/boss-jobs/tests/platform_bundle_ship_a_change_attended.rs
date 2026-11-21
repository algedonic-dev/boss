//! The appended attended terminal can be adopted explicitly without rewriting proof.
//!
//! And the `proven` step's procedure names the build the way the estate
//! names it now. The bare-metal `releases/<sha>` store was deleted with
//! the bare-metal deploy on 2026-09-18 (train #443) and boss-gcp's second
//! stack was retired on 2026-09-15, but the procedure kept telling every
//! prover to read `/usr/local/boss/current` and to run `boss prove` on
//! boss-gcp — a recipe that resolves nothing on any host (backlog b6836e0c).
use boss_core::job::StepStatus;

fn ship_a_change() -> boss_jobs::WorkflowSpec {
    boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap()
    .remove(0)
}

#[test]
fn the_proven_step_names_the_build_not_a_retired_releases_store() {
    let spec = ship_a_change();
    let proven = spec
        .steps
        .iter()
        .find(|step| step.title == "proven")
        .expect("ship-a-change declares a proven step");
    let procedure = proven.metadata_defaults["procedure"]
        .as_str()
        .expect("the proven step carries its procedure");
    for retired in [
        "/usr/local/boss/releases",
        "/usr/local/boss/current",
        "releases/ab43e1fd",
        "boss-gcp",
    ] {
        assert!(
            !procedure.contains(retired),
            "the proven step's procedure still names the retired `{retired}`"
        );
    }
    for live in [
        "capabilities.commit",
        "GET /api/jobs/health",
        "boss --version",
        "install-cli-from-image.sh",
    ] {
        assert!(
            procedure.contains(live),
            "the proven step's procedure does not name `{live}`, the way a build names its commit now"
        );
    }
}

#[test]
fn appending_the_terminal_does_not_strand_an_old_landed_packet() {
    let current = ship_a_change();
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
