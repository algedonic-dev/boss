//! What a refusing gate would have refused, on the log rather than in a
//! process (design 21946380, backlog b0787727; amends rows C and D of
//! design b08725c2).
//!
//! WHY IT EXISTS. The machine gate ([`crate::machine_gate`]) and the
//! policy check's mode (`boss_policy::check_mode`) each earn their
//! `enforce` on a clean window — no would-refuse for 72 hours — and each
//! kept that evidence in a tally in process memory. The boss Deployment
//! is `Recreate` and every train restarts it (26 trains on 2026-09-30),
//! so no tally was ever older than about two hours and the window could
//! never close; an enforce car built on it would either never flip or
//! flip on a reading that watched part of the window. Option (a) of the
//! design, decided 2026-10-01: each would-refuse is a FACT on the log,
//! and the window is a projection over the log.
//!
//! THE GRAIN — one fact per new tally KEY per process, never one per
//! request (Q2 of the design, ratified beside "No per-request events" in
//! docs/architecture-decisions.md). Both gates already key their tallies
//! on a bounded caller shape and spill the rest into an overflow, so the
//! facts are bounded by the key caps times process starts — on a clean
//! estate, none at all:
//!
//! * `<gate>.recording_began` — `{service, mode, since, instance}`, at
//!   every process start and every mode move, `off` included. Silence
//!   alone is never clean: without these a projection cannot tell a
//!   quiet gate from one in `off`, or from a process that never
//!   recorded. `instance` is the process's own id, on every fact it
//!   states.
//! * `<gate>.recording_ended` — `{instance, lost, unstated, clean}`,
//!   stated at SIGTERM after the queue has drained. A process's watch
//!   joins the next one's only through a CLEAN end; a process that
//!   began and stated none — killed, crashed, or ending while its
//!   database was down — may have lost facts with it, so the window is
//!   clean only from the next process's start (review e4417d48, B1).
//! * `<gate>.facts_lost` — `{count, since}`: facts the queue could not
//!   take, stated once the log takes writes again. Dirties the window.
//! * `<gate>.would_refuse` / `<gate>.refused` — a key's FIRST sighting in
//!   this tally: `would_refuse` while the gate admits it (`report`, or a
//!   degraded `enforce`), `refused` when it did not.
//! * `<gate>.tally_overflowed` — the first sighting past a cap: a
//!   would-refuse that names no one, so a window containing one is never
//!   clean.
//! * `machine_gate.previous_presented` — a key presenting the `previous`
//!   slot, which every mode admits: what a rotation's revoke drains on
//!   (design point 5), and NOT a would-refuse.
//! * `machine_gate.reader_presented` — a key presenting the probe-reader
//!   credential (design b35c22b4), at its first sighting or past its
//!   source's share: its reads are admitted and its writes refused in
//!   EVERY mode, so `enforce` changes nothing for it and it ends no
//!   clean window. On the log for the reader's own rotation.
//!
//! TWO HALVES, and neither is the verdict alone (design point 3). The
//! log half is [`window`] over what a [`GateEvidenceLog`] reads back.
//! The LIVE half is each process's tally: a key counted once on the log
//! may keep missing for days inside one process.
//!
//! NO FACT IS FORGIVEN (review e4417d48). A fact the recorder refuses is
//! RETRIED with backoff, never dropped — the live half reads not clean
//! while it waits. A fact the bounded queue cannot take is lost, counted,
//! and stated on the log as `facts_lost` the next time the log takes a
//! write; until then the live half names it, and no tally restart or
//! mode move clears it ([`EvidenceHealth`]). What no live reader can
//! see — a process that dies holding unrecorded facts — shows on the log
//! as a process with no clean end, which breaks the watch. Each degrades
//! to an ERROR line and a not-clean reading, never to silence. The
//! reader that joins the two halves, and the enforce flips that record
//! its answer, are later cars of the design.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actor::ActorId;
use crate::event::Event;
use crate::machine_gate::Mode;
use crate::port::EventRecorder;
use crate::publisher::EventStamp;

/// Which refusing gate a fact is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gate {
    /// Every service port's machine-token check (row C, backlog 2710c8fc).
    MachineGate,
    /// The policy check's two open arms (row D, backlog b8e75382).
    PolicyCheck,
}

/// What a fact says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fact {
    RecordingBegan,
    WouldRefuse,
    Refused,
    TallyOverflowed,
    /// A process stated its end, after its queue drained.
    RecordingEnded,
    /// Facts the queue could not take, stated once the log took writes.
    FactsLost,
    /// The machine gate only.
    PreviousPresented,
    /// The machine gate only: a probe-reader presentation (design
    /// b35c22b4) — reads admitted and writes refused in every mode, so
    /// never a would-refuse.
    ReaderPresented,
}

impl Fact {
    fn name(self) -> &'static str {
        match self {
            Fact::RecordingBegan => "recording_began",
            Fact::WouldRefuse => "would_refuse",
            Fact::Refused => "refused",
            Fact::TallyOverflowed => "tally_overflowed",
            Fact::RecordingEnded => "recording_ended",
            Fact::FactsLost => "facts_lost",
            Fact::PreviousPresented => "previous_presented",
            Fact::ReaderPresented => "reader_presented",
        }
    }

    /// A fact that ends a clean window where it stands. A lost fact
    /// could have been any would-refuse, so it is one.
    pub fn dirties(self) -> bool {
        matches!(
            self,
            Fact::WouldRefuse | Fact::Refused | Fact::TallyOverflowed | Fact::FactsLost
        )
    }
}

impl Gate {
    pub fn prefix(self) -> &'static str {
        match self {
            Gate::MachineGate => "machine_gate",
            Gate::PolicyCheck => "policy.check",
        }
    }

    /// The facts this gate states.
    pub fn facts(self) -> &'static [Fact] {
        match self {
            Gate::MachineGate => &[
                Fact::RecordingBegan,
                Fact::WouldRefuse,
                Fact::Refused,
                Fact::TallyOverflowed,
                Fact::RecordingEnded,
                Fact::FactsLost,
                Fact::PreviousPresented,
                Fact::ReaderPresented,
            ],
            Gate::PolicyCheck => &[
                Fact::RecordingBegan,
                Fact::WouldRefuse,
                Fact::Refused,
                Fact::TallyOverflowed,
                Fact::RecordingEnded,
                Fact::FactsLost,
            ],
        }
    }

    /// The event kind of `fact` on this gate.
    pub fn kind(self, fact: Fact) -> String {
        format!("{}.{}", self.prefix(), fact.name())
    }

    /// Every kind this gate states — the set a [`GateEvidenceLog`] reads.
    pub fn kinds(self) -> Vec<String> {
        self.facts().iter().map(|f| self.kind(*f)).collect()
    }

    /// The fact `kind` names on this gate, if it is one of its kinds.
    pub fn fact_of(self, kind: &str) -> Option<Fact> {
        self.facts().iter().copied().find(|f| self.kind(*f) == kind)
    }
}

