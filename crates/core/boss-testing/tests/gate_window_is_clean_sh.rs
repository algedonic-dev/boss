//! `infra/ops/gate-window-is-clean.sh` — the machine reader of a refusing
//! flip's evidence (design 21946380, decision 4: "the flip refuses on any
//! half that is missing, unreadable or short").
//!
//! THE GAP IT CLOSES. `GET /api/events/gate-window` joins the log half
//! and the live half of a gate's clean window, and until row D's enforce
//! car (design b08725c2, backlog b8e75382) exactly one program read it:
//! the dispatcher's hourly alarm, over two hours. The 72-hour answer a
//! refusing flip is earned on was read by a person, off a JSON document
//! — which is the mostly-sure shape: `covers_requested_window: true`
//! glanced at, a non-zero `overflow` three screens down not.
//!
//! WHAT IT IS. A judge over one document on stdin, with no reader of its
//! own: the pod pipes `boss-api GET`, a recorded probe pipes
//! `boss-sor-read`, and the operator can save the answer to a file,
//! judge THAT file, and copy it onto the flip's packet as the receipt —
//! the bytes judged are the bytes recorded. A reader that failed hands
//! it nothing, and nothing is refused, never passed.
//!
//! These cases build the answer's shape by hand (the shape
//! `boss_core::gate_window::JoinedWindow` serializes) and break one half
//! at a time.

use boss_testing::repo_root;
use serde_json::{Value, json};
use std::process::{Command, Stdio};

const SCRIPT: &str = "infra/ops/gate-window-is-clean.sh";

/// The script's verdict on `stdin`: exit code, stdout, stderr.
fn judge(args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash runs the judge");
    // A misuse is refused before stdin is read, which closes the pipe.
    boss_testing::feed_stdin(&mut child, stdin.as_bytes());
    let out = child.wait_with_output().expect("the judge exits");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A clean 72-hour answer for the policy check: one required service,
/// recording in `report` across the window, nothing on the log, an empty
/// live tally.
fn clean() -> Value {
    json!({
        "gate": "policy-check",
        "from": "2026-10-03T17:39:13.689216985Z",
        "now": "2026-10-06T17:39:13.689216985Z",
        "required_services": ["policy"],
        "log": {
            "gate": "policy-check",
            "log_clean_since": "2026-10-03T17:39:13.689216985Z",
            "dirty": [],
            "coverage": [{
                "service": "policy",
                "recording_since": "2026-10-03T17:25:05.161140641Z",
                "gap": null,
                "started_after": null
            }],
            "not_clean": []
        },
        "log_error": null,
        "live": [{
            "service": "policy",
            "snapshot": {
                "mode": "report",
                "mode_error": null,
                "rows": [],
                "overflow": 0,
                "not_clean": [],
                "recording_since": "2026-10-06T16:44:44.220804918Z",
                "clean_since": "2026-10-06T16:44:44.220804918Z"
            },
            "error": null,
            "not_clean": []
        }],
        "not_clean": [],
        "clean_since": "2026-10-03T17:39:13.689216985Z",
        "covers_requested_window": true,
        "input_errors": []
    })
}

/// `clean()` with one JSON pointer set to `value`.
fn with(pointer: &str, value: Value) -> String {
    let mut doc = clean();
    *doc.pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer} is in the clean answer")) = value;
    doc.to_string()
}

/// `clean()` with one key removed from the object at `pointer`.
fn without(pointer: &str, key: &str) -> String {
    let mut doc = clean();
    doc.pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer} is an object in the clean answer"))
        .remove(key)
        .unwrap_or_else(|| panic!("{pointer}/{key} is in the clean answer"));
    doc.to_string()
}

#[test]
fn a_clean_window_passes_and_says_what_it_read() {
    let (code, out, err) = judge(&["policy-check", "72"], &clean().to_string());
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("GATE-WINDOW-CLEAN gate=policy-check hours=72"),
        "{out}"
    );
    assert!(out.contains("services=policy"), "{out}");
    assert!(
        out.contains("clean_since=2026-10-03T17:39:13.689216985Z"),
        "the receipt line carries the instant, copied from the answer: {out}"
    );
    // The default window is the design's 72 hours.
    assert_eq!(judge(&["policy-check"], &clean().to_string()).0, 0);
    // `enforce` records too: the window is read again after the flip.
    let enforcing = with("/live/0/snapshot/mode", json!("enforce"));
    assert_eq!(judge(&["policy-check"], &enforcing).0, 0);
}

