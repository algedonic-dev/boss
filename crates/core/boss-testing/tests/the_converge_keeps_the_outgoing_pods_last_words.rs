//! The cluster converge keeps what the OUTGOING boss pod said while it
//! was terminated, from outside the pod, on its own packet.
//!
//! WHY (backlog d44ac67f). After 9f1e4063 bounded and retried each gate's
//! `recording_ended`, the machine-gate rolls at 16:31Z and 17:11Z on
//! 2026-10-22 stated 2 of 21 and 0 of 21 ends, and the clean window that
//! gates the machine token's enforce (6805c764) reset to 17:11:58Z. Both
//! rolls took 75 and 76 s against ~41 s for the eleven around them
//! (`roll_s` on their maintenance-cluster-converge packets), so the old
//! pod held its grace — but the only account of WHY was the ERROR line
//! `Evidence::end` writes, and that line lived in the pod's own log,
//! which a `Recreate` roll deletes with the pod. A dying process cannot
//! write its cause to a database it may be failing to reach, and the
//! next process cannot know what its predecessor said. The runner can:
//! it holds the admin credential, it owes nothing to the pod it rolls,
//! and it is already watching the roll. So `roll_deployment` follows the
//! outgoing pod's log from just before its patch until that pod is gone,
//! keeps the whole capture on the forge host, and records the part that
//! answers the question — whether the launcher saw SIGTERM, every
//! `gate evidence` line, the first and last instants — as
//! `outgoing_pod_<namespace>` on the converge's run step.
//!
//! Each case drives `roll_deployment` (infra/forge/cluster-deploy-lib.sh)
//! with a stub kubectl that records its calls and answers `logs` from a
//! fixture.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const SUMMARY_LIB: &str = "infra/run-summary.sh";
const REGISTRY: &str = "10.20.0.15:3000/david/boss";

/// What a dropped end looks like in the pod's log, as kubectl
/// `--timestamps` hands it back: the timestamp kubelet adds, then the
/// line — one of them coloured the way tracing colours a terminal.
const DROPPED: &str = "\
2026-10-22T16:31:17.190000000Z ==> launcher SIGTERM — stopping 31 children
2026-10-22T16:31:17.300000000Z 2026-10-22T16:31:17.300Z  INFO boss_jobs: draining
2026-10-22T16:31:25.210000000Z 2026-10-22T16:31:25.210Z \u{1b}[31mERROR\u{1b}[0m boss_core::gate_evidence: gate evidence: this process's end is NOT on the log, so its watch reads broken: 4 attempt(s) over 8s — attempt 1 timed out after 2s service=accounts gate=machine_gate
2026-10-22T16:31:27.200000000Z 2026-10-22T16:31:27.200Z ERROR boss_core::gate_evidence: gate evidence did not state its end within 10s of SIGTERM; exiting anyway — this process's watch reads broken
2026-10-22T16:31:47.000000000Z 2026-10-22T16:31:47.000Z  INFO boss_gateway: bye
";

struct Case {
    root: PathBuf,
    kubectl: PathBuf,
    summary: PathBuf,
    outgoing: PathBuf,
}

impl Case {
    /// A stub kubectl: `get pods` answers `pod`, `logs` prints `logs` and
    /// exits `logs_rc`; every call is recorded in order.
    fn new(name: &str, pod: &str, logs: &str, logs_rc: i32) -> Self {
        let root = scratch_dir(&format!("outgoing-last-words-{name}"));
        std::fs::write(root.join("fixture"), logs).unwrap();
        let kubectl = root.join("kubectl");
        write_exec(
            &kubectl,
            &format!(
                r#"#!/usr/bin/env bash
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) kubectl $*" >> "$STUB_LOG"
case "$*" in
  *"get deploy boss"*) echo "{REGISTRY}:running-before" ;;
  *"get pods"*) printf '%s' '{pod}' ;;
  "logs "*)
    if [ -n "${{STUB_LOGS_HANG:-}}" ]; then exec sleep 30; fi
    cat "$STUB_FIXTURE"
    [ {logs_rc} -eq 0 ] || echo 'Error from server (NotFound): pods "{pod}" not found' >&2
    exit {logs_rc} ;;
esac
exit 0
"#
            ),
        );
        Self {
            summary: root.join("summary.json"),
            outgoing: root.join("outgoing"),
            kubectl,
            root,
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn summary(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&self.summary).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null)
    }

    /// Run `roll_deployment` for namespace `ns`, outputs to files, under
    /// the test's own limit: a follower that holds the roll is a RED
    /// that names itself, not a hung suite.
    fn roll(&self, ns: &str, env: &[(&str, &str)]) -> (i32, String) {
        let script = format!(
            ". '{}'\n. '{}'\nroll_deployment \"$KUBECTL\" '{REGISTRY}' headsha lastgood \"$FAILED\" '{ns}'\n",
            repo_root().join(SUMMARY_LIB).display(),
            repo_root().join(LIB).display(),
        );
        let (out, err) = (self.root.join("stdout"), self.root.join("stderr"));
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STUB_FIXTURE", self.root.join("fixture"))
            .env("KUBECTL", &self.kubectl)
            .env("FAILED", self.root.join("failed"))
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary)
            .env("BOSS_OUTGOING_DIR", &self.outgoing)
            .env("BOSS_OUTGOING_CONNECT_S", "0")
            .env("BOSS_MACHINE_TOKEN_DIR", "/nonexistent/no-machine-token")
            .env("BOSS_SOR_ENV", "/nonexistent/no-sor.env")
            .env_remove("BOSS_JOBS_URL")
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
                    "roll_deployment had not returned after {limit:?}: the outgoing pod's \
                     follower holds the roll (stderr: {})",
                    std::fs::read_to_string(&err).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        (
            status.code().unwrap_or(-1),
            std::fs::read_to_string(&err).unwrap_or_default(),
        )
    }
}