/// Every kind both gates state, as the `event_kinds` migration declares
/// them — held equal to that migration's rows by a test in boss-events
/// (`every_gate_evidence_kind_is_registered`), because a fact that lives
/// twice gets an equality test (CLAUDE.md §9a).
pub const KINDS: [&str; 14] = [
    "machine_gate.recording_began",
    "machine_gate.would_refuse",
    "machine_gate.refused",
    "machine_gate.tally_overflowed",
    "machine_gate.recording_ended",
    "machine_gate.facts_lost",
    "machine_gate.previous_presented",
    "machine_gate.reader_presented",
    "policy.check.recording_began",
    "policy.check.would_refuse",
    "policy.check.refused",
    "policy.check.tally_overflowed",
    "policy.check.recording_ended",
    "policy.check.facts_lost",
];

/// How many facts may wait for the recorder. A process emits at most a
/// tally's keys plus a handful; past this the recorder is not keeping
/// up, and what does not fit is LOST — counted, stated on the log as
/// `facts_lost` once it takes writes, never waited on: the request path
/// never blocks on the log.
pub const QUEUE_DEPTH: usize = 2048;

/// The first and the longest wait between attempts at a fact the
/// recorder refuses. Refused, a fact is retried, never dropped: the
/// outbox refuses while its database is down or full (incident
/// d3c0a67c), and a dropped would-refuse is one the log never holds.
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_millis(500);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a process given SIGTERM spends draining its queue and
/// stating its end before it exits regardless. Inside the pod's grace
/// period (30 s); a process that runs out states no clean end, which
/// the window reads as a break in the watch.
const END_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawned hand-offs in this process that have not yet stated their end.
/// The LAST one to finish after SIGTERM exits the process: registering a
/// SIGTERM listener replaces the signal's default (terminate), so the
/// process's exit has to be someone's act, and it must wait for every
/// gate the process holds — the policy service holds two. Signal
/// disposition is process-wide by nature, so this count is too.
///
/// THE LIBRARY OWNS SIGTERM IN A GATED BINARY (review c49cb4e1, S2): a
/// binary that registered its own SIGTERM shutdown would be cut short by
/// this exit within milliseconds, in silence. No gated binary does
/// today — jobs, assets and ledger shut down on SIGINT — and the pin
/// `a_gated_binary_does_not_listen_for_sigterm` in boss-testing refuses
/// one that starts to, naming this module as the place to change first.
static OPEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How often a hand-off that has stated its end, while another in the
/// process still drains, states any miss the server answered since.
const LINGER: std::time::Duration = std::time::Duration::from_millis(50);

/// Whether this process's facts reach the log — the live half's account
/// of its own recording. Counts are since the process started, and NO
/// tally restart or mode move resets them: a loss is cleared only by
/// being stated on the log (review e4417d48, B2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceHealth {
    /// False when the process was mounted with no recorder (a binary
    /// with no database). Its facts reach no log, so its service never
    /// shows as recording in the log half — the coverage the window
    /// requires names it there.
    pub recorder: bool,
    /// This process's id on every fact it states.
    pub instance: String,
    /// Facts the queue could not take since the process started.
    pub lost: u64,
    /// Of those, how many no `facts_lost` on the log states yet.
    pub unstated: u64,
    /// The recorder refuses a fact right now, which is being retried.
    pub retrying: bool,
    /// The latest reason a fact was refused or lost.
    pub last_error: Option<String>,
}

impl EvidenceHealth {
    /// Why this process's recording is not clean, one reason each.
    pub fn not_clean(&self) -> Vec<String> {
        let why = self.last_error.as_deref().unwrap_or("reason unknown");
        let mut out = Vec::new();
        if self.unstated > 0 {
            out.push(format!(
                "{} fact(s) this process stated are not on the log and no facts_lost says so \
                 yet ({why}) — a would-refuse the log does not hold could be any caller",
                self.unstated
            ));
        }
        if self.retrying {
            out.push(format!(
                "the recorder is refusing a fact, which waits for it ({why})"
            ));
        }
        out
    }
}

struct Inner {
    gate: Gate,
    service: String,
    instance: String,
    tx: Option<tokio::sync::mpsc::Sender<Event>>,
    lost: AtomicU64,
    /// Lost and not yet stated, with the instant of the oldest — under
    /// one lock, so a statement and a new loss cannot pass each other.
    unstated: Mutex<(u64, Option<DateTime<Utc>>)>,
    retrying: std::sync::atomic::AtomicBool,
    last_error: Mutex<Option<String>>,
}

/// One process's hand-off of a gate's facts to the log. Cheap to clone;
/// [`Evidence::emit`] never blocks and never fails the request it
/// describes.
#[derive(Clone)]
pub struct Evidence {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evidence")
            .field("gate", &self.inner.gate)
            .field("service", &self.inner.service)
            .field("health", &self.health())
            .finish()
    }
}

impl Evidence {
    fn with(gate: Gate, service: &str, tx: Option<tokio::sync::mpsc::Sender<Event>>) -> Self {
        Evidence {
            inner: Arc::new(Inner {
                gate,
                service: service.to_string(),
                instance: uuid::Uuid::new_v4().to_string(),
                tx,
                lost: AtomicU64::new(0),
                unstated: Mutex::new((0, None)),
                retrying: std::sync::atomic::AtomicBool::new(false),
                last_error: Mutex::new(None),
            }),
        }
    }

    /// No recorder: facts go to the debug log only, and the health says
    /// `recorder: false`. For a binary with no database and for a gate
    /// built in a test.
    pub fn none(gate: Gate, service: &str) -> Self {
        Self::with(gate, service, None)
    }

    /// Facts handed to the returned receiver — the testable half of
    /// [`Evidence::spawn`].
    pub fn channel(gate: Gate, service: &str) -> (Self, tokio::sync::mpsc::Receiver<Event>) {
        let (tx, rx) = tokio::sync::mpsc::channel(QUEUE_DEPTH);
        (Self::with(gate, service, Some(tx)), rx)
    }

