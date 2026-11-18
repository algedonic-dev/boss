//! The two ops verbs over a cluster chore's spec.suspend (backlog
//! d28c0051): the read-only `plan-a-chore-suspend`
//! (`infra/forge/chore-suspend.sh --plan`) and the passkey-approved
//! `suspend-a-chore`, whose `false` is the resume.
//!
//! WHY. On 2026-11-11 ~05:00Z the hourly boss/boss-search-reindex spilled
//! about 13 GB of temp per run onto a degraded pgdata volume (832386eb).
//! spec.suspend=true was the safe lever and no verb pulled it, so David
//! ran `kubectl patch` from his workstation. The verb is bounded to a
//! CronJob THIS CHECKOUT declares (rendered for every instance exactly as
//! the converge renders it) with the label boss-chore=true, whose live
//! object carries the label, whose spec the converge does not itself
//! write, and whose value would change.
//!
//! HOW THIS IS MEASURED. The script runs for real against the real tree's
//! manifests (the bound is the tree, so the fixture is the tree), with
//! `sudo` stubbed on PATH as a small API server behind ops_kubectl's
//! `docker run … kubectl --kubeconfig=/kc`: one CronJob object per file
//! under `state/cj/`. Every door call lands in `argv`, so a refusal can be
//! shown to have patched nothing — and a bound the TREE decides to have
//! opened no door at all. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/chore-suspend.sh";
const REINDEX: &str = "boss-search-reindex";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `cj/<namespace>__<name>.json`, the CronJob object. A patch applies a
/// JSON patch's `test` + `replace` of /spec/suspend and leaves the
/// marker `patched`. STUB_FORBIDDEN=1 answers every read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses the patch; STUB_EMPTY=1 answers a get with
/// exit 0 and no bytes at all; STUB_BACK_SUSPEND=<v> answers every get
/// after a patch with that spec.suspend (a read-back that disagrees).
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
if [ -n "${STUB_FORBIDDEN:-}" ]; then
    echo "Error from server (Forbidden): cronjobs.batch \"$3\" is forbidden: User \"system:serviceaccount:boss-dev:boss-dev\" cannot get resource \"cronjobs\" in API group \"batch\" in the namespace \"$5\"" >&2
    exit 1
fi
f="$S/cj/$5__$3.json"
case "$1 $2" in
"get cronjob")
    [ -z "${STUB_EMPTY:-}" ] || exit 0
    [ -f "$f" ] || { echo "Error from server (NotFound): cronjobs.batch \"$3\" not found" >&2; exit 1; }
    if [ -f "$S/patched" ] && [ -n "${STUB_BACK_SUSPEND:-}" ]; then
        jq --argjson v "$STUB_BACK_SUSPEND" '.spec.suspend = $v' "$f"
    else
        cat "$f"
    fi
    exit 0 ;;
"patch cronjob")
    p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server (Forbidden): cronjobs.batch \"x\" is forbidden: admission webhook denied the request" >&2; exit 1; }
    want=$(printf '%s' "$p" | jq -c '.[] | select(.op == "test" and .path == "/spec/suspend") | .value')
    new=$(printf '%s' "$p" | jq -c '.[] | select(.op == "replace" and .path == "/spec/suspend") | .value')
    cur=$(jq -c .spec.suspend "$f")
    [ -n "$want" ] && [ "$want" = "$cur" ] || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    jq --argjson n "$new" '.spec.suspend = $n' "$f" > "$f.new" && mv "$f.new" "$f"
    : > "$S/patched"
    echo "cronjob.batch/x patched"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// A live CronJob as the API server answers it (suspend defaulted, the
/// way batch/v1 always stores it).
fn cronjob(ns: &str, name: &str, chore: bool, suspend: bool, active: &[&str]) -> Value {
    let mut labels = json!({"app.kubernetes.io/part-of": "boss"});
    if chore {
        labels["boss-chore"] = json!("true");
    }
    let active: Vec<Value> = active
        .iter()
        .map(|j| json!({"kind": "Job", "namespace": ns, "name": j, "apiVersion": "batch/v1"}))
        .collect();
    json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": name, "namespace": ns, "labels": labels,
                     "uid": "6f1e9b99-0000-4000-8000-000000000001"},
        "spec": {"schedule": "7 * * * *", "concurrencyPolicy": "Forbid", "suspend": suspend,
                 "successfulJobsHistoryLimit": 3, "failedJobsHistoryLimit": 3,
                 "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "chore"}]}}}}},
        "status": {"lastScheduleTime": "2026-11-11T05:07:00Z", "active": active}
    })
}

