//! Fixed read-only Talos health argv through the real container door,
//! including active output bounds and refusal/redaction conservation.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_talos_health_door_is_fixed_bounded_and_read_only() {
    let output = Command::new("python3")
        .arg(repo_root().join("infra/forge/tests/talos-node-health-test.py"))
        .output()
        .expect("run the owning Python fixture");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
