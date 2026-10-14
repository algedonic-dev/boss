//! The moved policy adapter retains operational timestamps without a file exemption.
#[test]
fn moved_policy_inserts_do_not_hide_same_source_business_wallclock() {
    let output = std::process::Command::new("python3")
        .arg(boss_testing::repo_root().join("infra/lint/lib/policy-insert-wallclock_test.py"))
        .output()
        .expect("run actual lint and moved policy INSERT controls");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
