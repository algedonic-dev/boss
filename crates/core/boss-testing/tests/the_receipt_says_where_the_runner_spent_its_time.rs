//! A gate's receipt says where its time went BEFORE the checks.
//!
//! THE DEFECT (backlog d777b20a, measured by the factory-speed research
//! session of 2026-10-15 over 3,535 gate-runs). Green car gates went from
//! a median 6.0 minutes (W38) to 34.6 (W41), and the record could not say
//! how much of that was the queue, the runner's setup or the checks: the
//! packet keeps `launched_at` (the Job exists) and the receipt keeps each
//! check's duration, but the runner's own start, its seed copy, its
//! seed-order walk and everything between them went to stdout only, and
//! the runtime evidence's intervals are relative, with its `runner-start`
//! sample taken after all of it.
//!
//! THE FIX. run.sh stamps an absolute instant at each phase boundary,
//! starting with its own first line, and the receipt carries them as
//! `timeline`: `runner_started_at`, each phase with its start and its
//! seconds, `setup_s` (runner start to the checks), and `ended_at`. With
//! the packet's `gate_reservation.queued_at` and `launched_at`, a gate's
//! wall time divides into queue, pod start, setup and checks off the
//! record alone.
//!
//! These pins lift the blocks of run.sh as they ship and run them.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::fs;
use std::path::Path;
use std::process::Command;

const RUN_SH: &str = "infra/gate-runner/run.sh";

fn run_sh() -> String {
    fs::read_to_string(repo_root().join(RUN_SH)).expect("read run.sh")
}

/// The text between two markers of run.sh, exactly as it ships.
fn block(begin: &str, end: &str) -> String {
    let src = run_sh();
    let a = src
        .find(begin)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{begin}` marker"));
    let b = src
        .find(end)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{end}` marker"));
    assert!(a < b, "`{begin}` must precede `{end}`");
    src[a..b].to_string()
}

/// The lines run.sh EXECUTES, in order — comments are prose.
fn executed_lines() -> Vec<String> {
    run_sh()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn only_line(lines: &[String], what: &str, pred: impl Fn(&str) -> bool) -> usize {
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| pred(l))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "run.sh must have exactly one {what}; found {}: {:?}",
        hits.len(),
        hits.iter().map(|&i| &lines[i]).collect::<Vec<_>>()
    );
    hits[0]
}

fn mark(lines: &[String], phase: &str) -> usize {
    only_line(lines, &format!("`setup_mark {phase}`"), |l| {
        l.trim() == format!("setup_mark {phase}")
    })
}

/// THE ORDER PIN. Each mark stands where its phase begins: the first one
/// before anything else the runner does, the seed copy's before the copy,
/// the tree's before the `git init`, the walk's after the checkout and
/// before the walk, the checks' before gate.sh and their end after it.
#[test]
fn each_mark_stands_where_its_phase_begins() {
    let lines = executed_lines();
    let set_e = only_line(&lines, "`set -euo pipefail`", |l| {
        l.trim() == "set -euo pipefail"
    });
    let prepare = mark(&lines, "prepare");
    let forge_url = only_line(&lines, "FORGE_URL default", |l| l.starts_with("FORGE_URL="));
    let seed_copy = mark(&lines, "seed-copy");
    let seed_call = only_line(&lines, "seed_target call", |l| {
        l.contains("seed_target") && !l.contains("seed_target()")
    });
    let tree = mark(&lines, "tree");
    let init = only_line(&lines, "git init of the workspace repository", |l| {
        l.trim_start().starts_with("git init") && l.contains("/gate-target/repo")
    });
    let checkout = only_line(&lines, "branch checkout", |l| {
        l.trim_start().starts_with("git checkout -B")
    });
    let seed_order = mark(&lines, "seed-order");
    let walk = only_line(&lines, "seed_order_json call", |l| {
        l.contains("seed_order_json") && !l.contains("seed_order_json()")
    });
    let environment = mark(&lines, "environment");
    let caches = only_line(&lines, "CARGO_HOME on the seed", |l| {
        l.trim() == r#"export CARGO_HOME="$SEED/cargo""#
    });
    let checks = mark(&lines, "checks");
    let gate = only_line(&lines, "gate.sh call", |l| {
        l.contains("./infra/gate.sh ${GATE_MODE:-}")
    });
    let checks_end = mark(&lines, "checks-end");
    let summary = only_line(&lines, "receipt summary", |l| {
        l.trim_start().starts_with("SUMMARY=$(python3")
    });

    let order = [
        ("set -euo pipefail", set_e),
        ("setup_mark prepare", prepare),
        ("FORGE_URL", forge_url),
        ("setup_mark seed-copy", seed_copy),
        ("seed_target call", seed_call),
        ("setup_mark tree", tree),
        ("git init", init),
        ("git checkout -B", checkout),
        ("setup_mark seed-order", seed_order),
        ("seed_order_json call", walk),
        ("setup_mark environment", environment),
        ("CARGO_HOME", caches),
        ("setup_mark checks", checks),
        ("gate.sh", gate),
        ("setup_mark checks-end", checks_end),
        ("receipt summary", summary),
    ];
    for pair in order.windows(2) {
        assert!(
            pair[0].1 < pair[1].1,
            "`{}` (executed line {}) must precede `{}` (executed line {}) — a mark out of \
             place charges one phase's seconds to another",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1
        );
    }
}

