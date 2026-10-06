//! `infra/ops/retire-ops-runner.sh` and the converge branches that
//! witness it in `infra/ops/install-ops-runner.sh` are RUN, not read —
//! against a stubbed `systemctl` that keeps each unit's state, a stubbed
//! system of record, and a scratch `/etc/systemd/system`, so every
//! verdict below is one the scripts actually reached.
//!
//! WHY (backlog 98eb9349; design a79a8067, decided by David 2026-10-01).
//! A host that drops the `ops-runner` role keeps the runner it has, and
//! its converge says STILL INSTALLED on every packet (backlog 1c7f8f23)
//! — a report naming a deliberate step the tree had no door for. Every
//! bounded host verb arrives through the host's runner, so the verb that
//! retires the runner cannot run through another door: the runner
//! retires ITSELF, stopping and disabling only `boss-ops-runner.timer`
//! and never the oneshot `boss-ops-runner.service` it is running in, so
//! the pass survives to report. The host's converge, on its own timer
//! and packet, is the second witness: RETIRED from the marker the verb
//! wrote, DISABLED BY HAND without one, then REMOVED.
//!
//! THE STUB CONFIRMATION the operator asked for first: the stub keeps
//! the timer and the service as two units with their own state, the
//! service `activating` the way a Type=oneshot pass is while it runs.
//! The verb's one acting call names the timer; the service's state is
//! the same after the stop as before it; and the verb goes on, after the
//! stop, to read the timer back and print its OK line — the pass that
//! issued the stop outlived it. What the stub cannot confirm is systemd
//! itself; that half is the unit semantics the script's header states (a
//! timer only triggers its service, which carries no Requires=/BindsTo=/
//! PartOf= on the timer), and the first live run proves it on the record:
//! an OK line on the request and RETIRED on the next converge.
//!
//! Nothing here touches a host. `systemctl` and `curl` are stubs on
//! every path, and every file lives in a scratch directory.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const VERB: &str = "infra/ops/retire-ops-runner.sh";
const INSTALLER: &str = "infra/ops/install-ops-runner.sh";
const TIMER: &str = "boss-ops-runner.timer";
const SERVICE: &str = "boss-ops-runner.service";
const REQUEST: &str = "5eed0000-1111-4222-8333-444455556666";
const OTHER_REQUEST: &str = "0be00000-1111-4222-8333-444455556666";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// jq and sha256sum are on the gate image and the hosts; a box without
/// them skips loudly rather than passing.
fn tools() -> bool {
    for t in ["jq", "sha256sum", "bash"] {
        if !has(t) {
            eprintln!("retire_ops_runner_sh: SKIPPED — no {t} on this box; the gate image has it");
            return false;
        }
    }
    true
}

/// The stub systemctl: each unit's is-enabled and is-active WORD lives
/// in `$STUB_STATE/<unit>.en` / `.act`, answered with systemd's own exit
/// codes (is-enabled exits 1 for disabled, is-active 3 for inactive), so
/// a script that reads the exit instead of the word is caught. `disable
/// --now` changes ONLY the unit it names — as systemd does for a timer,
/// whose stop does not propagate to the service it triggered — unless
/// STUB_DISABLE_NOOP leaves it as it was (a disable that claimed success
/// and did not take). Every call is logged.
const SYSTEMCTL_STUB: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_LOG"
cmd="$1"; shift
now=""
[ "${1:-}" = "--now" ] && { now=1; shift; }
[ "${1:-}" = "--" ] && shift
u="${1:-}"
case "$cmd" in
  is-enabled)
    w=$(cat "$STUB_STATE/$u.en" 2>/dev/null)
    [ -n "$w" ] && echo "$w"
    case "$w" in enabled|static) exit 0 ;; '') echo "Failed to get unit file state for $u: No such file or directory" >&2; exit 1 ;; *) exit 1 ;; esac ;;
  is-active)
    w=$(cat "$STUB_STATE/$u.act" 2>/dev/null)
    [ -n "$w" ] || w=inactive
    echo "$w"
    case "$w" in active|activating|reloading) exit 0 ;; *) exit 3 ;; esac ;;
  disable)
    [ -n "${STUB_FAIL_DISABLE:-}" ] && { echo "Failed to disable unit: Access denied (stub)" >&2; exit 1; }
    [ -n "${STUB_DISABLE_NOOP:-}" ] && exit 0
    echo disabled > "$STUB_STATE/$u.en"
    [ -n "${STUB_STOP_FAILS:-}" ] && { echo "Job for $u canceled (stub)" >&2; exit 1; }
    [ -n "$now" ] && echo inactive > "$STUB_STATE/$u.act"
    [ -n "${STUB_EXIT_AFTER:-}" ] && { echo "Failed to reload daemon (stub)" >&2; exit 1; }
    exit 0 ;;
  enable)
    echo enabled > "$STUB_STATE/$u.en"
    [ -n "$now" ] && echo active > "$STUB_STATE/$u.act"
    exit 0 ;;
  daemon-reload) exit 0 ;;
  *) echo "stub systemctl: unexpected $cmd $*" >&2; exit 99 ;;