struct Api {
    dir: PathBuf,
}

fn needs_tools() {
    // awk is POSIX and every box has one; not every awk answers --version.
    for tool in ["jq", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "chore_suspend_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

impl Api {
    /// boss/boss-search-reindex live and running, not suspended.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state/cj")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let a = Api { dir };
        a.put(&cronjob(
            "boss",
            REINDEX,
            true,
            false,
            &["boss-search-reindex-29380387"],
        ));
        a
    }

    fn put(&self, cj: &Value) {
        let ns = cj["metadata"]["namespace"].as_str().expect("ns");
        let name = cj["metadata"]["name"].as_str().expect("name");
        write_file(
            &self.dir.join(format!("state/cj/{ns}__{name}.json")),
            &cj.to_string(),
        );
    }

    fn suspend_now(&self, ns: &str, name: &str) -> Value {
        let p = self.dir.join(format!("state/cj/{ns}__{name}.json"));
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(p).expect("cronjob")).expect("json");
        v["spec"]["suspend"].clone()
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
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
            // The bound is THIS checkout: a caller's override must not move it.
            .env("BOSS_CLUSTER_TREE", self.dir.join("not-the-tree"))
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
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

    fn patches(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("patch "))
            .collect()
    }

    fn plan(&self, ns: &str, name: &str, value: &str) -> (String, String, String) {
        let o = self.run(&["--plan", ns, name, value], &[]);
        assert!(o.status.success(), "the plan renders: {}", text(&o));
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        let hash = stderr
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("plan-sha256 on stderr: {stderr}"))
            .trim()
            .to_string();
        (String::from_utf8_lossy(&o.stdout).to_string(), hash, stderr)
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn verb(name: &str) -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Does `line` match `re` in the runner's own engine (jq's `test`)?
fn jq_test(re: &str, line: &str) -> bool {
    let o = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "re",
            re,
            "--arg",
            "l",
            line,
            "$l | test($re)",
        ])
        .output()
        .expect("jq runs");
    assert!(o.status.success(), "jq judges {re:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim() == "true"
}

