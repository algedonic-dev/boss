//! A converge that an ops-request started says so on its packet, and
//! never reads as a timer run (backlog e4d51e12).
//!
//! MEASURED 2026-10-18 on run 73832267, joining 191 closed pr-trains to
//! the maintenance-cluster-converge runs that built them, and again
//! 2026-10-20 on origin/main 2fbe993a5: train #1288's converge-on-merge
//! ops-request a7b45bdf opened at 12:04:44Z, and the converge packet it
//! started (57fa9ab8) opened 3 s later reading `opened_by
//! automation:maintenance-timer` with its `scheduled` trigger
//! `trigger_kind periodic, trigger_name systemd-timer` — the same as the
//! no-op ticks the 10-minute safety-net timer files. Provenance naming
//! the wrong trigger is what sent 8c8e01f4's triage after the timer.
//!
//! The cause is that the packet is opened by `boss-maintenance-wrap.sh`
//! from the unit's ExecStartPre, the same way whoever started the unit,
//! and the protocol had one trigger. But the merge path already leaves
//! its provenance where the wrap can read it BEFORE the run: the
//! `converge` verb (converge-now.sh) writes the requesting ops-request's
//! id into the converge's request inbox and only then starts the unit.
//!
//! WHAT THIS PINS, driven through the REAL wrap against a stub jobs API:
//!   * a unit that names its inbox (`BOSS_WRAP_REQUESTS`) and finds
//!     request ids in it files a packet whose `trigger_name` is
//!     `ops-request` — which the protocol's `requested` trigger fires on
//!     — and which carries the ids as `ops_requests`;
//!   * no inbox, an empty one, or one holding no well-formed id is a
//!     timer run, exactly as before;
//!   * a run systemd says a TIMER started (`TRIGGER_UNIT=*.timer`) stays
//!     a timer run even when it finds a request left for it, and still
//!     names the request it answers;
//!   * the unit's inbox is the file the verb writes (one path, held
//!     equal — CLAUDE.md §9a), and the protocol declares the trigger the
//!     wrap names.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const WRAP: &str = "infra/boss-maintenance-wrap.sh";
const KIND: &str = "maintenance-cluster-converge";
const UNIT: &str = "infra/forge/cluster-deploy-runner.service";
const REQ_A: &str = "a7b45bdf-57be-4453-a434-587dc8b2d0b6";
const REQ_B: &str = "0040fe4c-decf-4a05-8e33-8748a7775ba1";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A stub jobs API: GET answers no open packet, POST appends the body
/// it was sent to the store as one line and echoes it.
const STUB_API: &str = r#"#!/usr/bin/env bash
set -euo pipefail
method=GET; data=""
while [ $# -gt 0 ]; do
    case "$1" in
        -X) method="$2"; shift 2 ;;
        -H) shift 2 ;;
        --data-binary) data=$(cat "${2#@}"); shift 2 ;;
        *) shift ;;
    esac
done
case "$method" in
    GET) echo '{"data":[],"total":0}' ;;
    POST) printf '%s' "$data" | jq -c . >> "$STUB_STORE"; printf '%s\n' "$data" ;;
esac
"#;