esac
"#;

/// The stub curl: the estate registry and the jobs API's open
/// ops-request read, each from a file (absent file = a dark system of
/// record, exit 7). node-roles.sh asks with `-w '\n%{http_code}'` and
/// gets the code appended, as real curl prints it. Each read is logged
/// as `<url> <x-boss-user>`, so a test can say who signed it.
const CURL_STUB: &str = r#"#!/bin/sh
url=""; code=""; prev=""; user=""
for a in "$@"; do
  case "$a" in http://*|https://*) url="$a" ;; esac
  [ "$prev" = "-w" ] && code=1
  case "$prev:$a" in "-H:x-boss-user: "*) user="${a#x-boss-user: }" ;; esac
  prev="$a"
done
echo "$url $user" >> "$STUB_CURL_LOG"
case "$url" in
  */api/estate/nodes) f="$STUB_NODES" ;;
  */api/jobs\?*) f="$STUB_QUEUE" ;;
  *) echo "stub curl: unexpected $url" >&2; exit 6 ;;
esac
[ -r "$f" ] || { echo "curl: (7) Failed to connect (stub)" >&2; exit 7; }
cat "$f"
[ -n "$code" ] && printf '\n200'
exit 0
"#;

struct Host {
    root: PathBuf,
    bin: PathBuf,
    state: PathBuf,
    etc: PathBuf,
    log: PathBuf,
    nodes: PathBuf,
    queue: PathBuf,
    marker: PathBuf,
    cache: PathBuf,
    summary: PathBuf,
}

