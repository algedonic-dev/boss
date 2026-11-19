//! 67e08c6a: synthetic capture controls do not attest a live mailbox login.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn bridge_capture_keeps_credentials_local_and_refuses_uncertain_state() {
    let result = Command::new("python3")
        .arg(repo_root().join("infra/gcp/proton_bridge_test.py"))
        .output()
        .expect("run owning Bridge contract tests");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