    /// Facts recorded by `recorder` — the outbox, in every binary with a
    /// database — from a task of their own, which ends the process's
    /// watch at SIGTERM: it drains the queue, states `recording_ended`,
    /// and the last such task in the process exits it (within
    /// [`END_GRACE`]). Call it inside the runtime; outside one there is
    /// nothing to drain the queue, so it degrades to [`Evidence::none`]
    /// and says so at ERROR.
    pub fn spawn(gate: Gate, service: &str, recorder: Arc<dyn EventRecorder>) -> Self {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                service,
                gate = gate.prefix(),
                "gate evidence mounted outside a runtime: its facts reach no log"
            );
            return Self::none(gate, service);
        };
        let (evidence, rx) = Self::channel(gate, service);
        let sink = evidence.clone();
        let open = Open::hold();
        rt.spawn(async move {
            let ended = sink.run(rx, Arc::clone(&recorder), terminated()).await;
            // Done: the last hand-off still open exits the process here.
            drop(open);
            if ended {
                // Another hand-off in this process is still draining,
                // and the server still answers: a miss now is counted as
                // lost, and stated until the process exits.
                loop {
                    tokio::time::sleep(LINGER).await;
                    sink.state_losses(recorder.as_ref()).await;
                }
            }
        });
        evidence
    }

    /// The hand-off's task: record each fact in order, retrying one the
    /// recorder refuses; state any losses once the log takes a write;
    /// and when `shutdown` completes, drain what is queued and state the
    /// end. Answers whether it ended that way (rather than by every
    /// sender dropping, which only a test does).
    pub async fn run<S>(
        &self,
        mut rx: tokio::sync::mpsc::Receiver<Event>,
        recorder: Arc<dyn EventRecorder>,
        shutdown: S,
    ) -> bool
    where
        S: std::future::Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        let mut closing = false;
        loop {
            let event = if closing {
                match rx.try_recv() {
                    Ok(e) => e,
                    Err(_) => break,
                }
            } else {
                tokio::select! {
                    biased;
                    e = rx.recv() => match e {
                        Some(e) => e,
                        None => return false,
                    },
                    () = &mut shutdown => {
                        // Closed BEFORE the drain, so a miss the server
                        // answers from here on is counted as lost, not
                        // queued where nobody reads again (review
                        // c49cb4e1, D2).
                        rx.close();
                        closing = true;
                        continue;
                    }
                }
            };
            let mut wait = RETRY_FIRST;
            loop {
                match recorder.record(&event).await {
                    Ok(()) => {
                        self.inner.retrying.store(false, Ordering::SeqCst);
                        break;
                    }
                    Err(e) if closing => {
                        self.lose(
                            &event,
                            &format!("the process is ending and the recorder refuses it: {e}"),
                        );
                        break;
                    }
                    Err(e) => {
                        self.refused(&event, &e);
                        tokio::select! {
                            () = tokio::time::sleep(wait) => {}
                            () = &mut shutdown => {
                                rx.close();
                                closing = true;
                            }
                        }
                        wait = (wait * 2).min(RETRY_MAX);
                    }
                }
            }
            self.state_losses(recorder.as_ref()).await;
        }
        self.state_losses(recorder.as_ref()).await;
        self.end(recorder.as_ref()).await;
        // A miss counted while the end was being written is stated
        // AFTER it, where it dirties the window: the end's `clean` was
        // true only of what it saw.
        self.state_losses(recorder.as_ref()).await;
        true
    }

    pub fn gate(&self) -> Gate {
        self.inner.gate
    }

    pub fn service(&self) -> &str {
        &self.inner.service
    }

    pub fn health(&self) -> EvidenceHealth {
        EvidenceHealth {
            recorder: self.inner.tx.is_some(),
            instance: self.inner.instance.clone(),
            lost: self.inner.lost.load(Ordering::SeqCst),
            unstated: self.unstated().0,
            retrying: self.inner.retrying.load(Ordering::SeqCst),
            last_error: self
                .inner
                .last_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    fn unstated(&self) -> std::sync::MutexGuard<'_, (u64, Option<DateTime<Utc>>)> {
        self.inner
            .unstated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn note_error(&self, why: &str) {
        *self
            .inner
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(why.to_string());
    }

    /// A fact the recorder refused, about to be tried again.
    fn refused(&self, event: &Event, why: &str) {
        let first = !self.inner.retrying.swap(true, Ordering::SeqCst);
        self.note_error(why);
        if first {
            tracing::error!(
                kind = %event.kind,
                service = %self.inner.service,
                why,
                "gate evidence refused by the recorder — retrying; the live tally reads not \
                 clean until the log takes it"
            );
        }
    }

    /// A fact that will never reach the log: counted, and owed to it as
    /// a `facts_lost`.
    fn lose(&self, event: &Event, why: &str) {
        self.note_error(why);
        self.inner.lost.fetch_add(1, Ordering::SeqCst);
        let mut unstated = self.unstated();
        unstated.0 += 1;
        unstated.1.get_or_insert(event.timestamp);
        drop(unstated);
        // The line is the record until the log takes the loss: degrade
        // to it, never to silence.
        tracing::error!(
            kind = %event.kind,
            service = %self.inner.service,
            payload = %event.payload,
            why,
            "gate evidence LOST — the live tally reads not clean until a facts_lost is on the log"
        );
    }

    /// The event for `fact`, stamped with this service, its automation
    /// actor and this process's instance.
    fn event(&self, fact: Fact, fields: Value) -> Event {
        let mut payload = fields;
        if let Value::Object(map) = &mut payload {
            map.insert("service".into(), json!(self.inner.service));
            map.insert("instance".into(), json!(self.inner.instance));
        }
        // The record stamp every live emit takes: wall time, and the
        // service's own automation actor.
        EventStamp::new(
            self.inner.service.clone(),
            ActorId::Automation(self.inner.service.clone()),
        )
        .event(&self.inner.gate.kind(fact), payload)
    }

    /// State every loss not yet on the log as one `facts_lost`. The
    /// count leaves the live half only once the log holds it.
    async fn state_losses(&self, recorder: &dyn EventRecorder) {
        let (count, since) = *self.unstated();
        if count == 0 {
            return;
        }
        let fact = self.event(Fact::FactsLost, json!({ "count": count, "since": since }));
        match recorder.record(&fact).await {
            Ok(()) => {
                let mut unstated = self.unstated();
                unstated.0 -= count;
                if unstated.0 == 0 {
                    unstated.1 = None;
                }
            }
            Err(e) => self.note_error(&format!("stating {count} lost fact(s): {e}")),
        }
    }

    /// `recording_ended`: clean only when nothing this process stated is
    /// missing from the log without a `facts_lost` saying so.
    async fn end(&self, recorder: &dyn EventRecorder) {
        let health = self.health();
        let fact = self.event(
            Fact::RecordingEnded,
            json!({
                "lost": health.lost,
                "unstated": health.unstated,
                "clean": health.unstated == 0,
            }),
        );
        if let Err(e) = recorder.record(&fact).await {
            tracing::error!(
                service = %self.inner.service,
                error = %e,
                "gate evidence: this process's end is NOT on the log, so its watch reads broken"
            );
        }
    }

    /// State `fact` with `fields` (an object), stamped with this
    /// service, its automation actor and this process's instance.
    pub fn emit(&self, fact: Fact, fields: Value) {
        let event = self.event(fact, fields);
        let Some(tx) = &self.inner.tx else {
            tracing::debug!(kind = %event.kind, payload = %event.payload, "gate evidence (no recorder)");
            return;
        };
        if let Err(e) = tx.try_send(event) {
            let (why, event) = match e {
                tokio::sync::mpsc::error::TrySendError::Full(ev) => {
                    ("the queue to the recorder is full", ev)
                }
                tokio::sync::mpsc::error::TrySendError::Closed(ev) => {
                    ("the recorder's task has stopped", ev)
                }
            };
            self.lose(&event, why);
        }
    }

    /// `<gate>.recording_began`: the gate records in `mode` from `since`.
    pub fn recording_began(&self, mode: Mode, since: DateTime<Utc>) {
        self.emit(
            Fact::RecordingBegan,
            json!({ "mode": mode, "since": since }),
        );
    }
}

/// Whether this process has had SIGTERM — set by the watcher thread,
/// read by every hand-off in whatever runtime it runs.
static TERM: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> = std::sync::OnceLock::new();

fn term() -> &'static tokio::sync::watch::Sender<bool> {
    TERM.get_or_init(|| tokio::sync::watch::channel(false).0)
}