/// A registry answer: `(id, roles)` per node.
fn nodes_json(nodes: &[(&str, &[&str])]) -> String {
    let rows: Vec<String> = nodes
        .iter()
        .map(|(id, roles)| {
            let r: Vec<String> = roles.iter().map(|r| format!("\"{r}\"")).collect();
            format!(
                r#"{{"id":"{id}","roles":[{}],"retired":false}}"#,
                r.join(",")
            )
        })
        .collect();
    format!(r#"{{"data":[{}]}}"#, rows.join(","))
}

/// The open ops-request read: `(id, host, verb)` rows and the server's
/// `total`.
fn queue_json(rows: &[(&str, &str, &str)], total: usize) -> String {
    let r: Vec<String> = rows
        .iter()
        .map(|(id, host, verb)| {
            format!(
                r#"{{"id":"{id}","status":"open","metadata":{{"host":"{host}","verb":"{verb}"}}}}"#
            )
        })
        .collect();
    format!(r#"{{"data":[{}],"total":{total}}}"#, r.join(","))
}

impl Host {
    /// The forge after the estate dropped its ops-runner role: boss-gcp
    /// still runs a door, the runner's timer is on, and the pass running
    /// the verb is the service, `activating`.
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("retire-ops-runner-{name}"));
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
            nodes: root.join("nodes.json"),
            queue: root.join("queue.json"),
            marker: root.join("lib/ops-runner.retired"),
            cache: root.join("lib/node-roles.forge"),
            summary: root.join("summary.json"),
            root,
            bin,
            state,
            etc,
        };
        h.set_nodes(&[
            ("boss-gcp", &["cluster-operator", "ops-runner"]),
            ("forge", &["cluster-operator"]),
            ("w-1", &[]),
        ]);
        h.set_queue(&[(REQUEST, "forge", "retire-ops-runner")], 1);
        h.unit(TIMER, "enabled", "active");
        h.unit(SERVICE, "static", "activating");
        // The files a converge installed while the role was declared.
        write_file(&h.etc.join(TIMER), "[Timer]\n");
        write_file(&h.etc.join(SERVICE), "[Service]\n");
        std::fs::create_dir_all(h.etc.join("boss-ops-runner.service.d")).unwrap();
        write_file(
            &h.etc.join("boss-ops-runner.service.d/forge.conf"),
            "[Service]\nEnvironment=HOST_ID=forge\n",
        );
        h
    }

    fn set_nodes(&self, nodes: &[(&str, &[&str])]) {
        write_file(&self.nodes, &nodes_json(nodes));
    }

    fn set_queue(&self, rows: &[(&str, &str, &str)], total: usize) {
        write_file(&self.queue, &queue_json(rows, total));
    }

    fn unit(&self, unit: &str, en: &str, act: &str) {
        write_file(&self.state.join(format!("{unit}.en")), &format!("{en}\n"));
        write_file(&self.state.join(format!("{unit}.act")), &format!("{act}\n"));
    }

    fn word(&self, unit: &str, which: &str) -> String {
        std::fs::read_to_string(self.state.join(format!("{unit}.{which}")))
            .unwrap_or_default()
            .trim()
            .to_string()
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
            .filter(|c| !c.starts_with("is-enabled") && !c.starts_with("is-active"))
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
            .env("STUB_CURL_LOG", self.root.join("curl.log"))
            .env("STUB_NODES", &self.nodes)
            .env("STUB_QUEUE", &self.queue)
            // The address file is absent; the address is in the
            // environment, as the runner's EnvironmentFile= puts it.
            .env("BOSS_SOR_ENV", self.root.join("absent-sor.env"))
            .env("BOSS_JOBS_URL", "http://sor.test")
            .env("BOSS_NODE_ROLES_CACHE", &self.cache)
            .env("BOSS_OPS_RUNNER_RETIRED", &self.marker)
            .env("INSTALL_SYSTEMCTL", self.bin.join("systemctl"))
            .env("INSTALL_ETC", &self.etc);
        cmd
    }

    /// The verb, as the runner runs it: HOST_ID from its drop-in,
    /// OPS_REQUEST_ID from its exec.
    fn verb(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = self.base(VERB);
        cmd.args(args)
            .env("HOST_ID", "forge")
            .env("OPS_REQUEST_ID", REQUEST);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("retire-ops-runner.sh runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The plan verb, then the hash it printed on stderr.
    fn plan(&self) -> (String, String) {
        let (rc, plan, err) = self.verb(&["--dry-run"], &[]);
        assert_eq!(rc, 0, "the dry run passes every bound:\n{plan}\n{err}");
        let hash = err
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 on stderr: {err}"))
            .to_string();
        (plan, hash)
    }

    /// The host's converge calling its installer with the roles it read.
    fn converge(&self, roles: &str) -> (i32, String) {
        self.converge_from(roles, "registry")
    }

    /// The same, naming where the roles came from — `registry` (a live
    /// read), `cache` or `none` (node-roles.sh's dark-registry fallbacks).
    fn converge_from(&self, roles: &str, source: &str) -> (i32, String) {
        let mut cmd = self.base(INSTALLER);
        cmd.arg("forge")
            .env("BOSS_NODE_ROLES", roles)
            .env("BOSS_NODE_ROLES_SOURCE", source)
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary);
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

    fn files_present(&self) -> Vec<&'static str> {
        [SERVICE, TIMER, "boss-ops-runner.service.d"]
            .into_iter()
            .filter(|f| self.etc.join(f).exists())
            .collect()
    }

    fn marker(&self) -> Option<String> {
        std::fs::read_to_string(&self.marker).ok()
    }
}

fn sha256(text: &str) -> String {
    let p = scratch_dir("retire-ops-runner-sha").join("plan");
    std::fs::write(&p, text).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

fn effect_regex() -> String {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/retire-ops-runner.json"))
            .unwrap(),
    )
    .unwrap();
    v["effect"].as_str().unwrap().to_string()
}

/// Does any line of `text` match `re` in the runner's own engine?
fn jq_any_line(re: &str, text: &str) -> bool {
    let out = Command::new("jq")
        .args([
            "-nR",
            "--arg",
            "re",
            re,
            "[inputs | select(test($re))] | length > 0",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(text.as_bytes())?;
            c.wait_with_output()
        })
        .expect("jq runs");
    String::from_utf8_lossy(&out.stdout).trim() == "true"
}

// ---------------------------------------------------------------------------
// The verb.
// ---------------------------------------------------------------------------