/// Each half, broken alone, is refused with exit 1 and a line naming it.
#[test]
fn every_dirty_or_short_half_is_refused_and_named() {
    let dirty = json!([{"at": "2026-10-05T00:00:00Z", "kind": "policy.check.would_refuse"}]);
    for (what, doc, names) in [
        (
            "the join says the window is not covered",
            with("/covers_requested_window", json!(false)),
            "covers_requested_window",
        ),
        (
            "no clean instant",
            with("/clean_since", Value::Null),
            "clean_since",
        ),
        (
            "a would-refuse on the log",
            with("/log/dirty", dirty),
            "policy.check.would_refuse",
        ),
        (
            "the log could not be read",
            with("/log_error", json!("the audit log is dark")),
            "the audit log is dark",
        ),
        ("no log half at all", with("/log", Value::Null), "log"),
        (
            "the join names a gap",
            with("/not_clean", json!(["live: policy: unread"])),
            "live: policy: unread",
        ),
        (
            "the query was not understood",
            with("/input_errors", json!(["hours"])),
            "input_errors",
        ),
        (
            "nothing is required",
            with("/required_services", json!([])),
            "required_services",
        ),
        (
            "the live tally overflowed",
            with("/live/0/snapshot/overflow", json!(3)),
            "overflow",
        ),
        (
            "the live tally holds a row",
            with(
                "/live/0/snapshot/rows",
                json!([{"arm": "unsigned", "count": 1}]),
            ),
            "row",
        ),
        (
            "the service is not recording",
            with("/live/0/snapshot/mode", json!("off")),
            "off",
        ),
        (
            "the mode word could not be read",
            with("/live/0/snapshot/mode_error", json!("not a mode: enfroce")),
            "enfroce",
        ),
        (
            "the live read failed",
            with("/live/0/error", json!("GET answered 503")),
            "GET answered 503",
        ),
        (
            "the live half was never taken",
            with("/live/0/snapshot", Value::Null),
            "snapshot",
        ),
        (
            "the tally names a lost fact",
            with("/live/0/snapshot/not_clean", json!(["1 fact lost"])),
            "1 fact lost",
        ),
        (
            "the service never recorded on the log",
            with(
                "/log/coverage/0/gap",
                json!("no recording_began from policy"),
            ),
            "no recording_began from policy",
        ),
        (
            "a required service has no coverage row",
            with("/log/coverage", json!([])),
            "coverage",
        ),
        (
            "a required service has no live row",
            with("/live", json!([])),
            "live",
        ),
        (
            "the answer is about the other gate",
            with("/gate", json!("machine-gate")),
            "machine-gate",
        ),
        (
            "the window read is shorter than the one required",
            with("/from", json!("2026-10-06T15:39:13.689216985Z")),
            "72",
        ),
    ] {
        let (code, out, err) = judge(&["policy-check", "72"], &doc);
        assert_eq!(code, 1, "{what}: {out}{err}");
        assert!(!out.contains("GATE-WINDOW-CLEAN"), "{what}: {out}");
        assert!(err.contains("NOT CLEAN"), "{what}: {err}");
        assert!(err.contains(names), "{what} names `{names}`: {err}");
    }
}

/// A half that is MISSING is refused like a dirty one: an answer from a
/// build that does not carry the field cannot be read as zero.
#[test]
fn a_missing_half_is_refused_never_read_as_zero() {
    for (pointer, key) in [
        ("", "covers_requested_window"),
        ("", "not_clean"),
        ("", "live"),
        ("/log", "dirty"),
        ("/log", "coverage"),
        ("/live/0/snapshot", "overflow"),
        ("/live/0/snapshot", "rows"),
        ("/live/0/snapshot", "mode"),
        ("/live/0/snapshot", "not_clean"),
    ] {
        let (code, out, err) = judge(&["policy-check"], &without(pointer, key));
        assert_eq!(code, 1, "{pointer}/{key}: {out}{err}");
        assert!(err.contains(key), "{pointer}/{key} is named: {err}");
    }
}

