//! The log half of a refusing gate's clean window answers the same on
//! both adapters (design 21946380, backlog b0787727; reliability
//! mechanism C of design 3036296f, `boss_testing::adapters_agree!`):
//! `GateEvidenceLog`, which `InMemoryGateEvidence` and `PgGateEvidence`
//! implement.
//!
//! WHY THIS PORT. The machine gate's and the policy check's tallies
//! restart with their process, and every train restarts the pod, so the
//! 72-hour window rows C and D of design b08725c2 ask for was unmeetable
//! from memory. Each gate now states its facts on the log; this port
//! reads them back, and `gate_evidence::window` judges them. The enforce
//! cars, the hourly alarm and the rotation's revoke will read through it
//! — their tests through the double, production through Postgres — so
//! the two must answer one question one way.
//!
//! THE POSTGRES WORLD RECORDS THE WAY PRODUCTION DOES: through the
//! outbox (`PgOutboxRecorder`, what every service binary hands its
//! gate) and the relay's drain into `audit_log`. So a green case here is
//! also the claim that a fact a gate states reaches the log the window
//! is read from — `the_machine_gates_facts_outlive_its_process` drives a
//! real gate down that path end to end.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal. Every service this file names
//! starts `suite-`, and both adapters start empty.

use std::sync::{Arc, Mutex};

use boss_core::event::Event;
use boss_core::gate_evidence::{Fact, Gate, GateEvidenceLog, InMemoryGateEvidence, KINDS, window};
use boss_core::machine_gate::Mode;
use boss_core::port::{EventBus, EventRecorder};
use boss_events::PgGateEvidence;
use boss_events::outbox::{PgOutboxRecorder, drain_outbox_once};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

/// The port, and the one way into a read-only port: record a fact the
/// way a gate does.
trait World {
    type R: GateEvidenceLog;
    async fn record(&self, event: Event);
    /// The adapter over everything recorded so far.
    fn log(&self) -> Self::R;
}

struct InMemory(Mutex<Vec<Event>>);

impl World for InMemory {
    type R = InMemoryGateEvidence;
    async fn record(&self, event: Event) {
        self.0.lock().expect("facts").push(event);
    }
    fn log(&self) -> InMemoryGateEvidence {
        InMemoryGateEvidence::new(self.0.lock().expect("facts").clone())
    }
}

struct Postgres(sqlx::PgPool);

