//! The two halves of design 21946380's gate window, read together.
//! This reports evidence; neither a clean answer nor a caller-chosen
//! lookback authorizes a mode change. The log arithmetic stays in
//! `gate_evidence::window`; here a live tally must match its latest
//! sourced recording start, and every required service must answer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::Event;
use crate::gate_evidence::{EvidenceHealth, Fact, Gate, Window, window};
use crate::machine_gate::{Misses, Mode};

/// One requested service's answer, or the complete transport refusal.
#[derive(Clone, Debug)]
pub struct LiveRead {
    pub service: String,
    pub answer: Result<Value, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveReport {
    pub service: String,
    /// The whole native tally, including rows and recorder health.
    pub snapshot: Option<Value>,
    pub error: Option<String>,
    /// Includes the producer's reasons and this reader's provenance gaps.
    pub not_clean: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinedWindow {
    pub gate: Gate,
    pub from: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub required_services: Vec<String>,
    pub log: Option<Window>,
    pub log_error: Option<String>,
    pub live: Vec<LiveReport>,
    pub not_clean: Vec<String>,
    pub clean_since: Option<DateTime<Utc>>,
    pub covers_requested_window: bool,
}

// No defaults: an older producer without either half's identity or
// health is missing evidence, never an empty clean tally.
#[derive(Deserialize)]
struct CommonTally {
    mode: Mode,
    recording_since: DateTime<Utc>,
    evidence: EvidenceHealth,
    not_clean: Vec<String>,
    overflow: u64,
    rows: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PolicyArm {
    Unsigned,
    Service,
}

#[derive(Deserialize)]
struct PolicyRow {
    arm: PolicyArm,
    caller: String,
    role: String,
    peer: String,
    count: u64,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

fn live_reasons(
    gate: Gate,
    service: &str,
    body: &Value,
    now: DateTime<Utc>,
    facts: &[Event],
) -> Result<Vec<String>, String> {
    let tally: CommonTally = serde_json::from_value(body.clone())
        .map_err(|e| format!("tally is missing or malformed: {e}"))?;
    let mut why = tally.not_clean.clone();
    if tally.mode == Mode::Off {
        why.push("mode off watches nothing".into());
    }
    if !tally.evidence.recorder {
        why.push("this process has no durable recorder".into());
    }
    if tally.evidence.instance.is_empty() {
        why.push("this process has no instance identity".into());
    }
    why.extend(tally.evidence.not_clean());
    if tally.recording_since > now {
        why.push("the tally begins after this observation".into());
    }
    if tally.overflow != 0 {
        why.push(format!("tally overflow {} names no caller", tally.overflow));
    }
    match gate {
        Gate::MachineGate => {
            if !body.get("source_overflow").is_some_and(Value::is_array) {
                return Err("machine tally has no complete source_overflow array".into());
            }
            let misses: Misses = serde_json::from_value(body.clone())
                .map_err(|e| format!("machine tally is malformed: {e}"))?;
            if misses.service != service {
                why.push(format!(
                    "tally names {}, requested {service}",
                    misses.service
                ));
            }
            if misses
                .rows
                .iter()
                .any(|r| r.count == 0 || r.first_seen > r.last_seen || r.last_seen > now)
            {
                why.push("machine tally has an invalid row count or time".into());
            }
            if misses
                .rows
                .iter()
                .any(|r| r.key.presented.enforce_refuses())
            {
                why.push("machine tally still holds an enforce-refused caller".into());
            }
            if misses
                .source_overflow
                .iter()
                .any(|r| r.count == 0 || r.presented.enforce_refuses())
            {
                why.push("machine tally has invalid or enforce-refused source overflow".into());
            }
        }
        Gate::PolicyCheck => {
            if service != "policy" {
                why.push("policy-check tally was requested from another service".into());
            }
            if body
                .get("service")
                .is_some_and(|s| s.as_str() != Some(service))
            {
                why.push("policy tally names a different service".into());
            }
            // The policy check has no service field; the requested port
            // and the independently sourced instance below bind it.
            for key in ["switch", "file"] {
                if body
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(|v| v.is_empty())
                {
                    return Err(format!("policy tally has no {key}"));
                }
            }
            match body.get("mode_error") {
                Some(Value::Null) => {}
                Some(Value::String(error)) => why.push(format!("policy mode is degraded: {error}")),
                _ => return Err("policy tally has no valid mode_error".into()),
            }
            let clean: Option<DateTime<Utc>> = serde_json::from_value(
                body.get("clean_since")
                    .cloned()
                    .ok_or("policy tally has no clean_since")?,
            )
            .map_err(|e| format!("policy clean_since is malformed: {e}"))?;
            if tally.not_clean.is_empty() && clean != Some(tally.recording_since) {
                why.push("policy clean_since disagrees with its clean live tally".into());
            }
            for row in &tally.rows {
                let row: PolicyRow = serde_json::from_value(row.clone())
                    .map_err(|e| format!("policy tally row is malformed: {e}"))?;
                if row.count == 0
                    || row.first_seen > row.last_seen
                    || row.last_seen > now
                    || row.caller.is_empty()
                    || row.role.is_empty()
                    || row.peer.is_empty()
                {
                    why.push("policy tally has an invalid caller, count or time".into());
                }
                match row.arm {
                    PolicyArm::Unsigned | PolicyArm::Service => {}
                }
            }
            if !tally.rows.is_empty() {
                why.push("policy tally still holds an enforce-refused caller".into());
            }
        }
    }
    let began = gate.kind(Fact::RecordingBegan);
    let latest = facts
        .iter()
        .filter(|e| {
            e.kind == began
                && e.source == service
                && e.payload.get("service").and_then(Value::as_str) == Some(service)
        })
        .max_by_key(|e| (e.timestamp, e.id));
    match latest {
        None => why.push("no sourced recording start binds this live tally to the log".into()),
        Some(e) => {
            let mode: Result<Mode, _> =
                serde_json::from_value(e.payload.get("mode").cloned().unwrap_or(Value::Null));
            let since: Result<DateTime<Utc>, _> =
                serde_json::from_value(e.payload.get("since").cloned().unwrap_or(Value::Null));
            if e.payload.get("instance").and_then(Value::as_str)
                != Some(tally.evidence.instance.as_str())
                || mode.ok() != Some(tally.mode)
                || since.ok() != Some(tally.recording_since)
            {
                why.push(format!(
                    "live instance/mode/start does not match sourced recording_began {}",
                    e.id
                ));
            }
        }
    }
    Ok(why)
}

/// Neither source alone can earn a clean reading. Preserve all answers
/// even when one fails, so a refusal names its cause and its coverage.
pub fn join_window(
    gate: Gate,
    required: &[String],
    from: DateTime<Utc>,
    now: DateTime<Utc>,
    facts: Result<Vec<Event>, String>,
    reads: Vec<LiveRead>,
) -> JoinedWindow {
    let mut why = Vec::new();
    if from >= now {
        why.push("the requested window has no positive duration".into());
    }
    let mut required_services = required.to_vec();
    required_services.sort();
    required_services.dedup();
    if required_services.is_empty()
        || required_services.len() != required.len()
        || required_services.iter().any(String::is_empty)
    {
        why.push("required service roster is empty, ambiguous or malformed".into());
    }
    let (facts, log_error) = match facts {
        Ok(facts) => (facts, None),
        Err(e) => {
            why.push(format!("log: {e}"));
            (Vec::new(), Some(e))
        }
    };
    if facts
        .iter()
        .any(|e| e.timestamp > now || e.payload.get("_simulated") == Some(&Value::Bool(true)))
    {
        why.push("log contains future or simulated gate evidence".into());
    }
    // The existing coverage projection groups starts by the named
    // service. A malformed newer start must not disappear from that
    // grouping and leave an older live instance looking current.
    // A pre-window malformed start can expire only after the same
    // source states a complete later watch before the window opens.
    // Do not use a late or backdated start to bridge unknown history.
    // Keep the projection unchanged and name each relevant unbound
    // input's immutable record and payload.
    let began = gate.kind(Fact::RecordingBegan);
    for event in facts.iter().filter(|event| event.kind == began) {
        let superseded_before_window = event.timestamp < from
            && facts.iter().any(|later| {
                let since = later
                    .payload
                    .get("since")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<DateTime<Utc>>(value).ok());
                let mode = later
                    .payload
                    .get("mode")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<Mode>(value).ok());
                later.kind == began
                    && later.source == event.source
                    && later.payload.get("service").and_then(Value::as_str)
                        == Some(event.source.as_str())
                    && later.timestamp > event.timestamp
                    && later.timestamp <= from
                    && since.is_some_and(|since| since > event.timestamp && since <= from)
                    && mode.is_some_and(|mode| mode != Mode::Off)
                    && later
                        .payload
                        .get("instance")
                        .and_then(Value::as_str)
                        .is_some_and(|instance| !instance.is_empty())
            });
        if event
            .payload
            .get("service")
            .and_then(Value::as_str)
            .is_none_or(|service| service.is_empty() || service != event.source)
            && !superseded_before_window
        {
            why.push(format!(
                "log recording_began {} from {} has missing or conflicting service provenance: {}",
                event.id, event.source, event.payload
            ));
        }
    }
    let services: Vec<&str> = required_services.iter().map(String::as_str).collect();
    let log = log_error
        .is_none()
        .then(|| window(gate, &services, from, now, &facts));
    if let Some(log) = &log {
        why.extend(log.not_clean.iter().map(|e| format!("log: {e}")));
    }
    let mut live = Vec::new();
    for service in &required_services {
        let mine: Vec<&LiveRead> = reads.iter().filter(|r| &r.service == service).collect();
        if mine.len() != 1 {
            why.push(format!(
                "{service}: {} live answers, expected exactly one",
                mine.len()
            ));
        }
        if mine.is_empty() {
            live.push(LiveReport {
                service: service.clone(),
                snapshot: None,
                error: Some("required live tally was not read".into()),
                not_clean: vec!["required live tally was not read".into()],
            });
        }
        for read in mine {
            let (snapshot, error, not_clean) = match &read.answer {
                Err(error) => (None, Some(error.clone()), vec![error.clone()]),
                Ok(body) => match live_reasons(gate, service, body, now, &facts) {
                    Ok(why) => (Some(body.clone()), None, why),
                    Err(error) => (Some(body.clone()), Some(error.clone()), vec![error]),
                },
            };
            why.extend(not_clean.iter().map(|e| format!("{service}: {e}")));
            live.push(LiveReport {
                service: service.clone(),
                snapshot,
                error,
                not_clean,
            });
        }
    }
    for read in reads
        .iter()
        .filter(|r| !required_services.contains(&r.service))
    {
        let error = "live answer names a service outside the required roster".to_string();
        why.push(format!("{}: {error}", read.service));
        live.push(LiveReport {
            service: read.service.clone(),
            snapshot: read.answer.clone().ok(),
            error: Some(error.clone()),
            not_clean: vec![error],
        });
    }
    let clean_since = why
        .is_empty()
        .then(|| log.as_ref().and_then(|l| l.log_clean_since))
        .flatten();
    JoinedWindow {
        gate,
        from,
        now,
        required_services,
        log,
        log_error,
        live,
        not_clean: why,
        clean_since,
        covers_requested_window: clean_since.is_some_and(|s| s <= from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use serde_json::json;
    use uuid::Uuid;

    fn at(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap() + Duration::hours(hours)
    }
    fn event(gate: Gate, fact: Fact, hour: i64, payload: Value) -> Event {
        Event {
            id: Uuid::new_v4(),
            timestamp: at(hour),
            source: "policy".into(),
            kind: gate.kind(fact),
            payload,
        }
    }
    fn facts(gate: Gate) -> Vec<Event> {
        vec![
            event(
                gate,
                Fact::RecordingBegan,
                0,
                json!({"service":"policy","mode":"report","since":at(0),"instance":"old"}),
            ),
            event(
                gate,
                Fact::RecordingEnded,
                74,
                json!({"service":"policy","instance":"old","clean":true,"lost":0,"unstated":0}),
            ),
            event(
                gate,
                Fact::RecordingBegan,
                74,
                json!({"service":"policy","mode":"report","since":at(74),"instance":"current"}),
            ),
        ]
    }
    fn snapshot(gate: Gate) -> Value {
        let mut v = json!({"mode":"report","recording_since":at(74),"rows":[],"overflow":0,"not_clean":[],
            "evidence":{"recorder":true,"instance":"current","lost":0,"unstated":0,"retrying":false,"last_error":null}});
        match gate {
            Gate::MachineGate => {
                v["service"] = json!("policy");
                v["source_overflow"] = json!([]);
            }
            Gate::PolicyCheck => {
                v["switch"] = json!("policy check");
                v["file"] = json!("fixture-mode");
                v["mode_error"] = Value::Null;
                v["clean_since"] = json!(at(74));
            }
        }
        v
    }
    fn read(body: Value) -> LiveRead {
        LiveRead {
            service: "policy".into(),
            answer: Ok(body),
        }
    }
    fn judge(gate: Gate, log: Vec<Event>, body: Value) -> JoinedWindow {
        join_window(
            gate,
            &["policy".into()],
            at(3),
            at(75),
            Ok(log),
            vec![read(body)],
        )
    }
    fn refuses(v: &JoinedWindow) {
        assert!(
            !v.covers_requested_window && !v.not_clean.is_empty(),
            "{v:?}"
        );
        assert!(v.clean_since.is_none(), "{v:?}");
    }

    #[test]
    fn a_young_clean_live_process_joins_a_clean_durable_restart_window() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            let v = judge(gate, facts(gate), snapshot(gate));
            assert!(v.covers_requested_window, "{v:?}");
            assert_eq!(v.clean_since, Some(at(3)));
            assert_eq!(v.log.unwrap().coverage[0].recording_since, Some(at(0)));
            assert_eq!(
                v.live[0].snapshot.as_ref().unwrap()["recording_since"],
                json!(at(74))
            );
        }
    }

    #[test]
    fn neither_a_clean_log_nor_a_clean_live_tally_is_enough() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            let mut body = snapshot(gate);
            body["not_clean"] = json!(["caller keeps missing"]);
            if gate == Gate::PolicyCheck {
                body["clean_since"] = Value::Null;
            }
            let v = judge(gate, facts(gate), body);
            refuses(&v);
            assert_eq!(v.live[0].not_clean[0], "caller keeps missing");
            refuses(&judge(gate, vec![], snapshot(gate)));
            let v = join_window(
                gate,
                &["policy".into()],
                at(3),
                at(75),
                Err("audit read refused".into()),
                vec![read(snapshot(gate))],
            );
            refuses(&v);
            assert_eq!(v.log_error.as_deref(), Some("audit read refused"));
            assert!(
                v.live[0].snapshot.is_some(),
                "the readable half is retained"
            );
        }
    }