#[test]
fn the_dry_run_prints_every_bound_and_the_plan_and_stops_nothing() {
    if !tools() {
        return;
    }
    let h = Host::new("dry");
    let (plan, hash) = h.plan();
    for needle in [
        "roles forge declares, read live from the estate registry: cluster-operator",
        "other hosts declaring ops-runner, among the hosts this verb serves (forge boss-gcp): boss-gcp",
        "declared, not proven alive",
        "open ops-requests for forge besides this one: none (1 open, this request the only one)",
        "boss-ops-runner.timer now: is-enabled answers enabled, is-active answers active",
        "would stop+disable boss-ops-runner.timer (systemctl disable --now boss-ops-runner.timer)",
        "would not touch boss-ops-runner.service",
        REQUEST,
    ] {
        assert!(plan.contains(needle), "the plan names `{needle}`:\n{plan}");
    }
    assert_eq!(
        hash,
        sha256(&plan),
        "plan-sha256 is the hash of the stdout bytes"
    );
    assert!(h.acts().is_empty(), "a dry run acted: {:?}", h.calls());
    assert!(h.marker().is_none(), "a dry run wrote the marker");
    assert_eq!(h.word(TIMER, "en"), "enabled");
    // The live read refreshed the converge's roles cache, so a dark
    // registry at its next tick cannot reinstall from a stale one.
    assert_eq!(
        std::fs::read_to_string(&h.cache).unwrap_or_default().trim(),
        "cluster-operator"
    );
    // Every read is signed: a refused read answers a smaller world
    // (every_estate_read_is_signed.rs). The registry reads sign as named
    // readers; the queue read as the runner, which is who reads queues.
    let reads = std::fs::read_to_string(h.root.join("curl.log")).unwrap_or_default();
    let nodes_reads: Vec<&str> = reads
        .lines()
        .filter(|l| l.starts_with("http://sor.test/api/estate/nodes "))
        .collect();
    assert_eq!(
        nodes_reads.len(),
        2,
        "node-roles.sh's read and the verb's own: {reads}"
    );
    for l in &nodes_reads {
        assert!(
            l.contains(r#""id":"automation:retire-ops-runner","role":"audit-readonly""#),
            "the registry read signs as a named reader: {l}"
        );
    }
    assert!(
        reads.lines().any(|l| l
            .starts_with("http://sor.test/api/jobs?kind=ops-request&status=open&metadata=")
            && l.contains(r#""id":"automation:ops-runner","role":"platform-admin""#)),
        "the queue read is narrowed to the host and signed as the runner: {reads}"
    );
    // Rendered twice, the bytes are the same — the passkey signs them
    // once and the write re-renders.
    assert_eq!(h.plan().1, hash, "the plan carries no clock");
}

/// THE STUB CONFIRMATION, first: stopping the timer does not stop the
/// service run the timer started, and the verb never asks it to.
#[test]
fn the_real_run_stops_only_the_timer_and_the_pass_running_it_survives_to_report() {
    if !tools() {
        return;
    }
    let h = Host::new("real");
    let (_, hash) = h.plan();
    let (rc, out, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 0, "{out}\n{err}");

    assert_eq!(
        h.acts(),
        vec![format!("disable --now -- {TIMER}")],
        "the ONE acting call names the timer, and nothing else acts"
    );
    for c in h.calls() {
        assert!(
            !c.contains(SERVICE) || c == format!("is-active -- {SERVICE}"),
            "the service is only ever read, never acted on: {c}"
        );
    }
    assert_eq!(h.word(TIMER, "en"), "disabled");
    assert_eq!(h.word(TIMER, "act"), "inactive");
    assert_eq!(
        h.word(SERVICE, "act"),
        "activating",
        "the pass the timer started is still running after the timer stopped"
    );
    assert!(
        out.contains(&format!(
            "read only: systemctl is-active {SERVICE} answers activating"
        )),
        "the record shows the pass outlived the stop: {out}"
    );
    let ok = err
        .lines()
        .find(|l| l.starts_with("retire-ops-runner: OK — "))
        .unwrap_or_else(|| panic!("no OK line:\n{out}\n{err}"));
    assert!(
        jq_any_line(&effect_regex(), ok),
        "the verb file's effect matches the OK line: {ok}"
    );
    assert!(
        !jq_any_line(&effect_regex(), &out),
        "the effect matches nothing printed on stdout (the plan, the stop, the reads): {out}"
    );
    let marker = h.marker().expect("the marker is written");
    assert!(
        marker.starts_with(&format!("ops-request={REQUEST} host=forge at=20")),
        "the marker names the request, the host and the time: {marker}"
    );
}

#[test]
fn a_second_run_of_an_applied_plan_is_refused() {
    if !tools() {
        return;
    }
    let h = Host::new("twice");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 0, "{err}");
    let before = h.acts();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("already off"), "{err}");
    assert_eq!(h.acts(), before, "the second run acted");
}

#[test]
fn a_plan_that_no_longer_holds_stops_nothing() {
    if !tools() {
        return;
    }
    let h = Host::new("drift");
    let (_, hash) = h.plan();
    // The forge's declaration moved since the plan was signed: the
    // reading changed, so the signed bytes no longer hold. (A Talos node
    // declaring ops-runner would NOT move them — it is no door, F2.)
    h.set_nodes(&[
        ("boss-gcp", &["cluster-operator", "ops-runner"]),
        ("forge", &["cluster-operator", "off-cluster-observer"]),
        ("w-1", &["ops-runner"]),
    ]);
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("not the approved"), "{err}");
    assert!(h.acts().is_empty(), "{:?}", h.calls());
    assert!(h.marker().is_none());
}

