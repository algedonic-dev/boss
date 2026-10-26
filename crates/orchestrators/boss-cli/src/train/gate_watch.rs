//! The gate-end watch: a verdict is recorded when the gate ENDS, not on
//! the next reconcile.
//!
//! WHY (backlog 301d6c3b, 2026-10-17). The runner of the pod-log layout
//! writes nothing to the system of record (design bdc60b65: a pod that
//! runs branch code holds no estate token), so the conductor records its
//! verdict from the pod log (`carried_verdict`). It did that ONLY on its
//! train-reconcile pass, every two minutes. Measured over the 35 closed
//! gate-runs of 2026-10-18T22:25Z..2026-10-19T05:22Z: the record-verdict
//! step completed a median 85 s, max 147 s, after the gate container
//! ended (`recorded_by.ended_at` against the step's `completed_at`) —
//! pure waiting, on every one of ~140 gates a day, with a builder or a
//! train blocked behind each.
//!
//! WHAT THE GATE JOB GIVES, AND WHERE. The end of a gate is a fact of the
//! CLUSTER: the `gate` container's `terminated` state on its pod (an INIT
//! container on the judged-init layout, design b4bf8bba, so it ends
//! before the Job does — the Job's own `Complete` condition waits for the
//! seed-refresh helper and is the wrong instant). Nothing of it reaches
//! the jobs API: the runner cannot write there by design, so there is no
//! `jobs.*` event a dispatcher rule could select, which is the shape the
//! deploy runner's `rolled` report took (backlog 090720c3). What IS
//! observable is the Kubernetes watch on the gate pods, which the
//! conductor's ServiceAccount already holds (`dev-session-gates`: pods
//! get/list/watch). So the conductor watches.
//!
//! WHAT THE WATCH DECIDES: NOTHING. An event says only "the gate
//! container of Job X has ended". The watch then runs the SAME recording
//! the reconcile pass runs (`Conductor::record_gate_end`), for that one
//! Job: the verdict comes from the receipt frame the termination message
//! names, read from the pod log and judged by `carried_verdict::judge` —
//! never from the pod's status, which is only the trigger.
//!
//! RECONCILE STAYS THE SAFETY NET, unchanged. A gate that ends while the
//! watch is down, restarting, or failing to read is recorded by the next
//! reconcile exactly as before; the watch only makes the common case
//! early. A watch that cannot start says so (hourly while it lasts, and
//! when it recovers) and costs nothing else.
//!
//! ONE VERDICT, NOT TWO. The watch runs in the cadence process and the
//! reconcile pass in its own child process, so both may reach one packet
//! at once. Each takes [`VERDICT_LOCK_FILE`] in the conductor's home
//! around its read-judge-write of one packet, and re-reads the packet
//! inside it: the second sees the step done and writes nothing.

use super::*;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

/// The lock file, in the conductor's home, that serialises the two
/// recorders' read-then-write of one packet.
pub(crate) const VERDICT_LOCK_FILE: &str = "gate-verdict.lock";

/// How long a recorder waits for that lock before leaving the packet to
/// its next pass. A holder reads at most two bounded kubectl reads
/// ([`KUBECTL_PROCESS_TIMEOUT_SECS`] each) and writes two steps.
pub(crate) const VERDICT_LOCK_WAIT: Duration = Duration::from_secs(90);

/// The longest one watch runs before it is restarted. A watch whose
/// connection died without an error says nothing; restarting bounds how
/// long such a silence can last, and the reconcile pass covers the gap.
pub(crate) const WATCH_ROTATE: Duration = Duration::from_secs(600);

/// The most stdout one watch may hold without a whole event in it. A pod
/// object is tens of kilobytes; more than this is not a watch event.
pub(crate) const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

/// How many ended pods the watch remembers having handed over, so the
/// several MODIFIED events every ended pod still emits (helper start,
/// helper end, phase, finaliser) cost one recording, not five.
pub(crate) const SEEN_CAPACITY: usize = 512;

