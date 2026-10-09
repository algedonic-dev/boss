//! The read-only ops verb `check-node-trim`
//! (`infra/forge/check-node-trim.sh`): the forge reading, from OUTSIDE its
//! namespace, whether the build node's daily trim ran and what it told the
//! drives (backlog 33e62de8; review 8b1981da of the trim car, F1 to F3).
//!
//! WHY. CronJob `boss-node-trim` is one attempt a day in the one namespace
//! labelled privileged, given no machine token and no door to the jobs
//! API. A failed image pull, a down node, a pod refused at admission and a
//! trim that exits RED each left a failed or absent Job nothing read. The
//! namespace's admission policy refuses a Role, a RoleBinding and any
//! second workload, so the reader is a forge verb over the admin
//! kubeconfig; `check-node-trim-daily` files it and
//! `watch-check-node-trim-daily` judges its last line.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small API server behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: the namespace list and the CronJob, Job, Pod and
//! Event lists of `boss-node-maintenance`, and a pod log. Every door call
//! lands in `argv`, so the check can be shown to have only read. The
//! watch's own `verdict_pattern` and `when` are read off its rule file and
//! run over the script's REAL output. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/check-node-trim.sh";
const WATCH: &str = "watch-check-node-trim-daily.toml";
const NS: &str = "boss-node-maintenance";
const CJ: &str = "boss-node-trim";
/// The instant every case reads the cluster at.
const NOW: &str = "2026-10-08T15:00:00Z";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `namespaces.json`, `cronjobs.json`, `jobs.json`, `pods.json` and
/// `events.json` item arrays, and `log.txt` (a pod's log; absent, `logs`
/// is refused). STUB_UNREACHABLE=1 answers every call as a dark API
/// server does; STUB_FAIL=<resource> fails that one list; STUB_HALF=
/// <resource> prints that list whole and THEN exits 1; STUB_EMPTY=
/// <resource> answers it with exit 0 and no bytes at all. A namespaced
/// read aimed anywhere but boss-node-maintenance is an unexpected call.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
if [ -n "${STUB_UNREACHABLE:-}" ]; then
    echo "Unable to connect to the server: dial tcp: i/o timeout" >&2
    exit 1
fi
if [ "$1" = get ] && [ "$2" = "${STUB_FAIL:-}" ]; then
    echo "Error from server (ServiceUnavailable): the server is currently unable to handle the request" >&2
    exit 1
fi
[ "$1" = get ] && [ "$2" = "${STUB_EMPTY:-}" ] && exit 0
half=0
[ "$1" = get ] && [ "$2" = "${STUB_HALF:-}" ] && half=1
list() { printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/$1")"; exit "$half"; }
case "$1 $2" in
"get namespaces") list namespaces.json ;;
esac
case "$*" in *"-n boss-node-maintenance"*) ;; *) echo "stub: a read outside the namespace: $*" >&2; exit 2 ;; esac
case "$1 $2" in
"get cronjobs.batch") list cronjobs.json ;;
"get jobs.batch") list jobs.json ;;
"get pods") list pods.json ;;
"get events") list events.json ;;
esac
if [ "$1" = logs ]; then
    [ -f "$S/log.txt" ] && { cat "$S/log.txt"; exit 0; }
    echo "Error from server (NotFound): pods \"$2\" not found" >&2
    exit 1
fi
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// What the trim's own script prints on a good day (the manifest's `say`
/// lines): each NVMe's counters before and after, fstrim's byte lines
/// between them, and the closing word. nvme0n1 moved; nvme1n1 did not.
const TRIM_SAID: &str = "node-trim: before /sys/block/nvme0n1: discards=1200 sectors=880000\n\
node-trim: before /sys/block/nvme1n1: discards=40 sectors=5000\n\
node-trim: /trim/gate: 1.2 TiB (1288490188800 bytes) trimmed\n\
node-trim: /trim/ephemeral: 300 GiB (322122547200 bytes) trimmed\n\
node-trim: after /sys/block/nvme0n1: discards=1900 sectors=990000\n\
node-trim: after /sys/block/nvme1n1: discards=40 sectors=5000\n\
node-trim: OK\n";

/// The same day with drives that were told nothing: both counters stand
/// where they stood, and fstrim's byte lines are IDENTICAL to a good
/// day's — review F2's measurement, two hand trims a day apart.
const TRIM_SAID_UNMOVED: &str = "node-trim: before /sys/block/nvme0n1: discards=1900 sectors=990000\n\
node-trim: before /sys/block/nvme1n1: discards=40 sectors=5000\n\
node-trim: /trim/gate: 1.2 TiB (1288490188800 bytes) trimmed\n\
node-trim: /trim/ephemeral: 300 GiB (322122547200 bytes) trimmed\n\
node-trim: after /sys/block/nvme0n1: discards=1900 sectors=990000\n\
node-trim: after /sys/block/nvme1n1: discards=40 sectors=5000\n\
node-trim: OK\n";

const TRIM_SAID_RED: &str = "node-trim: before /sys/block/nvme0n1: discards=1900 sectors=990000\n\
node-trim: RED fstrim /trim/gate failed: fstrim: /trim/gate: FITRIM ioctl failed: Operation not permitted\n\
node-trim: /trim/ephemeral: 300 GiB (322122547200 bytes) trimmed\n\
node-trim: after /sys/block/nvme0n1: discards=1950 sectors=991000\n\
node-trim: RED\n";

