//! Packaging a new read adapter must preserve the existing service door.
//! The old memory library must neither own the HTTP binary nor acquire
//! an upward policy dependency; the container must still build the same
//! executable and the config pin must inspect its actual new owner.

use boss_testing::repo_root;

fn manifest(path: &str) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(repo_root().join(path)).unwrap()).unwrap()
}

#[test]
fn the_events_api_has_one_apps_package_and_the_same_runtime_contract() {
    let path = "crates/orchestrators/boss-events-api";
    assert!(
        repo_root().join(path).join("Cargo.toml").is_file(),
        "the events API must belong to an Apps package, not add another HTTP exception in memory"
    );
    let workspace = manifest("Cargo.toml");
    assert!(
        workspace["workspace"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(path))
    );
    let app = manifest(&format!("{path}/Cargo.toml"));
    assert_eq!(app["package"]["name"].as_str(), Some("boss-events-api"));
    let binary = app["bin"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bin| bin["name"].as_str() == Some("boss-events-api"))
        .unwrap();
    assert_eq!(binary["path"].as_str(), Some("src/main.rs"));
    assert!(
        binary["required-features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str() == Some("events-api"))
    );
    let memory = manifest("crates/core/boss-events/Cargo.toml");
    assert!(
        memory["bin"]
            .as_array()
            .unwrap()
            .iter()
            .all(|bin| { bin["name"].as_str() != Some("boss-events-api") })
    );
    for section in ["dependencies", "dev-dependencies"] {
        assert!(memory[section].get("boss-policy").is_none());
    }
    assert!(
        !repo_root()
            .join("crates/core/boss-events/src/bin/boss_events_api.rs")
            .exists()
    );
    assert!(
        !repo_root()
            .join("crates/core/boss-events/src/gate_window_http.rs")
            .exists()
    );
    let main = std::fs::read_to_string(repo_root().join(path).join("src/main.rs")).unwrap();
    for unchanged in [
        "boss_events::events_api_config::EventsApiConfig",
        "/etc/boss-events-api.toml",
        "audit_tail_router(pool.clone())",
        ".merge(outbox_router(pool.clone()))",
        "boss_core::machine_gate::mount(",
        "\"events\",",
        "\"/api/events/health\"",
        "boss_events::outbox::PgOutboxRecorder::shared(&pool)",
    ] {
        assert!(
            main.contains(unchanged),
            "lost runtime contract: {unchanged}"
        );
    }
}

#[test]
fn the_container_and_config_pin_follow_the_same_apps_binary() {
    let root = repo_root();
    let docker = std::fs::read_to_string(root.join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(docker.contains("-p boss-events-api --bin boss-events-api --features events-api"));
    assert!(!docker.contains("-p boss-events --bin boss-events-api"));
    let pin =
        std::fs::read_to_string(root.join("crates/core/boss-testing/tests/generate_configs_sh.rs"))
            .unwrap();
    assert!(pin.contains("crates/orchestrators/boss-events-api/src/main.rs"));
    assert!(!pin.contains("crates/core/boss-events/src/bin/boss_events_api.rs"));
    let layers = std::fs::read_to_string(root.join("infra/lint/layer-order-audit.sh")).unwrap();
    assert!(
        layers
            .lines()
            .any(|line| line.contains("boss-events-api") && line.contains("echo apps"))
    );
    assert!(
        !layers
            .lines()
            .any(|line| line.starts_with("boss-events/") && line.contains("gate_window"))
    );
}
