//! The actor-role report states what `enforce` would answer differently
//! as FACTS on the log, and reads its window from the log (backlog
//! e0bdba74). Until this the tally was process memory in a pod every
//! train restarts, so the 72 hours row H of design abf9eeae asks for
//! could never be read: `durable_window` was a constant `false`.
//!
//! Everything here goes through the two ports — the recorder the facts
//! are handed to and the log the window is read from — with one
//! in-memory adapter standing for both, as the outbox and `audit_log`
//! are one record in production.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::gate_evidence::{Evidence, Fact, Gate, GateEvidenceLog, InMemoryGateEvidence};
use boss_core::machine_gate::Mode;
use boss_core::port::EventRecorder;
use boss_policy_client::role_reporting::{
    ReportMode, ReportModeSource, ReportTally, RoleObservation, RoleReportSink,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

const GATE: Gate = Gate::ActorRole;

/// The log: what the recorder took, read back as the window's port.
#[derive(Default)]
struct Log {
    events: Mutex<Vec<Event>>,
    dark: Mutex<Option<String>>,
}

impl Log {
    fn kinds(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect()
    }
    fn of(&self, fact: Fact) -> Vec<Event> {
        let kind = GATE.kind(fact);
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl EventRecorder for Log {
    async fn record(&self, event: &Event) -> Result<(), String> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[async_trait]
impl GateEvidenceLog for Log {
    async fn facts(&self, gate: Gate, from: DateTime<Utc>) -> Result<Vec<Event>, String> {
        if let Some(why) = self.dark.lock().unwrap().clone() {
            return Err(why);
        }
        let held = self.events.lock().unwrap().clone();
        InMemoryGateEvidence::new(held).facts(gate, from).await
    }
}

/// A mode word that can be moved, as the mounted file is.
struct Word(Mutex<(Mode, DateTime<Utc>)>);

impl Word {
    fn at(mode: Mode) -> Arc<Self> {
        Arc::new(Word(Mutex::new((mode, Utc::now() - Duration::seconds(5)))))
    }
    fn move_to(&self, mode: Mode) {
        *self.0.lock().unwrap() = (mode, Utc::now());
    }
}

impl ReportModeSource for Word {
    fn mode(&self) -> ReportMode {
        match self.0.lock().unwrap().0 {
            Mode::Off => ReportMode::Off,
            _ => ReportMode::Report,
        }
    }
    fn reading_since(&self) -> (Mode, Option<DateTime<Utc>>) {
        let held = self.0.lock().unwrap();
        (held.0, Some(held.1))
    }
}

/// One process of `service`: its tally, and the task handing its facts
/// to `log`, which states the process's end when `end` is awaited.
struct Process {
    tally: Arc<ReportTally>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<bool>,
}

impl Process {
    fn start(service: &str, word: Arc<Word>, log: Arc<Log>) -> Self {
        let (evidence, rx) = Evidence::channel(GATE, service);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let hand_off = evidence.clone();
        let recorder = log.clone() as Arc<dyn EventRecorder>;
        let task = tokio::spawn(async move {
            hand_off
                .run(rx, recorder, async {
                    let _ = stopped.await;
                })
                .await
        });
        let tally = Arc::new(ReportTally::recording(8, word, evidence).reading(log));
        Process { tally, stop, task }
    }

    /// SIGTERM, as the library handles it: drain, then state the end.
    async fn end(self) {
        self.stop.send(()).unwrap();
        assert!(self.task.await.unwrap());
    }

    async fn drained(&self, log: &Log, kinds: usize) {
        for _ in 0..500 {
            if log.kinds().len() >= kinds {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the log holds {:?}, wanted {kinds}", log.kinds());
    }
}

fn matching(actor: &str) -> RoleObservation {
    RoleObservation {
        actor: actor.into(),
        asserted_role: "platform-admin".into(),
        recorded_actor: Some(actor.into()),
        recorded_role: Some("platform-admin".into()),
        action: "update".into(),
        resource: "job".into(),
        lookup_status: "registered".into(),
        asserted_allowed: Some(true),
        recorded_allowed: Some(true),
        would_deny: Some(false),
        would_change_scope: Some(false),
    }
}

fn denied() -> RoleObservation {
    RoleObservation {
        recorded_role: Some("engineering-agent".into()),
        recorded_allowed: Some(false),
        would_deny: Some(true),
        ..matching("agent-claude")
    }
}

fn unregistered_writer() -> RoleObservation {
    RoleObservation {
        recorded_actor: None,
        recorded_role: None,
        lookup_status: "unregistered".into(),
        recorded_allowed: None,
        would_deny: None,
        would_change_scope: None,
        ..matching("automation:nobody-registered-this")
    }
}

#[tokio::test]
async fn a_shape_enforce_would_answer_differently_is_stated_once_and_a_match_never() {
    let log = Arc::new(Log::default());
    let p = Process::start("jobs", Word::at(Mode::Report), log.clone());
    for _ in 0..3 {
        p.tally.record(matching("emp-david"));
        p.tally.record(denied());
        p.tally.record(unregistered_writer());
    }
    p.drained(&log, 3).await;
    assert_eq!(
        log.kinds(),
        [
            "actor_role.recording_began",
            "actor_role.would_refuse",
            "actor_role.would_refuse"
        ],
        "one fact per new shape per process — never one per request, and none for a match"
    );
    let stated = log.of(Fact::WouldRefuse);
    assert_eq!(stated[0].source, "jobs");
    assert_eq!(stated[0].payload["reason"], "would-deny");
    assert_eq!(stated[0].payload["mode"], "report");
    assert_eq!(stated[0].payload["key"]["actor"], "agent-claude");
    assert_eq!(
        stated[0].payload["key"]["recorded_role"],
        "engineering-agent"
    );
    assert_eq!(stated[1].payload["reason"], "unregistered-writer");
    // What a fact carries, whole: the shape and its provenance. No
    // header, no token, no request body, no path beyond the door's name.
    let mut fields: Vec<&str> = stated[0]
        .payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "_actor",
            "instance",
            "key",
            "mode",
            "reason",
            "recording_since",
            "service"
        ]
    );
    assert_eq!(
        stated[0].payload["_actor"], "automation:jobs",
        "the stamp, not the caller"
    );
    let mut shape: Vec<&str> = stated[0].payload["key"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    shape.sort();
    assert_eq!(
        shape,
        [
            "action",
            "actor",
            "asserted_allowed",
            "asserted_role",
            "lookup_status",
            "recorded_actor",
            "recorded_allowed",
            "recorded_role",
            "resource",
            "would_change_scope",
            "would_deny"
        ]
    );

    let live = p.tally.snapshot();
    assert_eq!(live.rows.len(), 3);
    assert_eq!(live.rows.iter().map(|r| r.count).sum::<u64>(), 9);
    assert_eq!(
        live.not_clean,
        ["2 shape(s), 6 observation(s), that `enforce` answers differently"]
    );
    assert!(live.evidence.recorder);
}

#[tokio::test]
async fn the_bounds_are_the_tallys_and_past_them_one_fact_says_so() {
    let log = Arc::new(Log::default());
    let p = Process::start("jobs", Word::at(Mode::Report), log.clone());
    // Eight rows are held; a caller spraying ids gets no ninth fact.
    for n in 0..40 {
        p.tally.record(RoleObservation {
            actor: format!("automation:sprayed-{n}"),
            ..unregistered_writer()
        });
    }
    // An oversized shape is counted, never shortened into evidence.
    p.tally.record(RoleObservation {
        actor: "x".repeat(5000),
        ..unregistered_writer()
    });
    p.drained(&log, 10).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(log.of(Fact::WouldRefuse).len(), 8);
    let overflowed = log.of(Fact::TallyOverflowed);
    assert_eq!(overflowed.len(), 1, "the first overflow, once");
    assert_eq!(overflowed[0].payload["scope"], "tally");
    assert_eq!(log.kinds().len(), 10, "began + 8 shapes + 1 overflow");
    let live = p.tally.snapshot();
    assert_eq!(live.overflow, 33);
    assert!(live.not_clean.iter().any(|why| why.contains("overflow 33")));
    assert!(
        log.events
            .lock()
            .unwrap()
            .iter()
            .all(|e| serde_json::to_vec(e).unwrap().len() < 8192),
        "every fact is bounded by the tally's byte cap"
    );
}

#[tokio::test]
async fn a_mode_move_begins_the_tally_again_and_off_records_nothing() {
    let log = Arc::new(Log::default());
    let word = Word::at(Mode::Report);
    let p = Process::start("jobs", word.clone(), log.clone());
    p.tally.record(denied());
    let first = p.tally.snapshot().recording_since;

    word.move_to(Mode::Off);
    // Read with no request arriving: the move is stated all the same.
    let off = p.tally.snapshot();
    assert_eq!(off.mode, Mode::Off);
    assert!(
        off.rows.is_empty(),
        "a tally spanning a move describes two modes"
    );
    assert!(off.recording_since > first);
    assert!(off.not_clean.iter().any(|why| why.contains("`off`")));
    p.tally.record(denied());
    assert!(p.tally.snapshot().rows.is_empty(), "off records nothing");

    word.move_to(Mode::Report);
    p.tally.record(denied());
    p.drained(&log, 5).await;
    assert_eq!(
        log.kinds(),
        [
            "actor_role.recording_began",
            "actor_role.would_refuse",
            "actor_role.recording_began",
            "actor_role.recording_began",
            "actor_role.would_refuse"
        ]
    );
    let began = log.of(Fact::RecordingBegan);
    assert_eq!(
        began
            .iter()
            .map(|e| e.payload["mode"].clone())
            .collect::<Vec<_>>(),
        [json!("report"), json!("off"), json!("report")]
    );
    assert_eq!(began[0].payload["instance"], began[2].payload["instance"]);
}

/// The item's claim, reversed: the process that answers is seconds old,
/// and the window it reads reaches back through the process before it.
#[tokio::test]
async fn the_window_is_read_from_the_log_and_a_restart_does_not_reset_it() {
    let log = Arc::new(Log::default());
    let word = Word::at(Mode::Report);

    let first = Process::start("jobs", word.clone(), log.clone());
    first.tally.record(matching("emp-david"));
    first.drained(&log, 1).await;
    let began = first.tally.snapshot().recording_since;
    first.end().await;
    assert_eq!(log.kinds().last().unwrap(), "actor_role.recording_ended");

    let second = Process::start("jobs", word.clone(), log.clone());
    second.drained(&log, 3).await;
    let answer = second.tally.durable_snapshot().await;
    assert!(answer.durable_window, "{answer:#?}");
    assert_eq!(answer.window_error, None);
    let window = answer.window.as_ref().expect("a window read from the log");
    assert_eq!(window.gate, GATE);
    assert_eq!(window.required_services, ["jobs"]);
    assert!(window.not_clean.is_empty(), "{:#?}", window.not_clean);
    assert_eq!(
        window.clean_since,
        Some(began),
        "clean since the FIRST process began, not since this one did"
    );
    assert!(answer.recording_since > began);
    assert!(
        !window.covers_requested_window,
        "seconds of watching are not the 72 hours asked for"
    );
    assert_eq!((window.now - window.from).num_hours(), 72);

    // A shape the first process would have stated is still in the
    // window after it is gone.
    let third_log = Arc::new(Log::default());
    let a = Process::start("jobs", word.clone(), third_log.clone());
    a.tally.record(denied());
    a.drained(&third_log, 2).await;
    a.end().await;
    let b = Process::start("jobs", word, third_log.clone());
    b.drained(&third_log, 4).await;
    let answer = b.tally.durable_snapshot().await;
    assert!(answer.rows.is_empty(), "this process has seen nothing");
    assert!(answer.durable_window);
    let window = answer.window.unwrap();
    let log_half = window.log.unwrap();
    assert_eq!(log_half.dirty.len(), 1);
    assert_eq!(log_half.dirty[0].payload["key"]["actor"], "agent-claude");
    assert!(
        window
            .clean_since
            .is_some_and(|since| since > log_half.dirty[0].at)
    );
}

#[tokio::test]
async fn the_answer_is_about_this_service_and_another_services_facts_are_not_its_window() {
    let log = Arc::new(Log::default());
    let word = Word::at(Mode::Report);
    let people = Process::start("people", word.clone(), log.clone());
    people.tally.record(denied());
    let jobs = Process::start("jobs", word, log.clone());
    jobs.drained(&log, 3).await;
    let answer = jobs.tally.durable_snapshot().await;
    let window = answer.window.unwrap();
    assert!(window.log.unwrap().dirty.is_empty());
    assert!(window.not_clean.is_empty(), "{:#?}", window.not_clean);
}

#[tokio::test]
async fn durable_is_true_only_when_the_log_was_read() {
    // No log given: the tally every test double and a binary with no
    // database has.
    let bare = ReportTally::new(8);
    let answer = bare.durable_snapshot().await;
    assert!(!answer.durable_window);
    assert!(answer.window.is_none());
    assert!(answer.window_error.as_deref().unwrap().contains("no log"));
    assert!(!bare.snapshot().durable_window, "a read that read no log");

    // A dark log reads not clean and says why.
    let log = Arc::new(Log::default());
    let p = Process::start("jobs", Word::at(Mode::Report), log.clone());
    p.drained(&log, 1).await;
    *log.dark.lock().unwrap() = Some("audit_log: connection refused".into());
    let answer = p.tally.durable_snapshot().await;
    assert!(!answer.durable_window);
    assert!(answer.window.is_none());
    assert_eq!(
        answer.window_error.as_deref(),
        Some("audit_log: connection refused")
    );
    // The log answers again: so does the window.
    *log.dark.lock().unwrap() = None;
    assert!(p.tally.durable_snapshot().await.durable_window);

    // A log to read, and no recorder writing to it: what this process
    // saw is on no log, so no window over the log is its window.
    let unrecorded =
        ReportTally::recording(8, Word::at(Mode::Report), Evidence::none(GATE, "jobs"))
            .reading(log.clone());
    let answer = unrecorded.durable_snapshot().await;
    assert!(!answer.durable_window);
    assert!(
        answer
            .window_error
            .as_deref()
            .unwrap()
            .contains("no recorder"),
        "{:?}",
        answer.window_error
    );
}

#[tokio::test]
async fn a_process_whose_start_the_log_does_not_hold_yet_is_not_read_clean() {
    // The relay has not delivered this process's start: the log's newest
    // start for the service is another process's.
    let log = Arc::new(Log::default());
    let earlier = Process::start("jobs", Word::at(Mode::Report), log.clone());
    earlier.drained(&log, 1).await;
    let (evidence, _undelivered) = Evidence::channel(GATE, "jobs");
    let answer = ReportTally::recording(8, Word::at(Mode::Report), evidence)
        .reading(log)
        .durable_snapshot()
        .await;
    assert!(answer.durable_window, "the log was read");
    let window = answer.window.unwrap();
    assert!(!window.covers_requested_window);
    assert!(window.clean_since.is_none());
    assert!(
        window
            .not_clean
            .iter()
            .any(|why| why.contains("does not match sourced recording_began")),
        "{:#?}",
        window.not_clean
    );
}

/// The mounted word, as the tally reads it: `off` is off, a word this
/// precursor cannot act on is the `report` it behaves as, and a move of
/// the file is a new instant.
#[test]
fn the_mounted_word_is_stated_as_what_the_binary_does_under_it() {
    use boss_core::machine_gate::ModeSwitch;
    use boss_policy_client::role_reader::MountedReportMode;
    let switch = Arc::new(ModeSwitch::new("test", "unused", (Mode::Enforce, None)));
    let mounted = MountedReportMode::over(switch.clone());
    let (stated, since) = mounted.reading_since();
    assert_eq!(stated, Mode::Report);
    let since = since.expect("a mounted word says when it last moved");
    assert!(switch.observe((Mode::Off, None)));
    let (stated, moved) = mounted.reading_since();
    assert_eq!(stated, Mode::Off);
    assert!(moved.unwrap() >= since);
    // A fixed mode never moves.
    assert_eq!(ReportMode::Report.reading_since(), (Mode::Report, None));
    assert_eq!(ReportMode::Off.reading_since(), (Mode::Off, None));
}

#[test]
fn the_report_serializes_what_the_joined_reader_requires() {
    let tally = ReportTally::new(8);
    tally.record(denied());
    let wire: Value = serde_json::to_value(tally.snapshot()).unwrap();
    for field in [
        "mode",
        "recording_since",
        "evidence",
        "not_clean",
        "overflow",
        "rows",
    ] {
        assert!(!wire[field].is_null(), "{field} missing: {wire}");
    }
    let row = &wire["rows"][0];
    assert_eq!(row["would_refuse"], "would-deny");
    assert_eq!(row["count"], 1);
    assert!(row["first_seen"].is_string() && row["last_seen"].is_string());
    assert_eq!(wire["durable_window"], false);
}
