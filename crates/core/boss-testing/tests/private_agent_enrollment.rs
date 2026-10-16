//! Inert enrollment contracts: the private broker owns provider bytes and the
//! ordinary StepPlugin has neither a credential input nor a completion door.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_private_boundary_and_human_surface_pass_their_fake_controls() {
    let root = repo_root();
    for (program, arguments) in [
        ("python3", vec!["infra/agent/private_enrollment_test.py"]),
        (
            "bun",
            vec!["test", "infra/agent/private_enrollment_plugin_test.js"],
        ),
    ] {
        let output = Command::new(program)
            .args(arguments)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn enrollment_requires_human_scope_then_distinct_broker_effects() {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("infra/platform/workflows/enroll-a-managed-agent.toml"))
            .unwrap();
    let value: toml::Value = toml::from_str(&source).unwrap();
    let workflow = &value["workflow"][0];
    let steps = workflow["step"].as_array().unwrap();
    assert_eq!(steps[0]["kind"].as_str(), Some("sign-off"));
    assert_eq!(
        steps[0]["metadata_defaults"]["human_only"].as_bool(),
        Some(true)
    );
    assert_eq!(steps[0]["assurance_required"].as_str(), Some("presence"));
    for field in steps[0]["fields"].as_array().unwrap() {
        if matches!(
            field["name"].as_str(),
            Some("credential" | "expected_principal")
        ) {
            assert_eq!(field["filled_by"].as_str(), Some("filer"));
        }
    }
    assert_eq!(steps[1]["kind"].as_str(), Some("private-enrollment"));
    assert_eq!(
        steps[1]["ready_when"].as_str(),
        Some("steps.scope.done AND steps.scope.metadata.decision = \"approved\"")
    );
    assert_eq!(steps[2]["ready_when"].as_str(), Some("steps.enroll.done"));
    assert_eq!(steps[3]["ready_when"].as_str(), Some("steps.install.done"));
    assert_eq!(steps[4]["ready_when"].as_str(), Some("steps.verify.done"));
    for step in &steps[1..4] {
        assert_eq!(
            step["metadata_defaults"]["written_by"].as_str(),
            Some("rule:private-enrollment-broker")
        );
        for field in step["fields"].as_array().unwrap() {
            assert!(matches!(
                field["name"].as_str(),
                Some("broker_receipt_id" | "generation_id")
            ));
        }
    }
}