fn cronjob(created: &str, last_ok: Option<&str>, last_sched: Option<&str>) -> Value {
    let mut status = json!({});
    if let Some(t) = last_ok {
        status["lastSuccessfulTime"] = json!(t);
    }
    if let Some(t) = last_sched {
        status["lastScheduleTime"] = json!(t);
    }
    json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": CJ, "namespace": NS, "creationTimestamp": created},
        "spec": {"schedule": "40 12 * * *", "concurrencyPolicy": "Forbid", "suspend": false},
        "status": status
    })
}

/// A Job of the CronJob. `done`: its terminal condition as (type, reason,
/// message); None is a Job still without one.
fn job(name: &str, created: &str, done: Option<(&str, &str, &str)>) -> Value {
    let mut status = json!({"startTime": created});
    if let Some((kind, reason, message)) = done {
        status["conditions"] = json!([
            {"type": "SuccessCriteriaMet", "status": if kind == "Complete" { "True" } else { "False" }},
            {"type": kind, "status": "True", "reason": reason, "message": message}
        ]);
        if kind == "Complete" {
            status["completionTime"] = json!(created);
        }
    } else {
        status["active"] = json!(1);
    }
    json!({
        "apiVersion": "batch/v1", "kind": "Job",
        "metadata": {"name": name, "namespace": NS, "creationTimestamp": created,
                     "ownerReferences": [{"apiVersion": "batch/v1", "kind": "CronJob", "name": CJ, "controller": true}]},
        "status": status
    })
}

fn pod(job: &str, status: Value) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": format!("{job}-x7k2p"), "namespace": NS,
                     "creationTimestamp": "2026-10-08T12:40:02Z",
                     "labels": {"app": CJ, "job-name": job, "batch.kubernetes.io/job-name": job}},
        "status": status
    })
}

fn terminated(exit: i64, reason: &str, message: &str) -> Value {
    json!({
        "phase": if exit == 0 { "Succeeded" } else { "Failed" },
        "containerStatuses": [{"name": "trim", "state": {"terminated": {
            "exitCode": exit, "reason": reason, "message": message}}}]
    })
}

fn event(kind: &str, name: &str, reason: &str, message: &str) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Event", "type": "Warning", "reason": reason,
        "message": message, "count": 7,
        "involvedObject": {"kind": kind, "name": name, "namespace": NS}
    })
}

fn needs_tools() {
    // mawk knows no --version, so awk is asked to run a program.
    for (tool, arg) in [("jq", "--version"), ("awk", "BEGIN { exit 0 }")] {
        let ok = Command::new(tool)
            .arg(arg)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "node_trim_check_sh: no {tool} on this box — the gate image has it, and a test that \
             cannot run must fail, never pass by returning early"
        );
    }
}

/// `date -u -d <t> +%s`, the script's own reading of a time.
fn epoch(t: &str) -> i64 {
    let o = Command::new("date")
        .args(["-u", "-d", t, "+%s"])
        .output()
        .expect("date runs");
    String::from_utf8_lossy(&o.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("date -d {t}: {e}"))
}

struct Cluster {
    dir: PathBuf,
}

