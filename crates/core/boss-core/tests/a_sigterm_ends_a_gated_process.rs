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

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::gate_evidence::{Evidence, Fact, Gate};
use boss_core::port::EventRecorder;
use boss_testing::reexec::{self, ReExec};
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
/// SIGTERM. Says `reexec::ready()` once the fact has been handed over —
/// NOT as its first line, because the parent may signal it only once
/// the listener is registered. So the hand-off here is judged with the
/// child's start, by `START_CEILING`: a hand-off wedged before it is a
/// red with a sentence, never a hang (review 12a4ee82, B1).
fn child(recorder: Arc<dyn EventRecorder>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        let evidence = Evidence::spawn(Gate::MachineGate, "suite-child", recorder);
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        // Time for the task to take the fact (or to die on it), and for
        // the watcher thread to register its listener.
        tokio::time::sleep(Duration::from_millis(800)).await;
        reexec::ready();
        tokio::time::sleep(Duration::from_secs(60)).await;
        println!("still alive");
    });
}

/// How long the child has to exit once SIGTERMed before it is killed
/// and the case reds: the kubelet's own grace before SIGKILL. The cases
/// assert far less (5 s); this only keeps a child that ignores SIGTERM
/// from holding the test to its 60 s idle.
const SIGTERM_GRACE: Duration = Duration::from_secs(30);

/// Run `case` as a child, SIGTERM it once it is ready, and answer how
/// long it took to exit, its exit code, and every line it printed.
///
/// Through `boss_testing::reexec`, the one definition of this wait
/// (backlog 7e30ddb0): the start — this whole test binary's exec and
/// page-in, then the hand-off before `ready()` — is judged by liveness
/// under `reexec::START_CEILING`, never by a clock that a loaded host's
/// start can outrun. The bound this test exists for, SIGTERM to exit,
/// starts after. Review 12a4ee82 B1: an inline copy here had dropped
/// the ceiling, so a hand-off wedged before `ready` hung the test.
fn terminate(case: &str, test: &str) -> (Duration, Option<i32>, Vec<String>) {
    let mut re = ReExec::new(test);
    re.command().arg("--test-threads=1").env(CHILD, case);
    let mut run = re.spawn();
    if run.ready().is_err() {
        let done = run.finish(Duration::ZERO);
        panic!("the child ended before it got ready: {done:?}");
    }
    let sent = Instant::now();
    let status = Command::new("sh")
        .args(["-c", &format!("kill -TERM {}", run.child().id())])
        .status()
        .unwrap();
    assert!(status.success(), "kill -TERM failed");
    let done = run.finish(SIGTERM_GRACE);
    let took = sent.elapsed();
    let lines = done.stdout.lines().map(str::to_string).collect();
    (took, done.status.and_then(|s| s.code()), lines)
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
