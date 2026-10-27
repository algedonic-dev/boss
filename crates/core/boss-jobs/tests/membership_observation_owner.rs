//! HTTP observer cancellation must not detach the native query from its owner.
use boss_jobs::membership_observation::Observation;
use boss_jobs::membership_observation_service::MembershipObservationOwner;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

/// The child uses the same process owner as run_server, including the real
/// gate-evidence watcher. A direct owner.shutdown() test cannot expose its exit.
#[cfg(unix)]
#[test]
fn service_signals_join_the_observation_before_the_evidence_owner_exits() {
    use boss_testing::reexec::ReExec;
    use std::process::Command;
    use std::time::Duration;

    let server = include_str!("../src/bin/boss_jobs_api.rs");
    assert!(
        server.contains("let process_shutdown = boss_core::gate_evidence::ShutdownGuard::hold();")
    );
    assert!(server.contains("process_shutdown.requested().await;"));
    assert!(server.contains("process_shutdown.finish().await;"));
    for (signal, slow_evidence) in [("INT", false), ("TERM", false), ("TERM", true)] {
        let mut re = ReExec::new("observation_signal_child");
        re.command()
            .arg("--ignored")
            .env("BOSS_OBSERVATION_SIGNAL_CHILD", "1")
            .env(
                "BOSS_OBSERVATION_SLOW_EVIDENCE",
                if slow_evidence { "1" } else { "0" },
            );
        let mut child = re.spawn();
        if child.ready().is_err() {
            panic!(
                "signal child ended before readiness: {:?}",
                child.finish(Duration::ZERO)
            );
        }
        assert!(
            Command::new("kill")
                .args([&format!("-{signal}"), &child.child().id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        // Keep the original five-second WORK bound. ReExec owns start-up
        // separately, after the child registered both signals and actual work.
        let done = child.finish(Duration::from_secs(5));
        assert!(!done.overran(), "signal child did not finish: {done:?}");
        let status = done.status.unwrap();
        let output = done.stdout;
        let stderr = done.stderr;
        println!(
            "SIGNAL_CONTROL signal={signal} slow_evidence={slow_evidence} status={status} stdout={output:?} stderr={stderr:?}"
        );
        assert_eq!(
            status.code(),
            Some(if signal == "TERM" { 143 } else { 0 }),
            "{output}\n{stderr}"
        );
        let cancelled = output.find("OPERATION_CANCELLED").unwrap();
        let closed = output.find("ADMISSION_CLOSED").unwrap();
        let terminal = output.find("ACTUAL_OPERATION_TERMINAL").unwrap();
        let joined = output.find("SHUTDOWN_JOINED").unwrap();
        assert!(
            cancelled < terminal && closed < terminal && terminal < joined,
            "{output}"
        );
        if signal == "TERM" {
            let ended = output.find("EVIDENCE_ENDED").unwrap();
            if slow_evidence {
                assert!(
                    joined < ended,
                    "finishing service work must not cancel a slower evidence drain: {output}"
                );
            } else {
                assert!(
                    ended < terminal,
                    "the fast evidence drain must expose the original race: {output}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "child process of service_signals_join_the_observation_before_the_evidence_owner_exits"]
fn observation_signal_child() {
    if std::env::var_os("BOSS_OBSERVATION_SIGNAL_CHILD").is_none() {
        return;
    }
    struct Recorder {
        slow: bool,
    }
    #[async_trait::async_trait]
    impl boss_core::port::EventRecorder for Recorder {
        async fn record(&self, event: &boss_core::event::Event) -> Result<(), String> {
            if event.kind.ends_with(".recording_ended") {
                if self.slow {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                println!("EVIDENCE_ENDED");
            }
            Ok(())
        }
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let process_shutdown = boss_core::gate_evidence::ShutdownGuard::hold();
        let _evidence = boss_core::gate_evidence::Evidence::spawn(
            boss_core::gate_evidence::Gate::MachineGate,
            "observation-signal-test",
            Arc::new(Recorder {
                slow: std::env::var("BOSS_OBSERVATION_SLOW_EVIDENCE").as_deref() == Ok("1"),
            }),
        );
        let owner = Arc::new(MembershipObservationOwner::new());
        let (started, ready) = tokio::sync::oneshot::channel();
        let cancelled_seen = Arc::new(AtomicBool::new(false));
        let client = {
            let owner = owner.clone();
            let cancelled_seen = cancelled_seen.clone();
            tokio::spawn(async move {
                owner
                    .run(move |cancelled| async move {
                        let _ = started.send(());
                        while !cancelled.load(Ordering::Acquire) {
                            tokio::task::yield_now().await;
                        }
                        println!("OPERATION_CANCELLED");
                        cancelled_seen.store(true, Ordering::Release);
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        println!("ACTUAL_OPERATION_TERMINAL");
                        Ok(Observation::default())
                    })
                    .await
                    .unwrap();
            })
        };
        ready.await.unwrap();
        // Both signal registrations have been polled before the parent acts.
        {
            let requested = process_shutdown.requested();
            tokio::pin!(requested);
            assert!(futures::poll!(&mut requested).is_pending());
            println!("OWNED_OPERATION_STARTED");
            // The one TERM watcher registers on its own runtime. Keep the
            // existing settling interval inside readiness, never work timing.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            boss_testing::reexec::ready();
            requested.await;
        }
        let shutdown = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.shutdown().await })
        };
        while !cancelled_seen.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let refused = owner
            .run(|_| async { panic!("closed admission must not run") })
            .await;
        assert!(refused.unwrap_err().contains("closed"));
        println!("ADMISSION_CLOSED");
        shutdown.await.unwrap();
        client.await.unwrap();
        println!("SHUTDOWN_JOINED");
        process_shutdown.finish().await;
    });
}

#[tokio::test]
async fn a_dropped_reader_keeps_the_query_owned_and_busy_until_shutdown_joins_it() {
    let owner = Arc::new(MembershipObservationOwner::new());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cancelled_seen = Arc::new(AtomicBool::new(false));
    let client = {
        let owner = owner.clone();
        let started = started.clone();
        let release = release.clone();
        let cancelled_seen = cancelled_seen.clone();
        tokio::spawn(async move {
            owner
                .run(move |cancelled| async move {
                    started.notify_one();
                    while !cancelled.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                    cancelled_seen.store(true, Ordering::Release);
                    release.notified().await;
                    Ok(Observation::default())
                })
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    let refused = owner
        .run(|_| async { panic!("a second query must never be admitted") })
        .await;
    assert!(refused.unwrap_err().contains("busy"));
    let shutdown = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !cancelled_seen.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !shutdown.is_finished(),
        "shutdown must join actual work, not only signal it"
    );
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap();
    let refused = owner
        .run(|_| async { panic!("shutdown owner must not reopen") })
        .await;
    assert!(refused.unwrap_err().contains("closed"));
}

#[tokio::test]
async fn a_completed_observation_releases_its_slot_for_the_next_reader() {
    let owner = MembershipObservationOwner::new();
    for state in ["first", "second"] {
        let report = owner
            .run(move |_| async move {
                Ok(Observation {
                    state: state.into(),
                    ..Observation::default()
                })
            })
            .await
            .unwrap();
        assert_eq!(report.state, state);
    }
    owner.shutdown().await;
}
