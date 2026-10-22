//! `infra/cluster/dev-artifact-inventory.py` — the agent-artifact
//! lifecycle's dry run (backlog 25022367, design 195276f0) — accounts
//! every file under the policy's families by the reason it is retained,
//! refuses a policy that would grant an action, follows no link, counts
//! what it cannot read as uncertainty, and changes nothing it reads. The
//! causal cases live in its unittest file beside it; this runs them in
//! the gate with fixtures under this run's own scratch directory.
use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

#[test]
fn the_artifact_dry_run_accounts_every_byte_and_changes_nothing() {
    let root = scratch_dir("artifact-inventory-py");
    let result = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("infra/cluster/dev-artifact-inventory_test.py"))
        .env("BOSS_TEST_SCRATCH", &root)
        .output()
        .expect("the inventory's Python tests run");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
