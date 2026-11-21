//! `infra/dev/standby-banner.sh`: what the standby shell prints before
//! the prompt (design 0491591b, backlog 47601829).
//!
//! On 2026-11-07 the dev pod sat Pending for about 80 minutes and the
//! reason was in the scheduler's events, which nobody outside the LAN
//! could read. The standby (infra/cluster/manifests/boss-dev-standby.yaml)
//! is the shell an operator reaches instead, and this banner prints that
//! reason first: the dev pod's phase, its node and its newest event, the
//! newest door comparison, and the open work-sessions.
//!
//! Pinned against a stub `kubectl` and a stub `boss-api` (streams split
//! as the real door splits them: the body on stdout, `HTTP:<code>` or
//! curl's error on stderr). PATH holds the stubs and the few real
//! programs the script uses, so a kubectl installed on the machine
//! running this suite cannot answer for the stub. The script must never
//! fail the login it prints for, so every case also asserts exit 0.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/standby-banner.sh";

const KUBECTL_STUB: &str = r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
if [ "${STUB_KUBE_DOWN:-}" = 1 ]; then
  echo 'Unable to connect to the server: dial tcp 10.96.0.1:443: i/o timeout' >&2
  exit 1
fi
case "$2" in
  pods) printf '%b' "${STUB_PODS:-}" ;;
  events) printf '%b' "${STUB_EVENTS:-}" ;;
  *) exit 9 ;;
esac
"#;

const BOSS_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $*" >> "$STUB_LOG"
if [ "${STUB_API_DOWN:-}" = 1 ]; then
  echo 'curl: (7) Failed to connect to boss-jobs-internal port 7900' >&2
  exit 7
fi
case "$2" in
  /api/estate/comparisons*) printf '%s\n' "$STUB_DOOR" ;;
  /api/jobs*) printf '%s\n' "$STUB_SESSIONS" ;;
  *) exit 9 ;;
esac
echo 'HTTP:200' >&2
"#;

/// The newest door comparison as the record holds it (shape read from the
/// live jobs API on 2026-11-09), with the LAN half dark.
const DOOR_LAN_DARK: &str = r#"{"data":[{"event_id":"e1","kind":"estate.comparison","payload":{"observed_at":"2026-11-07T01:10:00Z","doors":[{"id":"dev-ssh","band_s":900,"halves":[{"half":"lan","target":"10.20.0.35:22","open":false,"dark_since":"2026-11-07T00:17:00Z","reason":"connect refused"},{"half":"public","target":"dev.algedonic.dev","open":true,"dark_since":null,"reason":null}]}],"findings":{"door_dark":[{"id":"dev-ssh/lan"}],"door_dimming":[]}}}],"total":1}"#;

const SESSIONS: &str = r#"{"data":[{"id":"f5daef34-e284-41d8-a00b-795994a9f92d","title":"Session: claude@algedonic.dev on boss-dev-858b849c4c-mz6gq","metadata":{"last_active_at":"2026-11-07T00:15:02Z"}}],"total":2}"#;

struct Fixture {
    path: String,
    log: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("standby-banner-{name}"));
        let stubs = root.join("stubs");
        create_dir(&stubs);
        write_exec(&stubs.join("kubectl"), KUBECTL_STUB);
        write_exec(&stubs.join("boss-api"), BOSS_API_STUB);
        for tool in [
            "bash", "jq", "timeout", "tail", "date", "mktemp", "rm", "sed",
        ] {
            let real = ["/usr/bin", "/bin"]
                .into_iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("{tool} in /usr/bin or /bin"));
            std::os::unix::fs::symlink(&real, stubs.join(tool))
                .unwrap_or_else(|e| panic!("link {tool}: {e}"));
        }
        Self {
            path: stubs.display().to_string(),
            log: root.join("stub.log"),
        }
    }

    fn run(&self, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .env_clear()
            .env("PATH", &self.path)
            .env("HOSTNAME", "boss-dev-standby-test")
            .env("STUB_LOG", &self.log)
            .env("STUB_DOOR", DOOR_LAN_DARK)
            .env("STUB_SESSIONS", SESSIONS);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run standby-banner.sh");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn has(out: &str, line: &str) {
    assert!(
        out.lines().any(|l| l.contains(line)),
        "expected a line holding `{line}`, got:\n{out}"
    );
}