impl Cluster {
    /// The cluster as it stands the day after the trim car converges and
    /// the first 12:40 run went well: the namespace, the CronJob with a
    /// success 2 h 19 min before NOW, its one Complete Job, and that Job's
    /// pod carrying the trim's own lines as its termination message.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let c = Cluster { dir };
        c.state(
            "namespaces.json",
            &json!([
                {"metadata": {"name": "kube-system"}},
                {"metadata": {"name": "boss"}},
                {"metadata": {"name": NS}}
            ]),
        );
        c.state(
            "cronjobs.json",
            &json!([cronjob(
                "2026-10-07T22:30:00Z",
                Some("2026-10-08T12:41:00Z"),
                Some("2026-10-08T12:40:00Z")
            )]),
        );
        c.state(
            "jobs.json",
            &json!([job(
                "boss-node-trim-29330680",
                "2026-10-08T12:40:00Z",
                Some((
                    "Complete",
                    "CompletionsReached",
                    "Reached expected number of succeeded pods"
                ))
            )]),
        );
        c.state(
            "pods.json",
            &json!([pod(
                "boss-node-trim-29330680",
                terminated(0, "Completed", TRIM_SAID)
            )]),
        );
        c.state("events.json", &json!([]));
        // The tree declares the CronJob, as it does once the trim car has
        // landed.
        write_file(
            &c.dir.join("manifest.yaml"),
            "kind: CronJob\nmetadata:\n  name: boss-node-trim\n  namespace: boss-node-maintenance\n",
        );
        c
    }

    fn state(&self, file: &str, body: &Value) {
        write_file(&self.dir.join("state").join(file), &body.to_string());
    }

    fn log(&self, text: &str) {
        write_file(&self.dir.join("state/log.txt"), text);
    }

    fn undeclared(&self) {
        std::fs::remove_file(self.dir.join("manifest.yaml")).expect("remove the manifest");
    }

    fn run_at(&self, now: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_NODE_TRIM_NOW", epoch(now).to_string())
            .env("BOSS_NODE_TRIM_MANIFEST", self.dir.join("manifest.yaml"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().expect("the script runs");
        // EVERY run of EVERY case: nothing but reads crosses the door, and
        // each namespaced one is aimed at the one namespace (the stub
        // refuses any other).
        for call in self.calls() {
            assert!(
                call.starts_with("get ") || call.starts_with("logs "),
                "the check sent something other than a read through the admin kubeconfig: {call}"
            );
        }
        o
    }

    fn run(&self, env: &[(&str, &str)]) -> Output {
        self.run_at(NOW, &[], env)
    }

    /// Every door call, with the door's prefix cut off.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                l.split_once("--kubeconfig=/kc ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .collect()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The watch's own pattern and `when`, off its rule file — the regex
/// `ops.judge` reads the check's verdict with (one fact, two files,
/// compiled here and run over the script's real output). The groups of
/// the last matching line, and whether `when` holds over them, by the
/// handler's own reading: a group that parses as an integer is one.
/// `check_node_trim_rules.rs` evaluates the same `when` with boss-expr.
fn watch_judges(out: &str) -> Option<(BTreeMap<String, String>, bool)> {
    let path = boss_testing::dispatcher_rules_dir().join(WATCH);
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args = &doc["rule"][0]["do"][0]["args"];
    let lit = |k: &str| {
        let src = args[k].as_str().unwrap_or_else(|| panic!("{k}"));
        src.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or_else(|| panic!("not an expr string literal: {src}"))
            .to_string()
    };
    assert_eq!(
        lit("when"),
        "missing = 0 AND failed = 0 AND unmoved = 0 AND hours < 26",
        "the threshold moved; the cases below judge 26 hours"
    );
    let re = regex::Regex::new(&lit("verdict_pattern")).unwrap();
    let line = out.lines().map(str::trim).rfind(|l| re.is_match(l))?;
    let caps = re.captures(line)?;
    let groups: BTreeMap<String, String> = re
        .capture_names()
        .flatten()
        .filter_map(|n| Some((n.to_string(), caps.name(n)?.as_str().to_string())))
        .collect();
    let num = |k: &str| groups[k].parse::<i64>().expect("an integer group");
    let holds =
        num("missing") == 0 && num("failed") == 0 && num("unmoved") == 0 && num("hours") < 26;
    assert_eq!(
        groups["state"] == "missing",
        num("missing") == 1,
        "the word and the number say the same: {line}"
    );
    Some((groups, holds))
}

/// Exit 0, a verdict the watch reads, and its groups.
fn answered(o: &Output) -> (String, BTreeMap<String, String>, bool) {
    let out = text(o);
    assert!(o.status.success(), "the check answered: {out}");
    let (g, holds) =
        watch_judges(&out).unwrap_or_else(|| panic!("the watch reads a verdict: {out}"));
    assert!(
        String::from_utf8_lossy(&o.stdout)
            .trim_end()
            .lines()
            .last()
            .is_some_and(|l| l.starts_with("check-node-trim: READ — state ")),
        "the verdict is the LAST line: {out}"
    );
    (out, g, holds)
}

// ---------------------------------------------------------------- healthy

#[test]
fn a_healthy_trim_reads_its_last_success_and_the_counters_that_moved() {
    let c = Cluster::new("ntc-healthy");
    let (out, g, holds) = answered(&c.run(&[]));
    for want in [
        "check-node-trim: LAST SUCCESS — 2026-10-08T12:41:00Z, 2 hour(s) ago",
        "check-node-trim: NEWEST FINISHED JOB — boss-node-trim-29330680 succeeded",
        "check-node-trim: COUNTERS — nvme0n1: completed discards 1200 -> 1900 (+700), discarded sectors 880000 -> 990000 (+110000) [the pod's termination message]",
        "check-node-trim: COUNTERS — nvme1n1: completed discards 40 -> 40 (+0), discarded sectors 5000 -> 5000 (+0) — DID NOT MOVE",
        "check-node-trim: THE TRIM SAID — node-trim: OK",
        "check-node-trim: READ — state present, missing 0, hours 2, failed 0, unmoved 0, last_success 2026-10-08T12:41:00Z, job boss-node-trim-29330680, outcome succeeded, reason Complete, devices 2, moved 1",
    ] {
        assert!(out.contains(want), "the check says {want:?}:\n{out}");
    }
    assert_eq!(
        (
            g["hours"].as_str(),
            g["devices"].as_str(),
            g["moved"].as_str()
        ),
        ("2", "2", "1")
    );
    assert!(holds, "a trim two hours old that moved a drive is no alarm");
    // Review F2: the fstrim byte count is not evidence and is not relayed.
    assert!(
        !out.contains("bytes) trimmed") && !out.contains("1288490188800"),
        "the byte count is not what this reads: {out}"
    );
    // Five lists, and nothing but reads: the message carried the counters,
    // so the log was never asked for.
    let calls = c.calls();
    assert_eq!(
        calls
            .iter()
            .map(|l| l.split(' ').take(2).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>(),
        [
            "get namespaces",
            "get cronjobs.batch",
            "get jobs.batch",
            "get pods",
            "get events"
        ],
        "{calls:?}"
    );
}

#[test]
fn counters_absent_from_the_termination_message_are_read_from_the_log() {
    let c = Cluster::new("ntc-log");
    c.state(
        "pods.json",
        &json!([pod(
            "boss-node-trim-29330680",
            terminated(0, "Completed", "")
        )]),
    );
    c.log(TRIM_SAID);
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("COUNTERS — nvme0n1: completed discards 1200 -> 1900 (+700)")
            && out.contains("[the pod's log]"),
        "{out}"
    );
    assert_eq!(g["moved"], "1");
    assert!(holds);
    assert!(
        c.calls()
            .iter()
            .all(|l| l.starts_with("get ") || l.starts_with("logs ")),
        "the check only reads: {:?}",
        c.calls()
    );

    // No message and no log: a success with nothing to show for it.
    let c = Cluster::new("ntc-nolog");
    c.state(
        "pods.json",
        &json!([pod(
            "boss-node-trim-29330680",
            terminated(0, "Completed", "")
        )]),
    );
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(out.contains("COUNTERS — none read"), "{out}");
    assert_eq!((g["devices"].as_str(), g["unmoved"].as_str()), ("0", "1"));
    assert!(!holds, "a success with no counters is not a pass: {g:?}");
}

/// Review F2. A Job that exits 0 and prints the SAME byte count as a good
/// day, with no counter risen, is an alarm: the bytes are what the
/// filesystem offered, the counters are what the drive was told.
#[test]
fn a_success_whose_counters_did_not_move_is_no_evidence_and_alarms() {
    let c = Cluster::new("ntc-unmoved");
    c.state(
        "pods.json",
        &json!([pod(
            "boss-node-trim-29330680",
            terminated(0, "Completed", TRIM_SAID_UNMOVED)
        )]),
    );
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(out.contains("check-node-trim: NO EVIDENCE — "), "{out}");
    assert_eq!(
        (
            g["outcome"].as_str(),
            g["devices"].as_str(),
            g["moved"].as_str(),
            g["unmoved"].as_str()
        ),
        ("succeeded", "2", "0", "1")
    );
    assert!(!holds, "{g:?}");
}

// ------------------------------------------------------ never, and stale

#[test]
fn a_trim_that_never_succeeded_is_not_due_for_a_day_and_then_alarms() {
    let c = Cluster::new("ntc-never");
    c.state(
        "cronjobs.json",
        &json!([cronjob("2026-10-08T13:00:00Z", None, None)]),
    );
    c.state("jobs.json", &json!([]));
    c.state("pods.json", &json!([]));
    // Two hours after the CronJob was created, before its first 12:40.
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("check-node-trim: NEVER SUCCEEDED — ") && out.contains("existed 2 hour(s)"),
        "{out}"
    );
    assert_eq!(
        (
            g["last_success"].as_str(),
            g["hours"].as_str(),
            g["job"].as_str(),
            g["outcome"].as_str()
        ),
        ("never", "2", "none", "none")
    );
    assert!(holds, "the first run is not due yet");

    // Thirty hours on and still no success: a schedule that never fired.
    let (_, g, holds) = answered(&c.run_at("2026-10-09T19:00:00Z", &[], &[]));
    assert_eq!(
        (g["last_success"].as_str(), g["hours"].as_str()),
        ("never", "30")
    );
    assert!(!holds, "thirty hours without a first trim is the alarm");
}

#[test]
fn a_last_success_26_hours_old_is_what_the_watch_alarms_on() {
    let c = Cluster::new("ntc-stale");
    for (now, hours, alarm) in [
        ("2026-10-09T13:40:59Z", "24", false),
        ("2026-10-09T14:40:59Z", "25", false),
        // A day's run was missed: nothing failed, nothing ran.
        ("2026-10-09T14:41:00Z", "26", true),
        ("2026-10-10T15:00:00Z", "50", true),
    ] {
        let (out, g, holds) = answered(&c.run_at(now, &[], &[]));
        assert_eq!(g["hours"], hours, "{now}: {out}");
        assert_eq!(g["last_success"], "2026-10-08T12:41:00Z");
        assert_eq!(holds, !alarm, "{now} ({hours} h): {g:?}");
    }
}

// ----------------------------------------------------- the newest Job failed

/// The Job the day after: failed, with `done` as its condition. The
/// healthy Job of the day before is still kept beside it.
fn failed_day(c: &Cluster, reason: &str, message: &str, pods: Value, events: Value) {
    c.state(
        "cronjobs.json",
        &json!([cronjob(
            "2026-10-07T22:30:00Z",
            Some("2026-10-08T12:41:00Z"),
            Some("2026-10-09T12:40:00Z")
        )]),
    );
    c.state(
        "jobs.json",
        &json!([
            job(
                "boss-node-trim-29330680",
                "2026-10-08T12:40:00Z",
                Some((
                    "Complete",
                    "CompletionsReached",
                    "Reached expected number of succeeded pods"
                ))
            ),
            job(
                "boss-node-trim-29332120",
                "2026-10-09T12:40:00Z",
                Some(("Failed", reason, message))
            )
        ]),
    );
    c.state("pods.json", &pods);
    c.state("events.json", &events);
}

const AFTER_FAILURE: &str = "2026-10-09T13:30:00Z";

#[test]
fn a_trim_that_exits_red_names_its_exit_code_and_the_trims_own_words() {
    let c = Cluster::new("ntc-exit");
    failed_day(
        &c,
        "BackoffLimitExceeded",
        "Job has reached the specified backoff limit",
        json!([
            pod(
                "boss-node-trim-29330680",
                terminated(0, "Completed", TRIM_SAID)
            ),
            pod(
                "boss-node-trim-29332120",
                terminated(1, "Error", TRIM_SAID_RED)
            )
        ]),
        json!([]),
    );
    let (out, g, holds) = answered(&c.run_at(AFTER_FAILURE, &[], &[]));
    for want in [
        "check-node-trim: NEWEST FINISHED JOB — boss-node-trim-29332120 FAILED",
        "BackoffLimitExceeded — Job has reached the specified backoff limit",
        "check-node-trim: CAUSE — exit-1: pod boss-node-trim-29332120-x7k2p exited 1 (Error)",
        "check-node-trim: THE TRIM SAID — node-trim: RED fstrim /trim/gate failed: fstrim: /trim/gate: FITRIM ioctl failed: Operation not permitted",
        "check-node-trim: COUNTERS — nvme0n1: completed discards 1900 -> 1950 (+50)",
    ] {
        assert!(out.contains(want), "the check says {want:?}:\n{out}");
    }
    assert_eq!(
        (
            g["failed"].as_str(),
            g["outcome"].as_str(),
            g["reason"].as_str(),
            g["job"].as_str()
        ),
        ("1", "failed", "exit-1", "boss-node-trim-29332120")
    );
    // The success of the day before is 24 hours old: only the failure
    // makes this an alarm.
    assert_eq!(g["hours"], "24");
    assert!(!holds, "a failed newest Job is the alarm: {g:?}");
}

#[test]
fn a_pod_refused_at_admission_is_named_from_the_jobs_own_event() {
    let c = Cluster::new("ntc-admission");
    let said = "Error creating: pods \"boss-node-trim-29332120-\" is forbidden: ValidatingAdmissionPolicy 'boss-node-maintenance-bounds' with binding 'boss-node-maintenance-bounds' denied request: only the trim workload runs here";
    failed_day(
        &c,
        "DeadlineExceeded",
        "Job was active longer than specified deadline",
        json!([pod(
            "boss-node-trim-29330680",
            terminated(0, "Completed", TRIM_SAID)
        )]),
        json!([
            event("Job", "boss-node-trim-29332120", "FailedCreate", said),
            // Another Job's event, and a Normal one, are not this Job's cause.
            event("Job", "boss-node-trim-29330680", "FailedCreate", "an older Job's trouble"),
            {"type": "Normal", "reason": "SuccessfulCreate", "message": "Created pod",
             "involvedObject": {"kind": "Job", "name": "boss-node-trim-29332120"}}
        ]),
    );
    let (out, g, holds) = answered(&c.run_at(AFTER_FAILURE, &[], &[]));
    assert!(
        out.contains("check-node-trim: CAUSE — FailedCreate: no pod of the Job remains"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "check-node-trim: EVENT — Job/boss-node-trim-29332120 FailedCreate x7: {said}"
        )),
        "the admission policy's own words ride the packet: {out}"
    );
    assert!(
        !out.contains("an older Job's trouble") && !out.contains("Created pod"),
        "{out}"
    );
    assert_eq!(
        (g["failed"].as_str(), g["reason"].as_str()),
        ("1", "FailedCreate")
    );
    assert_eq!((g["devices"].as_str(), g["moved"].as_str()), ("0", "0"));
    assert!(!holds);
}

