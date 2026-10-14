//! The actual image helper, without an image build or provider call.
use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

#[test]
fn image_helper_fences_incarnation_and_conserves_bounded_private_artifacts() {
    let root = scratch_dir("agent-image-io");
    let result = Command::new("python3")
        .arg(repo_root().join("infra/agent/agent_io_test.py"))
        .env("BOSS_TEST_SCRATCH", &root)
        .output()
        .expect("image helper Python test runs");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
