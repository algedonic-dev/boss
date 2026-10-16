//! Image publication conservation and independent core convergence controls.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn agent_image_publication_conserves_originals_and_refuses_unproved_readiness() {
    let result = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("infra/agent/test_image_witness.py"))
        .output()
        .expect("owning image publisher controls run");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