#[test]
fn an_image_that_would_not_pull_is_named_from_the_pod_or_its_events() {
    // The pod still stands (the check ran before the Job controller
    // removed it): its container's waiting reason is the cause.
    let c = Cluster::new("ntc-pull-pod");
    failed_day(
        &c,
        "DeadlineExceeded",
        "Job was active longer than specified deadline",
        json!([pod(
            "boss-node-trim-29332120",
            json!({"phase": "Pending", "containerStatuses": [{"name": "trim", "state": {"waiting": {
                "reason": "ImagePullBackOff",
                "message": "Back-off pulling image \"reg.test/david/alpine-k8s@sha256:cdeda0\""}}}]})
        )]),
        json!([]),
    );
    let (out, g, holds) = answered(&c.run_at(AFTER_FAILURE, &[], &[]));
    assert!(
        out.contains("check-node-trim: CAUSE — ImagePullBackOff: pod boss-node-trim-29332120-x7k2p is Pending, its container waiting — ImagePullBackOff: Back-off pulling image"),
        "{out}"
    );
    assert_eq!(
        (g["failed"].as_str(), g["reason"].as_str()),
        ("1", "ImagePullBackOff")
    );
    assert!(!holds);

    // The pod is gone and its events remain: the Job's own reason is the
    // token, and the pull failure is quoted beside it.
    let c = Cluster::new("ntc-pull-event");
    failed_day(
        &c,
        "DeadlineExceeded",
        "Job was active longer than specified deadline",
        json!([]),
        json!([event(
            "Pod",
            "boss-node-trim-29332120-x7k2p",
            "Failed",
            "Failed to pull image \"reg.test/david/alpine-k8s@sha256:cdeda0\": not found"
        )]),
    );
    let (out, g, holds) = answered(&c.run_at(AFTER_FAILURE, &[], &[]));
    assert!(
        out.contains("check-node-trim: EVENT — Pod/boss-node-trim-29332120-x7k2p Failed x7: Failed to pull image"),
        "{out}"
    );
    assert_eq!(
        (g["failed"].as_str(), g["reason"].as_str()),
        ("1", "DeadlineExceeded")
    );
    assert!(!holds);
}