#[test]
fn a_roll_records_what_the_outgoing_pod_said_on_its_way_out() {
    let c = Case::new("dropped", "boss-6d4f9c-x2x7q", DROPPED, 0);
    let (rc, err) = c.roll("boss", &[]);
    assert_eq!(rc, 0, "the roll itself is unchanged: {err}");

    let calls = c.calls();
    let follow = calls
        .iter()
        .find(|l| l.contains(" kubectl logs "))
        .unwrap_or_else(|| panic!("the outgoing pod's log was never followed: {calls:?}"));
    let patch = calls
        .iter()
        .find(|l| l.contains(" kubectl patch deploy boss"))
        .expect("the roll patched");
    // The follower runs in the background, so WHEN kubectl attaches is
    // the scheduler's; what the roll decides is the instant it reads
    // from, and that is taken before the patch. Nothing the pod says
    // after the patch can fall before it.
    // Both are `date -u +%Y-%m-%dT%H:%M:%SZ`, which orders as text.
    let patched_at = patch.split(' ').next().unwrap();
    let since = follow
        .split("--since-time=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("a --since-time");
    assert_eq!(since.len(), patched_at.len(), "{since} vs {patched_at}");
    assert!(
        since <= patched_at,
        "the follow reads from an instant at or before the patch ({since} vs {patch})"
    );
    for want in [
        "-n boss",
        "pod/boss-6d4f9c-x2x7q",
        "-c boss",
        "-f",
        "--timestamps",
        "--since-time=",
    ] {
        assert!(follow.contains(want), "`{want}` missing from: {follow}");
    }

    let said = &c.summary()["outgoing_pod_boss"];
    assert_eq!(said["pod"], "boss-6d4f9c-x2x7q", "{said}");
    assert_eq!(said["followed"], true, "{said}");
    assert_eq!(said["follow_rc"], 0, "{said}");
    assert_eq!(said["lines"], 5, "{said}");
    assert_eq!(said["sigterm_seen"], true, "{said}");
    assert_eq!(said["first_at"], "2026-10-22T16:31:17.190000000Z", "{said}");
    assert_eq!(said["last_at"], "2026-10-22T16:31:47.000000000Z", "{said}");
    assert_eq!(said["gate_evidence_lines"], 2, "{said}");
    let evidence: Vec<&str> = said["gate_evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("gate_evidence is a list: {said}"))
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(evidence.len(), 2, "{said}");
    assert!(
        evidence[0].contains(
            "ERROR boss_core::gate_evidence: gate evidence: this process's end is NOT on the log"
        ) && evidence[0].contains("service=accounts"),
        "the end's own reason, its colour codes stripped: {}",
        evidence[0]
    );
    assert!(
        evidence[1].contains("did not state its end within 10s of SIGTERM"),
        "{}",
        evidence[1]
    );

    // The whole capture is KEPT, unreduced, where the record says.
    let capture = said["capture"].as_str().expect("the capture is named");
    assert!(
        capture.starts_with(c.outgoing.to_str().unwrap()),
        "kept under the converge's own state: {capture}"
    );
    assert_eq!(
        std::fs::read_to_string(capture).expect("the capture exists"),
        DROPPED,
        "the capture is what kubectl handed back, byte for byte"
    );
    assert!(
        err.contains("boss-6d4f9c-x2x7q")
            && err.contains("gate evidence: this process's end is NOT on the log"),
        "the journal names the pod and carries the evidence lines: {err}"
    );
}

#[test]
fn a_follow_kubectl_refused_reads_as_unheard_not_as_silence() {
    let c = Case::new("refused", "boss-gone", "", 1);
    let (rc, err) = c.roll("boss", &[]);
    assert_eq!(rc, 0, "a refused follow never fails the roll: {err}");
    let said = &c.summary()["outgoing_pod_boss"];
    assert_eq!(said["followed"], true, "{said}");
    assert_eq!(
        said["follow_rc"], 1,
        "kubectl's exit is on the record: {said}"
    );
    assert_eq!(said["sigterm_seen"], false, "{said}");
    assert_eq!(said["gate_evidence_lines"], 0, "{said}");
    let capture = said["capture"].as_str().expect("the capture is named");
    assert!(
        std::fs::read_to_string(capture)
            .unwrap_or_default()
            .contains("NotFound"),
        "kubectl's own refusal is kept in the capture"
    );
}

#[test]
fn no_running_pod_is_said_and_nothing_is_followed() {
    let c = Case::new("none", "", "", 0);
    let (rc, err) = c.roll("boss", &[]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        !c.calls().iter().any(|l| l.contains(" kubectl logs ")),
        "nothing to follow: {:?}",
        c.calls()
    );
    let said = &c.summary()["outgoing_pod_boss"];
    assert_eq!(said["followed"], false, "{said}");
    assert!(
        said["why"]
            .as_str()
            .unwrap_or("")
            .contains("no Running boss pod"),
        "{said}"
    );
}

#[test]
fn each_instance_records_under_its_own_namespace() {
    let c = Case::new("playground", "boss-7a1b-pq", DROPPED, 0);
    let (rc, err) = c.roll("boss-playground", &[]);
    assert_eq!(rc, 0, "{err}");
    let summary = c.summary();
    assert_eq!(
        summary["outgoing_pod_boss-playground"]["pod"], "boss-7a1b-pq",
        "{summary}"
    );
    assert!(summary.get("outgoing_pod_boss").is_none(), "{summary}");
}

#[test]
fn a_follower_that_never_ends_costs_its_bound_and_no_more() {
    let c = Case::new("hang", "boss-stuck", "", 0);
    let start = Instant::now();
    let (rc, err) = c.roll(
        "boss",
        &[("STUB_LOGS_HANG", "1"), ("BOSS_OUTGOING_FOLLOW_S", "1")],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "the follow is bounded: {:?}",
        start.elapsed()
    );
    let said = &c.summary()["outgoing_pod_boss"];
    assert_eq!(
        said["follow_rc"], 124,
        "timeout's own exit says it was cut: {said}"
    );
}
