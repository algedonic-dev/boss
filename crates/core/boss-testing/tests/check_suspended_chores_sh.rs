//! The read-only ops verb `check-suspended-chores`
//! (`infra/forge/check-suspended-chores.sh`): the forge reading which
//! cluster chores — CronJobs labelled boss-chore=true — stand suspended,
//! and since when (backlog 87e08026, car 2 of d28c0051).
//!
//! WHY. The converge's client-side apply does not own spec.suspend, so a
//! chore suspended by hand stays suspended across every apply, and
//! check-manifests-applied only asks that each CronJob exists:
//! boss/boss-search-reindex, suspended at 2026-11-11 05:02Z (832386eb), was
//! reported by nothing. `check-suspended-chores-daily` files this verb and
//! `watch-check-suspended-chores-daily` judges its last line.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small API server behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: the labelled CronJob list, with the managedFields
//! kubectl shows only on `--show-managed-fields`. The live case is copied
//! from the triage's read of 2026-11-11 09:06Z: f:spec.f:suspend owned by
//! kubectl-patch at 05:02:13Z, and the converge's
//! kubectl-client-side-apply entry at 08:48:46Z owning other fields. Every
//! door call lands in `argv`, so the check can be shown to have only read.
//! The watch's own `verdict_pattern` and `when` are read off its rule file
//! and run over the script's REAL output. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/check-suspended-chores.sh";
const WATCH: &str = "watch-check-suspended-chores-daily.toml";
/// When the triage read the live cluster.
const NOW: &str = "2026-11-11T09:06:00Z";
const SUSPENDED_AT: &str = "2026-11-11T05:02:13Z";
const CONVERGED_AT: &str = "2026-11-11T08:48:46Z";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `cronjobs.json`, an item array. STUB_UNREACHABLE=1 answers as a dark
/// API server does; STUB_EMPTY=1 answers with exit 0 and no bytes at all.
/// A CronJob read without the label selector, across fewer than all
/// namespaces, or without managedFields is an unexpected call.
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
case "$*" in
"get cronjobs.batch -A -l boss-chore=true -o json --show-managed-fields "*)
    [ -z "${STUB_EMPTY:-}" ] || exit 0
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/cronjobs.json")"
    exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// A managedFields entry: `manager` wrote `fields` (FieldsV1) at `time`.
fn owner(manager: &str, time: &str, fields: Value) -> Value {
    json!({
        "manager": manager, "operation": "Update", "apiVersion": "batch/v1",
        "time": time, "fieldsType": "FieldsV1", "fieldsV1": fields
    })
}

/// The converge's entry: it owns the schedule and the template, never
/// spec.suspend (it was measured so on 2026-11-11).
fn converge_entry() -> Value {
    owner(
        "kubectl-client-side-apply",
        CONVERGED_AT,
        json!({"f:metadata": {"f:labels": {"f:boss-chore": {}}},
               "f:spec": {"f:schedule": {}, "f:jobTemplate": {}}}),
    )
}

/// A chore CronJob. `suspended`: None for a running chore; Some(entries)
/// for a suspended one, with the managedFields entries beside the
/// converge's.
fn chore(ns: &str, name: &str, suspended: Option<Vec<Value>>) -> Value {
    let suspend = suspended.is_some();
    let mut fields = vec![
        converge_entry(),
        // A status write owns no spec field, and must never date one.
        json!({"manager": "kube-controller-manager", "operation": "Update",
               "apiVersion": "batch/v1", "subresource": "status",
               "time": "2026-11-11T09:00:00Z", "fieldsType": "FieldsV1",
               "fieldsV1": {"f:status": {"f:lastScheduleTime": {}}}}),
    ];
    fields.extend(suspended.unwrap_or_default());
    json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": name, "namespace": ns,
                     "labels": {"boss-chore": "true"},
                     "managedFields": fields},
        "spec": {"schedule": "7 4 * * *", "suspend": suspend},
        "status": {"lastScheduleTime": "2026-11-11T04:07:00Z"}
    })
}

/// boss-search-reindex as the triage read it: suspended by kubectl-patch.
fn reindex_suspended() -> Value {
    chore(
        "boss",
        "boss-search-reindex",
        Some(vec![owner(
            "kubectl-patch",
            SUSPENDED_AT,
            json!({"f:spec": {"f:suspend": {}}}),
        )]),
    )
}

