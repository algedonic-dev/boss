use boss_jobs::{in_memory::InMemoryJobs, port::JobsRepository};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn a_flow_snapshot_finishes_without_reentering_its_state_lock() {
    const CHILD: &str = "BOSS_FLOW_SNAPSHOT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let rows = runtime
            .block_on(InMemoryJobs::new().step_flow_cube(chrono::Utc::now()))
            .unwrap();
        assert!(rows.is_empty(), "an empty history has no flow obligations");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_flow_snapshot_finishes_without_reentering_its_state_lock",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "flow snapshot child failed: {status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("flow snapshot reentered its held state lock instead of completing");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
