//! The actual runtime probe must judge complete hourly evidence and exact native instants.
#[test]
fn the_runtime_probe_refuses_hidden_newer_failures_and_accepts_complete_nonadmissions() {
    let scratch = boss_testing::scratch_dir("boss-worktree-inventory-probe");
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(include_str!("fixtures/worktree_inventory_probe.py"))
        .env("PROOF_SCRATCH", &scratch)
        .env(
            "PROOF_SCRIPT",
            boss_testing::repo_root().join("infra/forge/probe-worktree-hold-inventory.sh"),
        )
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(scratch).unwrap();
}
