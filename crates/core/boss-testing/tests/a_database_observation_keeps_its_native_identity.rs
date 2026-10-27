//! Bounded native reader fixtures, not a claim about production wait causes.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn database_identity_and_io_accounting_refuse_replacement_or_missing_evidence() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/jobs-database-test.py"))
        .output()
        .expect("python3 is required; silence is not evidence");
    assert!(
        output.status.success(),
        "database observation controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