/// Each bound refuses by name, exit 2, nothing stopped, no marker.
#[test]
fn every_bound_refuses_by_name_and_stops_nothing() {
    if !tools() {
        return;
    }
    type Setup = fn(&Host);
    let cases: &[(&str, Setup, &str)] = &[
        (
            "role-still-declared",
            |h| {
                h.set_nodes(&[
                    ("boss-gcp", &["ops-runner"]),
                    ("forge", &["cluster-operator", "ops-runner"]),
                ])
            },
            "still gets a runner by its declaration",
        ),
        (
            "no-roles-at-all",
            |h| h.set_nodes(&[("boss-gcp", &["ops-runner"]), ("forge", &[])]),
            "a host that declares no roles installs one",
        ),
        (
            "dark-registry",
            |h| std::fs::remove_file(&h.nodes).unwrap(),
            "did not come from a live read",
        ),
        (
            "the-last-door",
            |h| {
                h.set_nodes(&[
                    ("boss-gcp", &["cluster-operator"]),
                    ("forge", &["cluster-operator"]),
                ])
            },
            "runs the estate's last door",
        ),
        // Review 9c02f3b0, F2: a Talos node that declared ops-runner by
        // mistake has no installer and no runner, so it is not a door.
        (
            "a-node-with-no-installer-is-no-door",
            |h| {
                h.set_nodes(&[
                    ("boss-gcp", &["cluster-operator"]),
                    ("forge", &["cluster-operator"]),
                    ("w-1", &["ops-runner"]),
                ])
            },
            "no other host this verb serves (forge boss-gcp) declares ops-runner",
        ),
        (
            "another-request-waits",
            |h| {
                h.set_queue(
                    &[
                        (REQUEST, "forge", "retire-ops-runner"),
                        (OTHER_REQUEST, "forge", "disk-report"),
                    ],
                    2,
                )
            },
            "other ops-requests are open for forge: 0be00000 (disk-report)",
        ),
        (
            "a-read-that-cannot-see-this-request",
            |h| h.set_queue(&[], 0),
            "does not show this very request",
        ),
        (
            "one-page-is-not-the-queue",
            |h| h.set_queue(&[(REQUEST, "forge", "retire-ops-runner")], 2),
            "holds 1 of 2 open requests",
        ),
        (
            "a-read-not-narrowed-to-the-host",
            |h| {
                h.set_queue(
                    &[
                        (REQUEST, "forge", "retire-ops-runner"),
                        (OTHER_REQUEST, "boss-gcp", "df"),
                    ],
                    2,
                )
            },
            "was not narrowed to forge",
        ),
        (
            "a-dark-queue",
            |h| std::fs::remove_file(&h.queue).unwrap(),
            "did not answer the queue read",
        ),
        (
            "the-timer-already-off",
            |h| h.unit(TIMER, "disabled", "inactive"),
            "is already off on forge",
        ),
    ];
    for (name, setup, needle) in cases {
        let h = Host::new(name);
        setup(&h);
        let (rc, out, err) = h.verb(&["--dry-run"], &[]);
        assert_eq!(rc, 2, "{name}: refused, exit 2:\n{out}\n{err}");
        assert!(
            err.contains("retire-ops-runner: REFUSED — ") && err.contains(needle),
            "{name}: the refusal names `{needle}`:\n{err}"
        );
        assert!(err.contains("Nothing was stopped"), "{name}: {err}");
        assert!(out.is_empty(), "{name}: a refusal renders no plan: {out}");
        assert!(h.acts().is_empty(), "{name}: {:?}", h.calls());
        assert!(h.marker().is_none(), "{name}");
    }
}