/// The lines of `out` the write's declared `effect` matches.
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("suspend-a-chore")["effect"]
        .as_str()
        .expect("suspend-a-chore declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

/// The chores the tree declares in namespace `boss`, read independently
/// of the script's awk: each manifest's CronJob document, its `  name:`
/// and `  namespace:` lines, and whether a non-comment line of it carries
/// `boss-chore: "true"` (block or flow map alike).
fn chores_declared_in_boss() -> Vec<String> {
    let dir = repo_root().join("infra/cluster/manifests");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).expect("manifests") {
        let p = e.expect("entry").path();
        if p.extension().and_then(|x| x.to_str()) != Some("yaml") {
            continue;
        }
        let body = std::fs::read_to_string(&p).expect("manifest");
        for doc in body.split("\n---") {
            let lines: Vec<&str> = doc
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect();
            if !lines.iter().any(|l| l.trim_end() == "kind: CronJob") {
                continue;
            }
            let field = |k: &str| {
                lines
                    .iter()
                    .find_map(|l| l.strip_prefix(&format!("  {k}: ")))
                    .map(|v| v.trim().trim_matches('"').to_string())
            };
            let meta_end = lines
                .iter()
                .position(|l| l.starts_with("spec:"))
                .unwrap_or(lines.len());
            let chore = lines[..meta_end]
                .iter()
                .any(|l| l.contains("boss-chore: \"true\""));
            if chore && field("namespace").as_deref() == Some("boss") {
                out.push(field("name").expect("a CronJob names itself"));
            }
        }
    }
    out.sort();
    out
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_chore_its_declaration_the_value_and_the_schedule() {
    let a = Api::new("cs-plan");
    let (plan, hash, stderr) = a.plan("boss", REINDEX, "true");
    for want in [
        "plan: suspend-a-chore",
        "cronjob: boss/boss-search-reindex",
        "declared: infra/cluster/manifests/boss-search-reindex.yaml, label boss-chore=true; the live object carries boss-chore=true",
        "spec.suspend: false -> true",
        "schedule: 7 * * * *",
        "startingDeadlineSeconds: unset",
        "concurrencyPolicy: Forbid",
        "test that /spec/suspend is still false, replace it with true",
        "suspending stops the NEXT runs only",
        "stays suspended until this verb runs with false",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(a.patches().is_empty(), "the plan patches nothing");
    assert!(effect_lines(&plan).is_empty(), "{plan}");
    // The last run and the running Jobs ride stderr, unsigned.
    assert!(
        stderr.contains("last scheduled 2026-11-11T05:07:00Z")
            && stderr.contains("active Jobs (1): boss-search-reindex-29380387"),
        "{stderr}"
    );
    assert!(!plan.contains("2026-11-11T05:07:00Z"), "{plan}");

    // Deterministic, and blind to what moves with every run: a chore
    // scheduled every minute could never be approved otherwise.
    let mut moved = cronjob("boss", REINDEX, true, false, &[]);
    moved["status"]["lastScheduleTime"] = json!("2026-11-11T06:07:00Z");
    a.put(&moved);
    assert_eq!(
        a.plan("boss", REINDEX, "true").1,
        hash,
        "a run's state is not signed"
    );
}

#[test]
fn the_resume_plan_says_a_missed_run_starts_at_once() {
    let a = Api::new("cs-resume-plan");
    a.put(&cronjob("boss", REINDEX, true, true, &[]));
    let (plan, _, _) = a.plan("boss", REINDEX, "false");
    assert!(plan.contains("spec.suspend: true -> false"), "{plan}");
    assert!(
        plan.contains("starts the most recent run missed while suspended AT ONCE"),
        "{plan}"
    );
    let mut d = cronjob("boss", REINDEX, true, true, &[]);
    d["spec"]["startingDeadlineSeconds"] = json!(300);
    a.put(&d);
    let (plan, _, _) = a.plan("boss", REINDEX, "false");
    assert!(
        plan.contains("startingDeadlineSeconds: 300s") && plan.contains("within the last 300s"),
        "{plan}"
    );
}

#[test]
fn an_instance_chore_is_admitted_in_every_namespace_the_roster_renders_it_into() {
    // The tree writes the chore for prod; the converge renders it into
    // the playground too, and that copy is as declared as the source.
    let a = Api::new("cs-playground");
    a.put(&cronjob("boss-playground", REINDEX, true, false, &[]));
    let (plan, _, _) = a.plan("boss-playground", REINDEX, "true");
    assert!(
        plan.contains("cronjob: boss-playground/boss-search-reindex"),
        "{plan}"
    );

    // A flow-map label (`labels: {…, boss-chore: "true"}`) is read too.
    a.put(&cronjob(
        "boss",
        "boss-machine-token-push",
        true,
        false,
        &[],
    ));
    let (plan, _, _) = a.plan("boss", "boss-machine-token-push", "true");
    assert!(
        plan.contains("declared: infra/cluster/manifests/boss-machine-token-push.yaml"),
        "{plan}"
    );
}

#[test]
fn the_tree_bounds_refuse_before_any_door_opens() {
    let a = Api::new("cs-tree-bounds");
    // Shapes.
    for args in [
        vec!["--plan", "boss", REINDEX],
        vec!["--plan", "boss", REINDEX, "yes"],
        vec!["--plan", "Boss", REINDEX, "true"],
        vec!["--plan", "boss", "-x", "true"],
        vec!["--plan", "boss", REINDEX, "true", "extra"],
        vec!["boss", REINDEX, "true"],
        vec!["boss", REINDEX, "true", "not-a-hash"],
    ] {
        let o = a.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    // Not declared at all, and the refusal names what IS declared there —
    // every chore the tree writes for namespace boss, flow and block
    // labels alike, read here by a second, independent parser.
    let o = a.run(&["--plan", "kube-system", "coredns-sweep", "true"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    let o = a.run(&["--plan", "boss", "boss-not-a-chore", "true"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    let declared = chores_declared_in_boss();
    assert!(
        declared.len() >= 10,
        "the tree's chores in boss: {declared:?}"
    );
    for c in &declared {
        assert!(out.contains(c.as_str()), "the refusal lists {c}: {out}");
    }
    assert!(
        !out.contains("boss-pg-backup"),
        "no label, not a chore: {out}"
    );
    // A pipeline chore exists only where its file puts it.
    let o = a.run(
        &[
            "--plan",
            "boss-playground",
            "boss-machine-token-push",
            "true",
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    // Declared, without boss-chore=true.
    let o = a.run(&["--plan", "boss", "boss-pg-backup", "true"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("without the label boss-chore=true"), "{out}");
    // A chore whose whole spec the converge writes on every train.
    let o = a.run(&["--plan", "boss", "boss-break-glass-deposit", "true"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("deposit-config-patch.jq"), "{out}");
    assert!(
        a.calls().is_empty(),
        "a bound the tree decides opens no door: {:?}",
        a.calls()
    );
}

#[test]
fn the_live_bounds_refuse_and_patch_nothing() {
    let a = Api::new("cs-live-bounds");
    // The value that already stands.
    let o = a.run(&["--plan", "boss", REINDEX, "false"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("already false"), "{out}");
    // The live object without the label the tree declares.
    a.put(&cronjob("boss", REINDEX, false, false, &[]));
    let o = a.run(&["--plan", "boss", REINDEX, "true"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("does not carry the label boss-chore=true"),
        "{out}"
    );
    assert!(a.patches().is_empty(), "{:?}", a.calls());
}

#[test]
fn a_read_that_could_not_look_cannot_answer_and_renders_no_plan() {
    let a = Api::new("cs-dark");
    for env in [("STUB_FORBIDDEN", "1"), ("STUB_EMPTY", "1")] {
        let o = a.run(&["--plan", "boss", REINDEX, "true"], &[env]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{env:?}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{env:?}: {out}");
        assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    }
    // Declared in the tree, absent from the cluster.
    let o = a.run(&["--plan", "boss", "boss-files-gc", "true"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("NotFound"), "{out}");
    // A CronJob whose suspend is not a boolean is no answer either.
    let mut odd = cronjob("boss", REINDEX, true, false, &[]);
    odd["spec"].as_object_mut().expect("spec").remove("suspend");
    a.put(&odd);
    let o = a.run(&["--plan", "boss", REINDEX, "true"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(a.patches().is_empty(), "{:?}", a.calls());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_there_and_back() {
    let a = Api::new("cs-write");
    let (plan, hash, _) = a.plan("boss", REINDEX, "true");
    let o = a.run(&["boss", REINDEX, "true", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    assert!(
        out.contains("at the write: last scheduled 2026-11-11T05:07:00Z; active Jobs (1): boss-search-reindex-29380387"),
        "the record names what was running as it patched: {out}"
    );
    let patches = a.patches();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains("patch cronjob boss-search-reindex -n boss --type=json"),
        "{p}"
    );
    assert!(
        p.contains(r#"[{"op":"test","path":"/spec/suspend","value":false},{"op":"replace","path":"/spec/suspend","value":true}]"#),
        "a compare-and-set on spec.suspend and nothing else: {p}"
    );
    assert_eq!(a.suspend_now("boss", REINDEX), json!(true));
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("(was false; schedule 7 * * * *;"),
        "{hits:?}"
    );

    // Applied once, the same approval is spent.
    let o = a.run(&["boss", REINDEX, "true", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(a.patches().len(), 1, "no second patch");

    // And back, by the same verb.
    let (_, hash, _) = a.plan("boss", REINDEX, "false");
    let o = a.run(&["boss", REINDEX, "false", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert_eq!(a.suspend_now("boss", REINDEX), json!(false));
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan() {
    let a = Api::new("cs-drift");
    let (_, hash, _) = a.plan("boss", REINDEX, "true");
    // The schedule moved after the plan was signed.
    let mut moved = cronjob("boss", REINDEX, true, false, &[]);
    moved["spec"]["schedule"] = json!("*/5 * * * *");
    a.put(&moved);
    let o = a.run(&["boss", REINDEX, "true", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    let o = a.run(&["boss", REINDEX, "true", &"0".repeat(64)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(a.patches().is_empty(), "{:?}", a.calls());
    assert_eq!(a.suspend_now("boss", REINDEX), json!(false));
}

#[test]
fn a_patch_the_server_refuses_or_a_read_back_that_disagrees_is_not_an_effect() {
    let a = Api::new("cs-refused");
    let (_, hash, _) = a.plan("boss", REINDEX, "true");
    let o = a.run(
        &["boss", REINDEX, "true", &hash],
        &[("STUB_PATCH_FAIL", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("admission webhook denied"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(a.suspend_now("boss", REINDEX), json!(false));

    // The patch lands and the read-back says otherwise.
    let o = a.run(
        &["boss", REINDEX, "true", &hash],
        &[("STUB_BACK_SUSPEND", "false")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("NOT proven"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("suspend-a-chore");
    let p = verb("plan-a-chore-suspend");
    for (name, v) in [("suspend-a-chore", &w), ("plan-a-chore-suspend", &p)] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
    }
    assert_eq!(p["argv"], json!([SCRIPT, "--plan", "{1}", "{2}", "{3}"]));
    assert!(
        !p["about"].as_str().unwrap_or_default().contains("MUTATING"),
        "the plan is read-only"
    );
    assert!(p.get("requires_approval").is_none());
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-chore-suspend");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["argv"], json!([SCRIPT, "{1}", "{2}", "{3}", "{4}"]));
    let pat = |v: &Value, i: usize| {
        v["params"][i]["pattern"]
            .as_str()
            .expect("a pattern")
            .to_string()
    };
    for i in 0..3 {
        assert_eq!(
            pat(&w, i),
            pat(&p, i),
            "param {i} is one shape on both verbs"
        );
    }
    for bad in ["yes", "True", "1", "", "true,false"] {
        assert!(
            !jq_test(&pat(&w, 2), bad),
            "the value pattern admits {bad:?}"
        );
    }
    for bad in ["-x", "a b", "Boss", "boss/boss-search-reindex", ""] {
        assert!(
            !jq_test(&pat(&w, 0), bad),
            "the namespace pattern admits {bad:?}"
        );
        assert!(
            !jq_test(&pat(&w, 1), bad),
            "the cronjob pattern admits {bad:?}"
        );
    }
}

/// CONVERGE_OWNS_SPEC is a fact that lives twice (CLAUDE.md §9a): the
/// script's list, and the converge's jq that writes a CronJob's whole
/// /spec. Every CronJob that jq names must be in the script's list, so a
/// second such chore cannot be suspended into a silent undo.
#[test]
fn every_cronjob_whose_spec_the_converge_writes_is_refused_by_name() {
    let jq = std::fs::read_to_string(repo_root().join("infra/cluster/deposit-config-patch.jq"))
        .expect("the deposit configuration's patch");
    assert!(
        jq.contains(r#"{op:"add",path:"/spec",value:$job.spec}"#),
        "the converge still writes the deposit CronJob's whole /spec — if it no longer does, \
         CONVERGE_OWNS_SPEC in {SCRIPT} may shrink: {jq}"
    );
    let re = regex::Regex::new(r#"(?s)\.kind == "CronJob".*?\.metadata\.name == "([a-z0-9-]+)""#)
        .expect("re");
    let names: Vec<String> = re.captures_iter(&jq).map(|c| c[1].to_string()).collect();
    assert!(!names.is_empty(), "the jq names its CronJob: {jq}");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    let owned = script
        .lines()
        .find_map(|l| l.strip_prefix("CONVERGE_OWNS_SPEC=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("CONVERGE_OWNS_SPEC=\"…\" in the script");
    for n in &names {
        assert!(
            owned.split_whitespace().any(|o| o == n),
            "{n}'s spec is written by the converge; {SCRIPT} must refuse it by name"
        );
    }
}