#[test]
fn a_node_that_was_down_is_named_from_the_pod_that_was_never_scheduled() {
    let c = Cluster::new("ntc-node-down");
    failed_day(
        &c,
        "DeadlineExceeded",
        "Job was active longer than specified deadline",
        json!([pod(
            "boss-node-trim-29332120",
            json!({"phase": "Pending", "conditions": [{"type": "PodScheduled", "status": "False",
                "reason": "Unschedulable",
                "message": "0/4 nodes are available: 1 node(s) had untolerated taint {node.kubernetes.io/unreachable: }, 3 node(s) didn't match Pod's node affinity/selector."}]})
        )]),
        json!([]),
    );
    let (out, g, holds) = answered(&c.run_at(AFTER_FAILURE, &[], &[]));
    assert!(
        out.contains("check-node-trim: CAUSE — Unschedulable: pod boss-node-trim-29332120-x7k2p was not scheduled — Unschedulable: 0/4 nodes are available"),
        "{out}"
    );
    assert_eq!(
        (g["failed"].as_str(), g["reason"].as_str()),
        ("1", "Unschedulable")
    );
    assert!(!holds);
}

/// A Job that failed on its deadline, whose pod was removed and whose
/// events have expired: the cause is SAID to be unread, never guessed.
#[test]
fn a_deadline_failure_with_nothing_left_to_read_says_the_cause_is_unread() {
    let c = Cluster::new("ntc-deadline");
    failed_day(
        &c,
        "DeadlineExceeded",
        "Job was active longer than specified deadline",
        json!([]),
        json!([]),
    );
    let (out, g, holds) = answered(&c.run_at("2026-10-10T06:00:00Z", &[], &[]));
    for want in [
        "FAILED (created 2026-10-09T12:40:00Z): DeadlineExceeded — Job was active longer than specified deadline",
        "check-node-trim: CAUSE — DeadlineExceeded: no pod of the Job remains",
        "check-node-trim: EVENT — none remain for Job boss-node-trim-29332120",
        "which it was is no longer readable here",
        "check-node-trim: COUNTERS — none read: no pod of Job boss-node-trim-29332120 remains",
    ] {
        assert!(out.contains(want), "the check says {want:?}:\n{out}");
    }
    assert_eq!(
        (g["failed"].as_str(), g["reason"].as_str()),
        ("1", "DeadlineExceeded")
    );
    assert!(!holds);
}