fn needs_tools() {
    for tool in ["jq", "date"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "check_suspended_chores_sh: no {tool} on this box — the gate image has it, and a \
             test that cannot run must fail, never pass by returning early"
        );
    }
}

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
    fn new(name: &str, chores: &[Value]) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let c = Cluster { dir };
        c.chores(chores);
        c
    }

    fn chores(&self, chores: &[Value]) {
        write_file(
            &self.dir.join("state/cronjobs.json"),
            &Value::Array(chores.to_vec()).to_string(),
        );
    }

    fn run_at(&self, now: i64, args: &[&str], env: &[(&str, &str)]) -> Output {
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
            .env("BOSS_SUSPENDED_CHORES_NOW", now.to_string())
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().expect("the script runs");
        // EVERY run of EVERY case: nothing but reads crosses the door.
        for call in self.calls() {
            assert!(
                call.starts_with("get "),
                "the check sent something other than a read through the admin kubeconfig: {call}"
            );
        }
        o
    }

    fn run(&self, env: &[(&str, &str)]) -> Output {
        self.run_at(epoch(NOW), &[], env)
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
/// `ops.judge` reads the check's verdict with, compiled here and run over
/// the script's real output. The groups of the last matching line, and
/// whether `when` holds over them — by the handler's own reading: groups
/// that parse as integers are integers.
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
        "suspended = 0 OR oldest_hours < 24",
        "the threshold moved; the cases below judge 24 hours"
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
    let holds = num("suspended") == 0 || num("oldest_hours") < 24;
    Some((groups, holds))
}

/// Exit 0, a verdict the watch reads, and its groups.
fn answered(o: &Output) -> (String, BTreeMap<String, String>, bool) {
    let out = text(o);
    assert!(o.status.success(), "the check answered: {out}");
    let (g, holds) =
        watch_judges(&out).unwrap_or_else(|| panic!("the watch reads a verdict: {out}"));
    (out, g, holds)
}

/// Exit 1, CANNOT ANSWER, and no verdict for the watch to read.
fn cannot_answer(o: &Output) -> String {
    let out = text(o);
    assert_eq!(o.status.code(), Some(1), "a failed read: {out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(
        watch_judges(&out).is_none(),
        "no verdict line from a run that could not read: {out}"
    );
    out
}

#[test]
fn no_chore_suspended_is_a_verdict_and_no_alarm() {
    let c = Cluster::new(
        "csc-none",
        &[
            chore("boss", "boss-files-gc", None),
            chore("boss", "boss-search-reindex", None),
            chore("boss-break-glass", "boss-break-glass-deposit", None),
        ],
    );
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains("check-suspended-chores: READ — suspended 0, oldest_hours 0, chores none"),
        "{out}"
    );
    assert!(
        out.contains("3 chore(s) labelled boss-chore=true read, 0 suspended"),
        "{out}"
    );
    assert_eq!(g["suspended"], "0");
    assert!(holds, "nothing suspended is no alarm");
    // ONE read: every namespace, by the chore label, with managedFields.
    let calls = c.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0]
            .starts_with("get cronjobs.batch -A -l boss-chore=true -o json --show-managed-fields"),
        "{calls:?}"
    );
}

#[test]
fn a_chore_left_suspended_past_24_hours_is_what_the_watch_alarms_on() {
    let c = Cluster::new(
        "csc-live",
        &[chore("boss", "boss-files-gc", None), reindex_suspended()],
    );
    // The triage's read, four hours in: named, dated by the write that
    // owns spec.suspend — not by the converge that applied since.
    let (out, g, holds) = answered(&c.run(&[]));
    assert!(
        out.contains(&format!(
            "SUSPENDED — boss/boss-search-reindex since {SUSPENDED_AT} (4 hour(s)), set by kubectl-patch; last scheduled 2026-11-11T04:07:00Z"
        )),
        "{out}"
    );
    assert_eq!(
        (
            g["suspended"].as_str(),
            g["oldest_hours"].as_str(),
            g["chores"].as_str()
        ),
        ("1", "4", "boss/boss-search-reindex")
    );
    assert!(holds, "four hours suspended is no alarm yet");

    // Forgotten: a day and more after the suspension, and the watch alarms.
    for h in [24, 30] {
        let o = c.run_at(epoch(SUSPENDED_AT) + h * 3600, &[], &[]);
        let (out, g, holds) = answered(&o);
        assert_eq!(g["oldest_hours"], h.to_string(), "{out}");
        assert!(!holds, "{h} hours suspended is the alarm: {g:?}");
    }
}

#[test]
fn every_suspended_chore_is_named_and_the_oldest_decides() {
    let c = Cluster::new(
        "csc-two",
        &[
            reindex_suspended(),
            chore("boss", "boss-files-gc", None),
            // Suspended two days earlier, in another namespace, with two
            // owners of spec.suspend: the LATER write dates it.
            chore(
                "boss-break-glass",
                "boss-break-glass-deposit",
                Some(vec![
                    owner(
                        "kubectl-edit",
                        "2026-11-08T09:06:00Z",
                        json!({"f:spec": {"f:suspend": {}}}),
                    ),
                    owner(
                        "kubectl-patch",
                        "2026-11-09T09:06:00Z",
                        json!({"f:spec": {"f:suspend": {}, "f:schedule": {}}}),
                    ),
                ]),
            ),
        ],
    );
    let (out, g, holds) = answered(&c.run(&[]));
    assert_eq!(
        (
            g["suspended"].as_str(),
            g["oldest_hours"].as_str(),
            g["chores"].as_str()
        ),
        (
            "2",
            "48",
            "boss-break-glass/boss-break-glass-deposit,boss/boss-search-reindex"
        ),
        "{out}"
    );
    assert!(
        out.contains("boss-break-glass/boss-break-glass-deposit since 2026-11-09T09:06:00Z (48 hour(s)), set by kubectl-patch"),
        "{out}"
    );
    assert!(!holds, "the oldest suspension decides: {g:?}");
}

