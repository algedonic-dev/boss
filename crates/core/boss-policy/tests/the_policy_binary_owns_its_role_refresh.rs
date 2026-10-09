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
        // A refused policy write reaches the log only through this layer
        // (review 1a73d5ce F2): without it the doors still warn, and
        // state no event.
        "boss_policy::refusals::recorded(app,Arc::clone(&recorder))",
        // The boot reports the service read and never refuses on it
        // (backlog 0028804f): every arm of the match falls through.
        "matchboss_policy::service_read::found(repo.as_ref()).await{Ok(None)=>{}Ok(Some(report))=>tracing::error!(\"{report}\"),Err(e)=>warn!(",
    ] {
        assert!(compact.contains(required), "policy binary lacks {required}");
    }
    assert!(
        !compact.contains("SignalKind::terminate()"),
        "gate evidence owns termination"
    );
}