/// The check can land inside a run's thirty minutes. The Job in flight is
/// said and not judged; the newest FINISHED Job is — so a failed day is
/// not lost to a check that only ever saw it running.
#[test]
fn a_job_in_flight_is_said_and_the_newest_finished_job_is_judged() {
    let c = Cluster::new("ntc-flight");
    failed_day(
        &c,
        "BackoffLimitExceeded",
        "Job has reached the specified backoff limit",
        json!([
            pod(
                "boss-node-trim-29332120",
                terminated(1, "Error", TRIM_SAID_RED)
            ),
            pod("boss-node-trim-29333560", json!({"phase": "Running"}))
        ]),
        json!([]),
    );
    let mut jobs: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(c.dir.join("state/jobs.json")).expect("jobs"),
    )
    .expect("json");
    jobs.push(job("boss-node-trim-29333560", "2026-10-10T12:40:00Z", None));
    c.state("jobs.json", &json!(jobs));
    let (out, g, holds) = answered(&c.run_at("2026-10-10T12:45:00Z", &[], &[]));
    assert!(
        out.contains("check-node-trim: IN FLIGHT — Job boss-node-trim-29333560 (created 2026-10-10T12:40:00Z) has not finished: pod boss-node-trim-29333560-x7k2p is Running"),
        "{out}"
    );
    assert_eq!(
        (
            g["job"].as_str(),
            g["failed"].as_str(),
            g["reason"].as_str()
        ),
        ("boss-node-trim-29332120", "1", "exit-1")
    );
    assert!(!holds);

    // A first Job in flight and none finished: nothing to judge yet.
    let c = Cluster::new("ntc-first-flight");
    c.state(
        "cronjobs.json",
        &json!([cronjob(
            "2026-10-07T22:30:00Z",
            None,
            Some("2026-10-08T12:40:00Z")
        )]),
    );
    c.state(
        "jobs.json",
        &json!([job("boss-node-trim-29330680", "2026-10-08T12:40:00Z", None)]),
    );
    c.state(
        "pods.json",
        &json!([pod("boss-node-trim-29330680", json!({"phase": "Running"}))]),
    );
    let (out, g, holds) = answered(&c.run_at("2026-10-08T12:50:00Z", &[], &[]));
    assert!(out.contains("check-node-trim: NO FINISHED JOB — "), "{out}");
    assert_eq!((g["job"].as_str(), g["hours"].as_str()), ("none", "14"));
    assert!(holds, "fourteen hours old with its first run in flight");
}

/// A Job in the namespace that the CronJob does not own is not its run.
#[test]
fn only_the_cronjobs_own_jobs_are_its_runs() {
    let c = Cluster::new("ntc-foreign");
    let mut stray = job(
        "something-else",
        "2026-10-08T14:00:00Z",
        Some(("Failed", "BackoffLimitExceeded", "not the trim")),
    );
    stray["metadata"]["ownerReferences"] = json!([]);
    c.state(
        "jobs.json",
        &json!([
            stray,
            job(
                "boss-node-trim-29330680",
                "2026-10-08T12:40:00Z",
                Some(("Complete", "CompletionsReached", "ok"))
            )
        ]),
    );
    let (_, g, holds) = answered(&c.run(&[]));
    assert_eq!(
        (g["job"].as_str(), g["failed"].as_str()),
        ("boss-node-trim-29330680", "0")
    );
    assert!(holds);
}

// -------------------------------------------------------- absent, and dark

#[test]
fn a_cronjob_the_tree_does_not_declare_yet_is_a_not_yet_and_never_an_alarm() {
    let c = Cluster::new("ntc-not-yet");
    c.undeclared();
    c.state("cronjobs.json", &json!([]));
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(out.contains("check-node-trim: NOT YET — CronJob boss-node-trim is not in namespace boss-node-maintenance"), "{out}");
    assert_eq!(
        (g["state"].as_str(), g["last_success"].as_str()),
        ("not-yet", "never")
    );
    assert!(
        holds,
        "before the trim car lands there is nothing to alarm on"
    );
    // Nothing past the CronJob list was asked for.
    assert_eq!(c.calls().len(), 2, "{:?}", c.calls());

    // The namespace itself not there yet reads the same way.
    let c = Cluster::new("ntc-no-namespace");
    c.undeclared();
    c.state(
        "namespaces.json",
        &json!([{"metadata": {"name": "kube-system"}}]),
    );
    c.state("cronjobs.json", &json!([]));
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("namespace boss-node-maintenance is ABSENT"),
        "{out}"
    );
    assert_eq!(g["state"], "not-yet");
    assert!(holds);
}