#[test]
fn a_suspension_whose_time_cannot_be_read_cannot_answer() {
    // Suspended, and no entry owns spec.suspend (the converge's and the
    // status write own other fields).
    let c = Cluster::new(
        "csc-unowned",
        &[chore("boss", "boss-search-reindex", Some(vec![]))],
    );
    let out = cannot_answer(&c.run(&[]));
    assert!(
        out.contains("boss/boss-search-reindex") && out.contains("spec.suspend"),
        "{out}"
    );
    assert!(
        out.contains("no managedFields entry owns spec.suspend"),
        "{out}"
    );

    // An owner whose time is not a time.
    c.chores(&[chore(
        "boss",
        "boss-search-reindex",
        Some(vec![owner(
            "kubectl-patch",
            "2026-13-45T99:99:99Z",
            json!({"f:spec": {"f:suspend": {}}}),
        )]),
    )]);
    let out = cannot_answer(&c.run(&[]));
    assert!(out.contains("2026-13-45T99:99:99Z"), "{out}");

    // An entry that OWNS spec.suspend and carries no time: owned, and
    // still undated — the message must say so, not that nothing owns it
    // (review run 7682a54f, F3).
    c.chores(&[chore(
        "boss",
        "boss-search-reindex",
        Some(vec![json!({
            "manager": "kubectl-patch", "operation": "Update",
            "apiVersion": "batch/v1", "fieldsType": "FieldsV1",
            "fieldsV1": {"f:spec": {"f:suspend": {}}}
        })]),
    )]);
    let out = cannot_answer(&c.run(&[]));
    assert!(
        out.contains("kubectl-patch owns spec.suspend and its entry carries no time"),
        "{out}"
    );
}

/// GNU sort truncates its -o target BEFORE it writes, so a sort that dies
/// mid-write (ENOSPC on the forge's scratch — its known disk-floor failure)
/// leaves the list of suspended chores empty or short. Read on, that is
/// "suspended 0" and no alarm: no evidence read as a pass (review run
/// 7682a54f, F2). The stand-in does exactly what GNU sort does on a full
/// disk: empties the target, says why, exits 2.
const SORT_ENOSPC: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
    case "$1" in -o) : > "$2"; shift ;; esac
    shift
done
echo "sort: write failed: standard output: No space left on device" >&2
exit 2
"#;

#[test]
fn a_sort_that_fails_cannot_answer_rather_than_read_none_suspended() {
    let c = Cluster::new("csc-sort", &[reindex_suspended()]);
    write_exec(&c.dir.join("bin/sort"), SORT_ENOSPC);
    let out = cannot_answer(&c.run(&[]));
    assert!(out.contains("could not be sorted"), "{out}");
    assert!(out.contains("No space left on device"), "{out}");
}

#[test]
fn a_read_that_could_not_look_is_not_none_suspended() {
    let c = Cluster::new("csc-dark", &[reindex_suspended()]);
    let out = cannot_answer(&c.run(&[("STUB_UNREACHABLE", "1")]));
    assert!(out.contains("Unable to connect"), "{out}");

    // An answer carrying no document at all — exit 0, no bytes (d96e38ab).
    cannot_answer(&c.run(&[("STUB_EMPTY", "1")]));

    // A list naming no chore at all: every chore carries the label, so
    // this is a wrong target answering, not "none suspended".
    c.chores(&[]);
    let out = cannot_answer(&c.run(&[]));
    assert!(out.contains("wrong target"), "{out}");
}

#[test]
fn the_check_takes_nothing() {
    let c = Cluster::new("csc-usage", &[reindex_suspended()]);
    let o = c.run_at(epoch(NOW), &["boss-search-reindex"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(c.calls().is_empty(), "no door opened: {:?}", c.calls());
}

#[test]
fn the_verb_file_is_a_read_only_forge_verb_with_a_timeout() {
    let p = repo_root().join("infra/ops/verbs/check-suspended-chores.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).expect("the verb file"))
        .expect("the verb file is JSON");
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(v["argv"], json!([SCRIPT]));
    assert_eq!(v["params"], json!([]), "the check takes nothing");
    assert!(
        v["timeout"].as_u64().is_some_and(|t| t > 120 + 20),
        "the timeout outlasts the door image's pull and the read: {v}"
    );
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
    assert!(v.get("requires_approval").is_none(), "{v}");
}