/// `BOSS_TRAIN_GATE_WATCH=0` turns the watch off; anything else, or
/// unset, leaves it on. The kill switch is for an operator debugging the
/// recorder, as `BOSS_TRAIN_AUTO_CANCEL` is for the cancel.
pub(crate) const WATCH_ENV: &str = "BOSS_TRAIN_GATE_WATCH";

/// The recorder names a receipt's `recorded_by.recorder` carries.
pub(crate) const RECONCILE_RECORDER: &str = "boss train cadence (conductor reconcile)";
pub(crate) const WATCH_RECORDER: &str = "boss train cadence (gate-end watch)";

/// One gate whose `gate` container has ended, as a watch event names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateEnd {
    /// The Job the pod belongs to (the kubelet's `job-name` label).
    pub job: String,
    /// The pod's uid: what the watch remembers, so one end is handed
    /// over once.
    pub pod_uid: String,
}

/// PURE: the ended gate a watch event names, or `None`. An `ADDED` or
/// `MODIFIED` pod of `app=gate-runner` whose `gate` container — where
/// its own spec declares it — has a `terminated` state.
pub(crate) fn gate_end(event: &Value) -> Option<GateEnd> {
    if !matches!(
        event.get("type").and_then(Value::as_str),
        Some("ADDED" | "MODIFIED")
    ) {
        return None;
    }
    let pod = event.get("object")?;
    let labels = pod.pointer("/metadata/labels")?;
    if labels.get("app").and_then(Value::as_str) != Some("gate-runner") {
        return None;
    }
    let job = ["batch.kubernetes.io/job-name", "job-name"]
        .iter()
        .find_map(|k| labels.get(*k).and_then(Value::as_str))
        .filter(|j| !j.is_empty())?;
    // The recorder's own reading of a pod, so "ended" means here what it
    // means to `judge`: the gate's status, from the list its spec names.
    gate_pod_from_json(&json!({ "items": [pod] }))
        .ended
        .as_ref()?;
    Some(GateEnd {
        job: job.to_string(),
        pod_uid: pod
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// PURE: the whole JSON values at the front of `pending`, removed from
/// it, and what could not be read; a value still arriving stays. Text
/// that can never become a value is dropped to the end of its line and
/// said, so the events after it on the same read are still taken; a
/// pending value over [`MAX_PENDING_BYTES`] is dropped and said.
pub(crate) fn take_events(pending: &mut Vec<u8>) -> (Vec<Value>, Vec<String>) {
    let mut values = Vec::new();
    let mut refused = Vec::new();
    loop {
        let mut used = 0;
        let mut bad = None;
        let mut stream = serde_json::Deserializer::from_slice(pending).into_iter::<Value>();
        loop {
            match stream.next() {
                None => break,
                Some(Ok(v)) => {
                    values.push(v);
                    used = stream.byte_offset();
                }
                Some(Err(e)) if e.is_eof() => break,
                Some(Err(e)) => {
                    bad = Some(format!("the watch printed something that is not JSON: {e}"));
                    break;
                }
            }
        }
        let Some(why) = bad else {
            pending.drain(..used);
            break;
        };
        refused.push(why);
        // Resynchronise on the end of the bad text's own line: kubectl
        // ends every event with one.
        let start = used
            + pending[used..]
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(0);
        match pending[start..].iter().position(|b| *b == b'\n') {
            Some(nl) => {
                pending.drain(..start + nl + 1);
            }
            None => {
                pending.clear();
                break;
            }
        }
    }
    if pending.len() > MAX_PENDING_BYTES {
        refused.push(format!(
            "{} bytes of the watch held no whole event (over {MAX_PENDING_BYTES})",
            pending.len()
        ));
        pending.clear();
    }
    (values, refused)
}

/// The ended pods already handed over, oldest forgotten first.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    uids: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// Is this the first time `uid` is seen? Remembers it either way.
    pub(crate) fn first_time(&mut self, uid: &str) -> bool {
        if self.uids.contains(uid) {
            return false;
        }
        while self.order.len() >= SEEN_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.uids.remove(&old);
            }
        }
        self.uids.insert(uid.to_string());
        self.order.push_back(uid.to_string());
        true
    }
}

