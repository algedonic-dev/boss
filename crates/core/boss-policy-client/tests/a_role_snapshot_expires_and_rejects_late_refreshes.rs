//! A failed or late refresh cannot publish stale authority as current data.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boss_core::role_of_record::{RoleLookupError, RoleOfRecord};
use boss_policy_client::role_reader::{RegistryRoles, RoleSnapshotClock, SnapshotRoleReader};
use serde_json::json;

struct Clock(Mutex<Instant>);

impl RoleSnapshotClock for Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

fn records(role: &str) -> RegistryRoles {
    RegistryRoles::from_sources(
        json!({"data":[{"id":"agent-example","aliases":[],"role":role}],"total":1}),
        json!({"data":[],"total":0}),
        json!([]),
    )
    .unwrap()
}

#[test]
fn report_inventory_distinguishes_absence_fresh_expired_and_failed_snapshots() {
    let start = Instant::now();
    let clock = Arc::new(Clock(Mutex::new(start)));
    let roles = SnapshotRoleReader::new(Duration::from_secs(30), clock.clone());
    assert_eq!(
        serde_json::to_value(roles.snapshot_status()).unwrap()["state"],
        "never-loaded"
    );
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(records("engineering-agent"))));
    let fresh = serde_json::to_value(roles.snapshot_status()).unwrap();
    assert_eq!(fresh["state"], "ready");
    assert_eq!(fresh["age_seconds"], 0);
    assert_eq!(fresh["max_age_seconds"], 30);
    *clock.0.lock().unwrap() = start + Duration::from_secs(30);
    assert_eq!(
        serde_json::to_value(roles.snapshot_status()).unwrap()["state"],
        "expired"
    );
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(
        ticket,
        Err(RoleLookupError::Unavailable("people unavailable".into()))
    ));
    let failed = serde_json::to_value(roles.snapshot_status()).unwrap();
    assert_eq!(failed["state"], "unavailable");
    assert!(
        failed["reason"]
            .as_str()
            .unwrap()
            .contains("people unavailable")
    );
}

#[tokio::test]
async fn never_loaded_expired_or_failed_is_unavailable_instead_of_unregistered() {
    let start = Instant::now();
    let clock = Arc::new(Clock(Mutex::new(start)));
    let roles = SnapshotRoleReader::new(Duration::from_secs(30), clock.clone());
    assert!(matches!(
        roles.role_for("missing").await,
        Err(RoleLookupError::Unavailable(_))
    ));
    let refresh = roles.begin_refresh();
    assert!(roles.finish_refresh(refresh, Ok(records("engineering-agent"))));
    assert_eq!(roles.role_for("missing").await.unwrap(), None);
    assert_eq!(
        roles
            .role_for("agent-example")
            .await
            .unwrap()
            .unwrap()
            .role
            .as_deref(),
        Some("engineering-agent")
    );
    *clock.0.lock().unwrap() = start + Duration::from_secs(30);
    assert!(matches!(
        roles.role_for("missing").await,
        Err(RoleLookupError::Unavailable(_))
    ));
    let refresh = roles.begin_refresh();
    assert!(roles.finish_refresh(
        refresh,
        Err(RoleLookupError::Unavailable("people unavailable".into()))
    ));
    *clock.0.lock().unwrap() = start + Duration::from_secs(1);
    assert!(
        matches!(roles.role_for("agent-example").await, Err(RoleLookupError::Unavailable(reason)) if reason.contains("people unavailable"))
    );
}

#[tokio::test]
async fn a_late_success_cannot_erase_a_newer_failure_or_replace_a_newer_snapshot() {
    let clock = Arc::new(Clock(Mutex::new(Instant::now())));
    let roles = SnapshotRoleReader::new(Duration::from_secs(30), clock);
    let old = roles.begin_refresh();
    let newer = roles.begin_refresh();
    assert!(roles.finish_refresh(
        newer,
        Err(RoleLookupError::Unavailable("new failure".into()))
    ));
    assert!(!roles.finish_refresh(old, Ok(records("platform-admin"))));
    assert!(
        matches!(roles.role_for("agent-example").await, Err(RoleLookupError::Unavailable(reason)) if reason.contains("new failure"))
    );
    let old = roles.begin_refresh();
    let newer = roles.begin_refresh();
    assert!(roles.finish_refresh(newer, Ok(records("engineering-agent"))));
    assert!(!roles.finish_refresh(old, Ok(records("platform-admin"))));
    assert_eq!(
        roles
            .role_for("agent-example")
            .await
            .unwrap()
            .unwrap()
            .role
            .as_deref(),
        Some("engineering-agent")
    );
}

#[tokio::test]
async fn freshness_starts_before_the_fetch_and_not_when_a_slow_fetch_finishes() {
    let start = Instant::now();
    let clock = Arc::new(Clock(Mutex::new(start)));
    let roles = SnapshotRoleReader::new(Duration::from_secs(30), clock.clone());
    let refresh = roles.begin_refresh();
    *clock.0.lock().unwrap() = start + Duration::from_secs(31);
    assert!(roles.finish_refresh(refresh, Ok(records("platform-admin"))));
    assert!(matches!(
        roles.role_for("agent-example").await,
        Err(RoleLookupError::Unavailable(_))
    ));
}