/// No document is not a clean document: the reader upstream of the pipe
/// failed, and jq-1.6 would exit 0 over it (infra/lib/jq.sh).
#[test]
fn an_unreadable_answer_is_refused_as_unreadable() {
    for (what, stdin) in [
        ("nothing", ""),
        ("whitespace", "  \n"),
        ("not JSON", "HTTP:503 upstream dark"),
        ("not an object", "[]"),
    ] {
        let (code, out, err) = judge(&["policy-check"], stdin);
        assert_eq!(code, 2, "{what}: {out}{err}");
        assert!(!out.contains("GATE-WINDOW-CLEAN"), "{what}: {out}");
        assert!(err.contains("UNREADABLE"), "{what}: {err}");
    }
}

#[test]
fn it_refuses_a_gate_it_was_not_named_and_hours_that_are_not_a_number() {
    let doc = clean().to_string();
    for args in [
        &[][..],
        &["policy-check", "soon"][..],
        &["policy-check", "0"][..],
        &["policy-check", "72", "extra"][..],
    ] {
        let (code, out, err) = judge(args, &doc);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(err.contains("usage"), "{args:?}: {err}");
    }
}

/// `clean()` as the machine gate's answer: the same halves, and the one
/// launch roster generation its hours were judged under (design
/// 3cc6152a, backlog 14fe115c).
fn clean_machine() -> Value {
    let mut doc = clean();
    doc["gate"] = json!("machine-gate");
    doc["log"]["gate"] = json!("machine-gate");
    doc["roster_generations"] = json!([{
        "generation": "sha256:0f0e0d",
        "since": "2026-10-01T02:00:00Z",
        "stated_by": "5a0c2f0e-8f2a-4b4e-9d57-3c2f6f1f7a10",
        "service": "policy",
        "unstated": null
    }]);
    doc
}

/// The shell must ask the native gate's question, not whether it counted
/// any traffic. Accepted probe-reader and rotation observations are
/// counted too (01d841a3); pin the second representation to the first.
#[test]
fn machine_rows_follow_the_native_enforcement_verdict() {
    use boss_core::machine_gate::Presented;

    for presented in [
        Presented::None,
        Presented::Mismatch,
        Presented::Previous,
        Presented::ReaderCurrent,
        Presented::ReaderNext,
        Presented::ReaderPrevious,
        Presented::Asked,
    ] {
        let mut doc = clean_machine();
        doc["live"][0]["snapshot"]["rows"] = json!([{
            "peer": "192.0.2.15", "user": "automation:run-car-probe-reader",
            "method": "GET", "route": "/api/machine-gate/accepts",
            "presented": presented, "count": 3,
            "first_seen": "2026-10-06T16:50:00Z", "last_seen": "2026-10-06T17:00:00Z"
        }]);
        let (code, out, err) = judge(&["machine-gate"], &doc.to_string());
        assert_eq!(
            code,
            i32::from(presented.enforce_refuses()),
            "{presented:?}: {out}{err}"
        );
    }
    for presented in [Value::Null, json!("unknown-slot"), json!(true), json!(3)] {
        let mut doc = clean_machine();
        doc["live"][0]["snapshot"]["rows"] = json!([{"peer": "192.0.2.15", "user": "probe-reader",
            "method": "GET", "route": "/api/jobs", "presented": presented, "count": 3}]);
        assert_eq!(judge(&["machine-gate"], &doc.to_string()).0, 1);
    }
    for count in [Value::Null, json!(0), json!(-1), json!(1.5), json!("3")] {
        let mut doc = clean_machine();
        doc["live"][0]["snapshot"]["rows"] = json!([{"peer": "192.0.2.15", "user": "probe-reader",
                "method": "GET", "route": "/api/jobs", "presented": "reader.current", "count": count}]);
        assert_ne!(judge(&["machine-gate"], &doc.to_string()).0, 0);
    }
    for key in ["peer", "user", "method", "route"] {
        let mut doc = clean_machine();
        let mut row = json!({"peer": "192.0.2.15", "user": "probe-reader",
            "method": "GET", "route": "/api/jobs", "presented": "reader.current", "count": 3});
        row.as_object_mut().unwrap().remove(key);
        doc["live"][0]["snapshot"]["rows"] = json!([row]);
        assert_ne!(
            judge(&["machine-gate"], &doc.to_string()).0,
            0,
            "missing {key}"
        );
    }
    let mut mixed = clean_machine();
    mixed["live"][0]["snapshot"]["rows"] = json!([
        {"peer": "192.0.2.15", "user": "probe-reader", "method": "GET", "route": "/api/jobs", "presented": "reader.current", "count": 3},
        {"peer": "192.0.2.15", "user": "unsigned", "method": "GET", "route": "/api/jobs", "presented": "none", "count": 1}
    ]);
    assert_eq!(judge(&["machine-gate"], &mixed.to_string()).0, 1);
}

