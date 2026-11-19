//! tree-wide pin — a deploy joins HTTP and telemetry under the existing TERM owner.
use std::sync::{Arc, Mutex};

fn ordered(source: &str, required: &[&str]) -> bool {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let mut rest = compact.as_str();
    for part in required {
        let Some(at) = rest.find(part) else {
            return false;
        };
        rest = &rest[at + part.len()..];
    }
    true
}

#[test]
fn both_backend_owners_join_http_and_the_recorder_before_releasing_the_term_guard() {
    for (source, join) in [
        (
            include_str!("../src/bin/boss_jobs_api.rs"),
            "http_task.await",
        ),
        (
            include_str!("../../../modules/boss-messages/src/bin/boss_messages_api.rs"),
            "serve_task.await",
        ),
    ] {
        let required = [
            "boss_core::gate_evidence::ShutdownGuard::hold()",
            "machine_gate::mount_with_request_latency(",
            "process_shutdown.requested()",
            join,
            "latency_worker.shutdown().await",
            "process_shutdown.finish().await",
        ];
        assert!(
            ordered(source, &required),
            "real backend must close/join HTTP, drain telemetry, then release the shared TERM owner"
        );
        for removed in [
            "ShutdownGuard::hold()",
            "process_shutdown.requested()",
            join,
            "latency_worker.shutdown().await",
            "process_shutdown.finish().await",
        ] {
            assert!(
                source.contains(removed),
                "control must change actual source: {removed}"
            );
            assert!(
                !ordered(&source.replace(removed, "REMOVED"), &required),
                "missed {removed}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn real_signals_join_http_and_flush_the_epoch_end_before_gate_evidence_can_exit() {
    use boss_testing::reexec::ReExec;
    use std::{process::Command, time::Duration};
    for signal in ["INT", "TERM"] {
        let mut re = ReExec::new("request_latency_signal_child");
        re.command()
            .arg("--ignored")
            .env("BOSS_LATENCY_SIGNAL_CHILD", "1");
        let mut child = re.spawn();
        if child.ready().is_err() {
            panic!(
                "child unavailable before readiness: {:?}",
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
        let done = child.finish(Duration::from_secs(5));
        assert!(
            !done.overran(),
            "owned signal work exceeded its unchanged5s bound: {done:?}"
        );
        println!(
            "LATENCY_SIGNAL signal={signal} status={:?} stdout={:?} stderr={:?}",
            done.status, done.stdout, done.stderr
        );
        assert_eq!(
            done.status.unwrap().code(),
            Some(if signal == "TERM" { 143 } else { 0 })
        );
        let samples: Vec<serde_json::Value> = done
            .stdout
            .lines()
            .filter_map(|line| line.strip_prefix("TIMING_SAMPLE "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(samples.iter().filter(|s| s["class"] == "write").count(), 1);
        assert!(
            samples.last().unwrap()["recording_ended_at"].is_string(),
            "normal real-signal drain must persist its explicit epoch end"
        );
        assert!(ordered(
            &done.stdout,
            &["HTTP_JOINED", "TIMING_SAMPLE", "QUEUE_JOINED"]
        ));
    }
}

#[cfg(unix)]
#[test]
#[ignore = "isolated child of real_signals_join_http_and_flush_the_epoch_end_before_gate_evidence_can_exit"]
fn request_latency_signal_child() {
    if std::env::var_os("BOSS_LATENCY_SIGNAL_CHILD").is_none() {
        return;
    }
    use axum::{Router, routing::post};
    use boss_core::{
        gate_evidence::{Evidence, Gate, ShutdownGuard},
        machine_gate::{MachineGate, Mode, Reading, Slots},
        request_latency::{Class, Recorder, RecorderError, Sample, Timing},
    };
    struct EvidenceRecorder;
    #[async_trait::async_trait]
    impl boss_core::port::EventRecorder for EvidenceRecorder {
        async fn record(&self, event: &boss_core::event::Event) -> Result<(), String> {
            if event.kind.ends_with(".recording_ended") {
                println!("GATE_EVIDENCE_ENDED");
            }
            Ok(())
        }
    }
    struct Timings {
        ready: tokio::sync::Notify,
        release: tokio::sync::Notify,
        kept: Mutex<Vec<Sample>>,
    }
    #[async_trait::async_trait]
    impl Recorder for Timings {
        async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
            if samples.iter().any(|s| s.class == Class::Write) {
                self.ready.notify_one();
                self.release.notified().await;
            }
            self.kept.lock().unwrap().extend_from_slice(samples);
            for sample in samples {
                println!("TIMING_SAMPLE {}", serde_json::to_string(sample).unwrap());
            }
            Ok(())
        }
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let process_shutdown = ShutdownGuard::hold();
        let gate = Arc::new(
            MachineGate::new(
                "latency-signal-fixture",
                &[],
                Reading::new(Mode::Off, Slots::default()),
            )
            .with_evidence(Evidence::spawn(
                Gate::MachineGate,
                "latency-signal-fixture",
                Arc::new(EvidenceRecorder),
            )),
        );
        let timings = Arc::new(Timings {
            ready: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            kept: Mutex::new(Vec::new()),
        });
        let (timing, worker) = Timing::start(
            timings.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(chrono::Utc::now),
            |method, path| (method == "POST" && path == "/fixture").then_some(Class::Write),
        );
        let entered = Arc::new(tokio::sync::Notify::new());
        let release_handler = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().route(
            "/fixture",
            post({
                let entered = entered.clone();
                let release = release_handler.clone();
                move || {
                    let entered = entered.clone();
                    let release = release.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::http::StatusCode::CREATED
                    }
                }
            }),
        );
        let app =
            boss_core::request_latency::mount(boss_core::machine_gate::gated(app, gate), timing);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = stopped.changed().await;
            })
            .await
            .unwrap();
        });
        let client = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{address}/fixture"))
                .send()
                .await
                .unwrap()
                .status()
        });
        entered.notified().await;
        {
            let requested = process_shutdown.requested();
            tokio::pin!(requested);
            assert!(futures::poll!(&mut requested).is_pending());
            // Signal registration settling belongs to readiness, not the5s work bound.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            boss_testing::reexec::ready();
            requested.await;
        }
        let _ = stop.send(true);
        release_handler.notify_one();
        server.await.unwrap();
        assert_eq!(client.await.unwrap(), reqwest::StatusCode::CREATED);
        println!("HTTP_JOINED");
        timings.ready.notified().await;
        timings.release.notify_one();
        worker.shutdown().await;
        println!("QUEUE_JOINED");
        process_shutdown.finish().await;
    });
}