/// One open hand-off, counted in [`OPEN`] for as long as it lives —
/// however its task ends: finished, panicked, or cancelled by its
/// runtime dropping. Dropped after SIGTERM as the last one open, it
/// exits the process; so a hand-off that is gone never leaves SIGTERM
/// waiting on it (review c49cb4e1, S4).
struct Open;

impl Open {
    fn hold() -> Self {
        watch_for_sigterm();
        OPEN.fetch_add(1, Ordering::SeqCst);
        Open
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        if OPEN.fetch_sub(1, Ordering::SeqCst) == 1 && *term().borrow() {
            // 128 + SIGTERM: what the default disposition the watcher
            // replaced would have left.
            std::process::exit(143);
        }
    }
}

/// Starts, once per process, the thread that owns SIGTERM: on its own
/// runtime, so it outlives any runtime a hand-off ran in. At SIGTERM it
/// tells every hand-off to end; it exits the process at once when none
/// is open, else after [`END_GRACE`] whatever is still draining. Where
/// no listener can be registered SIGTERM keeps its default, and the
/// process ends stating no end — which the window reads as a break,
/// loud, never clean.
fn watch_for_sigterm() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        #[cfg(unix)]
        let spawned = std::thread::Builder::new()
            .name("gate-evidence-sigterm".into())
            .spawn(|| {
                use tokio::signal::unix::{SignalKind, signal};
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(error = %e, "gate evidence cannot watch for SIGTERM");
                        return;
                    }
                };
                rt.block_on(async {
                    let mut sigterm = match signal(SignalKind::terminate()) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "gate evidence cannot listen for SIGTERM: this process will end \
                                 stating no end, and its watch will read broken"
                            );
                            return;
                        }
                    };
                    sigterm.recv().await;
                    term().send_replace(true);
                    if OPEN.load(Ordering::SeqCst) == 0 {
                        std::process::exit(143);
                    }
                    tokio::time::sleep(END_GRACE).await;
                    tracing::error!(
                        "gate evidence did not state its end within {END_GRACE:?} of SIGTERM; \
                         exiting anyway — this process's watch reads broken"
                    );
                    std::process::exit(143);
                });
            });
        #[cfg(unix)]
        if let Err(e) = spawned {
            tracing::error!(error = %e, "gate evidence cannot start its SIGTERM watcher");
        }
    });
}

/// Completes once this process has had SIGTERM.
async fn terminated() {
    let mut told = term().subscribe();
    if told.wait_for(|t| *t).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The port the log half reads: the facts one gate stated.
#[async_trait]
pub trait GateEvidenceLog: Send + Sync {
    /// Every fact of `gate` recorded at or after `from`, AND each
    /// service's newest `recording_began` recorded before `from` — the
    /// mode a service was in when the window opened. Oldest first, by
    /// the recorded instant, then by event id (byte order), so a tie
    /// has one answer on every adapter. A process that ended before
    /// `from` is not read past its newest start, so when the next one
    /// began after `from` its end is not seen and the watch starts with
    /// the next process — shorter by one restart, never cleaner.
    async fn facts(&self, gate: Gate, from: DateTime<Utc>) -> Result<Vec<Event>, String>;
}

/// The log as a list of events — the double every reader's test asks.
#[derive(Clone, Debug, Default)]
pub struct InMemoryGateEvidence {
    events: Vec<Event>,
}

impl InMemoryGateEvidence {
    pub fn new(events: Vec<Event>) -> Self {
        InMemoryGateEvidence { events }
    }
}

fn service_of(e: &Event) -> Option<&str> {
    e.payload.get("service").and_then(Value::as_str)
}

#[async_trait]
impl GateEvidenceLog for InMemoryGateEvidence {
    async fn facts(&self, gate: Gate, from: DateTime<Utc>) -> Result<Vec<Event>, String> {
        let began = gate.kind(Fact::RecordingBegan);
        let order = |a: &Event, b: &Event| (a.timestamp, a.id).cmp(&(b.timestamp, b.id));
        let mut out: Vec<Event> = self
            .events
            .iter()
            .filter(|e| e.timestamp >= from && gate.fact_of(&e.kind).is_some())
            .cloned()
            .collect();
        let mut before: std::collections::BTreeMap<Option<&str>, &Event> = Default::default();
        for e in self
            .events
            .iter()
            .filter(|e| e.timestamp < from && e.kind == began)
        {
            let slot = before.entry(service_of(e)).or_insert(e);
            if order(e, slot).is_gt() {
                *slot = e;
            }
        }
        out.extend(before.into_values().cloned());
        out.sort_by(order);
        Ok(out)
    }
}

/// One service's watch over the window, as the log states it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub service: String,
    /// Since when the log shows this service recording, without a break,
    /// in a mode other than `off`; `None` when it does not reach now.
    pub recording_since: Option<DateTime<Utc>>,
    /// Why it does not, when it does not.
    pub gap: Option<String>,
    /// Why the watch starts where it does, when the log holds an
    /// earlier one it does not join: a process that stated no clean end,
    /// a stretch of `off`, an unreadable start.
    pub started_after: Option<String>,
}

/// A fact that ended a clean window: who would have been (or was)
/// refused, as the log names them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dirty {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub payload: Value,
}

/// The LOG half of a gate's clean window. Never the verdict alone: a
/// key is stated once per process, so a caller that keeps missing
/// inside one long-lived process is named here once, and only that
/// process's live tally says it is still missing (design point 3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub gate: Gate,
    /// The earliest instant read: nothing before it is judged, so the
    /// window is clean at most since here.
    pub from: DateTime<Utc>,
    pub now: DateTime<Utc>,
    /// Since when the log holds no would-refuse, no refusal and no
    /// overflow for this gate, with every service named recording
    /// throughout; `None` when some service's watch does not reach now.
    pub log_clean_since: Option<DateTime<Utc>>,
    /// Every would-refuse, refusal and overflow read, oldest first.
    pub dirty: Vec<Dirty>,
    pub coverage: Vec<Coverage>,
    /// Why the log half is not clean now — empty when it is.
    pub not_clean: Vec<String>,
}