/// What the lock attempt came to.
#[derive(Debug)]
pub(crate) enum VerdictLock {
    /// Held until dropped.
    Held(File),
    /// The lock file cannot be opened: record without it, as every pass
    /// did before the watch, and say why.
    Unavailable(String),
    /// Another recorder held it for the whole budget.
    Busy,
}

/// Take [`VERDICT_LOCK_FILE`] in `home`, waiting up to `budget`.
///
/// A flock, so it is released with the holder's process however that
/// ends, and polled rather than blocked on, so the wait never holds a
/// worker of the runtime. Held only around one packet: the reconcile
/// pass takes and drops it per packet, so the watch waits at most one
/// packet's recording, never a whole pass.
pub(crate) async fn verdict_lock(home: &Path, budget: Duration) -> VerdictLock {
    let path = home.join(VERDICT_LOCK_FILE);
    let file = match fs::create_dir_all(home).and_then(|()| {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
    }) {
        Ok(f) => f,
        Err(e) => return VerdictLock::Unavailable(format!("{}: {e}", path.display())),
    };
    let since = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return VerdictLock::Held(file),
            Err(TryLockError::WouldBlock) if since.elapsed() < budget => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(TryLockError::WouldBlock) => return VerdictLock::Busy,
            Err(TryLockError::Error(e)) => {
                return VerdictLock::Unavailable(format!("{}: {e}", path.display()));
            }
        }
    }
}

/// The watch's argv after the program: `-n <namespace>` first, only the
/// changes from now on (`--watch-only`: a gate that ended before the
/// watch started is the reconcile pass's), each as a typed event. No
/// client-config override: the conductor has no kubeconfig and kubectl
/// drops its in-cluster fallback when one is set (backlog 06ae925a).
pub(crate) fn watch_args(namespace: &str) -> Vec<String> {
    [
        "-n",
        namespace,
        "get",
        "pods",
        "-l",
        "app=gate-runner",
        "--watch-only",
        "--output-watch-events",
        "-o",
        "json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// How one watch ended, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchEnd {
    /// It ran its [`WATCH_ROTATE`] and was stopped, to be started again.
    Rotated,
    /// kubectl exited 0: the server closed the watch.
    Closed,
}

/// One watch: run `program` with [`watch_args`], hand each gate end not
/// already in `seen` to `on_end`, until kubectl exits or `rotate` has
/// passed. `Err` when it cannot start or exits non-zero, carrying what
/// it said on stderr — the caller says it and starts another.
pub(crate) async fn watch_once(
    program: &str,
    namespace: &str,
    rotate: Duration,
    seen: &mut Seen,
    on_end: &mut (dyn FnMut(GateEnd) + Send),
) -> Result<WatchEnd> {
    use tokio::io::AsyncReadExt;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(watch_args(namespace))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // fork/exec off the runtime, as the recorder's own reads do.
    let mut child = tokio::task::spawn_blocking(move || cmd.spawn())
        .await
        .map_err(anyhow::Error::from)?
        .with_context(|| format!("starting {program} get pods --watch"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("{program} started with no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("{program} started with no stderr"))?;
    // Drained beside stdout so a chatty stderr can never fill its pipe
    // and stall the watch; only the first 4 KiB is kept, to be said.
    let said = tokio::spawn(async move {
        let mut all = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            if all.len() < 4096 {
                all.extend_from_slice(&chunk[..n.min(4096 - all.len())]);
            }
        }
        String::from_utf8_lossy(&all).trim().to_string()
    });
    let deadline = tokio::time::Instant::now() + rotate;
    let mut pending = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = match tokio::time::timeout_at(deadline, stdout.read(&mut chunk)).await {
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Ok(WatchEnd::Rotated);
            }
            Ok(read) => read.context("reading the gate pod watch")?,
        };
        if n == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..n]);
        let (events, refused) = take_events(&mut pending);
        for why in refused {
            log(format!(
                "gate-end watch: dropped what it could not read ({why}); reading on — \
                 reconcile records anything missed"
            ));
        }
        for end in events.iter().filter_map(gate_end) {
            if seen.first_time(&end.pod_uid) {
                on_end(end);
            }
        }
    }
    let status = child
        .wait()
        .await
        .context("waiting on the gate pod watch")?;
    let stderr = said.await.unwrap_or_default();
    if status.success() {
        return Ok(WatchEnd::Closed);
    }
    bail!(
        "{program} get pods --watch exited {}: {}",
        status
            .code()
            .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
        if stderr.is_empty() {
            "(nothing on stderr)"
        } else {
            stderr.as_str()
        }
    )
}

