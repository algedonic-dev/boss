use boss_jobs::{in_memory::InMemoryJobs, port::JobsRepository};
use std::time::Duration;

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
        boss_testing::reexec::ready();
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
    // The start/work split this test pioneered is boss_testing::reexec's
    // now, shared by every re-exec in the tree (backlog 7e30ddb0): the
    // three seconds are the snapshot's, from the child's ready().
    let mut child = boss_testing::reexec::ReExec::new(
        "a_flow_snapshot_finishes_without_reentering_its_state_lock",
    );
    child
        .command()
        .env(CHILD, "1")
        .env(
            "BOSS_FLOW_SNAPSHOT_STARTUP_MS",
            startup_delay_ms.to_string(),
        )
        .env("BOSS_FLOW_SNAPSHOT_BLOCKED", blocked.to_string());
    let done = child.spawn().finish(Duration::from_secs(3));
    assert!(
        !done.overran(),
        "flow snapshot child exceeded its operation watchdog (start {:?})",
        done.start
    );
    assert!(
        done.succeeded(),
        "flow snapshot child failed: {:?}\n{}\n{}",
        done.status,
        done.stdout,
        done.stderr
    );
}
