//! Fixed-purpose capacity planning, passkey boundary and no-shrink converge.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn dev_work_expansion_preserves_its_boundaries_and_native_evidence() {
    let result = Command::new("python3")
        .arg(repo_root().join("infra/forge/tests/dev-work-volume-test.py"))
        .output()
        .expect("run the real fixed-purpose planner controls");
    assert!(
        result.status.success(),
        "dev work expansion controls failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn the_work_volume_plan_keeps_complete_bounded_originals() {
    let result = Command::new("python3")
        .arg(repo_root().join("infra/forge/tests/dev-work-evidence-test.py"))
        .arg(repo_root().join("infra/forge/dev-work-evidence.py"))
        .output()
        .expect("run the actual bounded evidence carrier controls");
    assert!(
        result.status.success(),
        "bounded work volume evidence controls failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