/// PURE: how long to wait before the next watch after `failures` in a
/// row: 2 s, doubling, at most a minute. After a watch that ran, at once
/// (one second), since the gap is what reconcile has to cover.
pub(crate) fn retry_pause(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::from_secs(1);
    }
    Duration::from_secs((1u64 << failures.min(6)).min(60))
}

/// A failing watch, as said so far.
struct Failing {
    since: Instant,
    said: Instant,
    cause: String,
}

/// PURE but for the clock it is handed: the line, if any, a failure or a
/// recovery earns — when it starts, when its cause changes, and again
/// every [`RESAY_MINUTES`] while it lasts (the recorder's own rule for a
/// cluster read, `cluster_read_line`); and once, when a watch runs again.
fn failure_line(
    failing: &mut Option<Failing>,
    failure: Option<&str>,
    now: Instant,
) -> Option<String> {
    let resay = Duration::from_secs(RESAY_MINUTES as u64 * 60);
    match (failure, failing.as_mut()) {
        (Some(why), None) => {
            *failing = Some(Failing {
                since: now,
                said: now,
                cause: cause_of(why),
            });
            Some(format!(
                "gate-end watch: cannot watch the gate pods — verdicts wait for the reconcile \
                 pass until it can (said again every {RESAY_MINUTES} min while it lasts): {why}"
            ))
        }
        (Some(why), Some(f)) => {
            let changed = f.cause != cause_of(why);
            if !changed && now.duration_since(f.said) < resay {
                return None;
            }
            f.said = now;
            f.cause = cause_of(why);
            Some(format!(
                "gate-end watch: STILL cannot watch the gate pods, for {}s{} — verdicts wait for \
                 the reconcile pass: {why}",
                now.duration_since(f.since).as_secs(),
                if changed {
                    ", and the cause CHANGED"
                } else {
                    ""
                }
            ))
        }
        (None, Some(f)) => {
            let lasted = now.duration_since(f.since).as_secs();
            *failing = None;
            Some(format!(
                "gate-end watch: watching the gate pods again, after {lasted}s unable to"
            ))
        }
        (None, None) => None,
    }
}

