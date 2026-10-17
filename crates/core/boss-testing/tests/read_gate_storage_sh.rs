//! The storage reader uses bounded native metadata and conserves its raw record.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_gate_storage_reader_preserves_identity_bounds_and_native_evidence() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/gate-storage-test.py"))
        .output()
        .expect("python3 is required; silence is not evidence");
    assert!(
        output.status.success(),
        "gate storage controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