impl World for Postgres {
    type R = PgGateEvidence;
    async fn record(&self, event: Event) {
        PgOutboxRecorder::new(self.0.clone())
            .record(&event)
            .await
            .expect("the outbox takes the fact");
        let bus = RecordingEventBus::new();
        let stats = drain_outbox_once(&self.0, &(bus as Arc<dyn EventBus>), 100)
            .await
            .expect("the relay drains it");
        assert_eq!(stats.delivered, 1, "{stats:?}");
    }
    fn log(&self) -> PgGateEvidence {
        PgGateEvidence::new(self.0.clone())
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(Mutex::new(Vec::new())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (Postgres(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_log_answers_nothing_and_no_error,
        the_window_holds_every_fact_from_its_start_inclusive,
        each_service_opens_the_window_in_its_newest_earlier_mode,
        only_the_asked_gates_kinds_are_read,
        a_tie_at_one_instant_is_broken_by_the_event_id,
        the_window_reads_the_same_verdict_through_either_adapter,
        two_processes_join_across_a_clean_end_read_through_either_adapter,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, h, 0, 0).unwrap()
}

fn fact(
    gate: Gate,
    f: Fact,
    service: &str,
    when: DateTime<Utc>,
    fields: serde_json::Value,
) -> Event {
    let mut payload = fields;
    payload["service"] = json!(service);
    Event {
        id: Uuid::new_v4(),
        timestamp: when,
        source: service.to_string(),
        kind: gate.kind(f),
        payload,
    }
}

/// A start or mode move of the service's one process in these cases.
fn began(service: &str, mode: Mode, since: DateTime<Utc>) -> Event {
    fact(
        Gate::MachineGate,
        Fact::RecordingBegan,
        service,
        since,
        json!({"mode": mode, "since": since, "instance": format!("{service}-p1")}),
    )
}

fn missed(service: &str, when: DateTime<Utc>, peer: &str) -> Event {
    fact(
        Gate::MachineGate,
        Fact::WouldRefuse,
        service,
        when,
        json!({"mode": "report", "key": {"peer": peer, "presented": "none"}}),
    )
}

async fn read<W: World>(w: &W, gate: Gate, from: DateTime<Utc>) -> Vec<Event> {
    w.log().facts(gate, from).await.expect("facts answers")
}

/// (kind, service, instant) — what a case compares.
fn shape(events: &[Event]) -> Vec<(String, String, DateTime<Utc>)> {
    events
        .iter()
        .map(|e| {
            (
                e.kind.clone(),
                e.payload["service"].as_str().unwrap_or("-").to_string(),
                e.timestamp,
            )
        })
        .collect()
}

fn row(kind: &str, service: &str, when: DateTime<Utc>) -> (String, String, DateTime<Utc>) {
    (kind.to_string(), service.to_string(), when)
}

// ----- cases ---------------------------------------------------------------

async fn an_empty_log_answers_nothing_and_no_error<W: World>(w: &W, adapter: &str) {
    assert!(
        read(w, Gate::MachineGate, at(0)).await.is_empty(),
        "{adapter}"
    );
    assert!(
        read(w, Gate::PolicyCheck, at(0)).await.is_empty(),
        "{adapter}"
    );
}

/// Every fact of the gate at or after the window's start — the boundary
/// instant INCLUDED — oldest first, whatever order it was recorded in,
/// with its payload whole.
async fn the_window_holds_every_fact_from_its_start_inclusive<W: World>(w: &W, adapter: &str) {
    for e in [
        missed("suite-jobs", at(5), "10.20.0.5"),
        missed("suite-jobs", at(3), "10.20.0.3"),
        missed("suite-jobs", at(2), "10.20.0.2"),
        fact(
            Gate::MachineGate,
            Fact::PreviousPresented,
            "suite-jobs",
            at(4),
            json!({}),
        ),
    ] {
        w.record(e).await;
    }
    let got = read(w, Gate::MachineGate, at(3)).await;
    assert_eq!(
        shape(&got),
        vec![
            row("machine_gate.would_refuse", "suite-jobs", at(3)),
            row("machine_gate.previous_presented", "suite-jobs", at(4)),
            row("machine_gate.would_refuse", "suite-jobs", at(5)),
        ],
        "{adapter}: 03:00 is inside a window from 03:00; 02:00 is not"
    );
    assert_eq!(got[0].payload["key"]["peer"], "10.20.0.3", "{adapter}");
    assert_eq!(got[0].source, "suite-jobs", "{adapter}");
}

/// The mode each service was in when the window opened is its NEWEST
/// `recording_began` before the start — one per service, never an older
/// one, and never another kind from before the start.
async fn each_service_opens_the_window_in_its_newest_earlier_mode<W: World>(w: &W, adapter: &str) {
    for e in [
        began("suite-jobs", Mode::Report, at(1)),
        began("suite-jobs", Mode::Off, at(2)),
        began("suite-jobs", Mode::Report, at(6)),
        began("suite-people", Mode::Report, at(0)),
        missed("suite-people", at(2), "10.20.0.2"),
    ] {
        w.record(e).await;
    }
    let got = read(w, Gate::MachineGate, at(4)).await;
    assert_eq!(
        shape(&got),
        vec![
            row("machine_gate.recording_began", "suite-people", at(0)),
            row("machine_gate.recording_began", "suite-jobs", at(2)),
            row("machine_gate.recording_began", "suite-jobs", at(6)),
        ],
        "{adapter}"
    );
    assert_eq!(got[1].payload["mode"], "off", "{adapter}");
}

/// A read for one gate never carries the other's facts, nor any kind
/// that is not a gate fact.
async fn only_the_asked_gates_kinds_are_read<W: World>(w: &W, adapter: &str) {
    for e in [
        missed("suite-jobs", at(2), "10.20.0.2"),
        fact(
            Gate::PolicyCheck,
            Fact::WouldRefuse,
            "suite-policy",
            at(3),
            json!({"key": {"arm": "unsigned"}}),
        ),
        Event {
            id: Uuid::new_v4(),
            timestamp: at(3),
            source: "suite-jobs".into(),
            kind: "suite.not.a.gate.fact".into(),
            payload: json!({"service": "suite-jobs"}),
        },
    ] {
        w.record(e).await;
    }
    assert_eq!(
        shape(&read(w, Gate::PolicyCheck, at(0)).await),
        vec![row("policy.check.would_refuse", "suite-policy", at(3))],
        "{adapter}"
    );
    assert_eq!(
        shape(&read(w, Gate::MachineGate, at(0)).await),
        vec![row("machine_gate.would_refuse", "suite-jobs", at(2))],
        "{adapter}"
    );
}

/// Two facts at one instant: ordered by event id in byte order, and the
/// newest-earlier `recording_began` of a service is the one whose id is
/// greatest — whichever was recorded first.
async fn a_tie_at_one_instant_is_broken_by_the_event_id<W: World>(w: &W, adapter: &str) {
    let id = |b: u8| Uuid::from_bytes([b; 16]);
    let mut low = began("suite-jobs", Mode::Off, at(1));
    low.id = id(0x11);
    let mut high = began("suite-jobs", Mode::Report, at(1));
    high.id = id(0xee);
    let mut a = missed("suite-jobs", at(5), "10.20.0.1");
    a.id = id(0xaa);
    let mut b = missed("suite-jobs", at(5), "10.20.0.2");
    b.id = id(0x22);
    for e in [high, low, a, b] {
        w.record(e).await;
    }
    let got = read(w, Gate::MachineGate, at(3)).await;
    let ids: Vec<Uuid> = got.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![id(0xee), id(0x22), id(0xaa)], "{adapter}");
}

/// The projection over either adapter's answer is one verdict: here a
/// service that watched the whole window, one that began watching
/// inside it, and a would-refuse in between.
async fn the_window_reads_the_same_verdict_through_either_adapter<W: World>(w: &W, adapter: &str) {
    for e in [
        began("suite-jobs", Mode::Report, at(0)),
        began("suite-people", Mode::Off, at(0)),
        began("suite-people", Mode::Report, at(3)),
        missed("suite-jobs", at(5), "10.20.0.9"),
    ] {
        w.record(e).await;
    }
    let facts = read(w, Gate::MachineGate, at(2)).await;
    let v = window(
        Gate::MachineGate,
        &["suite-jobs", "suite-people"],
        at(2),
        at(9),
        &facts,
    );
    assert_eq!(
        v.log_clean_since,
        Some(at(5) + chrono::Duration::microseconds(1)),
        "{adapter}: {v:?}"
    );
    assert_eq!(v.coverage[0].recording_since, Some(at(0)), "{adapter}");
    assert_eq!(v.coverage[1].recording_since, Some(at(3)), "{adapter}");
    assert_eq!(v.dirty.len(), 1, "{adapter}");
    assert_eq!(v.dirty[0].payload["key"]["peer"], "10.20.0.9", "{adapter}");
}

/// Review e4417d48, B1, through either adapter: process `a` opened the
/// window and process `b` followed it. The read carries `a`'s end, and
/// the watch joins across it only while that end says it is clean.
async fn two_processes_join_across_a_clean_end_read_through_either_adapter<W: World>(
    w: &W,
    adapter: &str,
) {
    let start = |instance: &str, at_: DateTime<Utc>| {
        fact(
            Gate::MachineGate,
            Fact::RecordingBegan,
            "suite-jobs",
            at_,
            json!({"mode": "report", "since": at_, "instance": instance}),
        )
    };
    let end = |instance: &str, at_: DateTime<Utc>, clean: bool| {
        fact(
            Gate::MachineGate,
            Fact::RecordingEnded,
            "suite-jobs",
            at_,
            json!({"instance": instance, "clean": clean}),
        )
    };
    for e in [
        start("suite-a", at(0)),
        end("suite-a", at(3) - chrono::Duration::minutes(2), true),
        start("suite-b", at(3)),
        start("suite-c", at(6)),
    ] {
        w.record(e).await;
    }
    let facts = read(w, Gate::MachineGate, at(1)).await;
    let v = window(Gate::MachineGate, &["suite-jobs"], at(1), at(9), &facts);
    assert_eq!(
        v.log_clean_since,
        Some(at(6)),
        "{adapter}: `b` stated no end, so the watch starts with `c`: {v:?}"
    );
    w.record(end("suite-b", at(6) - chrono::Duration::minutes(2), true))
        .await;
    let facts = read(w, Gate::MachineGate, at(1)).await;
    let v = window(Gate::MachineGate, &["suite-jobs"], at(1), at(9), &facts);
    assert_eq!(
        v.log_clean_since,
        Some(at(1)),
        "{adapter}: every earlier process ended clean, so the watch reaches the read's start: {v:?}"
    );
}

// ----- the path end to end, and the registry pin ----------------------------

/// A REAL machine gate, wired the way every service binary wires it
/// (`Evidence::spawn` over `PgOutboxRecorder`), states its start and a
/// tokenless caller; the relay carries both to `audit_log`; the gate —
/// the process — is dropped, which is what every train does; and the
/// log still names the caller and the mode, read through the port.
#[tokio::test(flavor = "multi_thread")]
async fn the_machine_gates_facts_outlive_its_process() {
    use axum::body::Body;
    use axum::routing::get;
    use boss_core::gate_evidence::Evidence;
    use boss_core::machine_gate::{MachineGate, Reading, Slots, gated};
    use tower::ServiceExt;

    let db = boss_testing::TestDb::new().await;
    let opened = Utc::now() - chrono::Duration::seconds(1);
    {
        let evidence = Evidence::spawn(
            Gate::MachineGate,
            "suite-things",
            PgOutboxRecorder::shared(&db.pool),
        );
        let gate = Arc::new(
            MachineGate::new(
                "suite-things",
                &[],
                Reading::new(Mode::Report, Slots::new(Some("cur".into()), None, None)),
            )
            .with_evidence(evidence.clone()),
        );
        let app = gated(
            axum::Router::new().route("/api/things", get(|| async { "ok" })),
            Arc::clone(&gate),
        );
        for _ in 0..3 {
            let req = axum::http::Request::builder()
                .uri("/api/things")
                .header("x-boss-user", r#"{"id":"agent-tokenless"}"#)
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), 200, "report admits");
        }
        // The recorder runs on its own task; wait for both facts.
        for _ in 0..400 {
            let staged: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            if staged >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            evidence.health().not_clean().is_empty(),
            "{:?}",
            evidence.health()
        );
    }
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("drain");

    let facts = PgGateEvidence::new(db.pool.clone())
        .facts(Gate::MachineGate, opened)
        .await
        .unwrap();
    assert_eq!(
        shape(&facts)
            .into_iter()
            .map(|(k, s, _)| (k, s))
            .collect::<Vec<_>>(),
        vec![
            (
                "machine_gate.recording_began".to_string(),
                "suite-things".to_string()
            ),
            (
                "machine_gate.would_refuse".to_string(),
                "suite-things".to_string()
            ),
        ],
        "three requests of one caller shape are ONE fact"
    );
    assert_eq!(facts[1].payload["key"]["user"], "agent-tokenless");
    assert_eq!(facts[1].payload["key"]["route"], "/api/things");
    let v = window(
        Gate::MachineGate,
        &["suite-things"],
        opened,
        Utc::now(),
        &facts,
    );
    assert_eq!(v.dirty.len(), 1, "{v:?}");
    assert!(v.log_clean_since.is_some_and(|s| s > facts[1].timestamp));
}

/// A FACT THAT LIVES TWICE (CLAUDE.md §9a): the kinds the gates emit
/// (`gate_evidence::KINDS`) and the `event_kinds` rows the migration
/// declares. Each kind must be registered, by exact row, in the live
/// table the migrations build — an emitted-but-undeclared kind is what
/// the audit integrity check exists to catch.
#[tokio::test(flavor = "multi_thread")]
async fn every_gate_evidence_kind_is_registered() {
    let db = boss_testing::TestDb::new().await;
    let registered: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds \
         WHERE kind_pattern LIKE 'machine\\_gate.%' OR kind_pattern LIKE 'policy.check.%' \
         ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    let mut kinds: Vec<String> = KINDS.iter().map(|k| k.to_string()).collect();
    kinds.sort();
    assert_eq!(registered, kinds);
}
