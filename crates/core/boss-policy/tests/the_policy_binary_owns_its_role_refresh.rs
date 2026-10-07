//! The service constructor must consume the tested owned refresh and
//! report mount; a library-only capability does not cover a live service.
#[test]
fn the_binary_mounts_reports_and_joins_its_refresh_owner() {
    let source = include_str!("../src/bin/boss_policy_api.rs");
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    for required in [
        "boss_policy::role_reports::mount(",
        ".run_refresh_loop(",
        ".with_graceful_shutdown(",
        "boss_core::machine_gate::mount(",
        "tokio::signal::ctrl_c().await",
        "role_stop.send(true)",
        "role_refresh.await",
    ] {
        assert!(compact.contains(required), "policy binary lacks {required}");
    }
    assert!(
        !compact.contains("SignalKind::terminate()"),
        "gate evidence owns termination"
    );
}
