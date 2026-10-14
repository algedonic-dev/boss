//! A LIVE TALLY THAT DID NOT COVER THE CUT IS NOT YET (backlog a3b383c5,
//! review aa1a9c75 of car 59e0d3dd).
//!
//! The machine gate's tally lives in memory: every jobs restart — every
//! train — empties it and moves its `recording_since`, a gate in `off`
//! records nothing, and a key past the tally's bound is counted only in
//! an overflow. A probe that clears a caller because "no tokenless row
//! since the cut" is then reading silence about a stretch the tally never
//! watched, and an hourly recheck usually does. The live leg every probe
//! door quotes (`boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE`) is the
//! repair they teach, so the repair is what runs here: cut out of the
//! evidence verbatim, fed a stub `boss-sor-read`, and judged by its exit
//! code — 75 (not yet) for every tally that did not record the whole
//! window, never 0. On origin/main before this car the leg answered 0 for
//! a tally that began after the cut.

use boss_testing::{scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::process::Command;

/// The cut every case judges against: 2026-10-10T12:00:00Z.
const CUT: i64 = 1_791_633_600;

fn needs_jq() {
    let ok = Command::new("jq")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok,
        "no jq on this box — the gate image and the forge host have it, and a test that \
         cannot run must fail, never pass by returning early"
    );
}

/// The live leg, verbatim from the evidence, with its caller placeholder
/// filled.
fn live_leg() -> String {
    let evidence = boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE;
    let at = evidence
        .find("live=$(")
        .expect("the evidence carries a live leg");
    evidence[at..].replace("<the caller>", "automation:x")
}

fn tally(recording_since: Value, mode: &str, overflow: u64, rows: Value) -> Value {
    json!({
        "gate": "machine-gate",
        "live": [{
            "service": "jobs",
            "error": null,
            "snapshot": {
                "service": "jobs",
                "mode": mode,
                "rows": rows,
                "overflow": overflow,
                "source_overflow": [],
                "recording_since": recording_since,
                "not_clean": [],
            },
        }],
    })
}

/// Run the leg against `window` and answer (exit code, stdout).
fn run(name: &str, window: &Value) -> (Option<i32>, String) {
    needs_jq();
    let dir = scratch_dir(name);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_file(&dir.join("window.json"), &window.to_string());
    write_exec(
        &bin.join("boss-sor-read"),
        &format!("#!/bin/sh\ncat '{}'\n", dir.join("window.json").display()),
    );
    let script = dir.join("probe.sh");
    write_file(
        &script,
        &format!("hours=24\ncut={CUT}\n{}\necho CLEARED\n", live_leg()),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(&script)
        .env("PATH", path)
        .output()
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const BEFORE: &str = "2026-10-10T11:00:00.123456789Z";
const AFTER: &str = "2026-10-10T12:30:00.5Z";

#[test]
fn a_tally_that_began_after_the_cut_is_not_yet_never_a_pass() {
    // The case the review measured: a train restarted jobs after the
    // cut, the caller has not called since, and the empty tally reads as
    // a clean window.
    let (code, out) = run(
        "live-tally-after-cut",
        &tally(json!(AFTER), "report", 0, json!([])),
    );
    assert_eq!(code, Some(75), "{out}");
    assert!(
        out.contains("not yet: the live tally did not record the whole window"),
        "{out}"
    );
    assert!(!out.contains("CLEARED"), "{out}");
}

#[test]
fn every_other_uncovering_tally_is_not_yet_too() {
    let cases = [
        ("off", tally(json!(BEFORE), "off", 0, json!([]))),
        ("overflow", tally(json!(BEFORE), "report", 3, json!([]))),
        ("no-start", tally(Value::Null, "report", 0, json!([]))),
        ("source-overflow", {
            let mut t = tally(json!(BEFORE), "report", 0, json!([]));
            t["live"][0]["snapshot"]["source_overflow"] =
                json!([{"source": "10.0.0.9", "presented": "none", "count": 4}]);
            t
        }),
    ];
    for (name, window) in cases {
        let (code, out) = run(&format!("live-tally-{name}"), &window);
        assert_eq!(code, Some(75), "{name}: {out}");
        assert!(!out.contains("CLEARED"), "{name}: {out}");
    }
}

#[test]
fn a_tally_that_covered_the_cut_judges_the_caller() {
    // Recording since before the cut, in report or enforce, nothing
    // spilled: an empty tally clears the caller …
    for mode in ["report", "enforce"] {
        let (code, out) = run(
            &format!("live-tally-covered-{mode}"),
            &tally(json!(BEFORE), mode, 0, json!([])),
        );
        assert_eq!(code, Some(0), "{mode}: {out}");
        assert!(out.contains("CLEARED"), "{mode}: {out}");
    }
    // … and a tokenless row after the cut fails it.
    let row = json!([{"user": "automation:x", "presented": "none", "last_seen": AFTER}]);
    let (code, out) = run(
        "live-tally-covered-dirty",
        &tally(json!(BEFORE), "report", 0, row),
    );
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("FAILED: 1 tokenless row(s)"), "{out}");
}
