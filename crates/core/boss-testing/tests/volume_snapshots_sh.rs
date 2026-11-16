//! Fixed read-only inventory, before any approved snapshot cleanup (34b7906a).
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn native_snapshot_inventory_retains_bounds_and_unavailable_channels() {
    let output = Command::new("python3")
        .args(["-B", "infra/forge/tests/volume-snapshots-test.py"])
        .current_dir(repo_root())
        .output()
        .expect("Python fixture starts");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let descriptor: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/read-volume-snapshots.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(descriptor["hosts"], serde_json::json!(["forge"]));
    assert_eq!(descriptor["params"], serde_json::json!([]));
    assert_eq!(
        descriptor["argv"],
        serde_json::json!(["infra/forge/read-volume-snapshots.sh"])
    );
    assert!(
        descriptor.get("requires_approval").is_none(),
        "this car adds no mutator"
    );
    let refusal = Command::new("bash")
        .arg(repo_root().join("infra/forge/read-volume-snapshots.sh"))
        .arg("injected-argument")
        .output()
        .unwrap();
    assert_eq!(refusal.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("no arguments accepted"));
}