struct Fixture {
    bin: PathBuf,
    store: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let bin = scratch_dir(&format!("requested-converge-{tag}"));
        write_exec(&bin.join("boss-maintenance-wrap.sh"), &read(WRAP));
        write_exec(&bin.join("boss-api-curl.sh"), STUB_API);
        create_dir(&bin.join("lib"));
        write_file(
            &bin.join("lib/secret-header.sh"),
            &read("infra/lib/secret-header.sh"),
        );
        let home = bin.join("home");
        create_dir(&home);
        let store = bin.join("store.jsonl");
        Fixture { bin, store, home }
    }

    fn inbox(&self, body: &str) -> PathBuf {
        let p = self.bin.join("requests");
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run the wrap as the unit's ExecStartPre does; the packet it POSTed.
    fn run(&self, env: &[(&str, &Path)], trigger_unit: Option<&str>) -> Value {
        let mut cmd = Command::new("bash");
        cmd.arg(self.bin.join("boss-maintenance-wrap.sh"))
            .arg(KIND)
            .arg("Cluster converge on forge main")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("HOST_ID", "forge")
            .env("BOSS_JOBS_URL", "http://stub:7900")
            .env("STUB_STORE", &self.store);
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(t) = trigger_unit {
            cmd.env("TRIGGER_UNIT", t);
        }
        let out = cmd.output().expect("bash runs");
        assert!(
            out.status.success(),
            "the wrap exits 0: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let posted = std::fs::read_to_string(&self.store).unwrap_or_else(|_| {
            panic!(
                "the wrap filed no packet: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let lines: Vec<&str> = posted.lines().collect();
        assert_eq!(lines.len(), 1, "one packet filed: {posted}");
        serde_json::from_str(lines[0]).expect("the packet is JSON")
    }
}

fn md(packet: &Value) -> &Value {
    &packet["metadata"]
}

#[test]
fn a_run_its_inbox_names_a_request_for_is_a_requested_run() {
    let f = Fixture::new("one");
    let inbox = f.inbox(&format!("{REQ_A}\n"));
    let p = f.run(&[("BOSS_WRAP_REQUESTS", &inbox)], None);
    assert_eq!(
        md(&p)["trigger_name"],
        "ops-request",
        "a merge-started run never reads as a timer run: {p}"
    );
    assert_eq!(md(&p)["ops_requests"], serde_json::json!([REQ_A]));
    assert_eq!(md(&p)["host"], "forge", "the host stamp is unchanged");
    // The wrap only READS the inbox: the runner takes it and answers
    // each request when the run ends (cluster-deploy-lib.sh).
    assert_eq!(
        std::fs::read_to_string(&inbox).unwrap(),
        format!("{REQ_A}\n"),
        "the inbox is left for the runner"
    );
}

#[test]
fn every_request_is_named_once_and_a_malformed_line_is_not_one() {
    let f = Fixture::new("many");
    let inbox = f.inbox(&format!(
        "{REQ_B}\nnot-a-job-id; rm -rf /\n{REQ_A}\n\n{REQ_B}\n"
    ));
    let p = f.run(&[("BOSS_WRAP_REQUESTS", &inbox)], None);
    assert_eq!(md(&p)["trigger_name"], "ops-request");
    let mut named: Vec<String> = md(&p)["ops_requests"]
        .as_array()
        .expect("ops_requests is a list")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    named.sort();
    let mut want = vec![REQ_A.to_string(), REQ_B.to_string()];
    want.sort();
    assert_eq!(named, want);
}

#[test]
fn no_inbox_an_empty_one_or_a_malformed_one_is_a_timer_run() {
    let missing = Fixture::new("missing");
    let absent = missing.bin.join("requests");
    let empty = Fixture::new("empty");
    let empty_inbox = empty.inbox("");
    let junk = Fixture::new("junk");
    let junk_inbox = junk.inbox("deadbeef\n../../etc/passwd\n");
    let unnamed = Fixture::new("unnamed");
    for (case, p) in [
        (
            "an absent inbox",
            missing.run(&[("BOSS_WRAP_REQUESTS", &absent)], None),
        ),
        (
            "an empty inbox",
            empty.run(&[("BOSS_WRAP_REQUESTS", &empty_inbox)], None),
        ),
        (
            "an inbox of no job id",
            junk.run(&[("BOSS_WRAP_REQUESTS", &junk_inbox)], None),
        ),
        ("a unit that names no inbox", unnamed.run(&[], None)),
    ] {
        assert!(
            md(&p).get("trigger_name").is_none(),
            "{case}: no trigger named, so the protocol's first (timer) trigger fires: {p}"
        );
        assert!(
            md(&p).get("ops_requests").is_none(),
            "{case}: no request named: {p}"
        );
    }
}

/// systemd tells a timer-activated service which unit triggered it
/// (`TRIGGER_UNIT`, systemd.exec(5)). A request that arrived while a run
/// was already active waits in the inbox for the NEXT run, which the
/// timer may start: that run answers the request, but the timer started
/// it, and its trigger says so.
#[test]
fn a_run_a_timer_started_is_a_timer_run_even_with_a_request_waiting() {
    let f = Fixture::new("timer");
    let inbox = f.inbox(&format!("{REQ_A}\n"));
    let p = f.run(
        &[("BOSS_WRAP_REQUESTS", &inbox)],
        Some("cluster-deploy-runner.timer"),
    );
    assert!(
        md(&p).get("trigger_name").is_none(),
        "the timer started this run: {p}"
    );
    assert_eq!(
        md(&p)["ops_requests"],
        serde_json::json!([REQ_A]),
        "and it still names the request it answers"
    );
}

/// The unit's inbox is the file the `converge` verb writes — the path
/// lives in the unit and in forge-defaults.sh, so it is held equal here
/// (CLAUDE.md §9a), and converge-now.sh writes forge-defaults' name.
#[test]
fn the_units_inbox_is_the_one_the_converge_verb_writes() {
    let unit = read(UNIT);
    let named: Vec<&str> = unit
        .lines()
        .filter_map(|l| l.strip_prefix("Environment=BOSS_WRAP_REQUESTS="))
        .collect();
    assert_eq!(named.len(), 1, "the unit names one request inbox");
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\" && printf '%s' \"$CONVERGE_REQUESTS\"")
        .arg("bash")
        .arg(repo_root().join("infra/forge/forge-defaults.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_SOR_ENV", "/nonexistent/sor.env")
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        named[0],
        "the unit's inbox is forge-defaults.sh's CONVERGE_REQUESTS"
    );
    let verb = read("infra/forge/converge-now.sh");
    assert!(
        verb.lines()
            .any(|l| l.trim() == "INBOX=\"$CONVERGE_REQUESTS\""),
        "converge-now.sh writes the request into CONVERGE_REQUESTS"
    );
    // ExecStartPre must see the inbox the verb wrote BEFORE the runner
    // (ExecStart) takes it: the wrap is the unit's ExecStartPre.
    assert!(
        unit.lines().any(|l| l.starts_with("ExecStartPre=")
            && l.contains("boss-maintenance-wrap.sh maintenance-cluster-converge")),
        "the wrap opens the packet from ExecStartPre"
    );
}

/// The trigger name the wrap stamps is one the protocol declares, on a
/// trigger step the run waits on — otherwise `resolve_triggers` falls
/// back to the first (timer) trigger and the stamp is silently lost.
#[test]
fn the_protocol_declares_the_trigger_the_wrap_names() {
    let wf = read("infra/platform/workflows/maintenance-cluster-converge.toml");
    assert!(
        wf.lines()
            .any(|l| l.contains("trigger_name = \"ops-request\"")),
        "maintenance-cluster-converge declares an `ops-request` trigger"
    );
    assert!(
        wf.lines()
            .any(|l| l.trim() == "ready_when = \"steps.scheduled.done OR steps.requested.done\""),
        "the run waits on either trigger"
    );
}
