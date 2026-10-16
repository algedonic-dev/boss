//! Accepted a21 R1 adds a fixed fixture purpose, never a manifest runner.
use boss_testing::{repo_root, scratch_dir};
use serde_json::Value;
use std::process::Command;

#[test]
fn the_registry_exposes_only_the_fixed_fixture_lifecycle() {
    let output = Command::new("sh")
        .arg(repo_root().join("infra/ops/verbs-allowlist.sh"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        rows["verbs"]["dev-build-fence-status"]["hosts"],
        serde_json::json!(["forge"])
    );
    for (name, phase, count) in [
        ("prepare-isolation-fixture", "prepare", 6),
        ("observe-isolation-fixture", "observe", 1),
        ("retire-isolation-fixture", "retire", 1),
    ] {
        let row = &rows["verbs"][name];
        assert!(
            row.is_object(),
            "accepted fixed fixture purpose missing: {name}"
        );
        assert_eq!(row["hosts"], serde_json::json!(["forge"]));
        assert_eq!(row["argv"][0], "infra/forge/isolation-fixture.sh");
        assert_eq!(row["argv"][1], phase);
        assert_eq!(row["params"].as_array().unwrap().len(), count);
        assert_eq!(row["capture"], "separate-streams");
        assert!(row["about"].as_str().unwrap().contains("a21c491c"));
    }
}

#[test]
fn fixture_effects_refusals_replay_and_teardown_use_the_same_ports() {
    let scratch = scratch_dir("isolation-fixture");
    let output = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("infra/forge/isolation-fixture_test.py"))
        .arg(repo_root())
        .arg(&scratch)
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("fixture source controls: ok"), "{text}");
}
