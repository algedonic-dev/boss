//! The measurement door supplies no SQL, caller identity, window or target arguments.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_membership_observation_reader_is_fixed_protected_and_lossless() {
    let output = Command::new("python3")
        .args(["-B", "infra/forge/tests/membership-starting-state-test.py"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let descriptor: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/ops/verbs/read-membership-starting-state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(descriptor["hosts"], serde_json::json!(["forge"]));
    assert_eq!(descriptor["params"], serde_json::json!([]));
    assert_eq!(
        descriptor["argv"],
        serde_json::json!(["infra/forge/read-membership-starting-state.sh"])
    );
    assert_eq!(descriptor["capture"], "separate-streams");
    assert_eq!(descriptor["timeout"], 250);
    assert!(
        descriptor.get("requires_approval").is_none(),
        "read-only measurement grants no mutating authority"
    );
}
