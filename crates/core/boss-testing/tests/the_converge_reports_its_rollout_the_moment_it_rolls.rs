//! The cluster converge reports `rolled` on its own packet the moment the
//! source instance's rollout succeeds — not when the whole run ends.
//!
//! WHY (backlog 090720c3). A train completed `converged` only when the
//! conductor's two-minute reconcile next read the cluster: measured over
//! 51 trains on 2026-10-18 (run 73832267), 72 s p50 and 138 s p90 of
//! every train's time on the track passed AFTER the rollout had finished.
//! The runner knew first and said nothing until its ExecStopPost, after
//! the other instances, the verification and the orphan sweep. So
//! `report_rolled` (infra/forge/cluster-deploy-lib.sh) writes `rolled`
//! and `rolled_at` onto the run step through boss-step.sh's metadata-only
//! door right after the roll, which the dispatcher rule
//! `complete-train-converged-on-runner-rolled` reacts to.
//!
//! What each case pins, against a stub boss-step that records what it was
//! handed: the write goes through the metadata-only door with the full
//! sha and a UTC instant; it does NOT hand boss-step the run summary file
//! (boss-step reads and DELETES that file, which belongs to the
//! ExecStopPost at the end of the run); the same two keys ride the
//! summary too, so a mid-run write the system of record refused is
//! recovered at the end of the run; a refused write never fails the
//! converge and says so; and the runner calls it right after the source
//! instance's roll, before any later stage.
//!
//! And it is BOUNDED (review ffb8efa0, B1): it runs inside ExecStart, and
//! boss-step.sh's curl has no --max-time and re-sends a refused connect
//! for 150 s, so a jobs API that accepted and never answered held the
//! whole converge until TimeoutStartSec=90min. A boss-step that never
//! returns must cost the converge the bound and a line, nothing more,
//! and the refused-connect window it is handed must end inside it.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const SUMMARY_LIB: &str = "infra/run-summary.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const SHA: &str = "9557a4d3fe62dfd3786b0d4f97cb3e3753f2bc0e";

struct Case {
    root: PathBuf,
    step: PathBuf,
    summary: PathBuf,
}

impl Case {
    fn new(name: &str, step_rc: i32) -> Self {
        Self::with_tail(name, &format!("exit {step_rc}"))
    }

    /// A stub boss-step that records what it was handed, then runs `tail`.
    fn with_tail(name: &str, tail: &str) -> Self {
        let root = scratch_dir(&format!("report-rolled-{name}"));
        let step = root.join("boss-step.sh");
        write_exec(
            &step,
            &format!(
                r#"#!/usr/bin/env bash
{{
  echo "args: $*"
  echo "metadata_only: ${{BOSS_STEP_METADATA_ONLY:-unset}}"
  echo "summary_file: ${{BOSS_RUN_SUMMARY_FILE:-unset}}"
  echo "retry_deadline: ${{BOSS_API_RETRY_DEADLINE:-unset}}"
}} >> "$STUB_LOG"
{tail}
"#
            ),
        );
        Self {
            summary: root.join("summary.json"),
            step,
            root,
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    fn summary(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&self.summary).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null)
    }

    fn run(&self) -> (i32, String) {
        self.run_with(&[])
    }

