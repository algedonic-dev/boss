//! 4196a7b1: native provenance is executed before a new reader is parked.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn native_evidence_refuses_false_sightings_and_keeps_real_controls() {
    // One roster: the build-free preflight and FULL gate execute the
    // same adversarial controls, including closed-original observation.
    let output = Command::new("bash")
        .arg(repo_root().join("infra/lint/native-proof-evidence-is-adversarially-checked.sh"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
