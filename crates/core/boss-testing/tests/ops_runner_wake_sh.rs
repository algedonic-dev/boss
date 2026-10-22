//! `infra/ops/ops-runner-wake.sh`, the unit that runs it, and the
//! installer half that puts it on a host are RUN, not read — against a
//! stubbed `systemctl` that keeps each unit's state, a stubbed `curl`
//! that serves a recorded event stream, and a scratch
//! `/etc/systemd/system`, so every verdict below is one the scripts
//! actually reached.
//!
//! WHY (backlog ca6f40c2). The forge's ops runner learned of a filed
//! ops-request only from its one-minute timer: across 199 requests
//! answered 2026-10-18T17:06Z..2026-10-19T08:30Z the forge's `queued_s`
//! read median 33 s, p90 67 s, max 107 s — the timer's phase, not the
//! work. `jobs.job.created` was already on the record for every filing;
//! nothing on the host listened. The waker listens, and its whole
//! authority is ONE fixed act: start the same `boss-ops-runner.service`
//! the timer starts. Nothing from an event reaches that act — the frame
//! is read only for whether it names an ops-request for this host — and
//! the timer stays the net that owes the waker nothing.
//!
//! Nothing here touches a host: `systemctl` and `curl` are stubs on every
//! path, and every file lives in a scratch directory.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const WAKER: &str = "infra/ops/ops-runner-wake.sh";
const WAKE_UNIT: &str = "infra/ops/boss-ops-runner-wake.service";
const TIMER_UNIT: &str = "infra/ops/boss-ops-runner.timer";
const INSTALLER: &str = "infra/ops/install-ops-runner.sh";
const TIMER: &str = "boss-ops-runner.timer";
const SERVICE: &str = "boss-ops-runner.service";
const WAKE: &str = "boss-ops-runner-wake.service";
const FILED: &str = "0f0f0000-1111-4222-8333-444455556666";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// jq and bash are on the gate image and the hosts; a box without them
/// skips loudly rather than passing.
fn tools() -> bool {
    for t in ["jq", "bash"] {
        if !has(t) {
            eprintln!("ops_runner_wake_sh: SKIPPED — no {t} on this box; the gate image has it");
            return false;
        }
    }
    true
}

/// The stub systemctl: each unit's is-active WORD lives in
/// `$STUB_STATE/<unit>.act`, answered with systemd's own exit codes.
/// `start` is logged and changes nothing — the test reads the log. Any
/// other verb is an error the test sees.
const SYSTEMCTL_STUB: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_LOG"
cmd="$1"; shift
[ "${1:-}" = "--" ] && shift
u="${1:-}"
case "$cmd" in
  is-active)
    w=$(cat "$STUB_STATE/$u.act" 2>/dev/null)
    [ -n "$w" ] || w=inactive
    echo "$w"
    case "$w" in active|activating|reloading) exit 0 ;; *) exit 3 ;; esac ;;
  start) exit 0 ;;
  is-enabled) echo enabled; exit 0 ;;
  enable|daemon-reload) exit 0 ;;
  *) echo "stub systemctl: unexpected $cmd $*" >&2; exit 99 ;;
esac
"#;

/// The stub curl: logs its whole argv, one word per line between
/// markers, then serves `$STUB_STREAM` (absent = a dark API, exit 7).
const CURL_STUB: &str = r#"#!/bin/sh
{ echo "--- call"; for a in "$@"; do printf '%s\n' "$a"; done; } >> "$STUB_CURL_LOG"
[ -r "$STUB_STREAM" ] || { echo "curl: (7) Failed to connect (stub)" >&2; exit 7; }
cat "$STUB_STREAM"
exit 0
"#;

/// One `jobs.job.created` frame as `/api/events/stream` sends it.
fn frame(job_kind: &str, host: &str, id: &str, title: &str) -> String {
    let entry = serde_json::json!({
        "event_id": "9e9e0000-1111-4222-8333-444455556666",
        "timestamp": "2026-10-19T08:30:01Z",
        "source": "jobs",
        "kind": "jobs.job.created",
        "payload": {
            "id": id,
            "kind": job_kind,
            "title": title,
            "status": "open",
            "metadata": {"host": host, "verb": "converge"}
        }
    });
    format!("data: {entry}\n\n")
}

