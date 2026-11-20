//! The passkey-bound filesystem trim of one instance volume (backlog
//! b1520f80 part b): plan first, the purge's own admission, a refusal
//! where Longhorn would mark snapshots removed, and status.actualSize read
//! back before and after before any effect line is printed.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn a_trim_holds_its_admission_refusals_and_read_back() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/instance_volume_trim_test.py"))
        .output()
        .expect("python3 is required for native-shaped trim controls");
    assert!(
        output.status.success(),
        "trim controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_trim_remains_one_passkey_approved_fixed_action() {
    let root = repo_root();
    let read = |path: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
    };
    let verb = read("infra/ops/verbs/trim-instance-volume.json");
    let plan = read("infra/ops/verbs/plan-an-instance-volume-trim.json");
    assert_eq!(verb["requires_approval"], true);
    assert_eq!(verb["plan_verb"], "plan-an-instance-volume-trim");
    assert_eq!(verb["approvers"], serde_json::json!(["emp-david"]));
    assert_eq!(verb["hosts"], plan["hosts"]);
    assert_eq!(plan["argv"][0], verb["argv"][0]);
    assert_eq!(plan["argv"][1], "--plan");
    assert_eq!(plan.get("requires_approval"), None);
    assert_eq!(verb["argv"].as_array().unwrap().len(), 4);
    assert_eq!(verb["params"][2]["name"], "plan_sha256");
    assert_eq!(
        plan["params"].as_array().unwrap()[..],
        verb["params"].as_array().unwrap()[..2]
    );
    // The script's own bounds sit inside the runner's kill: the trim's
    // call and the operation deadline (TRIM_SECONDS + 300), plus the
    // evidence attachment's 120 s.
    let py = std::fs::read_to_string(root.join("infra/forge/instance-volume-trim.py")).unwrap();
    assert!(
        py.contains("TRIM_SECONDS = 900\n"),
        "the trim's call bound moved; re-derive the verb's timeout"
    );
    assert!(verb["timeout"].as_u64().unwrap() >= 900 + 300 + 120);
}