#[test]
fn the_verb_runs_only_inside_a_runner_that_names_its_host_and_request() {
    if !tools() {
        return;
    }
    let h = Host::new("hand");
    for (k, v, needle) in [
        ("OPS_REQUEST_ID", "", "OPS_REQUEST_ID is ''"),
        ("HOST_ID", "", "HOST_ID is ''"),
        ("HOST_ID", "Forge;x", "not an estate node id"),
    ] {
        let (rc, _, err) = h.verb(&["--dry-run"], &[(k, v)]);
        assert_eq!(rc, 2, "{k}={v}: {err}");
        assert!(err.contains(needle), "{k}={v}: {err}");
    }
    for args in [
        &[][..],
        &["--for-real"][..],
        &["--for-real", "abc"][..],
        &["--dry-run", "--for-real"][..],
        &["--now"][..],
    ] {
        let (rc, _, err) = h.verb(args, &[]);
        assert_eq!(rc, 2, "{args:?}: {err}");
        assert!(err.contains("usage:"), "{args:?}: {err}");
    }
    assert!(h.acts().is_empty(), "{:?}", h.calls());
}

#[test]
fn a_disable_that_does_not_take_fails_by_name_and_takes_its_marker_back() {
    if !tools() {
        return;
    }
    let h = Host::new("noop");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[("STUB_DISABLE_NOOP", "1")]);
    assert_eq!(rc, 1, "{err}");
    assert!(
        err.contains("FAILED — disable --now returned success and boss-ops-runner.timer still answers is-enabled enabled"),
        "{err}"
    );
    assert!(
        !jq_any_line(&effect_regex(), &err),
        "no effect claimed: {err}"
    );
    assert!(
        h.marker().is_none(),
        "a failed retire leaves no marker to credit a later stop to this request"
    );

    let h = Host::new("refused-disable");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[("STUB_FAIL_DISABLE", "1")]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("Access denied (stub)"), "{err}");
    assert!(h.marker().is_none());
}

/// ADVERSARIAL REVIEW 9c02f3b0, F3. A `disable --now` that fails PART-way
/// still did something, and the record must credit what took to this
/// request — never to a hand. The marker follows what the timer reads,
/// not the exit: disabled (whatever is-active says) keeps it, so the
/// converge says RETIRED by this request beside this run's FAILED.
#[test]
fn a_disable_that_fails_part_way_keeps_the_marker_for_what_took() {
    if !tools() {
        return;
    }
    // The disable took and the stop did not: the door is still open, the
    // run FAILS, and the marker stands for the disable it did make.
    let h = Host::new("stop-fails");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[("STUB_STOP_FAILS", "1")]);
    assert_eq!(rc, 1, "{err}");
    assert!(
        err.contains("answers is-enabled disabled but is-active active, so the door is still open. The marker stands"),
        "{err}"
    );
    assert!(
        h.marker().is_some(),
        "the disable is this request's act: {err}"
    );
    // Whoever stops it later, the converge credits the request, not a hand.
    h.unit(TIMER, "disabled", "inactive");
    h.unit(SERVICE, "static", "inactive");
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("RETIRED (by ops-request={REQUEST}")),
        "{out}"
    );
    assert!(!out.contains("DISABLED BY HAND"), "{out}");

    // Both took and systemctl still exited non-zero: FAILED on the run,
    // and the converge reads RETIRED by this request.
    let h = Host::new("exit-after");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[("STUB_EXIT_AFTER", "1")]);
    assert_eq!(rc, 1, "{err}");
    assert!(
        err.contains("FAILED — disable --now exited 1, yet boss-ops-runner.timer reads off"),
        "{err}"
    );
    assert!(
        !jq_any_line(&effect_regex(), &err),
        "no effect claimed: {err}"
    );
    assert!(h.marker().is_some(), "{err}");
    h.unit(SERVICE, "static", "inactive");
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("RETIRED (by ops-request={REQUEST}")),
        "{out}"
    );
    assert!(h.files_present().is_empty(), "{out}");
}