struct Host {
    root: PathBuf,
    bin: PathBuf,
    state: PathBuf,
    etc: PathBuf,
    log: PathBuf,
    curl_log: PathBuf,
    stream: PathBuf,
    summary: PathBuf,
}

impl Host {
    /// The forge with its runner installed: the timer on, no pass running.
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("ops-runner-wake-{name}"));
        let bin = root.join("bin");
        let state = root.join("state");
        let etc = root.join("etc");
        for d in [&bin, &state, &etc] {
            std::fs::create_dir_all(d).unwrap();
        }
        write_exec(&bin.join("systemctl"), SYSTEMCTL_STUB);
        write_exec(&bin.join("curl"), CURL_STUB);
        let h = Self {
            log: root.join("systemctl.log"),
            curl_log: root.join("curl.log"),
            stream: root.join("stream.sse"),
            summary: root.join("summary.json"),
            root,
            bin,
            state,
            etc,
        };
        h.unit(TIMER, "active");
        h.unit(SERVICE, "inactive");
        h
    }

    fn unit(&self, unit: &str, act: &str) {
        write_file(&self.state.join(format!("{unit}.act")), &format!("{act}\n"));
    }

    fn serve(&self, frames: &[String]) {
        write_file(&self.stream, &frames.concat());
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Every systemctl call that is not a read.
    fn acts(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| !c.starts_with("is-active") && !c.starts_with("is-enabled"))
            .collect()
    }

    fn curl_calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.curl_log)
            .unwrap_or_default()
            .split("--- call\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(str::to_string).collect())
            .collect()
    }

    fn base(&self, script: &str) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(script))
            .env_clear()
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
            .env("STUB_CURL_LOG", &self.curl_log)
            .env("STUB_STREAM", &self.stream)
            // The address file is absent; the address is in the
            // environment, as the unit's EnvironmentFile= puts it.
            .env("BOSS_SOR_ENV", self.root.join("absent-sor.env"))
            .env("BOSS_JOBS_URL", "http://sor.test:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("no-token"))
            .env("INSTALL_SYSTEMCTL", self.bin.join("systemctl"))
            .env("INSTALL_ETC", &self.etc);
        cmd
    }

    /// The waker as its unit runs it, for one connection: the lifetime
    /// seam at 0 ends it when the stream ends, instead of reconnecting.
    fn wake(&self) -> (i32, String) {
        self.wake_with(&[("HOST_ID", "forge"), ("OPS_WAKE_MAX_S", "0")])
    }

    fn wake_with(&self, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = self.base(WAKER);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("ops-runner-wake.sh runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The host's converge calling its installer.
    fn converge(&self, roles: &str, extra: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = self.base(INSTALLER);
        cmd.arg("forge")
            .env("BOSS_NODE_ROLES", roles)
            .env("BOSS_NODE_ROLES_SOURCE", "registry")
            .env("BOSS_OPS_RUNNER_RETIRED", self.root.join("lib/retired"))
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("install-ops-runner.sh runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn summary(&self, key: &str) -> String {
        let text = std::fs::read_to_string(&self.summary).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get(key).and_then(|x| x.as_str()).map(str::to_string))
            .unwrap_or_default()
    }
}

fn starts(h: &Host) -> usize {
    h.acts()
        .iter()
        .filter(|c| *c == &format!("start -- {SERVICE}"))
        .count()
}

// ---------------------------------------------------------------------------
// The waker.
// ---------------------------------------------------------------------------

/// The one thing it is for: a request filed for THIS host starts one
/// runner pass. Frames for another host, another kind, or carrying shell
/// text in their title start nothing, and nothing from any frame reaches
/// a systemctl argv — every call names a literal unit.
#[test]
fn a_request_filed_for_this_host_starts_one_pass_and_nothing_else_does() {
    if !tools() {
        return;
    }
    let h = Host::new("filed");
    let pwned = h.root.join("pwned");
    h.serve(&[
        frame(
            "ops-request",
            "boss-gcp",
            "aaaa0000-1111-4222-8333-444455556666",
            "other host",
        ),
        frame(
            "gate-run",
            "forge",
            "bbbb0000-1111-4222-8333-444455556666",
            "not a request",
        ),
        frame(
            "backlog-item",
            "forge",
            "cccc0000-1111-4222-8333-444455556666",
            &format!("$(touch {})", pwned.display()),
        ),
        frame("ops-request", "forge", FILED, "converge forge"),
        // A keep-alive: quiet, and with the test's lifetime of 0 the
        // point where the waker ends this connection.
        ":\n\n".to_string(),
    ]);
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(starts(&h), 1, "one filing, one pass: {:?}", h.calls());
    assert_eq!(
        h.acts(),
        vec![format!("start -- {SERVICE}")],
        "{:?}",
        h.calls()
    );
    for c in h.calls() {
        assert!(
            [
                format!("is-active -- {TIMER}"),
                format!("is-active -- {SERVICE}"),
                format!("start -- {SERVICE}"),
            ]
            .contains(&c),
            "a systemctl call carried something other than a literal unit: {c}"
        );
    }
    assert!(!pwned.exists(), "a frame's text was executed");
    assert!(
        out.contains(&FILED[..8]),
        "the wake names the request it answered: {out}"
    );
}

#[test]
fn frames_for_another_host_or_kind_or_no_json_start_nothing() {
    if !tools() {
        return;
    }
    let h = Host::new("nothing");
    h.serve(&[
        frame("ops-request", "boss-gcp", FILED, "other host"),
        frame("ops-request", "", FILED, "no host"),
        frame("maintenance-forge-converge", "forge", FILED, "not a request"),
        "data: not json at all\n\n".to_string(),
        "data:\n\n".to_string(),
        "data: {\"source\":\"jobs\",\"kind\":\"jobs.job.created\",\"payload\":{\"kind\":\"ops-request\",\"metadata\":{\"host\":\"forge\"},\"id\":\"$(id)\"}}\n\n".to_string(),
        "event: unknown\n\n".to_string(),
    ]);
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(starts(&h), 0, "{:?}\n{out}", h.calls());
}

/// A burst — a train filing its car probes — arrives as frames back to
/// back. Every one of them was committed before the pass the first one
/// starts, so that one pass answers them all: one start, not one each.
#[test]
fn a_burst_of_filings_is_one_pass() {
    if !tools() {
        return;
    }
    let h = Host::new("burst");
    h.serve(&[
        frame("ops-request", "forge", FILED, "probe 1"),
        frame(
            "ops-request",
            "forge",
            "1f1f0000-1111-4222-8333-444455556666",
            "probe 2",
        ),
        frame(
            "ops-request",
            "forge",
            "2f2f0000-1111-4222-8333-444455556666",
            "probe 3",
        ),
    ]);
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(starts(&h), 1, "{:?}\n{out}", h.calls());
}

/// A pass already running began before this request existed and may
/// not see it. systemd merges a start into the running oneshot's job, so
/// the first start waits that pass out and the second is a pass that
/// began after the filing.
#[test]
fn a_pass_already_running_is_waited_out_then_a_fresh_one_starts() {
    if !tools() {
        return;
    }
    let h = Host::new("running");
    h.unit(SERVICE, "activating");
    h.serve(&[frame("ops-request", "forge", FILED, "converge forge")]);
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(starts(&h), 2, "{:?}\n{out}", h.calls());
}

/// The waker has no authority the timer does not: a runner whose timer
/// is off — retired, or stopped by a hand — is never started by a wake.
#[test]
fn a_timer_that_is_off_is_never_overridden() {
    if !tools() {
        return;
    }
    for word in ["inactive", "failed", ""] {
        let h = Host::new(&format!("timer-off-{word}"));
        h.unit(TIMER, word);
        h.serve(&[frame("ops-request", "forge", FILED, "converge forge")]);
        let (rc, out) = h.wake();
        assert_eq!(rc, 0, "{out}");
        assert_eq!(starts(&h), 0, "timer `{word}`: {:?}\n{out}", h.calls());
        assert!(out.contains("not waking"), "the skip is said: {out}");
    }
}

/// The stream it reads: the events port of the system of record's own
/// host — from the port table, never a literal — narrowed server-side
/// to job creations, signed as the waker on the auditor reader identity.
#[test]
fn the_stream_is_the_records_events_port_narrowed_and_signed() {
    if !tools() {
        return;
    }
    let h = Host::new("signed");
    h.serve(&[]);
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    let calls = h.curl_calls();
    assert_eq!(calls.len(), 1, "one connection: {calls:?}");
    let argv = &calls[0];
    let ports = std::fs::read_to_string(repo_root().join("infra/forge/sor-ports.env")).unwrap();
    let events = ports
        .lines()
        .find_map(|l| l.strip_prefix("events="))
        .expect("the port table names an events port")
        .trim();
    let url =
        format!("http://sor.test:{events}/api/events/stream?source=jobs&kind=jobs.job.created");
    assert!(argv.contains(&url), "{argv:?}");
    assert!(
        argv.iter().any(|a| a.starts_with("x-boss-user: ")
            && a.contains("automation:ops-runner-wake")
            && a.contains("\"access_tier\":\"auditor\"")),
        "the read is signed: {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "-N"),
        "the stream is not buffered: {argv:?}"
    );
}

/// A dark API is a reconnect, never a crash and never a wake.
#[test]
fn a_dark_stream_wakes_nothing_and_says_so() {
    if !tools() {
        return;
    }
    let h = Host::new("dark");
    let (rc, out) = h.wake();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(starts(&h), 0, "{:?}", h.calls());
    assert!(out.contains("stream ended"), "{out}");
}

/// Like the runner, no default host: a waker that guessed which host it
/// speaks for would wake on another host's requests. It refuses with
/// EX_CONFIG, which the unit does not restart.
#[test]
fn no_host_is_a_refusal_before_any_read() {
    if !tools() {
        return;
    }
    let h = Host::new("no-host");
    h.serve(&[frame("ops-request", "forge", FILED, "converge forge")]);
    for host in [None, Some(""), Some("Forge;id"), Some("../x")] {
        let mut env = vec![("OPS_WAKE_MAX_S", "0")];
        if let Some(v) = host {
            env.push(("HOST_ID", v));
        }
        let (rc, out) = h.wake_with(&env);
        assert_eq!(rc, 78, "HOST_ID {host:?}: {out}");
    }
    assert!(h.curl_calls().is_empty(), "{:?}", h.curl_calls());
    assert!(h.calls().is_empty(), "{:?}", h.calls());
}

// ---------------------------------------------------------------------------
// The units.
// ---------------------------------------------------------------------------

fn unit_lines(path: &str) -> Vec<String> {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The timer stays the net, unchanged in cadence (the stall alarm counts
/// in it), and it is what brings the waker up — at boot and on every
/// converge's `enable --now` — while the waker is PART OF the timer, so
/// the retire verb's one act on the timer stops the waker with it.
#[test]
fn the_timer_is_the_net_and_the_waker_lives_and_dies_with_it() {
    let timer = unit_lines(TIMER_UNIT);
    assert!(
        timer.contains(&"OnUnitActiveSec=1min".to_string()),
        "{timer:?}"
    );
    assert!(
        timer.contains(&format!("Wants={WAKE}")),
        "the timer brings the waker up: {timer:?}"
    );
    let wake = unit_lines(WAKE_UNIT);
    for want in [
        format!("PartOf={TIMER}"),
        "EnvironmentFile=/etc/boss/sor.env".to_string(),
        "Type=simple".to_string(),
        "Restart=always".to_string(),
        "RestartPreventExitStatus=78".to_string(),
        "ExecStart=/opt/boss/infra/ops/ops-runner-wake.sh".to_string(),
    ] {
        assert!(wake.contains(&want), "{WAKE_UNIT} lacks `{want}`: {wake:?}");
    }
    assert!(
        !wake.iter().any(|l| l == "[Install]"),
        "the waker is never enabled on its own; the timer wants it: {wake:?}"
    );
    // The runner itself still carries no tie to the timer (the retire
    // verb's header rests on that), and none to the waker.
    let runner = unit_lines("infra/ops/boss-ops-runner.service");
    assert!(
        !runner.iter().any(|l| l.starts_with("PartOf=")
            || l.starts_with("BindsTo=")
            || l.starts_with("Requires=")),
        "{runner:?}"
    );
}

// ---------------------------------------------------------------------------
// The installer.
// ---------------------------------------------------------------------------

/// A host whose runner runs from its tree directly gets the waker beside
/// it: the unit byte-identical, a drop-in naming this host and running
/// the waker from the same tree as the runner, and no start of its own —
/// the timer's `enable --now`, which already ran, pulls it in.
#[test]
fn the_installer_puts_the_waker_beside_the_runner() {
    if !tools() {
        return;
    }
    let h = Host::new("install");
    let tree = h.root.join("tree");
    let (rc, out) = h.converge("", &[("INSTALL_OPS_RUNNER_REPO", tree.to_str().unwrap())]);
    assert_eq!(rc, 0, "{out}");
    let installed = std::fs::read(h.etc.join(WAKE)).expect("the waker unit landed");
    assert_eq!(
        installed,
        std::fs::read(repo_root().join(WAKE_UNIT)).unwrap(),
        "the waker unit is installed byte-identical"
    );
    let dropin = std::fs::read_to_string(h.etc.join(format!("{WAKE}.d/forge.conf")))
        .expect("the waker's drop-in landed");
    assert!(dropin.contains("Environment=HOST_ID=forge\n"), "{dropin}");
    assert!(
        dropin.contains(&format!(
            "ExecStart=\nExecStart={}/infra/ops/ops-runner-wake.sh\n",
            tree.display()
        )),
        "{dropin}"
    );
    assert!(
        h.acts().contains(&format!("enable --now {TIMER}")),
        "{:?}",
        h.calls()
    );
    assert!(
        h.summary("ops_runner_wake").starts_with("installed"),
        "{out}"
    );
}

/// Under a tree helper (boss-gcp's root tree), every start holds the
/// helper's SHARED generation lock until its interpreter exits, and the
/// tree's refresh needs it EXCLUSIVE. A waker that never exits would
/// hold off every refresh, so it is not installed there — and the
/// converge says so rather than leaving it to be noticed.
#[test]
fn no_waker_under_a_tree_helper_whose_lock_it_would_hold() {
    if !tools() {
        return;
    }
    let h = Host::new("helper");
    let (rc, out) = h.converge(
        "",
        &[(
            "INSTALL_OPS_RUNNER_TREE_HELPER",
            "/var/lib/boss/x/gcp-root-launch.sh",
        )],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(h.etc.join("boss-ops-runner.service").is_file(), "{out}");
    assert!(!h.etc.join(WAKE).exists(), "{out}");
    assert!(!h.etc.join(format!("{WAKE}.d")).exists(), "{out}");
    let field = h.summary("ops_runner_wake");
    assert!(
        field.contains("not installed") && field.contains("lock"),
        "{field}"
    );
}

/// When a retired runner's files are removed, the waker's go with them,
/// before the timer file (the timer file goes last, as before).
#[test]
fn a_removed_runner_takes_its_waker_with_it() {
    if !tools() {
        return;
    }
    let h = Host::new("remove");
    let (rc, out) = h.converge("", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(h.etc.join(WAKE).is_file(), "{out}");
    // The retire verb turned the timer off; the pass has ended.
    h.unit(TIMER, "inactive");
    write_file(&h.state.join(format!("{TIMER}.en")), "disabled\n");
    let stub = std::fs::read_to_string(h.bin.join("systemctl")).unwrap().replace(
        "is-enabled) echo enabled; exit 0 ;;",
        "is-enabled) w=$(cat \"$STUB_STATE/$u.en\" 2>/dev/null); echo \"${w:-enabled}\"; [ \"${w:-enabled}\" = enabled ] ;;",
    );
    write_exec(&h.bin.join("systemctl"), &stub);
    let (rc, out) = h.converge("cluster-operator", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("REMOVED"), "{out}");
    for f in [WAKE, &format!("{WAKE}.d"), SERVICE, TIMER] {
        assert!(!h.etc.join(f).exists(), "{f} survived the removal: {out}");
    }
}
