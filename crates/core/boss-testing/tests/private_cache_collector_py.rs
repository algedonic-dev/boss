//! The private helper's synthetic controls are part of the full gate.
use boss_testing::repo_root;
use std::process::Command;
#[test]
fn private_collection_refuses_unavailable_authority_and_unsafe_transport() {
    let result = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("infra/agent/test_private_collector.py"))
        .output()
        .expect("synthetic collector controls run");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
