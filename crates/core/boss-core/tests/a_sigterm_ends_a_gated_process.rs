//! SIGTERM ends a process holding gate evidence, promptly, and states
//! its end first (design 21946380; review c49cb4e1, S4 and N1).
//!
//! WHY A CHILD PROCESS. Registering a SIGTERM listener replaces the
//! signal's default for the whole process, so `gate_evidence` owns the
//! exit of every gated binary. Two ways that can go wrong cannot be seen
//! from inside one test process: a process whose hand-off task is gone
//! (panicked, or its runtime dropped) would ignore SIGTERM until the
//! kubelet's SIGKILL 30 s later; and a process whose end is never stated
//! reads as a broken watch on every restart. So each case re-runs THIS
//! test binary as a child (the case named in `CHILD`), sends it a real
//! SIGTERM, and reads its exit and what its recorder printed.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::gate_evidence::{Evidence, Fact, Gate};
use boss_core::port::EventRecorder;
use serde_json::json;

/// The env var that makes this binary the child, naming its case.
const CHILD: &str = "BOSS_GATE_EVIDENCE_SIGTERM_CHILD";

/// Prints every fact it records, one line each, so the parent can read
/// what reached "the log".
struct Printing;

#[async_trait]
impl EventRecorder for Printing {
    async fn record(&self, e: &Event) -> Result<(), String> {
        println!("recorded {} clean={}", e.kind, e.payload["clean"]);
        Ok(())
    }
}

/// Panics on its first write: the hand-off's task dies with it.
struct Panicking;

#[async_trait]
impl EventRecorder for Panicking {
    async fn record(&self, _: &Event) -> Result<(), String> {
        panic!("the recorder fell over");
    }
}

/// The child: a hand-off over `recorder`, one fact, then idle until
/// SIGTERM. Prints `ready` once the fact has been handed over.
fn child(recorder: Arc<dyn EventRecorder>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        let evidence = Evidence::spawn(Gate::MachineGate, "suite-child", recorder);
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        // Time for the task to take the fact (or to die on it), and for
        // the watcher thread to register its listener.
        tokio::time::sleep(Duration::from_millis(800)).await;
        println!("ready");
        tokio::time::sleep(Duration::from_secs(60)).await;
        println!("still alive");
    });
}

/// Run `case` as a child, SIGTERM it once it is ready, and answer how
/// long it took to exit, its exit code, and every line it printed.
fn terminate(case: &str, test: &str) -> (Duration, Option<i32>, Vec<String>) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, case)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !lines.iter().any(|l: &String| l.ends_with("ready")) {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("the child never got ready: {lines:?}"));
        lines.push(line);
    }
    let sent = Instant::now();
    let status = Command::new("sh")
        .args(["-c", &format!("kill -TERM {}", child.id())])
        .status()
        .unwrap();
    assert!(status.success(), "kill -TERM failed");
    let exit = child.wait().unwrap();
    let took = sent.elapsed();
    lines.extend(rx.try_iter());
    (took, exit.code(), lines)
}

/// A healthy hand-off at SIGTERM drains what it holds, states a clean
/// `recording_ended`, and exits 143 within moments — not the 10 s grace,
/// and nowhere near the 30 s SIGKILL.
#[test]
fn sigterm_states_a_clean_end_and_exits() {
    if std::env::var(CHILD).as_deref() == Ok("healthy") {
        child(Arc::new(Printing));
        return;
    }
    let (took, code, lines) = terminate("healthy", "sigterm_states_a_clean_end_and_exits");
    assert_eq!(code, Some(143), "{lines:?}");
    assert!(took < Duration::from_secs(5), "took {took:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("recorded machine_gate.would_refuse clean=null")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("recorded machine_gate.recording_ended clean=true")),
        "the end is stated before the exit: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.ends_with("still alive")),
        "{lines:?}"
    );
}

/// Review c49cb4e1, S4: a hand-off whose task is GONE — here its
/// recorder panicked — holds nothing open, so SIGTERM exits at once
/// rather than waiting on a task that will never finish.
#[test]
fn a_hand_off_that_is_gone_does_not_hold_sigterm() {
    if std::env::var(CHILD).as_deref() == Ok("gone") {
        child(Arc::new(Panicking));
        return;
    }
    let (took, code, lines) = terminate("gone", "a_hand_off_that_is_gone_does_not_hold_sigterm");
    assert_eq!(code, Some(143), "{lines:?}");
    assert!(
        took < Duration::from_secs(5),
        "a gone hand-off must not hold SIGTERM to the grace: took {took:?}"
    );
}
