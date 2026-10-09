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

#[tokio::test]
async fn an_over_bound_pg_population_refuses_instead_of_returning_a_clean_prefix() {
    let db = boss_testing::TestDb::new().await;
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) SELECT gen_random_uuid(),$1,'suite-bound','machine_gate.previous_presented','{}'::jsonb FROM generate_series(1,65537)")
        .bind(at(1)).execute(&db.pool).await.unwrap();
    let result = PgGateEvidence::new(db.pool.clone())
        .facts(Gate::MachineGate, at(0))
        .await;
    assert!(
        result.is_err(),
        "a bounded adapter must not return an incomplete clean prefix"
    );
}

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
        the_actor_role_window_is_judged_per_process_through_either_adapter,
        the_starts_a_roster_generation_is_judged_by_read_the_same_through_either_adapter,
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
/// one. Facts from that opening epoch remain visible: a first sighting
/// cannot prove when the final usage stopped.
async fn each_service_opens_the_window_in_its_newest_earlier_mode<W: World>(w: &W, adapter: &str) {
    for e in [
        began("suite-jobs", Mode::Report, at(1)),
        began("suite-jobs", Mode::Off, at(2)),
        began("suite-jobs", Mode::Report, at(6)),
        began("suite-people", Mode::Report, at(0)),
        missed("suite-people", at(3), "10.20.0.2"),
    ] {
        w.record(e).await;
    }
    let got = read(w, Gate::MachineGate, at(4)).await;
    assert_eq!(
        shape(&got),
        vec![
            row("machine_gate.recording_began", "suite-people", at(0)),
            row("machine_gate.recording_began", "suite-jobs", at(2)),
            row("machine_gate.would_refuse", "suite-people", at(3)),
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

/// Backlog e0bdba74: the actor-role report's facts take the same road —
/// outbox, relay, `audit_log` — and are read by the same statement, so
/// its window is judged from either adapter alike. `suite-role-jobs`
/// restarted across a clean end; its first process sighted a shape
/// before the read opened. The read keeps that sighting, the join
/// refuses to call it unused, and another gate's read holds none of it.
async fn the_actor_role_window_is_judged_per_process_through_either_adapter<W: World>(
    w: &W,
    adapter: &str,
) {
    use boss_core::gate_window::{LiveRead, join_window};
    let gate = Gate::ActorRole;
    let service = "suite-role-jobs";
    let start = |instance: &str, at_: DateTime<Utc>| {
        fact(
            gate,
            Fact::RecordingBegan,
            service,
            at_,
            json!({"mode": "report", "since": at_, "instance": instance}),
        )
    };
    let sighting = fact(
        gate,
        Fact::WouldRefuse,
        service,
        at(1),
        json!({"instance": "role-a", "mode": "report", "recording_since": at(0),
               "reason": "would-deny",
               "key": {"actor": "agent-claude", "asserted_role": "platform-admin",
                       "recorded_role": "engineering-agent", "action": "update",
                       "resource": "job", "lookup_status": "registered",
                       "would_deny": true}}),
    );
    for e in [
        start("role-a", at(0)),
        sighting.clone(),
        fact(
            gate,
            Fact::RecordingEnded,
            service,
            at(5) - chrono::Duration::minutes(2),
            json!({"instance": "role-a", "clean": true, "lost": 0, "unstated": 0}),
        ),
        start("role-b", at(5)),
    ] {
        w.record(e).await;
    }
    let live = |instance: &str| LiveRead {
        service: service.into(),
        answer: Ok(json!({"service": service, "report": {
            "mode": "report", "recording_since": at(5), "rows": [], "overflow": 0,
            "not_clean": [],
            "evidence": {"recorder": true, "instance": instance, "lost": 0, "unstated": 0,
                         "retrying": false, "last_error": null}}})),
    };
    let facts = read(w, gate, at(2)).await;
    assert_eq!(
        shape(&facts),
        vec![
            row("actor_role.recording_began", service, at(0)),
            row("actor_role.would_refuse", service, at(1)),
            row(
                "actor_role.recording_ended",
                service,
                at(5) - chrono::Duration::minutes(2)
            ),
            row("actor_role.recording_began", service, at(5)),
        ],
        "{adapter}: the opening process's sighting is kept with its start"
    );
    let v = join_window(
        gate,
        &[service.to_string()],
        at(2),
        at(9),
        Ok(facts),
        vec![live("role-b")],
    );
    assert!(
        !v.covers_requested_window
            && v.not_clean
                .iter()
                .any(|why| why.contains("unknown final usage")),
        "{adapter}: first seen before the window by a process that ended inside it: {v:?}"
    );
    // Read from after that process ended, the window is the second
    // process's own, and clean.
    let facts = read(w, gate, at(6)).await;
    let v = join_window(
        gate,
        &[service.to_string()],
        at(6),
        at(9),
        Ok(facts),
        vec![live("role-b")],
    );
    assert!(v.covers_requested_window, "{adapter}: {v:?}");
    assert!(
        !shape(&read(w, Gate::MachineGate, at(0)).await)
            .iter()
            .any(|(kind, _, _)| kind.starts_with("actor_role.")),
        "{adapter}: another gate's read holds none of this gate's kinds"
    );
}

/// Review 6858ef1d, B1, through either adapter. The launch roster rule
/// judges each required service by the start of the process running
/// when the window opened and by each start inside it
/// (`gate_window::judged_generations`). The port must hand back exactly
/// those starts — each service's NEWEST before the window, roster stamp
/// whole, and every one inside — or the rule would read a different
/// verdict from the double than from the log.
async fn the_starts_a_roster_generation_is_judged_by_read_the_same_through_either_adapter<
    W: World,
>(
    w: &W,
    adapter: &str,
) {
    use boss_core::gate_window::judged_generations;
    let declared = "sha256:suite-declared";
    let stamped = |service: &str, when: DateTime<Utc>, roster: serde_json::Value| {
        let mut e = began(service, Mode::Report, when);
        e.payload["roster"] = roster;
        e
    };
    let good = || json!({"generation": declared, "commit": "suite"});
    let unreadable = "the launch record /etc/boss-launch-record is unreadable: EIO";
    for e in [
        // An older process of the required service: replaced, the past.
        stamped("suite-policy", at(0), good()),
        // Its process at the window's opening could not read its record.
        stamped(
            "suite-policy",
            at(1),
            json!({"error": unreadable, "commit": null}),
        ),
        // Another service states a good start after it, before the
        // window opens, and again inside it.
        stamped("suite-jobs", at(2), good()),
        stamped("suite-jobs", at(6), good()),
    ] {
        w.record(e).await;
    }
    let required = ["suite-policy".to_string()];
    let facts = read(w, Gate::MachineGate, at(4)).await;
    assert_eq!(
        shape(&facts),
        vec![
            row("machine_gate.recording_began", "suite-policy", at(1)),
            row("machine_gate.recording_began", "suite-jobs", at(2)),
            row("machine_gate.recording_began", "suite-jobs", at(6)),
        ],
        "{adapter}"
    );
    assert_eq!(facts[0].payload["roster"]["error"], unreadable, "{adapter}");
    let (generations, why) =
        judged_generations(Gate::MachineGate, &facts, &required, at(4), declared);
    assert_eq!(why.len(), 1, "{adapter}: {why:?}");
    assert!(
        why[0].starts_with("suite-policy: 1 of the 1 process start(s)")
            && why[0].contains(unreadable),
        "{adapter}: {why:?}"
    );
    assert_eq!(
        generations
            .iter()
            .map(|g| (g.service.as_str(), g.generation.as_deref(), g.since))
            .collect::<Vec<_>>(),
        vec![
            ("suite-policy", None, at(1)),
            ("suite-jobs", Some(declared), at(2)),
        ],
        "{adapter}"
    );
    // The required service relaunches under the declared generation
    // before the window opens: its unreadable start is no longer the
    // one the port returns, and the window is judged under one.
    w.record(stamped("suite-policy", at(3), good())).await;
    let facts = read(w, Gate::MachineGate, at(4)).await;
    let (generations, why) =
        judged_generations(Gate::MachineGate, &facts, &required, at(4), declared);
    assert!(why.is_empty(), "{adapter}: {why:?}");
    assert_eq!(generations.len(), 1, "{adapter}: {generations:?}");
    assert_eq!(
        generations[0].generation.as_deref(),
        Some(declared),
        "{adapter}"
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

/// Backlog e0bdba74, end to end and wired the way every service binary
/// wires it (`boss_events::role_tally::durable`): a process sees one
/// shape `enforce` would answer differently, three times, and is gone;
/// the relay carries its facts to `audit_log`; and the NEXT process —
/// which has seen nothing — answers a window that holds the shape. Until
/// this the answer was `durable_window: false` and an empty tally.
#[tokio::test(flavor = "multi_thread")]
async fn the_role_reports_window_is_read_from_audit_log_by_the_next_process() {
    use boss_policy_client::role_reporting::{
        ReportMode, ReportModeSource, RoleObservation, RoleReportSink,
    };
    let db = boss_testing::TestDb::new().await;
    let service = "suite-role-report";
    let mode: Arc<dyn ReportModeSource> = Arc::new(ReportMode::Report);
    let opened = Utc::now() - chrono::Duration::seconds(1);
    // Relay until the log holds `wanted` of this service's facts.
    let relayed = |wanted: usize| {
        let (pool, log) = (db.pool.clone(), PgGateEvidence::new(db.pool.clone()));
        async move {
            for _ in 0..400 {
                let bus = RecordingEventBus::new();
                drain_outbox_once(&pool, &(bus as Arc<dyn EventBus>), 100)
                    .await
                    .expect("drain");
                let held = log.facts(Gate::ActorRole, opened).await.unwrap();
                if held.len() >= wanted {
                    return held;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("the log never held {wanted} fact(s)");
        }
    };
    {
        let first = boss_events::role_tally::durable(service, Arc::clone(&mode), Some(&db.pool));
        for _ in 0..3 {
            first.record(RoleObservation {
                actor: "agent-claude".into(),
                asserted_role: "platform-admin".into(),
                recorded_actor: Some("agent-claude".into()),
                recorded_role: Some("engineering-agent".into()),
                action: "update".into(),
                resource: "job".into(),
                lookup_status: "registered".into(),
                asserted_allowed: Some(true),
                recorded_allowed: Some(false),
                would_deny: Some(true),
                would_change_scope: Some(false),
            });
        }
        let held = relayed(2).await;
        assert_eq!(
            shape(&held)
                .into_iter()
                .map(|(kind, service, _)| (kind, service))
                .collect::<Vec<_>>(),
            vec![
                (
                    "actor_role.recording_began".to_string(),
                    service.to_string()
                ),
                ("actor_role.would_refuse".to_string(), service.to_string()),
            ],
            "three observations of one shape are ONE fact"
        );
    }
    let second = boss_events::role_tally::durable(service, mode, Some(&db.pool));
    relayed(3).await;
    let answer = second.durable_snapshot().await;
    assert!(answer.rows.is_empty(), "this process has seen nothing");
    assert!(answer.durable_window, "{answer:?}");
    let window = answer.window.expect("a window read from audit_log");
    let log_half = window.log.expect("the log half");
    assert_eq!(log_half.dirty.len(), 1, "{log_half:?}");
    assert_eq!(log_half.dirty[0].payload["key"]["actor"], "agent-claude");
    assert_eq!(log_half.dirty[0].payload["reason"], "would-deny");
    // The first process was dropped, not ended: it stated no clean end,
    // so the watch starts with the second — said, never bridged.
    assert!(
        log_half.coverage[0]
            .started_after
            .as_deref()
            .is_some_and(|why| why.contains("stated no clean end")),
        "{log_half:?}"
    );
    assert_eq!(
        window.clean_since,
        Some(answer.recording_since),
        "{:?}",
        window.not_clean
    );
    assert!(!window.covers_requested_window);
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
            OR kind_pattern LIKE 'actor\\_role.%' \
         ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    let mut kinds: Vec<String> = KINDS.iter().map(|k| k.to_string()).collect();
    kinds.sort();
    assert_eq!(registered, kinds);
}

#[tokio::test]
async fn opening_epoch_sightings_survive_a_later_clean_retirement_in_both_adapters() {
    let db = boss_testing::TestDb::new().await;
    let memory = InMemory(Mutex::new(Vec::new()));
    let postgres = Postgres(db.pool.clone());
    let service = "suite-retired";
    let start = began(service, Mode::Report, at(0));
    let sighting = fact(
        Gate::MachineGate,
        Fact::PreviousPresented,
        service,
        at(1),
        json!({"instance":format!("{service}-p1"),"mode":"report","recording_since":at(0),"key":{"presented":"previous"}}),
    );
    let end = fact(
        Gate::MachineGate,
        Fact::RecordingEnded,
        service,
        at(3),
        json!({"instance":format!("{service}-p1"),"clean":true}),
    );
    for record in [start, sighting.clone(), end] {
        memory.record(record.clone()).await;
        postgres.record(record).await;
    }
    let expected = memory.log().facts(Gate::MachineGate, at(2)).await.unwrap();
    let actual = postgres
        .log()
        .facts(Gate::MachineGate, at(2))
        .await
        .unwrap();
    assert!(actual.iter().any(|record| record.id == sighting.id));
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[tokio::test]
async fn an_over_byte_bound_pg_fact_refuses_instead_of_allocating_a_clean_prefix() {
    let db = boss_testing::TestDb::new().await;
    sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES(gen_random_uuid(),$1,'suite-byte-bound','machine_gate.previous_presented',jsonb_build_object('oversized',repeat('x',8388609)))")
        .bind(at(1)).execute(&db.pool).await.unwrap();
    assert!(
        PgGateEvidence::new(db.pool.clone())
            .facts(Gate::MachineGate, at(0))
            .await
            .is_err()
    );
}
