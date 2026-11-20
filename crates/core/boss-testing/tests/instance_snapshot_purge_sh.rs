//! The passkey-bound purge names its exact scope and never equates a Job exit with effect.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn a_purge_preserves_binding_approval_backup_and_per_replica_evidence() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/instance_snapshot_purge_test.py"))
        .output()
        .expect("python3 is required for native-shaped purge controls");
    assert!(
        output.status.success(),
        "purge controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_purge_remains_one_passkey_approved_fixed_action() {
    let root = repo_root();
    let read = |path: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
    };
    let verb = read("infra/ops/verbs/purge-instance-snapshots.json");
    let plan = read("infra/ops/verbs/plan-an-instance-snapshot-purge.json");
    assert_eq!(verb["requires_approval"], true);
    assert_eq!(verb["plan_verb"], "plan-an-instance-snapshot-purge");
    assert_eq!(verb["approvers"], serde_json::json!(["emp-david"]));
    assert_eq!(plan["argv"][0], verb["argv"][0]);
    assert_eq!(plan["argv"][1], "--plan");
    assert_eq!(verb["argv"].as_array().unwrap().len(), 4);
    assert_eq!(verb["params"][2]["name"], "plan_sha256");
    assert_eq!(
        verb["effect"],
        "^instance-snapshot-purge: per-replica purge complete; capacity recovery not yet proven$"
    );
}
