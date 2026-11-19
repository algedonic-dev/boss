//! The history read preserves native provenance without acquiring mutation authority.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn native_replica_history_keeps_uid_binding_unknowns_and_complete_reads() {
    for script in [
        "infra/forge/tests/volume-replica-history-test.py",
        "infra/forge/tests/volume-snapshots-test.py",
    ] {
        let output = Command::new("python3")
            .args(["-B", script])
            .current_dir(repo_root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{script}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let descriptor: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/ops/verbs/read-volume-replica-history.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(descriptor["hosts"], serde_json::json!(["forge"]));
    assert_eq!(descriptor["params"], serde_json::json!([]));
    assert_eq!(
        descriptor["argv"],
        serde_json::json!(["infra/forge/read-volume-replica-history.sh"])
    );
    assert!(descriptor.get("requires_approval").is_none());
    let output = Command::new("bash")
        .arg(repo_root().join("infra/forge/read-volume-replica-history.sh"))
        .arg("injected-argument")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
}