    /// Run `report_rolled` with `env` added, its output to files rather
    /// than pipes (a stub's orphaned sleep would hold a pipe open), and
    /// the test's own limit on it: a call that never returns is a RED
    /// that names itself, not a hung suite.
    fn run_with(&self, env: &[(&str, &str)]) -> (i32, String) {
        let script = format!(
            ". '{}'\n. '{}'\nreport_rolled \"$STEP\" '{SHA}'\n",
            repo_root().join(SUMMARY_LIB).display(),
            repo_root().join(LIB).display(),
        );
        let (out, err) = (self.root.join("stdout"), self.root.join("stderr"));
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STEP", &self.step)
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary)
            .stdin(Stdio::null())
            .stdout(File::create(&out).unwrap())
            .stderr(File::create(&err).unwrap());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let limit = Duration::from_secs(60);
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "report_rolled had not returned after {limit:?}: a boss-step that never \
                     answers holds the converge (stderr: {})",
                    std::fs::read_to_string(&err).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        (
            status.code().unwrap_or(-1),
            std::fs::read_to_string(&err).unwrap_or_default(),
        )
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the shape the conductor's own stamps take.
fn is_utc_instant(s: &str) -> bool {
    let shape = "dddd-dd-ddTdd:dd:ddZ";
    s.len() == shape.len()
        && s.bytes().zip(shape.bytes()).all(|(c, want)| match want {
            b'd' => c.is_ascii_digit(),
            _ => c == want,
        })
}

#[test]
fn the_rollout_is_reported_through_the_metadata_only_door_with_the_full_sha() {
    let c = Case::new("ok", 0);
    let (rc, err) = c.run();
    assert_eq!(rc, 0, "{err}");
    let log = c.log();
    let args = log
        .lines()
        .find_map(|l| l.strip_prefix("args: "))
        .unwrap_or_else(|| panic!("boss-step was never called: {log}"));
    let words: Vec<&str> = args.split(' ').collect();
    assert_eq!(
        &words[..3],
        &[
            "maintenance-cluster-converge",
            "run",
            &format!("rolled={SHA}")
        ],
        "{args}"
    );
    let at = words
        .get(3)
        .and_then(|w| w.strip_prefix("rolled_at="))
        .unwrap_or_else(|| panic!("no rolled_at: {args}"));
    assert!(is_utc_instant(at), "rolled_at is a UTC instant: {at}");
    assert!(
        log.contains("metadata_only: 1"),
        "the step stays open — the run has not ended: {log}"
    );
    assert!(
        log.contains("summary_file: unset"),
        "boss-step reads and deletes the summary it is handed; the run's summary is \
         the ExecStopPost's, so the mid-run write must not be given it: {log}"
    );
    let s = c.summary();
    assert_eq!(s["rolled"], SHA, "the summary carries it too: {s}");
    assert_eq!(s["rolled_at"], at, "one instant, both records: {s}");
}

#[test]
fn a_refused_report_never_fails_the_converge_and_says_so() {
    let c = Case::new("refused", 1);
    let (rc, err) = c.run();
    assert_eq!(rc, 0, "the roll is real; the report is visibility: {err}");
    assert!(
        err.contains("could not record rolled="),
        "a refusal is a line, not silence: {err}"
    );
    assert_eq!(
        c.summary()["rolled"],
        SHA,
        "the end-of-run summary still carries it"
    );
}

#[test]
fn a_report_the_record_never_answers_costs_the_converge_its_bound_and_a_line() {
    // The stub accepts the call and never returns: the jobs API that
    // took the connection and sent nothing (review ffb8efa0, B1).
    let c = Case::with_tail("hangs", "sleep 120");
    let start = Instant::now();
    let (rc, err) = c.run_with(&[("BOSS_REPORT_ROLLED_TIMEOUT", "2")]);
    let took = start.elapsed();
    assert_eq!(
        rc, 0,
        "the roll is real; an unanswered report is visibility: {err}"
    );
    assert!(
        took < Duration::from_secs(15),
        "bounded by BOSS_REPORT_ROLLED_TIMEOUT (2 s, plus the 5 s kill grace), took {took:?}"
    );
    assert!(
        err.contains("could not record rolled="),
        "an unanswered report is a line, not silence: {err}"
    );
    assert_eq!(
        c.summary()["rolled"],
        SHA,
        "the end-of-run summary still carries it"
    );
}

#[test]
fn the_refused_connect_window_ends_inside_the_bound() {
    // boss-api-curl.sh re-sends a refused connect for
    // BOSS_API_RETRY_DEADLINE seconds, 150 by default: left unset, a dark
    // jobs API would spend the whole bound on retries, and the bound
    // would be the only thing ending them.
    let c = Case::new("deadline", 0);
    let (rc, err) = c.run_with(&[("BOSS_REPORT_ROLLED_TIMEOUT", "30")]);
    assert_eq!(rc, 0, "{err}");
    let log = c.log();
    let deadline: u64 = log
        .lines()
        .find_map(|l| l.strip_prefix("retry_deadline: "))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("boss-step was not handed a numeric retry window: {log}"));
    assert!(
        deadline < 30,
        "the retry window ({deadline} s) must end inside the 30 s bound: {log}"
    );
}

#[test]
fn the_runner_reports_right_after_the_source_instance_rolls() {
    let runner = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let at = |needle: &str| {
        runner
            .find(needle)
            .unwrap_or_else(|| panic!("{RUNNER} has no `{needle}`"))
    };
    let call = at(r#"report_rolled "$REPO/infra/boss-step.sh" "$HEAD_FULL""#);
    assert!(
        at("_stage_done roll_s") < call,
        "reported once the roll stage is done"
    );
    assert!(
        call < at(r#"STAGE="declared secrets""#),
        "and before every stage that follows the source instance's roll"
    );
}
