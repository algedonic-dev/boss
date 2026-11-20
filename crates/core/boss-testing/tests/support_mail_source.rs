//! Car2a owns the pure source contract; host transport/TLS belongs to car2b.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_mail_source_contract_executes_the_imap_standin() {
    let result = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("infra/gcp/support_mail_test.py"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "pure mail source contract failed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&result.stdout)
    );
    let count = stderr
        .lines()
        .find(|line| line.starts_with("Ran "))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(count > 0, "no actual source tests ran: {stderr}");
}
