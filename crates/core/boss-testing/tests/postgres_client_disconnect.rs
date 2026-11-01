//! tree-wide pin — the server declares dead-client prevention independently of a pool.
#[path = "support/postgres_disconnect_settings.rs"]
mod postgres_disconnect_settings;
use postgres_disconnect_settings::declared_settings;

#[test]
fn postgres_declares_finite_dead_client_detection_without_a_global_query_timeout() {
    let settings = declared_settings();
    for (key, value) in [
        ("max_connections", "400"),
        ("client_connection_check_interval", "10s"),
        ("tcp_keepalives_idle", "60"),
        ("tcp_keepalives_interval", "10"),
        ("tcp_keepalives_count", "3"),
    ] {
        assert_eq!(
            settings.iter().filter(|(name, _)| name == key).count(),
            1,
            "missing/duplicate {key}"
        );
        assert!(
            settings
                .iter()
                .any(|(name, actual)| name == key && actual == value),
            "wrong {key}"
        );
    }
    assert!(!settings.iter().any(|(name, _)| name == "statement_timeout"));
}