/// An array can match jq's index as a subarray (review ad438bc8).
/// Only the native presentation's string spelling can certify a row.
#[test]
fn malformed_machine_presentations_never_certify_a_clean_window() {
    use boss_core::machine_gate::Presented;

    for presented in [
        Value::Null,
        json!({}),
        json!(true),
        json!(3),
        json!([]),
        json!(["previous"]),
        json!(["reader.current"]),
        json!(["previous", "reader.current"]),
    ] {
        assert!(
            serde_json::from_value::<Presented>(presented.clone()).is_err(),
            "the native type refuses this malformed presentation: {presented}"
        );
        let mut doc = clean_machine();
        doc["live"][0]["snapshot"]["rows"] = json!([{
            "peer": "192.0.2.15", "user": "probe-reader",
            "method": "GET", "route": "/fixture", "presented": presented,
            "count": 3, "first_seen": "2026-10-06T16:50:00Z",
            "last_seen": "2026-10-06T17:00:00Z"
        }]);
        let (code, out, err) = judge(&["machine-gate"], &doc.to_string());
        assert_ne!(code, 0, "malformed {presented} certified clean: {out}{err}");
        assert!(!out.contains("GATE-WINDOW-CLEAN"), "{out}");
    }
}

/// Row C's window is judged under ONE launch roster generation, and the
/// judge reads that from the answer itself: the join already refuses a
/// spanning window in `not_clean`, and an answer that names no
/// generation at all — a build from before the rule — is refused like
/// any other missing half, never read as "one".
#[test]
fn the_machine_window_names_the_one_generation_it_was_judged_under() {
    let (code, out, err) = judge(&["machine-gate"], &clean_machine().to_string());
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("GATE-WINDOW-CLEAN gate=machine-gate hours=72")
            && out.contains("roster_generation=sha256:0f0e0d"),
        "the receipt names the generation, copied from the answer: {out}"
    );
    let second = json!({"generation": "sha256:aaaaaa", "since": "2026-10-05T00:00:00Z",
        "stated_by": "5a0c2f0e-8f2a-4b4e-9d57-3c2f6f1f7a11", "service": "jobs", "unstated": null});
    let unstated = json!({"generation": null, "since": "2026-10-01T02:00:00Z",
        "stated_by": "5a0c2f0e-8f2a-4b4e-9d57-3c2f6f1f7a10", "service": "policy",
        "unstated": "the process start carries no roster stamp"});
    let mut absent = clean_machine();
    absent.as_object_mut().unwrap().remove("roster_generations");
    let mut two = clean_machine();
    two["roster_generations"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let mut none = clean_machine();
    none["roster_generations"] = json!([]);
    let mut silent = clean_machine();
    silent["roster_generations"] = json!([unstated]);
    for (what, doc, names) in [
        (
            "a build that names no generation",
            absent,
            "roster_generations",
        ),
        ("two generations inside the window", two, "sha256:aaaaaa"),
        ("no generation on the log", none, "roster_generations"),
        ("a stretch that stated none", silent, "no roster stamp"),
    ] {
        let (code, out, err) = judge(&["machine-gate"], &doc.to_string());
        assert_eq!(code, 1, "{what}: {out}{err}");
        assert!(!out.contains("GATE-WINDOW-CLEAN"), "{what}: {out}");
        assert!(err.contains(names), "{what} names `{names}`: {err}");
    }
    // The policy check consults no launch record: it has no generation
    // to name, and its receipt line is what it was.
    let (code, out, err) = judge(&["policy-check"], &clean().to_string());
    assert_eq!(code, 0, "{out}{err}");
    assert!(!out.contains("roster_generation"), "{out}");
}