/// Run the timeline block's `setup_mark`, as it ships, under the
/// runner's own `set -e` and an ERR trap like its `fail_lost`.
fn marks_with(dir: &Path, path_prefix: Option<&Path>) -> (bool, String) {
    let script = format!(
        "set -euo pipefail\ntrap 'echo TRAPPED; exit 99' ERR\n{block}\nsetup_mark one\nsetup_mark two\nprintf '%s' \"$SETUP_MARKS\"\n",
        block = block(
            "# --- runner timeline (begin) ---",
            "# --- runner timeline (end) ---"
        ),
    );
    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script]).current_dir(dir);
    if let Some(prefix) = path_prefix {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", prefix.display()));
    }
    let out = cmd.output().expect("bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// A mark is the phase's name and a UTC instant to the millisecond, and
/// the block's own first call stamped `prepare` before anything else.
#[test]
fn a_mark_is_a_named_utc_instant() {
    let dir = scratch_dir("runner-timeline-marks");
    let (ok, marks) = marks_with(&dir, None);
    let _ = fs::remove_dir_all(&dir);
    assert!(ok, "the marks must not fail the runner: {marks}");
    let names: Vec<&str> = marks
        .split_whitespace()
        .map(|m| m.split('=').next().unwrap_or(""))
        .collect();
    assert_eq!(names, ["prepare", "one", "two"], "{marks}");
    // YYYY-MM-DDTHH:MM:SS.mmmZ, digit for digit.
    let shape = "dddd-dd-ddTdd:dd:dd.dddZ";
    for m in marks.split_whitespace() {
        let at = m.split_once('=').map(|(_, at)| at).unwrap_or("");
        let fits = at.len() == shape.len()
            && at
                .chars()
                .zip(shape.chars())
                .all(|(c, s)| if s == 'd' { c.is_ascii_digit() } else { c == s });
        assert!(fits, "`{m}` is not a millisecond UTC instant");
    }
}

/// A clock that cannot be read costs that mark, never the gate: the
/// mark is kept with no instant, and the ERR trap does not fire.
#[test]
fn a_clock_that_fails_fails_nothing() {
    let dir = scratch_dir("runner-timeline-broken-clock");
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("date"), "#!/bin/sh\nexit 1\n");
    let (ok, marks) = marks_with(&dir, Some(&bin));
    let _ = fs::remove_dir_all(&dir);
    assert!(ok, "a failed clock read must not end the run: {marks}");
    assert!(!marks.contains("TRAPPED"), "{marks}");
    assert_eq!(marks.trim(), "prepare= one= two=", "{marks}");
}

