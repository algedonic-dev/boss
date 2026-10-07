//! Installed-client evidence never reads credentials or implies historical completeness.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn installed_admission_reader_identity_refuses_unbound_or_raw_evidence() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/admission-identity-test.py"))
        .output()
        .expect("python3 is required; silence is not evidence");
    assert!(
        output.status.success(),
        "identity controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
