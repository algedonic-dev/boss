//! Refresh is an explicit side effect through a source port; lookup is local.
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord};
use boss_policy_client::role_reader::{
    MonotonicRoleSnapshotClock, RegistryRoles, RoleRefreshOutcome, RoleSnapshotSource,
    SnapshotRoleReader,
};
use serde_json::json;

struct InMemorySource {
    replies: Mutex<VecDeque<Result<RegistryRoles, RoleLookupError>>>,
    reads: AtomicUsize,
}

#[async_trait]
impl RoleSnapshotSource for InMemorySource {
    async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.replies.lock().unwrap().pop_front().unwrap()
    }
}

fn records() -> RegistryRoles {
    RegistryRoles::from_sources(
        json!({"data":[{"id":"agent-example","aliases":[],"role":"tenant-role"}],"total":1}),
        json!({"data":[],"total":0}),
        json!([]),
    )
    .unwrap()
}

#[tokio::test]
async fn lookups_do_not_load_and_failed_refresh_invalidates_previous_success() {
    let source = InMemorySource {
        replies: Mutex::new(VecDeque::from([
            Ok(records()),
            Err(RoleLookupError::Unavailable(
                "incomplete people response".into(),
            )),
        ])),
        reads: AtomicUsize::new(0),
    };
    let reader = SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    );
    assert!(reader.role_for("agent-example").await.is_err());
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        reader.refresh_from(&source).await,
        RoleRefreshOutcome::Published
    );
    for _ in 0..20 {
        assert_eq!(
            reader
                .role_for("agent-example")
                .await
                .unwrap()
                .unwrap()
                .role
                .as_deref(),
            Some("tenant-role")
        );
    }
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        reader.refresh_from(&source).await,
        RoleRefreshOutcome::Unavailable
    );
    assert!(
        matches!(reader.role_for("agent-example").await, Err(RoleLookupError::Unavailable(reason)) if reason.contains("incomplete people response"))
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_refresh_in_flight_does_not_hold_the_lookup_lock_or_beat_a_newer_failure() {
    struct PausedSource {
        entered: Arc<tokio::sync::Notify>,
        finish: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl RoleSnapshotSource for PausedSource {
        async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
            self.entered.notify_one();
            self.finish.notified().await;
            Ok(records())
        }
    }
    let reader = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let initial = reader.begin_refresh();
    assert!(reader.finish_refresh(initial, Ok(records())));
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let source = PausedSource {
        entered: entered.clone(),
        finish: finish.clone(),
    };
    let loading_reader = reader.clone();
    let task = tokio::spawn(async move { loading_reader.refresh_from(&source).await });
    entered.notified().await;
    let lookup = tokio::time::timeout(Duration::from_secs(1), reader.role_for("agent-example"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lookup.unwrap().role.as_deref(), Some("tenant-role"));
    let newer = reader.begin_refresh();
    assert!(reader.finish_refresh(
        newer,
        Err(RoleLookupError::Unavailable("new failure".into()))
    ));
    finish.notify_one();
    assert_eq!(task.await.unwrap(), RoleRefreshOutcome::Superseded);
    assert!(
        matches!(reader.role_for("agent-example").await, Err(RoleLookupError::Unavailable(reason)) if reason.contains("new failure"))
    );
}