impl Window {
    /// How long the log half has been clean at `now`.
    pub fn clean_for(&self) -> Option<Duration> {
        self.log_clean_since.map(|s| self.now - s)
    }
}

/// The mode a `recording_began` names, from when, and by which process;
/// `None` when the payload is not one — which the window reads as a
/// break, never as a watch.
fn began_of(e: &Event) -> Option<(Mode, DateTime<Utc>, &str)> {
    let mode = serde_json::from_value(e.payload.get("mode")?.clone()).ok()?;
    let since = serde_json::from_value(e.payload.get("since")?.clone()).ok()?;
    let instance = e.payload.get("instance")?.as_str()?;
    Some((mode, since, instance))
}

/// The longest stretch between one process's clean end and the next
/// start that a restart explains: the `Recreate` roll — the old pod
/// gone, the image pulled, the new one booted — measured in minutes
/// (boss.yaml calls it "a two-minute outage"). A wider stretch is a
/// break: a process may have lived in it with nothing on the log, its
/// start refused by a full disk and its end never written (review
/// c49cb4e1, D1). A whole-pod outage longer than this only shortens
/// the window, which is the safe direction.
pub const MAX_RESTART_GAP: Duration = Duration::minutes(10);

/// One service's unbroken watch, read newest first: the live process's
/// starts and mode moves, then each earlier process's, joined only
/// across a CLEAN `recording_ended` (review e4417d48, B1) and never
/// across `off` or an unreadable start.
fn coverage_of(gate: Gate, service: &str, facts: &[Event]) -> Coverage {
    let began_kind = gate.kind(Fact::RecordingBegan);
    let ended_kind = gate.kind(Fact::RecordingEnded);
    // A watch is read from facts the service itself stated — its source
    // — never from a payload that only names it (review c49cb4e1).
    let mine = |e: &&Event| service_of(e) == Some(service) && e.source == service;
    let mut began: Vec<&Event> = facts
        .iter()
        .filter(|e| e.kind == began_kind)
        .filter(mine)
        .collect();
    began.sort_by_key(|e| (e.timestamp, e.id));
    // Each process's end: clean only if every end it stated says so, and
    // when it was stated (the latest).
    let mut ends: std::collections::HashMap<&str, (bool, DateTime<Utc>)> = Default::default();
    for e in facts.iter().filter(|e| e.kind == ended_kind).filter(mine) {
        let instance = e
            .payload
            .get("instance")
            .and_then(Value::as_str)
            .unwrap_or("");
        let clean = e.payload.get("clean").and_then(Value::as_bool) == Some(true);
        let slot = ends.entry(instance).or_insert((true, e.timestamp));
        *slot = (slot.0 && clean, slot.1.max(e.timestamp));
    }
    let mut since = None;
    let mut gap = Some(format!(
        "no `{began_kind}` from `{service}` on the log: it has never recorded here"
    ));
    let mut started_after = None;
    let mut current: Option<&str> = None;
    for e in began.iter().rev() {
        let Some((mode, at, instance)) = began_of(e) else {
            let why = format!("`{service}`'s `{began_kind}` {} is unreadable", e.id);
            if since.is_none() {
                gap = Some(why);
            } else {
                started_after = Some(why);
            }
            break;
        };
        match current {
            None if ends.contains_key(instance) => {
                gap = Some(format!(
                    "`{service}`'s latest process `{instance}` stated its end and none has \
                     begun since: nothing is recording"
                ));
                break;
            }
            Some(cur) if cur != instance => match (ends.get(instance), since) {
                (Some((true, end)), Some(next)) if next - *end <= MAX_RESTART_GAP => {}
                (Some((true, end)), Some(next)) => {
                    started_after = Some(format!(
                        "`{service}` stated nothing from process `{instance}`'s clean end at \
                         {end} until the next start at {next}, a gap longer than a restart \
                         ({MAX_RESTART_GAP}) — a process may have lived in it with nothing on \
                         the log — so the watch starts with the next start"
                    ));
                    break;
                }
                _ => {
                    started_after = Some(format!(
                        "process `{instance}` of `{service}` (recording since {at}) stated no \
                         clean end — it may have died holding facts the log never took — so \
                         the watch starts with the process after it"
                    ));
                    break;
                }
            },
            _ => {}
        }
        if mode == Mode::Off {
            let why = format!("`{service}` recorded in `off` from {at}: off watches nothing");
            if since.is_none() {
                gap = Some(why);
            } else {
                started_after = Some(why);
            }
            break;
        }
        since = Some(at);
        gap = None;
        current = Some(instance);
    }
    Coverage {
        service: service.to_string(),
        recording_since: since,
        gap,
        started_after,
    }
}

