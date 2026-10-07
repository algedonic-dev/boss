//! The service owns refresh lifetime; off and shutdown perform no source read.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord};
use boss_policy_client::role_reader::{
    MonotonicRoleSnapshotClock, RegistryRoles, RoleSnapshotSource, SnapshotRoleReader,
};
use boss_policy_client::role_reporting::{ReportMode, ReportModeSource};
use serde_json::json;

struct ModeWitness {
    value: ReportMode,
    seen: Arc<tokio::sync::Notify>,
}

impl ReportModeSource for ModeWitness {
    fn mode(&self) -> ReportMode {
        self.seen.notify_one();
        self.value
    }
}

struct PausedSource {
    reads: AtomicUsize,
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl RoleSnapshotSource for PausedSource {
    async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn off_skips_refresh_and_shutdown_cancels_an_in_flight_source() {
    for mode in [ReportMode::Off, ReportMode::Report] {
        let mode_seen = Arc::new(tokio::sync::Notify::new());
        let mode = Arc::new(ModeWitness {
            value: mode,
            seen: mode_seen.clone(),
        });
        let entered = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(PausedSource {
            reads: AtomicUsize::new(0),
            entered: entered.clone(),
        });
        let reader = Arc::new(SnapshotRoleReader::new(
            Duration::from_secs(30),
            Arc::new(MonotonicRoleSnapshotClock),
        ));
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let running_source = source.clone();
        let running_mode = mode.clone();
        let task = tokio::spawn(async move {
            reader
                .run_refresh_loop(
                    running_source,
                    running_mode,
                    Duration::from_secs(3600),
                    receiver,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), mode_seen.notified())
            .await
            .unwrap();
        if mode.value == ReportMode::Report {
            tokio::time::timeout(Duration::from_secs(1), entered.notified())
                .await
                .unwrap();
        }
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            usize::from(mode.value == ReportMode::Report)
        );
    }
}

#[tokio::test]
async fn zero_cadence_is_refused_and_an_already_closed_owner_starts_no_refresh() {
    let source = Arc::new(PausedSource {
        reads: AtomicUsize::new(0),
        entered: Arc::new(tokio::sync::Notify::new()),
    });
    let reader = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let (stop, receiver) = tokio::sync::watch::channel(false);
    assert!(
        reader
            .clone()
            .run_refresh_loop(
                source.clone(),
                Arc::new(ReportMode::Report),
                Duration::ZERO,
                receiver
            )
            .await
            .is_err()
    );
    let (_, receiver) = tokio::sync::watch::channel(true);
    reader
        .run_refresh_loop(
            source.clone(),
            Arc::new(ReportMode::Report),
            Duration::from_secs(10),
            receiver,
        )
        .await
        .unwrap();
    drop(stop);
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn shutdown_invalidates_even_a_still_fresh_snapshot() {
    let reader = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let initial = reader.begin_refresh();
    assert!(reader.finish_refresh(initial, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":"agent-example","aliases":[],"role":"tenant-role"}],"total":1}),
        json!({"data":[],"total":0}),
        json!([]),
    ).unwrap())));
    assert!(reader.role_for("agent-example").await.unwrap().is_some());
    let entered = Arc::new(tokio::sync::Notify::new());
    let source = Arc::new(PausedSource {
        reads: AtomicUsize::new(0),
        entered: entered.clone(),
    });
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let running = reader.clone();
    let task = tokio::spawn(async move {
        running
            .run_refresh_loop(
                source,
                Arc::new(ReportMode::Report),
                Duration::from_secs(3600),
                receiver,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(reader.role_for("agent-example").await, Err(RoleLookupError::Unavailable(reason)) if reason.contains("stopped"))
    );
}