    #[test]
    fn a_recent_dirty_fact_shortens_the_full_requested_window() {
        let gate = Gate::PolicyCheck;
        let mut log = facts(gate);
        log.push(event(
            gate,
            Fact::WouldRefuse,
            20,
            json!({"service":"policy","key":{"caller":"old-caller"}}),
        ));
        let v = judge(gate, log, snapshot(gate));
        assert!(!v.covers_requested_window);
        assert_eq!(v.clean_since, Some(at(20) + Duration::microseconds(1)));
        assert_eq!(
            v.log.unwrap().dirty[0].payload["key"]["caller"],
            "old-caller"
        );
    }

    #[test]
    fn live_identity_mode_start_and_recording_health_are_all_required() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for path in ["instance", "recorder", "unstated", "retrying"] {
                let mut body = snapshot(gate);
                body["evidence"][path] = match path {
                    "instance" => json!("another-process"),
                    "recorder" => json!(false),
                    "unstated" => json!(1),
                    _ => json!(true),
                };
                refuses(&judge(gate, facts(gate), body));
            }
            for (key, value) in [
                ("mode", json!("off")),
                ("mode", json!("enforce")),
                ("recording_since", json!(at(73))),
                ("overflow", json!(1)),
            ] {
                let mut body = snapshot(gate);
                body[key] = value;
                refuses(&judge(gate, facts(gate), body));
            }
            for key in ["evidence", "not_clean", "rows", "mode", "recording_since"] {
                let mut body = snapshot(gate);
                body.as_object_mut().unwrap().remove(key);
                refuses(&judge(gate, facts(gate), body));
            }
        }
        let mut body = snapshot(Gate::MachineGate);
        body["service"] = json!("another-service");
        refuses(&judge(Gate::MachineGate, facts(Gate::MachineGate), body));
    }

    #[test]
    fn absent_duplicate_unexpected_and_dark_live_halves_never_disappear() {
        let gate = Gate::PolicyCheck;
        for reads in [
            vec![],
            vec![read(snapshot(gate)), read(snapshot(gate))],
            vec![LiveRead {
                service: "policy".into(),
                answer: Err("HTTP403: refused reader".into()),
            }],
            vec![LiveRead {
                service: "other".into(),
                answer: Ok(snapshot(gate)),
            }],
        ] {
            let v = join_window(
                gate,
                &["policy".into()],
                at(3),
                at(75),
                Ok(facts(gate)),
                reads,
            );
            refuses(&v);
            assert!(!v.live.is_empty());
        }
        for required in [
            vec![],
            vec!["policy".into(), "policy".into()],
            vec!["policy".into(), "missing".into()],
        ] {
            refuses(&join_window(
                gate,
                &required,
                at(3),
                at(75),
                Ok(facts(gate)),
                vec![read(snapshot(gate))],
            ));
        }
    }

    #[test]
    fn a_wrong_source_off_crash_or_long_gap_cannot_bridge_the_window() {
        let gate = Gate::PolicyCheck;
        let mut wrong = facts(gate);
        wrong[2].source = "another".into();
        let mut off = facts(gate);
        off[0].payload["mode"] = json!("off");
        let mut crash = facts(gate);
        crash.remove(1);
        let mut long = facts(gate);
        long[1].timestamp = at(73);
        for log in [wrong, off, crash, long] {
            let v = judge(gate, log, snapshot(gate));
            assert!(!v.covers_requested_window, "{v:?}");
        }
    }

    #[test]
    fn future_and_simulated_facts_are_not_real_coverage() {
        let gate = Gate::PolicyCheck;
        for (at_time, sim) in [(at(76), false), (at(74), true)] {
            let mut log = facts(gate);
            log[2].timestamp = at_time;
            log[2].payload["_simulated"] = json!(sim);
            refuses(&judge(gate, log, snapshot(gate)));
        }
    }

    #[test]
    fn accepted_previous_and_reader_presentations_do_not_break_the_window() {
        let gate = Gate::MachineGate;
        for presented in ["previous", "reader.current", "reader.previous"] {
            let mut body = snapshot(gate);
            body["rows"] = json!([{"peer":"fixture-peer","user":"fixture-reader","method":"GET",
                "route":"/fixture","presented":presented,"count":2,"first_seen":at(74),"last_seen":at(75)}]);
            assert!(judge(gate, facts(gate), body).covers_requested_window);
        }
    }

    #[test]
    fn contradictory_or_malformed_tallies_are_not_clean_by_omission() {
        let gate = Gate::PolicyCheck;
        for value in [Value::Null, json!({}), json!([]), json!(false)] {
            refuses(&judge(gate, facts(gate), value));
        }
        for (key, value) in [
            ("clean_since", Value::Null),
            ("mode_error", json!("mode file unreadable")),
            ("rows", json!([{"arm":"unknown"}])),
            ("not_clean", json!(null)),
            ("overflow", json!(-1)),
        ] {
            let mut body = snapshot(gate);
            body[key] = value;
            refuses(&judge(gate, facts(gate), body));
        }
    }

    #[test]
    fn a_newer_recording_start_cannot_hide_behind_missing_or_conflicting_service_provenance() {
        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for service in [
                None,
                Some(Value::Null),
                Some(json!("people")),
                Some(json!(false)),
                Some(json!(42)),
                Some(json!("")),
            ] {
                let mut log = facts(gate);
                let mut newer = event(
                    gate,
                    Fact::RecordingBegan,
                    75,
                    json!({"mode":"report","since":at(75),"instance":"newer"}),
                );
                if let Some(service) = service {
                    newer.payload["service"] = service;
                }
                let identity = newer.id.to_string();
                log.push(newer);
                let body = snapshot(gate);
                let report = judge(gate, log, body.clone());
                refuses(&report);
                assert!(
                    report.not_clean.iter().any(|why| why.contains(&identity)),
                    "{report:?}"
                );
                assert_eq!(report.live[0].snapshot.as_ref(), Some(&body));
                assert!(
                    report.log.is_some(),
                    "the readable durable half must remain visible"
                );
            }
        }
    }

    #[tokio::test]
    async fn an_obsolete_unbound_start_expires_only_after_a_complete_later_watch() {
        use crate::gate_evidence::{GateEvidenceLog, InMemoryGateEvidence};

        for gate in [Gate::MachineGate, Gate::PolicyCheck] {
            for wrong_service in [None, Some(json!("people"))] {
                for (bad_at, start_at, since, covers) in [
                    (-25, 1, 1, true),
                    (2, 1, 1, false),
                    (-25, 4, 1, false),
                    (-25, 1, -26, false),
                ] {
                    let mut obsolete = event(
                        gate,
                        Fact::RecordingBegan,
                        bad_at,
                        json!({"mode":"report","since":at(bad_at),"instance":"obsolete"}),
                    );
                    if let Some(service) = wrong_service.clone() {
                        obsolete.payload["service"] = service;
                    }
                    let identity = obsolete.id;
                    let current = event(
                        gate,
                        Fact::RecordingBegan,
                        start_at,
                        json!({"service":"policy","mode":"report","since":at(since),"instance":"current"}),
                    );
                    let log = InMemoryGateEvidence::new(vec![obsolete, current])
                        .facts(gate, at(3))
                        .await
                        .unwrap();
                    assert!(log.iter().any(|event| event.id == identity));
                    let mut body = snapshot(gate);
                    body["recording_since"] = json!(at(since));
                    if gate == Gate::PolicyCheck {
                        body["clean_since"] = json!(at(since));
                    }
                    let report = judge(gate, log, body);
                    assert_eq!(report.covers_requested_window, covers, "{report:?}");
                    if covers {
                        assert_eq!(report.clean_since, Some(at(3)));
                        assert!(report.not_clean.is_empty());
                    } else {
                        refuses(&report);
                        assert!(
                            report
                                .not_clean
                                .iter()
                                .any(|why| why.contains(&identity.to_string()))
                        );
                    }
                }
            }
        }
    }
}
