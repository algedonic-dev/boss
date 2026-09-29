//! `infra/forge/converge-now.sh` (the `converge` ops verb) reads back the
//! cluster-deploy-runner INVOCATION its start began, and declares that
//! read-back as its `effect` (backlog 1058e686, car D).
//!
//! WHY. `systemctl start --no-block` returns 0 the moment systemd
//! ACCEPTS the job, so the verb's only proof was "the job was queued" —
//! the ops-request read `answered` while the run it asked for never
//! began (the shape of d66f92b2). systemd names every run of a unit with
//! a fresh 128-bit InvocationID, so "a converge started" is a question
//! the unit answers: read `InvocationID` and `ActiveState` before the
//! start and after it, and only a NEW invocation, activating or active
//! (or already finished, with its Result said), is the effect. The same
//! invocation as before is a run already in progress that merged the
//! start — the request is noted for the NEXT run, which is a not-yet
//! (exit 75), never a claimed effect. A read that cannot answer is
//! CANNOT ANSWER, exit 1 — after the start, which still runs, because the
//! converge only accelerates the timer and a missing proof must not
//! become a missing converge.
//!
//! Nothing here touches a host: `systemctl` is a stub keeping the unit's
//! state in a file, and the start is what changes it.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/converge-now.sh";
const VERB: &str = "converge";
const OLD: &str = "0123456789abcdef0123456789abcdef";
const NEW: &str = "fedcba9876543210fedcba9876543210";

struct Case {
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    state: PathBuf,
}