/// Run the receipt-summary block with the runner's marks set.
fn summarize(dir: &Path, receipt_body: &str, vars: &str) -> serde_json::Value {
    let receipt = dir.join("receipt.json");
    write_file(&receipt, receipt_body);
    let script = format!(
        "set -euo pipefail\nRECEIPT={r}\nHEAD_SHA=deadbeef\n{vars}\n{block}\nprintf '%s' \"$SUMMARY\"\n",
        r = receipt.display(),
        block = block(
            "# --- receipt summary (begin) ---",
            "# --- receipt summary (end) ---"
        ),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "summary block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

const MARKS: &str = "SETUP_MARKS='prepare=2026-10-18T10:00:00.000Z seed-copy=2026-10-18T10:00:04.500Z \
tree=2026-10-18T10:01:04.500Z seed-order=2026-10-18T10:01:06.750Z \
environment=2026-10-18T10:01:30.000Z checks=2026-10-18T10:01:31.000Z \
checks-end=2026-10-18T10:31:31.000Z '\nSEED_COPY='seeded 0123abcd'";

/// The receipt carries the runner's start and every phase's start and
/// seconds — on a readable receipt and on one gate.sh never wrote.
#[test]
fn the_receipt_carries_the_runner_timeline() {
    let dir = scratch_dir("runner-timeline-on-the-receipt");
    let readable = summarize(&dir, r#"{"verdict":"green","head":"c0ffee"}"#, MARKS);
    let unreadable = summarize(&dir, "not json", MARKS);
    let _ = fs::remove_dir_all(&dir);

    for got in [&readable, &unreadable] {
        let t = &got["timeline"];
        assert_eq!(t["runner_started_at"], "2026-10-18T10:00:00.000Z", "{got}");
        assert_eq!(t["ended_at"], "2026-10-18T10:31:31.000Z", "{got}");
        assert_eq!(t["setup_s"], 91.0, "{got}");
        let phases: Vec<(String, f64)> = t["phases"]
            .as_array()
            .unwrap_or_else(|| panic!("timeline.phases is a list: {got}"))
            .iter()
            .map(|p| {
                (
                    p["phase"].as_str().unwrap_or("").to_string(),
                    p["s"].as_f64().unwrap_or(-1.0),
                )
            })
            .collect();
        let want: Vec<(String, f64)> = [
            ("prepare", 4.5),
            ("seed-copy", 60.0),
            ("tree", 2.25),
            ("seed-order", 23.25),
            ("environment", 1.0),
            ("checks", 1800.0),
        ]
        .iter()
        .map(|(p, s)| (p.to_string(), *s))
        .collect();
        assert_eq!(phases, want, "{got}");
        assert_eq!(t["phases"][1]["at"], "2026-10-18T10:00:04.500Z", "{got}");
        assert_eq!(t["phases"][1]["outcome"], "seeded", "{got}");
        assert_eq!(t["phases"][1]["seed_head"], "0123abcd", "{got}");
    }
    assert_eq!(readable["verdict"], "green", "{readable}");
}

/// ABSENT IS NEVER A ZERO. No marks at all is no `timeline`; a mark with
/// no instant leaves the two phases it bounds without seconds, and the
/// setup total with none, rather than a number that is not one.
#[test]
fn an_unread_clock_is_null_never_zero() {
    let dir = scratch_dir("runner-timeline-absent");
    let none = summarize(&dir, r#"{"verdict":"green"}"#, "");
    let holed = summarize(
        &dir,
        r#"{"verdict":"green"}"#,
        "SETUP_MARKS='prepare= seed-copy=2026-10-18T10:00:04.500Z tree=2026-10-18T10:00:05.000Z checks=2026-10-18T10:00:06.000Z '",
    );
    let _ = fs::remove_dir_all(&dir);

    assert!(none.get("timeline").is_none(), "{none}");
    let t = &holed["timeline"];
    assert!(t["runner_started_at"].is_null(), "{holed}");
    assert!(t["setup_s"].is_null(), "{holed}");
    assert!(t["phases"][0]["s"].is_null(), "{holed}");
    assert_eq!(t["phases"][1]["s"], 0.5, "{holed}");
    // `checks` has no end mark: a run summarised without one is not timed.
    assert!(t["phases"][3]["s"].is_null(), "{holed}");
    assert!(t["ended_at"].is_null(), "{holed}");
}

/// `seed_target`, as it ships, says what the copy did, for the receipt.
#[test]
fn the_seed_copy_says_what_it_did() {
    let dir = scratch_dir("runner-timeline-seed-copy");
    let seed = dir.join("seed");
    let dest = dir.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let run = |seed_present: bool| -> String {
        let _ = fs::remove_dir_all(&seed);
        fs::create_dir_all(&seed).unwrap();
        if seed_present {
            fs::create_dir_all(seed.join("target/debug")).unwrap();
            write_file(&seed.join("target/debug/a.rlib"), "x");
            write_file(&seed.join(".seed-head"), "feedface\n");
        }
        let script = format!(
            "set -euo pipefail\nSEED={s}\nSEED_LOCK={s}/.seed.lock\neval \"$(sed -n '/^seed_target()/,/^}}/p' {run_sh})\"\nseed_target {d}\nprintf 'COPY=%s' \"$SEED_COPY\"\n",
            s = seed.display(),
            d = dest.display(),
            run_sh = repo_root().join(RUN_SH).display(),
        );
        let out = Command::new("bash")
            .args(["-c", &script])
            .output()
            .expect("bash");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "seed_target failed: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    };
    let cold = run(false);
    let warm = run(true);
    let _ = fs::remove_dir_all(&dir);
    assert!(cold.ends_with("COPY=no-seed"), "{cold}");
    assert!(warm.ends_with("COPY=seeded feedface"), "{warm}");
}
