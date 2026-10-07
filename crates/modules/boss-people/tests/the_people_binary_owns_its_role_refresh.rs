#[test]
fn people_mounts_local_snapshot_comparison_and_joins_refresh_on_shutdown() {
    let source: String = include_str!("../src/bin/boss_people_api.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for required in [
        "ReportingPolicyClient::with_mode_source(",
        "boss_policy_client::role_inventory::router(",
        ".run_refresh_loop(",
        ".with_graceful_shutdown(",
        "boss_core::machine_gate::mount(",
        "tokio::signal::ctrl_c().await",
        "role_stop.send(true)",
        "role_refresh.await",
    ] {
        assert!(source.contains(required), "People binary lacks {required}");
    }
    assert!(
        !source.contains("SignalKind::terminate()"),
        "gate evidence owns termination"
    );
}