impl Case {
    /// A unit whose last invocation was `OLD`, now in `before_state`.
    fn new(name: &str, before_state: &str) -> Self {
        let root = scratch_dir(&format!("converge-now-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        let log = root.join("calls");
        let state = root.join("unit-state");
        write_file(
            &state,
            &format!("InvocationID={OLD}\nActiveState={before_state}\nResult=success\n"),
        );
        // `show` prints the state file (or fails, when the bus is mute);
        // `start` begins a NEW invocation unless the case says the start
        // merged into a run already in progress.
        write_exec(
            &bin.join("systemctl"),
            r#"#!/usr/bin/env bash
echo "systemctl $*" >> "$STUB_LOG"
case "$1" in
  show)
    if [ -n "${STUB_SHOW_MUTE:-}" ]; then
      echo "Failed to connect to bus: Connection refused" >&2
      exit 1
    fi
    # A PID 1 or dbus slowed by a build's load answers late.
    [ -n "${STUB_SHOW_SLEEP:-}" ] && sleep "$STUB_SHOW_SLEEP"
    cat "$STUB_STATE" ;;
  start)
    [ "${STUB_START:-new}" = merged ] && exit 0
    printf 'InvocationID=%s\nActiveState=%s\nResult=%s\n' \
      "$STUB_NEW_ID" "${STUB_NEW_STATE:-activating}" "${STUB_NEW_RESULT:-success}" > "$STUB_STATE" ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        Self {
            root,
            bin,
            log,
            state,
        }
    }

    fn run(&self, extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_LOG", &self.log)
            .env("STUB_STATE", &self.state)
            .env("STUB_NEW_ID", NEW)
            .env("BOSS_FORGE_REPO_DIR", self.root.join("repo"))
            .env("OPS_REQUEST_ID", "19df6925-0000-4000-8000-000000000000")
            // A run already in progress never produces a new invocation;
            // the case need not wait out the real bound to see that.
            .env("BOSS_CONVERGE_READBACK_SECONDS", "1");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("converge-now.sh runs")
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn effect(t: &str) -> Option<Vec<String>> {
    boss_testing::ops_runner_stub::effect_lines(VERB, t)
}

/// The ordinary case: the start begins a new invocation, the read-back
/// sees it activating, and that line — naming the invocation — is the
/// effect, and the only one.
#[test]
fn a_new_invocation_is_read_back_and_is_the_effect() {
    let c = Case::new("new", "inactive");
    let out = c.run(&[]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "{t}");
    let calls = c.calls();
    let show = calls.find("systemctl show").expect("a before-read");
    let start = calls
        .find("systemctl start --no-block cluster-deploy-runner.service")
        .expect("the start");
    assert!(show < start, "the before-read comes first:\n{calls}");
    assert!(
        calls[start..].contains("systemctl show"),
        "an after-read follows the start:\n{calls}"
    );
    assert!(
        t.contains(&format!("read back: invocation {NEW} (activating)"))
            && t.contains(&format!("was {OLD}")),
        "the effect names the new invocation and the one before:\n{t}"
    );
    if let Some(hits) = effect(&t) {
        assert_eq!(hits.len(), 1, "exactly the read-back line: {hits:?}\n{t}");
        assert!(hits[0].contains(NEW), "{hits:?}");
    }
}

/// A converge that is quick (main unchanged, a hold) may have FINISHED
/// before the read: a new invocation that ended successfully is still the
/// run this request began, said as such.
#[test]
fn a_new_invocation_that_already_finished_is_still_the_effect() {
    let c = Case::new("finished", "inactive");
    let out = c.run(&[("STUB_NEW_STATE", "inactive")]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "{t}");
    assert!(
        t.contains(&format!(
            "read back: invocation {NEW} (inactive, already finished: Result=success)"
        )),
        "{t}"
    );
    if let Some(hits) = effect(&t) {
        assert_eq!(hits.len(), 1, "{hits:?}\n{t}");
    }
}

/// A new invocation that has already FAILED is the converge this request
/// began, and it failed: exit 1, no effect.
#[test]
fn a_new_invocation_that_already_failed_fails() {
    let c = Case::new("failed", "inactive");
    let out = c.run(&[
        ("STUB_NEW_STATE", "failed"),
        ("STUB_NEW_RESULT", "exit-code"),
    ]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("FAILED") && t.contains("exit-code"), "{t}");
    if let Some(hits) = effect(&t) {
        assert!(hits.is_empty(), "a failed run claimed an effect: {hits:?}");
    }
}

/// A run already in progress merges the start: the invocation is the one
/// from before. That is not this request's converge — it is noted for the
/// next run — so the verb says not yet (75) and claims no effect.
#[test]
fn a_start_merged_into_a_running_converge_is_not_yet() {
    let c = Case::new("merged", "activating");
    let out = c.run(&[("STUB_START", "merged")]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(75), "{t}");
    assert!(
        t.contains("not yet") && t.contains(OLD) && t.contains("already running"),
        "{t}"
    );
    if let Some(hits) = effect(&t) {
        assert!(
            hits.is_empty(),
            "a merged start claimed an effect: {hits:?}"
        );
    }
}

/// The unit's state cannot be read: the start still runs (the converge
/// only accelerates the timer), and the run says it cannot answer.
#[test]
fn an_unreadable_unit_is_cannot_answer_after_the_start() {
    let c = Case::new("mute", "inactive");
    let out = c.run(&[("STUB_SHOW_MUTE", "1")]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(
        c.calls()
            .contains("systemctl start --no-block cluster-deploy-runner.service"),
        "the start still ran:\n{}",
        c.calls()
    );
    assert!(
        t.contains("CANNOT ANSWER") && t.contains("Connection refused"),
        "{t}"
    );
    if let Some(hits) = effect(&t) {
        assert!(hits.is_empty(), "{hits:?}");
    }
}

/// The read-back is bounded by WALL-CLOCK time, not by a count of polls:
/// with every `systemctl show` taking 2 s, a 3 s bound ends after two
/// reads of the loop (about 7 s with the before-read), where a count of
/// four polls would take about 14 s — and, at the real bound, outlast the
/// verb's declared timeout (review of car D, run 7fe34bd7).
#[test]
fn a_slow_unit_read_is_bounded_by_the_clock_not_the_poll_count() {
    let c = Case::new("slow", "activating");
    let t0 = std::time::Instant::now();
    let out = c.run(&[
        ("STUB_START", "merged"),
        ("STUB_SHOW_SLEEP", "2"),
        ("BOSS_CONVERGE_READBACK_SECONDS", "3"),
    ]);
    let elapsed = t0.elapsed().as_secs();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(75), "{t}");
    assert!(
        elapsed < 11,
        "the read-back took {elapsed} s against a 3 s bound — it counts polls, not seconds:\n{t}"
    );
}

/// The verb declares an `effect`, and a `timeout` that outlasts its own
/// read-back bound: the runner's 30 s default would otherwise be the
/// bound the verb is killed at, mid-read (note_2026_09_29_timeouts).
#[test]
fn the_verb_declares_its_effect_and_a_timeout_past_its_wait() {
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/converge.json")).unwrap(),
    )
    .unwrap();
    assert!(spec["effect"].as_str().is_some(), "{spec}");
    assert!(spec.get("effect_unread").is_none(), "{spec}");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let bound: u64 = script
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("READBACK_SECONDS=\"${BOSS_CONVERGE_READBACK_SECONDS:-")
                .and_then(|r| r.strip_suffix("}\""))
        })
        .and_then(|n| n.parse().ok())
        .expect("converge-now.sh sets READBACK_SECONDS=\"${BOSS_CONVERGE_READBACK_SECONDS:-<n>}\"");
    let timeout = spec["timeout"].as_u64().unwrap_or(0);
    assert!(
        timeout >= bound + 30,
        "converge declares timeout {timeout}, which does not outlast its {bound} s read-back plus its reads"
    );
}