/// The log half of `gate`'s window at `now`, from `facts` as a
/// [`GateEvidenceLog`] answers them for `from`, for the gated `services`
/// a reader holds the gate to. A pure function of the log.
pub fn window(
    gate: Gate,
    services: &[&str],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: &[Event],
) -> Window {
    let dirty: Vec<Dirty> = facts
        .iter()
        .filter(|e| e.timestamp >= from && gate.fact_of(&e.kind).is_some_and(Fact::dirties))
        .map(|e| Dirty {
            at: e.timestamp,
            kind: e.kind.clone(),
            payload: e.payload.clone(),
        })
        .collect();
    let coverage: Vec<Coverage> = services
        .iter()
        .map(|service| coverage_of(gate, service, facts))
        .collect();

    // A watch that does not reach now is the only thing that leaves NO
    // clean instant. A dirtying fact does not: it moves the instant to
    // just after itself, and `dirty` names it.
    let mut not_clean: Vec<String> = coverage.iter().filter_map(|c| c.gap.clone()).collect();
    if services.is_empty() {
        not_clean.push("no service was named, so nothing was shown to be watched".to_string());
    }
    let log_clean_since = not_clean.is_empty().then(|| {
        // The latest of: the read's own start, every service's watch,
        // and the instant after the latest dirtying fact.
        let watched = coverage.iter().filter_map(|c| c.recording_since).max();
        let after_dirty = dirty.iter().map(|d| d.at + Duration::microseconds(1)).max();
        [from]
            .into_iter()
            .chain(watched)
            .chain(after_dirty)
            .max()
            .unwrap_or(from)
    });
    Window {
        gate,
        from,
        now,
        log_clean_since,
        dirty,
        coverage,
        not_clean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, h, 0, 0).unwrap()
    }

    fn fact(gate: Gate, fact: Fact, service: &str, when: DateTime<Utc>, fields: Value) -> Event {
        let mut payload = fields;
        payload["service"] = json!(service);
        Event::new(service, gate.kind(fact), payload, when)
    }

    /// A start or mode move of the one process `p1` — the fixture most
    /// cases need, where every move is inside one process's life.
    fn began(service: &str, mode: Mode, since: DateTime<Utc>) -> Event {
        began_by(service, mode, since, "p1")
    }

    fn began_by(service: &str, mode: Mode, since: DateTime<Utc>, instance: &str) -> Event {
        fact(
            Gate::MachineGate,
            Fact::RecordingBegan,
            service,
            since,
            json!({"mode": mode, "since": since, "instance": instance}),
        )
    }

    /// Review e4417d48, B1: process `a` began recording at 01:00 and
    /// left no clean end — it may have lost a would-refuse while its
    /// database was down and then died with it. Process `b` began at
    /// 03:00. The log holds two `report` starts and nothing else, and
    /// chained as one watch they read clean since 01:00 over the loss.
    /// A process that stated no clean end ends the watch: clean from
    /// the next process's start, and never across it.
    #[test]
    fn a_process_that_stated_no_clean_end_breaks_the_watch() {
        let facts = vec![
            began_by("jobs", Mode::Report, at(1), "a"),
            began_by("jobs", Mode::Report, at(3), "b"),
        ];
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &facts);
        assert_eq!(w.log_clean_since, Some(at(3)), "{w:?}");
        assert!(
            w.coverage[0]
                .started_after
                .as_deref()
                .is_some_and(|s| s.contains("`a`") && s.contains("no clean end")),
            "{w:?}"
        );

        // A CLEAN end joins the two processes into one watch; an end
        // that says it is not clean does not.
        let mut joined = facts.clone();
        joined.push(ended("jobs", at(3) - Duration::minutes(2), "a", true));
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &joined);
        assert_eq!(w.log_clean_since, Some(at(1)), "{w:?}");
        let mut broken = facts.clone();
        broken.push(ended("jobs", at(3) - Duration::minutes(2), "a", false));
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &broken);
        assert_eq!(w.log_clean_since, Some(at(3)), "{w:?}");

        // The latest process ended and none began after it: nothing is
        // recording now, so there is no clean instant at all.
        let mut stopped = facts;
        stopped.push(ended("jobs", at(5), "b", true));
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &stopped);
        assert_eq!(w.log_clean_since, None, "{w:?}");
        assert!(w.not_clean[0].contains("nothing is recording"), "{w:?}");
    }

    /// Review c49cb4e1, D1: `a` ends clean at 02:00; `b` starts while
    /// the outbox refuses every insert, tallies a would-refuse, and is
    /// stopped with nothing landed; `c` starts at 05:00. The log holds
    /// a's start and clean end and c's start — and nothing of `b`. A
    /// clean end joins the next start only across a gap a restart can
    /// explain; a wider one is a break, since a process may have lived
    /// in it with nothing on the log.
    #[test]
    fn a_clean_end_joins_only_a_start_a_restart_away() {
        let facts = vec![
            began_by("jobs", Mode::Report, at(1), "a"),
            ended("jobs", at(2), "a", true),
            began_by("jobs", Mode::Report, at(5), "c"),
        ];
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &facts);
        assert_eq!(w.log_clean_since, Some(at(5)), "{w:?}");
        assert!(
            w.coverage[0]
                .started_after
                .as_deref()
                .is_some_and(|s| s.contains("longer than a restart")),
            "{w:?}"
        );

        // A restart's dark window joins.
        let next = at(2) + Duration::minutes(3);
        let facts = vec![
            began_by("jobs", Mode::Report, at(1), "a"),
            ended("jobs", at(2), "a", true),
            began_by("jobs", Mode::Report, next, "c"),
        ];
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &facts);
        assert_eq!(w.log_clean_since, Some(at(1)), "{w:?}");
    }

    /// Review c49cb4e1: the window reads a service's watch from facts
    /// that service stated — the event's source — and not from a
    /// payload that only claims it.
    #[test]
    fn a_start_another_source_claims_is_not_a_watch() {
        let mut claimed = began_by("jobs", Mode::Report, at(1), "a");
        claimed.source = "intruder".into();
        let w = window(Gate::MachineGate, &["jobs"], at(0), at(9), &[claimed]);
        assert_eq!(w.log_clean_since, None, "{w:?}");
        assert!(w.not_clean[0].contains("never recorded"), "{w:?}");
    }

    fn ended(service: &str, when: DateTime<Utc>, instance: &str, clean: bool) -> Event {
        fact(
            Gate::MachineGate,
            Fact::RecordingEnded,
            service,
            when,
            json!({"instance": instance, "clean": clean}),
        )
    }

    /// A recorder that refuses its first `fail` writes and takes every
    /// one after, keeping what it took.
    struct Flaky {
        fail: AtomicU64,
        took: Mutex<Vec<Event>>,
    }

    impl Flaky {
        fn failing(fail: u64) -> Arc<Self> {
            Arc::new(Flaky {
                fail: AtomicU64::new(fail),
                took: Mutex::new(Vec::new()),
            })
        }

        fn took(&self) -> Vec<Event> {
            self.took.lock().unwrap().clone()
        }

        fn kinds(&self) -> Vec<String> {
            self.took().into_iter().map(|e| e.kind).collect()
        }
    }

    #[async_trait]
    impl EventRecorder for Flaky {
        async fn record(&self, e: &Event) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) > 0 {
                self.fail.fetch_sub(1, Ordering::SeqCst);
                return Err("the database is down".into());
            }
            self.took.lock().unwrap().push(e.clone());
            Ok(())
        }
    }

    /// Wait up to ten seconds for `ready`.
    async fn until(ready: impl Fn() -> bool) {
        for _ in 0..1000 {
            if ready() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("not within ten seconds");
    }

    /// The hand-off's task over `recorder`, with a shutdown the test
    /// sends: what `spawn` runs, minus the process's SIGTERM and exit.
    fn running(
        evidence: &Evidence,
        rx: tokio::sync::mpsc::Receiver<Event>,
        recorder: Arc<Flaky>,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<bool>,
    ) {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let ev = evidence.clone();
        let task = tokio::spawn(async move {
            ev.run(rx, recorder as Arc<dyn EventRecorder>, async {
                let _ = stopped.await;
            })
            .await
        });
        (stop, task)
    }

    /// Review e4417d48, B1 repair 1: a fact the recorder refuses — the
    /// outbox while its disk is full (incident d3c0a67c) — is RETRIED
    /// until the log takes it, never dropped; while it waits the live
    /// half reads not clean, and once taken it reads clean again.
    #[tokio::test]
    async fn a_refused_fact_is_retried_until_the_log_takes_it() {
        let (evidence, rx) = Evidence::channel(Gate::MachineGate, "jobs");
        let flaky = Flaky::failing(2);
        let (stop, task) = running(&evidence, rx, Arc::clone(&flaky));
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        until(|| evidence.health().retrying).await;
        assert!(
            evidence
                .health()
                .not_clean()
                .iter()
                .any(|w| w.contains("refusing")),
            "{:?}",
            evidence.health()
        );
        until(|| flaky.took().len() == 1).await;
        let h = evidence.health();
        assert_eq!((h.lost, h.unstated, h.retrying), (0, 0, false), "{h:?}");
        assert!(h.not_clean().is_empty());

        // SIGTERM: the queue is empty, so the end is clean.
        stop.send(()).unwrap();
        assert!(task.await.unwrap(), "ended by its shutdown");
        assert_eq!(
            flaky.kinds(),
            vec!["machine_gate.would_refuse", "machine_gate.recording_ended"]
        );
        let end = &flaky.took()[1];
        assert_eq!(end.payload["clean"], true);
        assert_eq!(end.payload["instance"], json!(h.instance));
    }

    /// Review e4417d48, B1 repair 2: a fact the full queue could not take
    /// is lost — counted, named by the live half, and owed to the log.
    /// Once the log takes a write the loss is stated as `facts_lost`,
    /// which dirties the window; only then does the live half let it go.
    #[tokio::test]
    async fn a_lost_fact_is_stated_on_the_log_and_dirties_the_window() {
        let (evidence, rx) = Evidence::channel(Gate::MachineGate, "jobs");
        evidence.recording_began(Mode::Report, at(1));
        for _ in 0..QUEUE_DEPTH {
            evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        }
        let h = evidence.health();
        assert_eq!((h.lost, h.unstated), (1, 1), "{h:?}");
        assert!(h.not_clean()[0].contains("not on the log"), "{h:?}");

        let flaky = Flaky::failing(0);
        let (stop, task) = running(&evidence, rx, Arc::clone(&flaky));
        until(|| evidence.health().unstated == 0).await;
        let h = evidence.health();
        assert_eq!(h.lost, 1, "the count since the process started stays");
        assert!(h.not_clean().is_empty(), "{h:?}");
        let took = flaky.took();
        let lost = took
            .iter()
            .find(|e| e.kind == "machine_gate.facts_lost")
            .expect("the loss is on the log");
        assert_eq!(lost.payload["count"], 1);

        let w = window(Gate::MachineGate, &["jobs"], at(0), lost.timestamp, &took);
        assert!(
            w.dirty.iter().any(|d| d.kind == "machine_gate.facts_lost"),
            "{w:?}"
        );
        assert!(w.log_clean_since.is_some_and(|s| s > lost.timestamp));

        stop.send(()).unwrap();
        assert!(task.await.unwrap());
        assert_eq!(
            flaky.took().last().map(|e| e.payload["clean"].clone()),
            Some(json!(true)),
            "a loss stated on the log leaves a clean end"
        );
    }

    /// Review c49cb4e1, D2: the server keeps answering after SIGTERM, so
    /// a request can miss while the end itself is being written. That
    /// would-refuse must not go into a queue nobody reads again: it is
    /// counted as lost, and stated on the log AFTER the end — where it
    /// dirties the window — so the end's "clean" cannot hide it.
    #[tokio::test]
    async fn a_miss_while_the_end_is_written_is_stated_after_it() {
        /// Takes every write, and while it writes the end, the server
        /// answers one more miss.
        struct MissesDuringTheEnd {
            evidence: std::sync::OnceLock<Evidence>,
            took: Mutex<Vec<Event>>,
        }
        #[async_trait]
        impl EventRecorder for MissesDuringTheEnd {
            async fn record(&self, e: &Event) -> Result<(), String> {
                if e.kind == "machine_gate.recording_ended"
                    && let Some(ev) = self.evidence.get()
                {
                    ev.emit(Fact::WouldRefuse, json!({"mode": "report"}));
                }
                self.took.lock().unwrap().push(e.clone());
                Ok(())
            }
        }
        let (evidence, rx) = Evidence::channel(Gate::MachineGate, "jobs");
        let recorder = Arc::new(MissesDuringTheEnd {
            evidence: std::sync::OnceLock::new(),
            took: Mutex::new(Vec::new()),
        });
        let _ = recorder.evidence.set(evidence.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let ev = evidence.clone();
        let rec = Arc::clone(&recorder) as Arc<dyn EventRecorder>;
        let task = tokio::spawn(async move {
            ev.run(rx, rec, async {
                let _ = stopped.await;
            })
            .await
        });
        stop.send(()).unwrap();
        assert!(task.await.unwrap());
        let kinds: Vec<String> = recorder
            .took
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec!["machine_gate.recording_ended", "machine_gate.facts_lost"],
            "the miss is on the log after the end"
        );
        let h = evidence.health();
        assert_eq!((h.lost, h.unstated), (1, 0), "{h:?}");
    }

    /// SIGTERM while the database is down (review e4417d48, N1): what is
    /// still queued is tried once and LOST, counted rather than dropped
    /// in silence, and the process's end cannot be stated — so the log
    /// shows a process with no clean end, which breaks the watch.
    #[tokio::test]
    async fn an_end_while_the_log_refuses_states_no_clean_end() {
        let (evidence, rx) = Evidence::channel(Gate::MachineGate, "jobs");
        let down = Flaky::failing(u64::MAX);
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        let (stop, task) = running(&evidence, rx, Arc::clone(&down));
        until(|| evidence.health().retrying).await;
        stop.send(()).unwrap();
        assert!(task.await.unwrap());
        let h = evidence.health();
        assert_eq!((h.lost, h.unstated), (2, 2), "{h:?}");
        assert!(down.took().is_empty(), "no end reached the log");
    }

    fn missed(service: &str, when: DateTime<Utc>) -> Event {
        fact(
            Gate::MachineGate,
            Fact::WouldRefuse,
            service,
            when,
            json!({"mode": "report", "key": {"peer": "10.20.0.9", "presented": "none"}}),
        )
    }

    /// The migration's rows are `KINDS`; `KINDS` is every kind each gate
    /// states, so the one list a test holds the registry to is the list
    /// the gates emit from.
    #[test]
    fn kinds_are_every_kind_each_gate_states() {
        let stated: Vec<String> = [Gate::MachineGate, Gate::PolicyCheck]
            .into_iter()
            .flat_map(Gate::kinds)
            .collect();
        assert_eq!(stated, KINDS.map(String::from).to_vec());
        assert_eq!(
            Gate::PolicyCheck.fact_of("policy.check.would_refuse"),
            Some(Fact::WouldRefuse)
        );
        assert_eq!(Gate::PolicyCheck.fact_of("machine_gate.refused"), None);
        assert!(!Fact::PreviousPresented.dirties() && !Fact::RecordingBegan.dirties());
    }

    /// A fact names its service, carries the service's automation actor,
    /// and is sourced from the service: who stated it is on the record.
    #[tokio::test]
    async fn a_fact_names_its_service_and_actor() {
        let (evidence, mut rx) = Evidence::channel(Gate::PolicyCheck, "policy");
        evidence.emit(Fact::WouldRefuse, json!({"mode": "report"}));
        let e = rx.recv().await.unwrap();
        assert_eq!(e.kind, "policy.check.would_refuse");
        assert_eq!(e.source, "policy");
        assert_eq!(e.payload["service"], "policy");
        assert_eq!(e.payload["instance"], json!(evidence.health().instance));
        assert_eq!(
            e.payload["_actor"],
            json!(ActorId::Automation("policy".into()))
        );
        let h = evidence.health();
        assert!(h.recorder && h.lost == 0 && !h.retrying && h.not_clean().is_empty());
    }

    /// A fact the queue cannot take is COUNTED, never waited on and never
    /// dropped in silence: a full queue, a stopped task. A process with
    /// no recorder says so and counts nothing — its absence shows in the
    /// log half's coverage instead.
    #[tokio::test]
    async fn a_fact_the_queue_cannot_take_is_counted() {
        let none = Evidence::none(Gate::MachineGate, "simulator");
        none.recording_began(Mode::Report, at(1));
        let h = none.health();
        assert!(
            !h.recorder && h.lost == 0 && h.not_clean().is_empty(),
            "{h:?}"
        );

        let (full, _held) = Evidence::channel(Gate::MachineGate, "jobs");
        for _ in 0..=QUEUE_DEPTH {
            full.emit(Fact::WouldRefuse, json!({}));
        }
        let h = full.health();
        assert_eq!((h.lost, h.unstated), (1, 1), "{h:?}");
        assert!(h.last_error.as_deref().unwrap().contains("full"));
        assert!(h.not_clean()[0].starts_with("1 fact"), "{h:?}");

        let (closed, rx) = Evidence::channel(Gate::MachineGate, "jobs");
        drop(rx);
        closed.emit(Fact::WouldRefuse, json!({}));
        assert_eq!(closed.health().lost, 1);
    }

    /// Every service named must be recording, in a mode that watches,
    /// from before the window opened to now; one that never recorded,
    /// whose latest word is `off`, or whose latest fact is unreadable
    /// leaves no clean instant at all.
    #[test]
    fn the_window_needs_every_named_service_watching() {
        let facts = vec![
            began("jobs", Mode::Report, at(1)),
            began("people", Mode::Report, at(1)),
            began("people", Mode::Off, at(5)),
        ];
        let w = window(
            Gate::MachineGate,
            &["jobs", "people", "ledger"],
            at(2),
            at(9),
            &facts,
        );
        assert_eq!(w.log_clean_since, None, "{w:?}");
        assert_eq!(w.coverage[0].recording_since, Some(at(1)));
        assert!(w.coverage[1].gap.as_deref().unwrap().contains("`off`"));
        assert!(
            w.coverage[2]
                .gap
                .as_deref()
                .unwrap()
                .contains("never recorded")
        );
        assert_eq!(w.not_clean.len(), 2, "{:?}", w.not_clean);

        let mut unreadable = began("jobs", Mode::Report, at(6));
        unreadable.payload["mode"] = json!("sideways");
        let w = window(
            Gate::MachineGate,
            &["jobs"],
            at(2),
            at(9),
            &[began("jobs", Mode::Report, at(1)), unreadable],
        );
        assert_eq!(w.log_clean_since, None);
        assert!(w.not_clean[0].contains("unreadable"), "{:?}", w.not_clean);

        let w = window(Gate::MachineGate, &[], at(2), at(9), &facts);
        assert_eq!(w.log_clean_since, None, "naming nobody proves nothing");
    }

    /// The window is clean from the latest of: the read's start, the
    /// moment the last service began watching, and just after the
    /// latest would-refuse — which is named, with whom it names. A
    /// restart in `report` or a move to `enforce` keeps the watch; a
    /// stretch of `off` restarts it.
    #[test]
    fn the_window_is_clean_from_after_the_latest_would_refuse() {
        let facts = vec![
            began("jobs", Mode::Report, at(1)),
            began("jobs", Mode::Report, at(3)),
            began("jobs", Mode::Enforce, at(4)),
            began("people", Mode::Report, at(1)),
            began("people", Mode::Off, at(2)),
            began("people", Mode::Report, at(3)),
        ];
        let w = window(Gate::MachineGate, &["jobs", "people"], at(0), at(9), &facts);
        assert_eq!(w.coverage[0].recording_since, Some(at(1)));
        assert_eq!(
            w.coverage[1].recording_since,
            Some(at(3)),
            "off breaks the watch"
        );
        assert_eq!(w.log_clean_since, Some(at(3)));
        assert_eq!(w.clean_for(), Some(Duration::hours(6)));
        assert!(w.dirty.is_empty() && w.not_clean.is_empty(), "{w:?}");

        let mut more = facts.clone();
        more.push(missed("jobs", at(5)));
        more.push(fact(
            Gate::MachineGate,
            Fact::PreviousPresented,
            "jobs",
            at(7),
            json!({}),
        ));
        let w = window(Gate::MachineGate, &["jobs", "people"], at(0), at(9), &more);
        assert_eq!(
            w.log_clean_since,
            Some(at(5) + Duration::microseconds(1)),
            "a previous presentation is admitted in every mode and dirties nothing"
        );
        assert_eq!(w.dirty.len(), 1);
        assert_eq!(w.dirty[0].payload["key"]["peer"], "10.20.0.9");
        assert!(
            w.not_clean.is_empty(),
            "a past would-refuse shortens the window"
        );

        // Read from after it, the would-refuse is history the read does
        // not reach, and the window is clean from the read's start.
        let w = window(Gate::MachineGate, &["jobs", "people"], at(6), at(9), &more);
        assert_eq!(w.log_clean_since, Some(at(6)));
    }

    /// The double answers the port's contract on its own: the facts at
    /// or after the read's start, each service's newest
    /// `recording_began` before it, oldest first — and nothing of the
    /// other gate. The shared suite in boss-events holds the Postgres
    /// adapter to the same answers.
    #[tokio::test]
    async fn the_double_reads_the_window_and_the_mode_it_opened_in() {
        let log = InMemoryGateEvidence::new(vec![
            began("jobs", Mode::Off, at(1)),
            began("jobs", Mode::Report, at(2)),
            missed("jobs", at(1)),
            missed("jobs", at(4)),
            fact(
                Gate::PolicyCheck,
                Fact::WouldRefuse,
                "policy",
                at(5),
                json!({}),
            ),
        ]);
        let got = log.facts(Gate::MachineGate, at(3)).await.unwrap();
        let shape: Vec<(String, DateTime<Utc>)> =
            got.iter().map(|e| (e.kind.clone(), e.timestamp)).collect();
        assert_eq!(
            shape,
            vec![
                ("machine_gate.recording_began".to_string(), at(2)),
                ("machine_gate.would_refuse".to_string(), at(4)),
            ]
        );
    }
}