/// The case the standby exists for: the dev pod Pending with no node,
/// and the scheduler's reason printed beside it.
#[test]
fn a_pending_dev_pod_is_printed_with_the_schedulers_reason() {
    let f = Fixture::new("pending");
    let (code, out) = f.run(&[
        ("STUB_PODS", "boss-dev-858b849c4c-q2x7d Pending \\n"),
        (
            "STUB_EVENTS",
            "Killing: Stopping container dev\\nFailedScheduling: 0/5 nodes are available: 1 Insufficient memory, 1 node(s) were unschedulable, 3 node(s) had untolerated taint\\n",
        ),
    ]);
    assert_eq!(code, 0, "{out}");
    has(
        &out,
        "DEV POD: boss-dev-858b849c4c-q2x7d Pending on no node",
    );
    has(
        &out,
        "last event: FailedScheduling: 0/5 nodes are available: 1 Insufficient memory",
    );
    has(&out, "DOOR: observed 2026-11-07T01:10:00Z");
    has(
        &out,
        "dev-ssh: lan 10.20.0.35:22 DARK since 2026-11-07T00:17:00Z (connect refused); public dev.algedonic.dev open",
    );
    has(
        &out,
        "SESSIONS: 2 open; newest: Session: claude@algedonic.dev on boss-dev-858b849c4c-mz6gq, last active 2026-11-07T00:15:02Z",
    );
    // It asked for the dev pod by its label, and the events of that pod by name.
    let log = f.log();
    assert!(
        log.contains("get pods -n boss-dev -l app=boss-dev"),
        "{log}"
    );
    assert!(
        log.contains("--field-selector involvedObject.name=boss-dev-858b849c4c-q2x7d"),
        "{log}"
    );
    // The newest event is the newest CREATED one. The scheduler records
    // through events.k8s.io and leaves lastTimestamp null (14 of 14 in
    // boss-dev, measured 2026-11-15 by review run 75a760c9, finding 5), and
    // kubectl leaves null-keyed rows in API order, so sorting on it
    // answered right only by a naming coincidence.
    assert!(
        log.contains("--sort-by=.metadata.creationTimestamp"),
        "{log}"
    );
    assert!(!log.contains("--sort-by=.lastTimestamp"), "{log}");
}

/// The banner prints text other actors wrote (an event message, a
/// work-session title) into a root terminal, so a control character in it
/// must not reach the terminal: an ESC there is a terminal command, not
/// text (review run 75a760c9, finding 7). The words around it still print.
#[test]
fn control_characters_in_recorded_text_never_reach_the_terminal() {
    let f = Fixture::new("controls");
    let (code, out) = f.run(&[
        ("STUB_PODS", "boss-dev-858b849c4c-q2x7d Pending \\n"),
        (
            "STUB_EVENTS",
            "FailedScheduling: \\033]0;owned\\a0/5 nodes\\033[2J are available\\n",
        ),
        (
            "STUB_SESSIONS",
            r#"{"data":[{"title":"Session:\u001b[31m red\u001b[0m\u009b2J tail","metadata":{}}],"total":1}"#,
        ),
    ]);
    assert_eq!(code, 0, "{out}");
    for c in out.chars() {
        assert!(
            c == '\n' || !c.is_control(),
            "a control character {:?} reached the terminal:\n{out:?}",
            c
        );
    }
    has(
        &out,
        "last event: FailedScheduling: ]0;owned0/5 nodes[2J are available",
    );
    has(&out, "newest: Session:[31m red[0m2J tail");
}

#[test]
fn a_running_dev_pod_names_its_node() {
    let f = Fixture::new("running");
    let (code, out) = f.run(&[
        ("STUB_PODS", "boss-dev-858b849c4c-mz6gq Running w-1\\n"),
        ("STUB_EVENTS", ""),
    ]);
    assert_eq!(code, 0, "{out}");
    has(&out, "DEV POD: boss-dev-858b849c4c-mz6gq Running on w-1");
    has(&out, "last event: none recorded");
}

/// A dark cluster API and a dark jobs API are each a line saying so,
/// with the error, and the login still gets its prompt.
#[test]
fn a_dark_cluster_and_a_dark_record_are_each_named_and_the_login_goes_on() {
    let f = Fixture::new("dark");
    let (code, out) = f.run(&[("STUB_KUBE_DOWN", "1"), ("STUB_API_DOWN", "1")]);
    assert_eq!(code, 0, "a banner must never fail the login: {out}");
    has(
        &out,
        "DEV POD: unavailable (kubectl exit 1: Unable to connect to the server",
    );
    has(
        &out,
        "DOOR: unavailable (curl: (7) Failed to connect to boss-jobs-internal port 7900)",
    );
    has(
        &out,
        "SESSIONS: unavailable (curl: (7) Failed to connect to boss-jobs-internal port 7900)",
    );
}

/// No pod and no comparison are answers, not silence: each says what it
/// means.
#[test]
fn no_dev_pod_and_no_door_comparison_are_said_out_loud() {
    let f = Fixture::new("empty");
    let (code, out) = f.run(&[
        ("STUB_PODS", ""),
        ("STUB_DOOR", r#"{"data":[],"total":0}"#),
        ("STUB_SESSIONS", r#"{"data":[],"total":0}"#),
    ]);
    assert_eq!(code, 0, "{out}");
    has(&out, "DEV POD: NONE. No pod carries app=boss-dev");
    has(&out, "DOOR: NOT WATCHED");
    has(&out, "SESSIONS: 0 open");
}

/// The door read lives twice, in `boss orient` and here (CLAUDE.md §9a):
/// the banner must ask for exactly the comparison orient reads, or the two
/// can describe the door differently.
#[test]
fn the_banner_reads_the_door_comparison_orient_reads() {
    let door_rs =
        std::fs::read_to_string(repo_root().join("crates/orchestrators/boss-cli/src/door.rs"))
            .expect("door.rs");
    let read = door_rs
        .lines()
        .find_map(|l| l.trim().strip_prefix("pub(crate) const READ: &str = \""))
        .and_then(|l| l.strip_suffix("\";"))
        .expect("door.rs declares `pub(crate) const READ: &str = \"...\";`");
    let banner = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    assert!(
        banner.contains(&format!("boss-api GET '{read}'")),
        "{SCRIPT} must read the door through `boss-api GET '{read}'`, the read boss orient makes"
    );
}