/// The watch, for as long as this process lives: started beside the
/// cadence loop (`cadence::run`), never inside a verb. It records
/// through a conductor of its own, built from the same environment the
/// verbs read, and each gate end is recorded on a task of its own so a
/// slow recording never stalls the stream. NOTHING HERE IS FATAL: a
/// conductor that cannot be built, or a watch that cannot run, is said,
/// and the reconcile pass records exactly as it did before the watch.
pub(crate) async fn watch_gate_ends() {
    if std::env::var(WATCH_ENV).as_deref() == Ok("0") {
        log(format!(
            "gate-end watch: off ({WATCH_ENV}=0) — gate verdicts are recorded by the reconcile \
             pass alone"
        ));
        return;
    }
    let cfg = Config::from_env(false);
    let namespace = cfg.gate_namespace.clone();
    let conductor = match make_forge(&cfg).and_then(|forge| Conductor::new(cfg, forge)) {
        Ok(c) => c,
        Err(e) => {
            log(format!(
                "gate-end watch: NOT started — its conductor could not be built ({e:#}); gate \
                 verdicts are recorded by the reconcile pass alone"
            ));
            return;
        }
    };
    let policy = conductor.resolve_policy().await;
    let conductor = std::sync::Arc::new(conductor.with_policy(policy));
    log(format!(
        "gate-end watch: watching the gate pods in {namespace}; a gate's verdict is recorded \
         when its gate container ends, and the reconcile pass stays the safety net"
    ));
    let mut seen = Seen::default();
    let mut failing: Option<Failing> = None;
    let mut failures = 0u32;
    loop {
        let mut on_end = |end: GateEnd| {
            let conductor = conductor.clone();
            tokio::spawn(async move {
                if let Err(e) = conductor.record_gate_end(&end.job).await {
                    log(format!(
                        "gate-end watch: Job {} ended but its verdict was not recorded ({e:#}) — \
                         the reconcile pass records it",
                        end.job
                    ));
                }
            });
        };
        let outcome = watch_once("kubectl", &namespace, WATCH_ROTATE, &mut seen, &mut on_end).await;
        let failure = outcome.as_ref().err().map(|e| format!("{e:#}"));
        if let Some(line) = failure_line(&mut failing, failure.as_deref(), Instant::now()) {
            log(line);
        }
        failures = if failure.is_some() { failures + 1 } else { 0 };
        tokio::time::sleep(retry_pause(failures)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch event for a gate pod, shaped as `kubectl get pods -w
    /// --output-watch-events -o json` printed it on 2026-10-19 for gate
    /// d316b2a6 (judged-init: `gate` an init container, `seed-refresh`
    /// the regular one), trimmed to the fields the reader looks at.
    fn event(kind: &str, gate_state: Value, init: bool) -> Value {
        let gate = json!({"name": "gate", "state": gate_state});
        let helper =
            json!({"name": "seed-refresh", "state": {"waiting": {"reason": "PodInitializing"}}});
        let (spec, status) = if init {
            (
                json!({"initContainers": [{"name": "postgres"}, {"name": "gate"}],
                       "containers": [{"name": "seed-refresh"}]}),
                json!({"phase": "Pending",
                       "initContainerStatuses": [{"name": "postgres", "state": {"running": {}}}, gate],
                       "containerStatuses": [helper]}),
            )
        } else {
            (
                json!({"containers": [{"name": "gate"}]}),
                json!({"phase": "Running", "containerStatuses": [gate]}),
            )
        };
        json!({"type": kind, "object": {
            "kind": "Pod",
            "metadata": {
                "name": "gate-d316b2a64ee148798fe9ebedba5278c5-b4gcg",
                "uid": "pod-uid-1",
                "labels": {"app": "gate-runner",
                           "batch.kubernetes.io/job-name": "gate-d316b2a64ee148798fe9ebedba5278c5",
                           "job-name": "gate-d316b2a64ee148798fe9ebedba5278c5"}},
            "spec": spec,
            "status": status}})
    }

    fn terminated() -> Value {
        json!({"terminated": {"exitCode": 0, "reason": "Completed",
                              "finishedAt": "2026-10-19T05:33:13Z",
                              "message": "gate-runner: receipt-end v1 job=gate-d316b2a64ee148798fe9ebedba5278c5 verdict=green bytes=1 sha256=00\n"}})
    }

    /// The trigger: a gate container that has ended, on either layout,
    /// names its Job and its pod.
    #[test]
    fn an_ended_gate_container_names_its_job() {
        for init in [true, false] {
            for kind in ["ADDED", "MODIFIED"] {
                assert_eq!(
                    gate_end(&event(kind, terminated(), init)),
                    Some(GateEnd {
                        job: "gate-d316b2a64ee148798fe9ebedba5278c5".into(),
                        pod_uid: "pod-uid-1".into(),
                    }),
                    "{kind}, judged-init {init}"
                );
            }
        }
    }

    /// Everything else is not a gate end: a gate still running, a deleted
    /// pod (reconcile's lost floor owns that), a pod that is not a gate,
    /// one with no Job, and a container NAMED gate that the spec does not
    /// declare where its status sits.
    #[test]
    fn nothing_but_an_ended_gate_container_is_a_gate_end() {
        assert_eq!(
            gate_end(&event("MODIFIED", json!({"running": {}}), true)),
            None
        );
        assert_eq!(gate_end(&event("DELETED", terminated(), true)), None);
        let mut other = event("MODIFIED", terminated(), true);
        other["object"]["metadata"]["labels"]["app"] = json!("something-else");
        assert_eq!(gate_end(&other), None);
        let mut jobless = event("MODIFIED", terminated(), true);
        jobless["object"]["metadata"]["labels"] = json!({"app": "gate-runner"});
        assert_eq!(gate_end(&jobless), None);
        let mut moved = event("MODIFIED", terminated(), true);
        moved["object"]["spec"] = json!({"containers": [{"name": "gate"}]});
        assert_eq!(
            gate_end(&moved),
            None,
            "the end is read where the spec says"
        );
        assert_eq!(
            gate_end(&json!({"type": "ERROR", "object": {"kind": "Status"}})),
            None
        );
    }

    /// The stream: kubectl prints one JSON value per event, compact or
    /// indented, and a read may stop anywhere inside one.
    #[test]
    fn whole_events_are_taken_and_a_partial_one_waits() {
        let a = event("MODIFIED", terminated(), true).to_string();
        let b =
            serde_json::to_string_pretty(&event("ADDED", json!({"running": {}}), false)).unwrap();
        let text = format!("{a}\n{b}\n{a}");
        let cut = text.len() - 7;
        let mut pending = text.as_bytes()[..cut].to_vec();
        let (got, refused) = take_events(&mut pending);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(got.len(), 2, "two whole events, the third still arriving");
        assert_eq!(got[0]["type"], "MODIFIED");
        assert_eq!(got[1]["type"], "ADDED");
        pending.extend_from_slice(&text.as_bytes()[cut..]);
        let (rest, _) = take_events(&mut pending);
        assert_eq!(rest.len(), 1);
        assert!(
            pending.iter().all(u8::is_ascii_whitespace),
            "nothing left over"
        );
    }

    /// A line that can never be an event is dropped and said, and the
    /// events after it on the same read are still taken; an event that
    /// never ends is dropped and said.
    #[test]
    fn garbage_and_an_endless_event_are_refused_and_dropped() {
        let a = event("MODIFIED", terminated(), true).to_string();
        let mut pending = format!("{a}\nerror: You must be logged in\n{a}\n").into_bytes();
        let (got, refused) = take_events(&mut pending);
        assert_eq!(got.len(), 2, "the event after the bad line is still read");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(pending.iter().all(u8::is_ascii_whitespace));
        let mut endless = vec![b'{'];
        endless.extend(std::iter::repeat_n(b' ', MAX_PENDING_BYTES));
        let (none, refused) = take_events(&mut endless);
        assert!(none.is_empty() && refused.len() == 1, "{refused:?}");
        assert!(endless.is_empty());
    }

    /// One end, one hand-over — and a bounded memory of it.
    #[test]
    fn an_end_is_handed_over_once_and_memory_is_bounded() {
        let mut seen = Seen::default();
        assert!(seen.first_time("a"));
        assert!(!seen.first_time("a"));
        for n in 0..SEEN_CAPACITY {
            seen.first_time(&format!("x{n}"));
        }
        assert!(seen.uids.len() <= SEEN_CAPACITY);
        assert!(seen.first_time("a"), "the oldest is forgotten first");
        assert!(!seen.first_time(&format!("x{}", SEEN_CAPACITY - 1)));
    }

    /// The lock: one holder at a time, a waiter leaves at its budget, and
    /// the next taker gets it once the holder lets it go.
    #[tokio::test]
    async fn one_recorder_holds_the_verdict_lock_at_a_time() {
        let home = boss_testing::scratch_dir("gate-watch-verdict-lock");
        let VerdictLock::Held(held) = verdict_lock(&home, Duration::from_millis(50)).await else {
            panic!("a free lock was not taken");
        };
        assert!(matches!(
            verdict_lock(&home, Duration::from_millis(200)).await,
            VerdictLock::Busy
        ));
        // A flock belongs to the open file description, and every fork in
        // this test binary copies the holder's descriptor into the child
        // until it execs — the exec-handshake tests in carried_verdict.rs
        // hold children in pre_exec on purpose. Released by `drop`, the
        // lock stayed held while such a copy lived and the 50 ms budget
        // read Busy (6ff2ab2d, the same mechanism as a9ae095b in
        // entry.rs). The copy is planted here so the release is judged
        // with it alive: an explicit unlock releases the lock for every
        // copy.
        let inherited = held.try_clone().unwrap();
        held.unlock().unwrap();
        let next = verdict_lock(&home, Duration::from_millis(50)).await;
        assert!(matches!(next, VerdictLock::Held(_)), "{next:?}");
        drop(inherited);
    }

    /// A stand-in for kubectl in this test's own scratch directory that
    /// records its argv to `args` and then runs `body`. Written through
    /// `write_exec` (ETXTBSY, backlog 6eaef658).
    fn stand_in(name: &str, body: &str) -> (std::path::PathBuf, String) {
        let dir = boss_testing::scratch_dir(name);
        let program = dir.join("kubectl");
        boss_testing::write_exec(
            &program,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$(dirname \"$0\")/args\"\n{body}\n"
            ),
        );
        let path = program.display().to_string();
        (dir, path)
    }

    /// The watch end to end against a stand-in: each gate end handed over
    /// ONCE however many events repeat it, a running gate and garbage
    /// handed over never, and a clean exit read as the server closing.
    /// The argv is the one `watch_args` states, whole.
    #[tokio::test]
    async fn a_watch_hands_each_gate_end_over_once() {
        let running = event("MODIFIED", json!({"running": {}}), true);
        let ended = event("MODIFIED", terminated(), true);
        let mut other = event("ADDED", terminated(), false);
        other["object"]["metadata"]["uid"] = json!("pod-uid-2");
        other["object"]["metadata"]["labels"]["job-name"] = json!("gate-other");
        other["object"]["metadata"]["labels"]["batch.kubernetes.io/job-name"] = json!("gate-other");
        let dir = boss_testing::scratch_dir("gate-watch-stream-events");
        let stream = format!(
            "{running}\n{ended}\n{}\nnot json at all\n{ended}\n{other}\n",
            serde_json::to_string_pretty(&ended).unwrap()
        );
        std::fs::write(dir.join("events"), stream).unwrap();
        let (sdir, program) = stand_in(
            "gate-watch-stream",
            &format!("cat '{}'", dir.join("events").display()),
        );
        let mut seen = Seen::default();
        let mut got = Vec::new();
        let mut on_end = |end: GateEnd| got.push(end);
        let how = watch_once(
            &program,
            "boss-dev",
            Duration::from_secs(20),
            &mut seen,
            &mut on_end,
        )
        .await
        .unwrap();
        assert_eq!(how, WatchEnd::Closed);
        let jobs: Vec<&str> = got.iter().map(|e| e.job.as_str()).collect();
        assert_eq!(
            jobs,
            ["gate-d316b2a64ee148798fe9ebedba5278c5", "gate-other"],
            "one hand-over per ended pod"
        );
        let args = std::fs::read_to_string(sdir.join("args")).unwrap();
        assert_eq!(args.lines().collect::<Vec<_>>(), watch_args("boss-dev"));
    }

    /// A watch that fails says what kubectl said; one that runs its
    /// course is stopped at the rotation, not left running.
    #[tokio::test]
    async fn a_failed_watch_says_why_and_a_long_one_is_rotated() {
        let (_d, refused) = stand_in(
            "gate-watch-refused",
            "echo 'Error from server (Forbidden): pods is forbidden' >&2; exit 1",
        );
        let mut seen = Seen::default();
        let mut on_end = |_: GateEnd| panic!("nothing ended");
        let err = watch_once(
            &refused,
            "boss-dev",
            Duration::from_secs(20),
            &mut seen,
            &mut on_end,
        )
        .await
        .unwrap_err();
        let said = format!("{err:#}");
        assert!(
            said.contains("exited 1") && said.contains("Forbidden"),
            "{said}"
        );

        let (_d, hangs) = stand_in("gate-watch-hangs", "exec sleep 60");
        let started = Instant::now();
        let how = watch_once(
            &hangs,
            "boss-dev",
            Duration::from_millis(300),
            &mut seen,
            &mut on_end,
        )
        .await
        .unwrap();
        assert_eq!(how, WatchEnd::Rotated);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    /// The watch's argv carries no client-config override (backlog
    /// 06ae925a): with no kubeconfig, any of them sends kubectl to
    /// localhost:8080 instead of the pod's ServiceAccount.
    #[test]
    fn the_watch_rides_the_in_cluster_config() {
        let args = watch_args("boss-dev");
        assert_eq!(&args[..2], ["-n", "boss-dev"]);
        assert!(args.contains(&"--watch-only".to_string()));
        // Names without their dashes, so this list is not itself a hit of
        // the source pin in `carried_verdict`'s tests.
        let overrides = [
            "request-timeout",
            "server",
            "kubeconfig",
            "context",
            "as",
            "user",
        ];
        for a in &args {
            let name = a
                .trim_start_matches('-')
                .split('=')
                .next()
                .unwrap_or_default();
            assert!(
                !(a.starts_with("--") && overrides.contains(&name)) && a != "-s",
                "{a}"
            );
        }
    }

    /// Said when it starts, not again inside the hour unless the cause
    /// changes, again on the hour, and once when it recovers.
    #[test]
    fn a_failing_watch_is_said_when_it_starts_changes_lasts_and_ends() {
        let t0 = Instant::now();
        let mut failing = None;
        assert!(
            failure_line(&mut failing, None, t0).is_none(),
            "quiet when well"
        );
        let first = failure_line(&mut failing, Some("E1019 05:00:01 forbidden"), t0).unwrap();
        assert!(first.contains("cannot watch"), "{first}");
        let later = t0 + Duration::from_secs(120);
        assert!(
            failure_line(&mut failing, Some("E1019 05:02:01 forbidden"), later).is_none(),
            "the same cause, folded, inside the hour"
        );
        let changed = failure_line(&mut failing, Some("connection refused"), later).unwrap();
        assert!(changed.contains("CHANGED"), "{changed}");
        let hour = later + Duration::from_secs(RESAY_MINUTES as u64 * 60);
        assert!(failure_line(&mut failing, Some("connection refused"), hour).is_some());
        let back = failure_line(&mut failing, None, hour).unwrap();
        assert!(back.contains("again"), "{back}");
        assert!(failing.is_none());
    }

    /// `cadence::run` has no test of its own (it is the supervised loop),
    /// so its one line starting the watch is held here: beside the loop,
    /// on its own task, and not in a dry run.
    #[test]
    fn the_cadence_loop_starts_the_watch_on_its_own_task() {
        let src =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cadence.rs"))
                .unwrap();
        let run = &src[src.find("pub async fn run(once: bool, dry: bool)").unwrap()..];
        let start = run
            .find("tokio::spawn(train::watch_gate_ends())")
            .expect("the cadence loop starts the gate-end watch");
        assert!(run[..start].contains("if !dry {"), "only outside a dry run");
        assert!(
            start < run.find("loop {").unwrap(),
            "started once, before the loop, not per tick"
        );
    }

    #[test]
    fn the_retry_pause_doubles_to_a_minute() {
        let pauses: Vec<u64> = (0..9).map(|f| retry_pause(f).as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    /// A home that cannot hold the file is not a reason to stop
    /// recording: the pass goes on without the lock and says why.
    #[tokio::test]
    async fn an_unopenable_lock_is_unavailable_not_busy() {
        let home = boss_testing::scratch_dir("gate-watch-verdict-lock-file");
        let not_a_dir = home.join("plain-file");
        std::fs::write(&not_a_dir, "x").unwrap();
        assert!(matches!(
            verdict_lock(&not_a_dir, Duration::from_millis(50)).await,
            VerdictLock::Unavailable(_)
        ));
    }
}