// ---------------------------------------------------------------------------
// The converge: the independent witness.
// ---------------------------------------------------------------------------

#[test]
fn a_runner_still_on_is_still_installed_and_nothing_is_removed() {
    if !tools() {
        return;
    }
    let h = Host::new("still");
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("STILL INSTALLED"), "{out}");
    assert!(
        out.contains("boss-ops-runner.timer is-enabled enabled, is-active active"),
        "{out}"
    );
    assert!(
        out.contains("retire-ops-runner"),
        "the report names the door now: {out}"
    );
    assert_eq!(h.files_present().len(), 3, "{out}");
    assert!(h.acts().is_empty(), "{:?}", h.calls());
    assert!(h.summary("anomalies").contains("STILL RUNS"), "{out}");
}

/// The whole flow on one host: the verb retires the runner, its pass
/// ends, and the next converge says RETIRED, removes the files and
/// says REMOVED; the tick after that records the retirement as a field.
#[test]
fn the_converge_after_a_retire_says_retired_and_removes_the_files() {
    if !tools() {
        return;
    }
    let h = Host::new("retired");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 0, "{err}");

    // While the retiring pass is still running, the converge waits.
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("RETIRED"), "{out}");
    assert!(
        out.contains("a pass still running is not removed under itself"),
        "{out}"
    );
    assert_eq!(h.files_present().len(), 3, "{out}");

    // The pass has reported and exited.
    h.unit(SERVICE, "static", "inactive");
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!(
            "is RETIRED (by ops-request={REQUEST} host=forge at="
        )),
        "RETIRED names the request that did it: {out}"
    );
    assert!(out.contains("REMOVED"), "{out}");
    assert!(h.files_present().is_empty(), "{out}");
    assert!(
        h.calls().iter().any(|c| c == "daemon-reload"),
        "{:?}",
        h.calls()
    );
    let field = h.summary("ops_runner");
    assert!(
        field.contains("RETIRED") && field.contains("REMOVED"),
        "{field}"
    );
    assert!(
        !h.summary("anomalies").contains("DISABLED BY HAND"),
        "a retirement on the record is not an anomaly: {}",
        h.summary("anomalies")
    );

    let _ = std::fs::remove_file(&h.summary);
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(
        h.summary("ops_runner").contains("retired (ops-request="),
        "the next tick keeps saying who retired it: {out}"
    );
}

/// ADVERSARIAL REVIEW 9c02f3b0, F1. A dark registry with no cache hands
/// the converge the `registry-unread` sentinel (source `none`), which
/// names no role — right for "never widen what a host runs", and wrong
/// as a reason to REMOVE anything: the host may well still declare
/// ops-runner, its timer off only for maintenance. Removal is held to the
/// verb's own bar, a LIVE read; on any other source the files stay and
/// the packet says the declaration was not read, never that the host
/// "does not declare" a role nobody read.
#[test]
fn a_declaration_not_read_live_never_removes_the_runner() {
    if !tools() {
        return;
    }
    for (name, roles, source) in [
        ("dark-no-cache", "registry-unread", "none"),
        ("dark-cached", "cluster-operator", "cache"),
    ] {
        let h = Host::new(name);
        h.unit(TIMER, "disabled", "inactive");
        h.unit(SERVICE, "static", "inactive");
        let (rc, out) = h.converge_from(roles, source);
        assert_eq!(rc, 0, "{name}: {out}");
        assert_eq!(
            h.files_present().len(),
            3,
            "{name}: a declaration nobody read live removed the runner's files: {out}"
        );
        assert!(!out.contains("REMOVED"), "{name}: {out}");
        assert!(
            !out.contains("does not declare"),
            "{name}: the packet says the host does not declare a role nobody read: {out}"
        );
        assert!(
            out.contains("not read live"),
            "{name}: the run says the declaration was not read this tick: {out}"
        );
        assert!(
            !h.calls().iter().any(|c| c == "daemon-reload"),
            "{name}: {:?}",
            h.calls()
        );
        assert!(
            h.summary("ops_runner")
                .contains(&format!("source: {source}")),
            "{name}: {}",
            h.summary("ops_runner")
        );
    }
}