/// The tree declares the CronJob and the cluster does not hold it: a held
/// or failed converge, or a deleted object. A `not-yet` here would be
/// silence for ever.
#[test]
fn a_declared_cronjob_that_is_not_there_is_missing_and_alarms() {
    let c = Cluster::new("ntc-missing");
    c.state("cronjobs.json", &json!([]));
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("check-node-trim: MISSING — CronJob boss-node-trim is not in namespace boss-node-maintenance, and this checkout's tree declares it"),
        "{out}"
    );
    assert_eq!(
        (g["state"].as_str(), g["reason"].as_str()),
        ("missing", "Missing")
    );
    assert!(!holds, "a declared trim that is not scheduled is the alarm");

    // Another CronJob in the namespace is not this one.
    let mut other = cronjob("2026-10-07T22:30:00Z", Some("2026-10-08T12:41:00Z"), None);
    other["metadata"]["name"] = json!("boss-node-trim-2");
    c.state("cronjobs.json", &json!([other]));
    let (_, g, _) = answered(&c.run(&[]));
    assert_eq!(g["state"], "missing");
}

/// "Could not read" is its own answer: exit 1, CANNOT ANSWER, and NO
/// verdict line — never `failed 1`, never a pass. The watch alarms on the
/// failed run and says the read failed, not the trim.
#[test]
fn an_api_that_cannot_be_read_cannot_answer_and_is_neither_a_failure_nor_a_pass() {
    let dark = |name: &str, env: &[(&str, &str)], says: &str| {
        let c = Cluster::new(name);
        let o = c.run(env);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name}: {out}");
        assert!(
            out.contains("check-node-trim: FAILED — CANNOT ANSWER — ") && out.contains(says),
            "{name} says {says:?}: {out}"
        );
        assert!(
            out.contains("neither a failed trim nor a healthy one"),
            "{name}: {out}"
        );
        assert!(
            watch_judges(&out).is_none() && !out.contains("READ — state"),
            "{name}: no verdict from a run that could not read: {out}"
        );
    };
    dark(
        "ntc-dark",
        &[("STUB_UNREACHABLE", "1")],
        "namespaces: Unable to connect to the server",
    );
    for res in ["cronjobs.batch", "jobs.batch", "pods", "events"] {
        dark(
            &format!("ntc-dark-{res}"),
            &[("STUB_FAIL", res)],
            &format!("{res}: Error from server (ServiceUnavailable)"),
        );
        dark(
            &format!("ntc-empty-{res}"),
            &[("STUB_EMPTY", res)],
            &format!("{res} did not read back as a list"),
        );
        // A kubectl that printed a whole, well-formed list and THEN failed
        // did not finish reading: its exit is judged before its bytes.
        dark(
            &format!("ntc-half-{res}"),
            &[("STUB_HALF", res)],
            &format!("CANNOT ANSWER — {res}: "),
        );
    }

    // A wrong target answers instead of erroring: a namespace list with no
    // kube-system is not this cluster's, and its empty CronJob list would
    // read as a not-yet.
    let c = Cluster::new("ntc-wrong-target");
    c.undeclared();
    c.state(
        "namespaces.json",
        &json!([{"metadata": {"name": "default"}}]),
    );
    c.state("cronjobs.json", &json!([]));
    let o = c.run(&[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("names no kube-system"), "{out}");
    assert!(watch_judges(&out).is_none(), "{out}");

    // No kubectl image to run: the door's refusal, not an answer.
    let c = Cluster::new("ntc-no-image");
    let o = c.run(&[("BOSS_FORGE_REGISTRY_HOST", "")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && watch_judges(&out).is_none(),
        "{out}"
    );
}

#[test]
fn a_time_that_is_not_a_time_cannot_answer() {
    let c = Cluster::new("ntc-bad-time");
    c.state(
        "cronjobs.json",
        &json!([cronjob(
            "2026-10-07T22:30:00Z",
            Some("yesterday-ish o'clock"),
            None
        )]),
    );
    let o = c.run(&[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("is not a time"), "{out}");
    assert!(watch_judges(&out).is_none(), "{out}");
}

// ---------------------------------------------------- the tree's own trim

const TRIM_MANIFEST: &str = "infra/cluster/manifests/boss-node-maintenance.yaml";

/// The trim's inline script, cut out of the manifest that ships it: the
/// one block scalar (`- |`) of the file, de-indented.
fn the_trims_own_script() -> String {
    let text = std::fs::read_to_string(repo_root().join(TRIM_MANIFEST)).expect("the manifest");
    let lines: Vec<&str> = text.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let start = lines
        .iter()
        .position(|l| !l.trim_start().starts_with('#') && l.trim_end().ends_with("- |"))
        .expect("the CronJob carries its script as a block scalar");
    let depth = indent(lines[start]);
    let body: Vec<&str> = lines[start + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent(l) > depth)
        .copied()
        .collect();
    let strip = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .min()
        .expect("the script is not empty");
    body.iter()
        .map(|l| if l.len() >= strip { &l[strip..] } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The counters are one fact in two files: the trim's script PRINTS them
/// and this check PARSES them. So the script the manifest ships is run
/// here (fstrim and stat stubbed; the stub fstrim moves the drive's
/// counters as a real discard does), and what it wrote to its termination
/// message is handed to the check as the pod's. A change to the trim's
/// `say` line that this reader cannot follow fails here, by name.
#[test]
fn the_counters_the_trims_own_script_prints_are_the_ones_this_reads() {
    let c = Cluster::new("ntc-own-script");
    let sys = c.dir.join("sys/nvme9n1");
    for d in [
        &sys,
        &c.dir.join("trimbin"),
        &c.dir.join("gate"),
        &c.dir.join("ephemeral"),
    ] {
        std::fs::create_dir_all(d).expect("dir");
    }
    write_file(
        &sys.join("stat"),
        "1 2 3 4 5 6 7 8 9 10 11 699 13 4096 15 16 17\n",
    );
    write_exec(
        &c.dir.join("trimbin/fstrim"),
        "#!/bin/sh\necho \"1 2 3 4 5 6 7 8 9 10 11 750 13 8192 15 16 17\" > \"$TRIM_SYS_BLOCK/nvme9n1/stat\"\n\
         echo \"$2: 123456789 bytes trimmed\"\n",
    );
    write_exec(
        &c.dir.join("trimbin/stat"),
        "#!/bin/sh\ncase \"$3\" in */gate) echo 64769;; *) echo 64770;; esac\n",
    );
    write_file(&c.dir.join("trim.sh"), &the_trims_own_script());
    let message = c.dir.join("termination-log");
    let o = Command::new("sh")
        .arg(c.dir.join("trim.sh"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                c.dir.join("trimbin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("TRIM_GATE", c.dir.join("gate"))
        .env("TRIM_EPHEMERAL", c.dir.join("ephemeral"))
        .env("TRIM_TERMINATION_LOG", &message)
        .env("TRIM_SYS_BLOCK", c.dir.join("sys"))
        .output()
        .expect("sh runs the trim's script");
    assert!(
        o.status.success(),
        "the trim's own script ran: {}",
        text(&o)
    );
    let said = std::fs::read_to_string(&message).expect("a termination message");
    c.state(
        "pods.json",
        &json!([pod(
            "boss-node-trim-29330680",
            terminated(0, "Completed", &said)
        )]),
    );
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("check-node-trim: COUNTERS — nvme9n1: completed discards 699 -> 750 (+51), discarded sectors 4096 -> 8192 (+4096) [the pod's termination message]"),
        "the check reads what the trim's script wrote:\n{said}\n{out}"
    );
    assert!(
        out.contains("check-node-trim: THE TRIM SAID — node-trim: OK"),
        "{out}"
    );
    assert_eq!((g["devices"].as_str(), g["moved"].as_str()), ("1", "1"));
    assert!(holds);
}

/// "Declared" is read off the manifest the tree ships, at the path the
/// script finds by itself: with no CronJob in the cluster, this tree's
/// answer is `missing`, not `not-yet`.
#[test]
fn the_tree_that_ships_the_trim_declares_it() {
    let c = Cluster::new("ntc-real-manifest");
    c.state("cronjobs.json", &json!([]));
    let o = c.run(&[(
        "BOSS_NODE_TRIM_MANIFEST",
        repo_root().join(TRIM_MANIFEST).to_str().expect("utf-8"),
    )]);
    let (out, g, _) = answered(&o);
    assert_eq!(g["state"], "missing", "{out}");
    assert!(out.contains("(boss-node-maintenance.yaml)"), "{out}");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    assert!(
        script.contains(
            r#"MANIFEST="${BOSS_NODE_TRIM_MANIFEST:-$HERE/../cluster/manifests/$NS.yaml}""#
        ),
        "the script's own default is that manifest"
    );
}

// ------------------------------------------------------------------ the verb

#[test]
fn the_check_takes_nothing() {
    let c = Cluster::new("ntc-usage");
    let o = c.run_at(NOW, &["boss-node-trim"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(c.calls().is_empty(), "a refused call reads nothing");
}

#[test]
fn the_verb_file_bounds_what_a_packet_can_ask() {
    let p = repo_root().join("infra/ops/verbs/check-node-trim.json");
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    assert_eq!(
        v["hosts"],
        json!(["forge"]),
        "the forge holds the kubeconfig"
    );
    assert_eq!(v["argv"], json!([SCRIPT]));
    assert_eq!(v["params"], json!([]), "the check takes nothing");
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
    assert!(v.get("requires_approval").is_none(), "a clock files it");
    assert!(v.get("effect").is_none(), "a read declares no effect");
    // Six door calls at 20 s each and, the first time, the image's pull
    // bounded at 120 s.
    assert!(v["timeout"].as_u64().unwrap_or(0) >= 6 * 20 + 120, "{v}");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    for write in [
        " apply ",
        " patch ",
        " delete ",
        " create ",
        " annotate ",
        " label ",
        " exec ",
    ] {
        assert!(
            !script
                .lines()
                .any(|l| !l.trim_start().starts_with('#') && l.contains("$K") && l.contains(write)),
            "the script sends{write}through the admin kubeconfig"
        );
    }
}

/// The CronJob lint's exemption and this reader are one fact in two
/// places: `timers-leave-a-packet.sh` lets `boss-node-trim` file no
/// packet of its own BECAUSE this check files one for it. If the name is
/// exempt there, the rule that files the check and the watch that judges
/// it must both be in the shipped rule directory.
#[test]
fn the_lints_exemption_of_the_trim_rests_on_this_reader() {
    let lint = std::fs::read_to_string(repo_root().join("infra/lint/timers-leave-a-packet.sh"))
        .expect("the lint");
    assert!(
        lint.contains("\n    \"boss-node-trim\"\n"),
        "boss-node-trim left CRONJOB_OWN_VISIBILITY: either its manifest now files its own \
         packet, or this pin's reason to exist moved — read both before deleting it"
    );
    for rule in ["check-node-trim-daily.toml", WATCH] {
        assert!(
            boss_testing::dispatcher_rules_dir().join(rule).is_file(),
            "timers-leave-a-packet.sh exempts boss-node-trim, and {rule} — the reader that exemption rests on — is gone"
        );
    }
    assert!(
        lint.contains("check-node-trim"),
        "the exemption names the reader it rests on (check-node-trim)"
    );
}
