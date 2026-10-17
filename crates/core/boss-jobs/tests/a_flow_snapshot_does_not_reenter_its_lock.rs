use boss_jobs::{in_memory::InMemoryJobs, port::JobsRepository};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn a_flow_snapshot_finishes_without_reentering_its_state_lock() {
    check_snapshot(0, false);
}

#[test]
fn startup_delay_is_not_reported_as_a_snapshot_lock_failure() {
    check_snapshot(4_000, false);
}

#[test]
#[should_panic(expected = "operation watchdog")]
fn a_blocked_snapshot_is_reaped_after_readiness() {
    check_snapshot(0, true);
}

fn check_snapshot(startup_delay_ms: u64, blocked: bool) {
    const CHILD: &str = "BOSS_FLOW_SNAPSHOT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::sleep(Duration::from_millis(
            std::env::var("BOSS_FLOW_SNAPSHOT_STARTUP_MS")
                .unwrap()
                .parse()
                .unwrap(),
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        std::fs::write(std::env::var("BOSS_FLOW_SNAPSHOT_READY").unwrap(), b"ready").unwrap();
        if std::env::var("BOSS_FLOW_SNAPSHOT_BLOCKED").unwrap() == "true" {
            loop {
                std::thread::park();
            }
        }
        let rows = runtime
            .block_on(InMemoryJobs::new().step_flow_cube(chrono::Utc::now()))
            .unwrap();
        assert!(rows.is_empty(), "an empty history has no flow obligations");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("snapshot-ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_flow_snapshot_finishes_without_reentering_its_state_lock",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(
            "BOSS_FLOW_SNAPSHOT_STARTUP_MS",
            startup_delay_ms.to_string(),
        )
        .env("BOSS_FLOW_SNAPSHOT_READY", &ready)
        .env("BOSS_FLOW_SNAPSHOT_BLOCKED", blocked.to_string())
        .spawn()
        .unwrap();
    let startup_deadline = Instant::now() + Duration::from_secs(60);
    let mut snapshot_deadline = None;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "flow snapshot child failed: {status}");
            return;
        }
        if snapshot_deadline.is_none() && ready.exists() {
            snapshot_deadline = Some(Instant::now() + Duration::from_secs(3));
        }
        let expired = match snapshot_deadline {
            Some(deadline) => Instant::now() >= deadline,
            None => Instant::now() >= startup_deadline,
        };
        if expired {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "flow snapshot child exceeded its {} watchdog",
                if snapshot_deadline.is_some() {
                    "operation"
                } else {
                    "startup"
                }
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
