//! Declarative wiring pin: a lighter judgement must govern the actual jobs.
use boss_testing::repo_root;

#[test]
fn the_actual_ci_commands_cannot_run_a_candidates_eligibility_adapter() {
    let output = std::process::Command::new("python3")
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "infra/forge",
            "-p",
            "test_delivery_ci_workflow.py",
            "-v",
        ])
        .current_dir(repo_root())
        .output()
        .expect("start the actual workflow selector controls");
    assert!(
        output.status.success(),
        "workflow selector controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn job<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("\n  {name}:\n");
    let (_, rest) = source
        .split_once(&marker)
        .unwrap_or_else(|| panic!("missing {name}"));
    rest.split("\n  #").next().unwrap_or(rest)
}

#[test]
fn frozen_delivery_selection_controls_image_build_and_validation_together() {
    let source = std::fs::read_to_string(repo_root().join(".forgejo/workflows/ci.yml")).unwrap();
    let planning = job(&source, "delivery-plan");
    assert!(planning.contains("infra/forge/delivery-ci.py"));
    assert!(planning.contains("github.event.pull_request.base.sha"));
    assert!(planning.contains("GITHUB_OUTPUT"));
    let build = job(&source, "build-image");
    assert!(build.contains("needs: [delivery-plan]"));
    assert!(build.contains("if: needs.delivery-plan.outputs.route != 'data'"));
    let light = job(&source, "lighter-validation");
    assert!(light.contains("if: needs.delivery-plan.outputs.route == 'data'"));
    assert!(light.contains("bash infra/gate.sh --quick"));
    assert!(
        light.contains("infra/forge/delivery-ci.py"),
        "the reused runner stamp is checked again"
    );
    for name in ["locomotive", "web"] {
        let heavy = job(&source, name);
        assert!(
            heavy.contains("if: needs.delivery-plan.outputs.route != 'data'"),
            "{name}"
        );
        assert!(
            heavy.contains("boss-ci:${{ github.sha }}"),
            "heavy validation keeps its per-head image"
        );
    }
}

#[test]
fn the_published_image_carries_the_actual_compiler_receipt() {
    let source =
        std::fs::read_to_string(repo_root().join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(source.contains("--message-format=json-render-diagnostics"));
    assert!(source.contains("channels --compiler-inputs"));
    assert!(source.contains("--compiler-artifacts /out"));
    assert!(source.contains("> /out/boss-inputs.json"));
    assert!(
        source.contains("COPY infra/oss-quickstart/Dockerfile ./infra/oss-quickstart/Dockerfile")
    );
    assert!(source.contains("COPY --from=rust-build /out/ /usr/local/bin/"));
}

#[test]
fn channel_completion_precedes_image_effects_and_preserves_independent_agent_publication() {
    let source =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let hold = source.find("if reason=$(converge_held").unwrap();
    let selection = source
        .find("delivery_channel_stage")
        .expect("the actual runner selects its frozen channel");
    let build = source.find("STAGE=\"registry login\"").unwrap();
    assert!(hold < selection && selection < build);
    let light = &source[selection..build];
    assert!(light.contains("agent_image_publication_stage"));
    assert!(
        light.contains("observe_data_surfaces"),
        "image-stamp preservation must not stop ordinary liveness observations"
    );
    assert!(light.contains("delivery_pending="));
    assert!(light.contains("delivery_arrived="));
    assert!(
        !light.contains("$STAMP_FILE"),
        "a data arrival cannot relabel the actual image stamp"
    );
    assert!(!light.contains("roll_deployment"));
    let first_check = source
        .find("    require_cluster_credentials\n")
        .expect("the unchanged observation still requires its existing cluster credential");
    let unchanged = source.find("if [ \"$HEAD\" = \"$LAST\" ]; then").unwrap();
    assert!(unchanged < first_check && first_check < selection);
    let last_check = source.rfind("require_cluster_credentials\n").unwrap();
    assert!(
        selection < last_check && last_check < build,
        "data delivery does not require a cluster credential"
    );
}