#[test]
fn a_timer_turned_off_with_no_record_is_disabled_by_hand_and_loud() {
    if !tools() {
        return;
    }
    for (name, marker) in [
        ("no-marker", None),
        (
            "another-hosts-marker",
            Some(format!(
                "ops-request={REQUEST} host=boss-gcp at=2026-10-01T12:00:00Z\n"
            )),
        ),
        ("not-a-marker", Some("retired, trust me\n".to_string())),
    ] {
        let h = Host::new(name);
        h.unit(TIMER, "disabled", "inactive");
        h.unit(SERVICE, "static", "inactive");
        if let Some(m) = &marker {
            std::fs::create_dir_all(h.marker.parent().unwrap()).unwrap();
            write_file(&h.marker, m);
        }
        let (rc, out) = h.converge("cluster-operator");
        assert_eq!(rc, 0, "{name}: {out}");
        assert!(out.contains("DISABLED BY HAND"), "{name}: {out}");
        assert!(out.contains("REMOVED"), "{name}: {out}");
        assert!(h.files_present().is_empty(), "{name}: {out}");
        assert!(
            h.summary("anomalies").contains("DISABLED BY HAND"),
            "{name}: nothing on the record did it, so the packet says so as an anomaly: {}",
            h.summary("anomalies")
        );
    }
}

#[test]
fn declaring_the_role_again_reinstalls_the_runner_and_drops_the_marker() {
    if !tools() {
        return;
    }
    let h = Host::new("undo");
    let (_, hash) = h.plan();
    let (rc, _, err) = h.verb(&["--for-real", &hash], &[]);
    assert_eq!(rc, 0, "{err}");
    h.unit(SERVICE, "static", "inactive");
    let (rc, out) = h.converge("cluster-operator");
    assert_eq!(rc, 0, "{out}");
    assert!(h.files_present().is_empty(), "{out}");

    let (rc, out) = h.converge("cluster-operator,ops-runner");
    assert_eq!(rc, 0, "{out}");
    assert!(
        h.etc.join(TIMER).is_file() && h.etc.join(SERVICE).is_file(),
        "{out}"
    );
    assert_eq!(h.word(TIMER, "en"), "enabled", "{:?}", h.calls());
    assert_eq!(h.word(TIMER, "act"), "active");
    assert!(
        h.marker().is_none(),
        "the undone retirement's marker is gone: {out}"
    );
    assert!(out.contains("declares ops-runner again"), "{out}");
}

// ---------------------------------------------------------------------------
// The facts that live twice.
// ---------------------------------------------------------------------------

/// The marker's path is spelled in the verb that writes it and the
/// converge that reads it. Collapsing it would need a third file for one
/// line, so it is pinned (CLAUDE.md §9a): the same assignment, byte for
/// byte, in both.
#[test]
fn the_verb_and_the_converge_name_one_marker() {
    let line = |script: &str| -> String {
        let text = std::fs::read_to_string(repo_root().join(script)).unwrap();
        let found: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("RETIRED_MARKER="))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{script} assigns RETIRED_MARKER once: {found:?}"
        );
        found[0].to_string()
    };
    assert_eq!(line(VERB), line(INSTALLER));
}

/// The verb is reachable on exactly the two hosts that run runners, its
/// write is passkey-gated on its plan, and the one unit is a literal in
/// the script — never in the argv.
#[test]
fn the_verb_files_say_what_the_design_decided() {
    let read = |n: &str| -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(repo_root().join(format!("infra/ops/verbs/{n}.json")))
                .unwrap(),
        )
        .unwrap()
    };
    let w = read("retire-ops-runner");
    let p = read("plan-retire-ops-runner");
    assert_eq!(w["hosts"], serde_json::json!(["forge", "boss-gcp"]));
    assert_eq!(p["hosts"], w["hosts"]);
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-retire-ops-runner");
    assert_eq!(w["approvers"], serde_json::json!(["emp-david"]));
    assert!(w["about"].as_str().unwrap().contains("MUTATING"));
    assert!(!p["about"].as_str().unwrap().contains("MUTATING"));
    for v in [&w, &p] {
        let argv = v["argv"].to_string();
        assert!(
            !argv.contains("boss-ops-runner"),
            "a unit in the argv: {argv}"
        );
    }
    let lint = std::fs::read_to_string(
        repo_root().join("infra/lint/a-verb-declares-the-hosts-it-serves.sh"),
    )
    .unwrap();
    assert!(
        lint.contains(r#""retire-ops-runner": "David 2026-10-01, design a79a8067""#),
        "the hosts lint admits the verb on boss-gcp by name, with its authorization"
    );
}
