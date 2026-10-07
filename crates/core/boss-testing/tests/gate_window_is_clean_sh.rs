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

/// Every required service is judged, not the first: a second service
/// that never recorded refuses the window (row C's shape, 27 services).
#[test]
fn every_required_service_is_judged() {
    let mut doc = clean();
    doc["gate"] = json!("machine-gate");
    doc["log"]["gate"] = json!("machine-gate");
    doc["required_services"] = json!(["policy", "assets"]);
    let (code, _, err) = judge(&["machine-gate"], &doc.to_string());
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("assets"), "{err}");
    assert!(!err.contains("policy:"), "policy is clean: {err}");
}