/// A clean answer for the actor-role window (row H): the live half is a
/// service's `actor-role-reports` answer, whose tally is its `report`
/// member, and whose rows also count every MATCHING request — so a clean
/// tally is not an empty one, it is one with no row `enforce` would
/// answer differently.
fn clean_actor_role() -> Value {
    let mut doc = clean_machine();
    doc["gate"] = json!("actor-role");
    doc["log"]["gate"] = json!("actor-role");
    doc["required_services"] = json!(["jobs"]);
    doc["log"]["coverage"][0]["service"] = json!("jobs");
    doc["roster_generations"][0]["service"] = json!("jobs");
    doc["live"] = json!([{
        "service": "jobs",
        "snapshot": {
            "service": "jobs",
            "mode": "report",
            "snapshot": {"state": "ready"},
            "report": {
                "mode": "report",
                "recording_since": "2026-10-06T16:44:44.220804918Z",
                "rows": [{"observation": {"actor": "emp-david"}, "count": 41,
                          "first_seen": "2026-10-06T16:45:00Z",
                          "last_seen": "2026-10-06T17:39:00Z", "would_refuse": null}],
                "overflow": 0,
                "not_clean": [],
                "evidence": {"recorder": true, "instance": "i", "lost": 0, "unstated": 0,
                             "retrying": false, "last_error": null}
            }
        },
        "error": null,
        "not_clean": []
    }]);
    doc
}

/// Row H's window has a machine reader too (backlog b13bd730): until
/// this car the judge refused `actor-role` as "not a gate", so the
/// 72-hour answer that flip is earned on had only a person to read it.
/// It is held to every line the other gates are, to the ONE launch
/// roster generation its excuses are bound to, and to its own tally's
/// shape.
#[test]
fn the_actor_role_window_is_judged_and_names_its_one_generation() {
    let (code, out, err) = judge(&["actor-role"], &clean_actor_role().to_string());
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("GATE-WINDOW-CLEAN gate=actor-role hours=72")
            && out.contains("services=jobs")
            && out.contains("modes=jobs:report")
            && out.contains("roster_generation=sha256:0f0e0d"),
        "{out}"
    );
    let set = |pointer: &str, value: Value| {
        let mut doc = clean_actor_role();
        *doc.pointer_mut(pointer)
            .unwrap_or_else(|| panic!("{pointer} is in the clean answer")) = value;
        doc
    };
    let mut absent = clean_actor_role();
    absent.as_object_mut().unwrap().remove("roster_generations");
    let mut unjudged = clean_actor_role();
    unjudged["live"][0]["snapshot"]["report"]["rows"][0]
        .as_object_mut()
        .unwrap()
        .remove("would_refuse");
    for (what, doc, names) in [
        (
            "an answer from a build that binds no excuse",
            absent,
            "roster_generations",
        ),
        (
            "no generation on the log",
            set("/roster_generations", json!([])),
            "roster_generations",
        ),
        (
            "a start that stated no stamp",
            set(
                "/roster_generations/0",
                json!({"generation": null, "since": "2026-10-01T02:00:00Z",
                       "unstated": "the process start carries no roster stamp"}),
            ),
            "no roster stamp",
        ),
        (
            "a shape enforce would answer differently",
            set(
                "/live/0/snapshot/report/rows/0/would_refuse",
                json!("would-deny"),
            ),
            "enforce",
        ),
        ("a row the producer did not judge", unjudged, "enforce"),
        (
            "the tally overflowed",
            set("/live/0/snapshot/report/overflow", json!(2)),
            "overflow",
        ),
        (
            "the report is off",
            set("/live/0/snapshot/report/mode", json!("off")),
            "off",
        ),
        (
            "the tally names a lost fact",
            set("/live/0/snapshot/report/not_clean", json!(["1 fact lost"])),
            "1 fact lost",
        ),
        (
            "the answer carries no report",
            set("/live/0/snapshot/report", Value::Null),
            "report",
        ),
    ] {
        let (code, out, err) = judge(&["actor-role"], &doc.to_string());
        assert_eq!(code, 1, "{what}: {out}{err}");
        assert!(!out.contains("GATE-WINDOW-CLEAN"), "{what}: {out}");
        assert!(err.contains(names), "{what} names `{names}`: {err}");
    }
    // The machine gate's answer is not this gate's, whatever it holds.
    let (code, _, err) = judge(&["actor-role"], &clean_machine().to_string());
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("machine-gate"), "{err}");
}

/// Every required service is judged, not the first: a second service
/// that never recorded refuses the window (row C's shape, 27 services).
#[test]
fn every_required_service_is_judged() {
    let mut doc = clean_machine();
    doc["required_services"] = json!(["policy", "assets"]);
    let (code, _, err) = judge(&["machine-gate"], &doc.to_string());
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("assets"), "{err}");
    assert!(!err.contains("policy:"), "policy is clean: {err}");
}
