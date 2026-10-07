//! Source contract checks for the dev controller. These run the real
//! controller's admission and transport ports with inert fixtures;
//! they do not attest a live Kubernetes deployment.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_dev_controller_preserves_its_worker_and_receipt_contract() {
    for script in [
        "dev-build_test.py",
        "dev-lifecycle_test.py",
        "control-artifact_test.py",
        "activate-control_test.py",
        "control-tools_test.py",
    ] {
        let output = Command::new("python3")
            .arg("-I")
            .arg(repo_root().join("infra/dev").join(script))
            .output()
            .expect("run the controller contract suite");
        assert!(
            output.status.success(),
            "controller contracts failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
