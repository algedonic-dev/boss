#[test]
fn jobs_mounts_local_snapshot_comparison_and_joins_refresh_on_shutdown() {
    let source: String = include_str!("../src/bin/boss_jobs_api.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for required in [
        "ReportingPolicyClient::with_mode_source(",
        "boss_policy_client::role_inventory::router(",
        ".run_refresh_loop(",
        "boss_core::machine_gate::mount(",
        "tokio::signal::ctrl_c().await",
        "cancel_tx.send(true)",
        "role_refresh.await",
    ] {
        assert!(source.contains(required), "Jobs binary lacks {required}");
    }
    assert!(
        !source.contains("SignalKind::terminate()"),
        "gate evidence owns termination"
    );
}
