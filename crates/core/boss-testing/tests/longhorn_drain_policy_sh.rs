//! The three ops verbs over Longhorn's node-drain-policy (backlog
//! 46584350): the read-only `plan-a-longhorn-drain-policy`
//! (`infra/forge/longhorn-drain-policy.sh --plan`), the passkey-approved
//! `set-longhorn-drain-policy`, and the read-only
//! `check-longhorn-drain-policy` (`--check`) a daily rule files and a
//! watch rule judges.
//!
//! WHY. w-1 drains on the morning of 2026-09-30 for its second NVMe, and
//! under Longhorn's default policy (block-if-contains-last-replica) the
//! one-replica /work and gate-runner-disk volumes on w-1 hold that drain.
//! w-1 is the only talos-worker, so a second replica could only land on a
//! control plane beside etcd; David chose (2026-09-30 ~05:05Z, review
//! 8599a3a8 M1/M2) to relax the policy for the window and set it back
//! after. Both directions are one verb, bounded to the ONE setting and to
//! the options Longhorn's LIVE definition enumerates — never a list typed
//! in the tree — and the check makes a policy left relaxed an alarm.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: the Setting object (with the managedFields kubectl
//! shows only on `--show-managed-fields`), Longhorn's manager-API answer
//! for the setting behind `get --raw` (the definition as Longhorn v1.11.3
//! served it on 2026-09-30), and the Node, replica and volume lists. Every
//! door call lands in `argv`, so a refusal can be shown to have patched
//! nothing. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-drain-policy.sh";
const DEFAULT: &str = "block-if-contains-last-replica";
const RELAXED: &str = "allow-if-replica-is-stopped";
const WORK_VOL: &str = "pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091";
const GATE_VOL: &str = "pvc-6a0e1c2b-1111-4222-8333-944455556666";
const BOTH_VOL: &str = "pvc-0b0b0b0b-2222-4333-8444-955566667777";
const CP_VOL: &str = "pvc-0c0c0c0c-3333-4444-8555-966677778888";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `setting.json` (the Setting object), `def.json` (Longhorn's API
/// answer; its `value` is the stored one unless STUB_API_VALUE, its
/// `applied` STUB_APPLIED or true), and `nodes.json`, `replicas.json`,
/// `volumes.json` item arrays. A patch applies a JSON patch's `test` +
/// `replace` of /value and records a managedFields write at
/// STUB_PATCH_TIME. STUB_FORBIDDEN=1 answers every read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses the patch; STUB_EMPTY=<word> answers a get
/// whose second word is that with exit 0 and no bytes at all.
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
[ "$1" = get ] && [ "$2" = "${STUB_EMPTY:-}" ] && exit 0
if [ -n "${STUB_FORBIDDEN:-}" ]; then
    echo "Error from server (Forbidden): $2 is forbidden: User \"system:serviceaccount:boss-dev:boss-dev\" cannot get resource \"$2\"" >&2
    exit 1
fi
case "$1 $2" in
"get settings.longhorn.io")
    [ -f "$S/setting.json" ] || { echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1; }
    case "$*" in
    *--show-managed-fields*) cat "$S/setting.json" ;;
    *) jq 'del(.metadata.managedFields)' "$S/setting.json" ;;
    esac
    exit 0 ;;
"get --raw")
    v=$(jq -r .value "$S/setting.json")
    [ -z "${STUB_API_VALUE:-}" ] || v="$STUB_API_VALUE"
    jq --arg v "$v" --argjson a "${STUB_APPLIED:-true}" '.value = $v | .applied = $a' "$S/def.json"; exit 0 ;;
"get nodes")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/nodes.json")"; exit 0 ;;
"get replicas.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"; exit 0 ;;
"get volumes.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/volumes.json")"; exit 0 ;;
"patch settings.longhorn.io")
    p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    want=$(printf '%s' "$p" | jq -r '.[] | select(.op == "test" and .path == "/value") | .value')
    new=$(printf '%s' "$p" | jq -r '.[] | select(.op == "replace" and .path == "/value") | .value')
    cur=$(jq -r .value "$S/setting.json")
    [ -n "$want" ] && [ "$want" = "$cur" ] || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    jq --arg n "$new" --arg t "${STUB_PATCH_TIME:-2026-09-30T15:00:00Z}" '
        .value = $n
        | .metadata.managedFields = ([.metadata.managedFields[]
            | if (.subresource // "") == "" then .fieldsV1 |= del(.["f:value"]) else . end]
            + [{"manager":"kubectl-patch","operation":"Update","apiVersion":"longhorn.io/v1beta2",
                "time":$t,"fieldsType":"FieldsV1","fieldsV1":{"f:value":{}}}])' \
        "$S/setting.json" > "$S/setting.new" && mv "$S/setting.new" "$S/setting.json"
    echo "setting.longhorn.io/node-drain-policy patched"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// Longhorn's manager-API answer for the setting, as v1.11.3 served it
/// on 2026-09-30 (measured from the dev pod; `value` and `applied` are
/// the stub's).
fn definition(read_only: bool) -> Value {
    json!({
        "actions": {}, "applied": true, "id": "node-drain-policy", "name": "node-drain-policy",
        "type": "setting", "value": DEFAULT,
        "definition": {
            "category": "general", "default": DEFAULT, "displayName": "Node Drain Policy",
            "description": "Define the policy to use when a node with the last healthy replica of a volume is drained.\n- **block-for-eviction** Longhorn will automatically evict all replicas and block the drain until eviction is complete.\n- **block-for-eviction-if-contains-last-replica** Longhorn will automatically evict any replicas that don't have a healthy counterpart and block the drain until eviction is complete.\n- **block-if-contains-last-replica** Longhorn will block the drain when the node contains the last healthy replica of a volume.\n- **allow-if-replica-is-stopped** Longhorn will allow the drain when the node contains the last healthy replica of a volume but the replica is stopped. WARNING: possible data loss if the node is removed after draining. Select this option if you want to drain the node and do in-place upgrade/maintenance.\n- **always-allow** Longhorn will allow the drain even though the node contains the last healthy replica of a volume. WARNING: possible data loss if the node is removed after draining. Also possible data corruption if the last replica was running during the draining.\n",
            "options": ["block-for-eviction", "block-for-eviction-if-contains-last-replica",
                        DEFAULT, RELAXED, "always-allow"],
            "readOnly": read_only, "required": true, "type": "string"
        }
    })
}

/// The Setting object: `value`, a status write, and the write of the
/// value at `written` (None: no entry owns `f:value`).
fn setting(value: &str, written: Option<&str>) -> Value {
    let mut fields = vec![json!({
        "manager": "longhorn-manager", "operation": "Update", "subresource": "status",
        "apiVersion": "longhorn.io/v1beta2", "time": "2026-09-30T14:00:00Z",
        "fieldsType": "FieldsV1", "fieldsV1": {"f:status": {"f:applied": {}}}
    })];
    if let Some(t) = written {
        fields.push(json!({
            "manager": "longhorn-manager", "operation": "Update",
            "apiVersion": "longhorn.io/v1beta2", "time": t,
            "fieldsType": "FieldsV1", "fieldsV1": {"f:value": {}}
        }));
    }
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
        "metadata": {"name": "node-drain-policy", "namespace": "longhorn-system",
                     "managedFields": fields},
        "value": value,
        "status": {"applied": true}
    })
}

fn node(name: &str, control_plane: bool) -> Value {
    let mut labels = json!({"kubernetes.io/hostname": name});
    if control_plane {
        labels["node-role.kubernetes.io/control-plane"] = json!("");
    }
    json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": name, "labels": labels}})
}

fn replica(vol: &str, name: &str, node: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"volumeName": vol, "nodeID": node, "diskID": format!("disk-{node}"),
                 "healthyAt": if healthy { "2026-09-12T10:00:00Z" } else { "" },
                 "failedAt": if failed { "2026-09-20T10:00:00Z" } else { "" }}
    })
}

fn volume(name: &str, claim: Option<(&str, &str)>, state: &str) -> Value {
    let ks = claim.map_or(
        json!({}),
        |(ns, pvc)| json!({"namespace": ns, "pvcName": pvc, "pvName": name}),
    );
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "status": {"state": state, "kubernetesStatus": ks}
    })
}

struct Longhorn {
    dir: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "longhorn_drain_policy_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

impl Longhorn {
    /// The estate of 2026-09-30: three control planes and one worker,
    /// w-1. /work and gate-runner-disk keep their one healthy replica on
    /// w-1; a third volume has healthy replicas on w-1 AND cp-1, a fourth
    /// its only one on cp-2 (a control plane), and a failed replica on w-1
    /// is not a healthy one.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.state(
            "setting.json",
            &setting(DEFAULT, Some("2026-08-01T00:00:00Z")),
        );
        l.state("def.json", &definition(false));
        l.state(
            "nodes.json",
            &json!([
                node("cp-1", true),
                node("cp-2", true),
                node("cp-3", true),
                node("w-1", false)
            ]),
        );
        l.state(
            "replicas.json",
            &json!([
                replica(WORK_VOL, "work-r-1", "w-1", true, false),
                replica(GATE_VOL, "gate-r-1", "w-1", true, false),
                replica(BOTH_VOL, "both-r-1", "w-1", true, false),
                replica(BOTH_VOL, "both-r-2", "cp-1", true, false),
                replica(CP_VOL, "cp-r-1", "cp-2", true, false),
                replica(CP_VOL, "cp-r-dead", "w-1", true, true),
            ]),
        );
        l.volumes("attached", "detached");
        l
    }

    fn volumes(&self, work: &str, gate: &str) {
        self.state(
            "volumes.json",
            &json!([
                volume(WORK_VOL, Some(("boss-dev", "boss-dev-work")), work),
                volume(GATE_VOL, Some(("boss-gate", "gate-runner-disk")), gate),
                volume(BOTH_VOL, Some(("boss", "postgres")), "attached"),
                volume(CP_VOL, None, "detached"),
            ]),
        );
    }

    fn state(&self, file: &str, body: &Value) {
        write_file(&self.dir.join("state").join(file), &body.to_string());
    }

    fn value_now(&self) -> String {
        let p = self.dir.join("state/setting.json");
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(p).expect("setting")).expect("json");
        v["value"].as_str().expect("a value").to_string()
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
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_DRAIN_POLICY_WAIT_S", "2")
            .env("BOSS_DRAIN_POLICY_POLL_S", "1")
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

    fn plan(&self, value: &str) -> (String, String) {
        let o = self.run(&["--plan", value], &[]);
        assert!(o.status.success(), "the plan renders: {}", text(&o));
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        let hash = stderr
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("plan-sha256 on stderr: {stderr}"))
            .trim()
            .to_string();
        (String::from_utf8_lossy(&o.stdout).to_string(), hash)
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
    let re = verb("set-longhorn-drain-policy")["effect"]
        .as_str()
        .expect("set-longhorn-drain-policy declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

/// The watch's own pattern and `when`, off its rule file — the regex
/// `ops.judge` reads the check's verdict with (the prune_registry_versions
/// precedent: one fact, two files, compiled here and run over the
/// script's real output). The groups of the last matching line, and
/// whether `when` holds over them — by the handler's own reading: groups
/// that parse as integers are integers.
fn watch_judges(out: &str) -> Option<(BTreeMap<String, String>, bool)> {
    let path =
        boss_testing::dispatcher_rules_dir().join("watch-check-longhorn-drain-policy-daily.toml");
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
        "non_default = 0 OR hours < 24",
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
    let holds = num("non_default") == 0 || num("hours") < 24;
    Some((groups, holds))
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_value_the_options_and_the_volumes_the_policy_decides_for() {
    let l = Longhorn::new("ldp-plan");
    let (plan, hash) = l.plan(RELAXED);
    for want in [
        "setting: settings.longhorn.io/node-drain-policy in longhorn-system",
        &format!("value: {DEFAULT} -> {RELAXED}"),
        &format!("Longhorn's default: {DEFAULT}"),
        "allowed (Longhorn's live definition): block-for-eviction, block-for-eviction-if-contains-last-replica, block-if-contains-last-replica, allow-if-replica-is-stopped, always-allow",
        "Longhorn's words for allow-if-replica-is-stopped: Longhorn will allow the drain when the node contains the last healthy replica of a volume but the replica is stopped. WARNING: possible data loss",
        "test that value is still block-if-contains-last-replica, replace it with allow-if-replica-is-stopped",
        "is NOT Longhorn's default",
        "check-longhorn-drain-policy",
        "== longhorn volumes whose only healthy replica is on a worker (2) ==",
        &format!(
            "volume {WORK_VOL} (pvc boss-dev/boss-dev-work): only healthy replica on worker w-1"
        ),
        &format!(
            "volume {GATE_VOL} (pvc boss-gate/gate-runner-disk): only healthy replica on worker w-1"
        ),
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    for not in [BOTH_VOL, CP_VOL] {
        assert!(
            !plan.contains(not),
            "a volume with a healthy replica off the worker is not one the policy decides for: {plan}"
        );
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(l.patches().is_empty(), "the plan patches nothing");
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to the volumes' states: a gate's disk
    // attaches and detaches many times an hour, and a plan that signed it
    // could never be run.
    assert_eq!(l.plan(RELAXED), (plan.clone(), hash.clone()));
    l.volumes("detached", "attached");
    let o = l.run(&["--plan", RELAXED], &[]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("live state attached"),
        "the states ride stderr: {}",
        text(&o)
    );
    assert_eq!(l.plan(RELAXED).1, hash, "a volume's state is not signed");
}

#[test]
fn the_plan_refuses_what_the_bound_does_not_admit_and_patches_nothing() {
    let l = Longhorn::new("ldp-bounds");
    for args in [
        vec!["--plan", "Allow-If-Replica-Is-Stopped"],
        vec!["--plan", "-x"],
        vec!["--plan", "allow if"],
        vec!["--plan"],
        vec!["--plan", RELAXED, "extra"],
        vec![RELAXED],
        vec![RELAXED, "not-a-hash"],
        vec!["--check", "extra"],
    ] {
        let o = l.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(l.calls().is_empty(), "no door opened: {:?}", l.calls());

    // A word Longhorn's live definition does not enumerate: the bound is
    // Longhorn's list, read, and the refusal names it.
    let o = l.run(&["--plan", "allow-everything"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("not a node-drain-policy option") && out.contains("always-allow"),
        "{out}"
    );
    // The value that already stands.
    let o = l.run(&["--plan", DEFAULT], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("already"), "{out}");
    // A read-only definition.
    l.state("def.json", &definition(true));
    let o = l.run(&["--plan", RELAXED], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("read-only"), "{out}");
    assert!(l.patches().is_empty(), "{:?}", l.calls());
}

#[test]
fn a_read_that_could_not_look_cannot_answer_and_renders_no_plan() {
    let l = Longhorn::new("ldp-dark");
    let o = l.run(&["--plan", RELAXED], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");

    // An answer carrying no document at all — exit 0, no bytes — is not
    // an empty cluster and not a default (d96e38ab).
    for word in [
        "settings.longhorn.io",
        "--raw",
        "nodes",
        "replicas.longhorn.io",
        "volumes.longhorn.io",
    ] {
        let o = l.run(&["--plan", RELAXED], &[("STUB_EMPTY", word)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{word}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{word}: {out}");
        assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    }

    // The stored setting and Longhorn's API disagree on what stands.
    let o = l.run(&["--plan", RELAXED], &[("STUB_API_VALUE", "always-allow")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("disagree"), "{out}");

    // A Longhorn that holds no such setting.
    std::fs::remove_file(l.dir.join("state/setting.json")).expect("rm");
    let o = l.run(&["--plan", RELAXED], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("NotFound"), "{out}");
    assert!(l.patches().is_empty(), "{:?}", l.calls());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_there_and_back() {
    let l = Longhorn::new("ldp-write");
    let (plan, hash) = l.plan(RELAXED);
    let o = l.run(&[RELAXED, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let patches = l.patches();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains("patch settings.longhorn.io node-drain-policy -n longhorn-system --type=json"),
        "{p}"
    );
    assert!(
        p.contains(r#"{"op":"test","path":"/value","value":"block-if-contains-last-replica"}"#)
            && p.contains(
                r#"{"op":"replace","path":"/value","value":"allow-if-replica-is-stopped"}"#
            ),
        "a compare-and-set on value and nothing else: {p}"
    );
    assert_eq!(l.value_now(), RELAXED);
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains(&format!("was {DEFAULT}")),
        "names what it was: {hits:?}"
    );

    // Applied once, the same approval is spent.
    let o = l.run(&[RELAXED, &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(l.patches().len(), 1, "no second patch");

    // And back, by the same verb, with a plan that says so.
    let (back, hash) = l.plan(DEFAULT);
    assert!(
        back.contains(&format!("value: {RELAXED} -> {DEFAULT}"))
            && back.contains("is Longhorn's default — this puts the setting back"),
        "{back}"
    );
    let o = l.run(&[DEFAULT, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert_eq!(l.value_now(), DEFAULT);
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan() {
    let l = Longhorn::new("ldp-drift");
    let (_, hash) = l.plan(RELAXED);
    // A volume's last replica moved to the worker after the plan was
    // signed: the plan the passkey saw no longer describes the cluster.
    l.state(
        "replicas.json",
        &json!([
            replica(WORK_VOL, "work-r-1", "w-1", true, false),
            replica(GATE_VOL, "gate-r-1", "w-1", true, false),
            replica(BOTH_VOL, "both-r-1", "w-1", true, false),
        ]),
    );
    let o = l.run(&[RELAXED, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    let o = l.run(&[RELAXED, &"0".repeat(64)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(l.patches().is_empty(), "{:?}", l.calls());
    assert_eq!(l.value_now(), DEFAULT);
}

#[test]
fn a_patch_the_server_refuses_or_longhorn_never_applies_is_not_an_effect() {
    let l = Longhorn::new("ldp-refused");
    let (_, hash) = l.plan(RELAXED);
    let o = l.run(&[RELAXED, &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(l.value_now(), DEFAULT);

    // The stored value moves and Longhorn never reports it applied.
    let o = l.run(&[RELAXED, &hash], &[("STUB_APPLIED", "false")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("NOT proven") && out.contains("applied false"),
        "{out}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

// ------------------------------------------------------------------ check

#[test]
fn the_check_reads_the_default_as_nothing_to_alarm_and_writes_nothing() {
    let l = Longhorn::new("ldp-check-default");
    let o = l.run(&["--check"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains(
            "check-longhorn-drain-policy: READ — node-drain-policy block-if-contains-last-replica, default block-if-contains-last-replica, non_default 0, hours 0"
        ),
        "{out}"
    );
    let (g, holds) = watch_judges(&out).expect("the watch reads a verdict");
    assert_eq!(g["non_default"], "0");
    assert!(holds, "the default is no alarm: {g:?}");
    assert!(l.patches().is_empty());
    assert!(
        l.calls()
            .iter()
            .all(|c| c.starts_with("get ") && !c.contains("patch")),
        "the check only reads: {:?}",
        l.calls()
    );
}

#[test]
fn a_policy_left_relaxed_past_24_hours_is_what_the_watch_alarms_on() {
    let l = Longhorn::new("ldp-check-relaxed");
    let (_, hash) = l.plan(RELAXED);
    let o = l.run(
        &[RELAXED, &hash],
        &[("STUB_PATCH_TIME", "2026-09-30T15:00:00Z")],
    );
    assert!(o.status.success(), "{}", text(&o));
    // 2026-09-30T15:00:00Z is 1790780400 (`date -u -d … +%s`).
    let at = |h: i64| (1_790_780_400 + h * 3600).to_string();

    // Inside the window: relaxed, and no alarm.
    let o = l.run(&["--check"], &[("BOSS_DRAIN_POLICY_NOW", &at(3))]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("NON-DEFAULT") && out.contains("since 2026-09-30T15:00:00Z"),
        "{out}"
    );
    assert!(
        out.contains("written by kubectl-patch"),
        "names who wrote it: {out}"
    );
    let (g, holds) = watch_judges(&out).expect("a verdict");
    assert_eq!((g["non_default"].as_str(), g["hours"].as_str()), ("1", "3"));
    assert!(holds, "three hours into the window is no alarm");

    // Forgotten: a day and more, and the watch alarms.
    for h in [24, 33] {
        let o = l.run(&["--check"], &[("BOSS_DRAIN_POLICY_NOW", &at(h))]);
        let out = text(&o);
        assert!(o.status.success(), "{out}");
        assert!(
            out.contains("plan-a-longhorn-drain-policy block-if-contains-last-replica"),
            "the line names the way back: {out}"
        );
        let (g, holds) = watch_judges(&out).expect("a verdict");
        assert_eq!(g["hours"], h.to_string());
        assert!(!holds, "{h} hours relaxed is the alarm: {g:?}");
    }
}

#[test]
fn a_relaxed_policy_whose_age_cannot_be_read_cannot_answer() {
    let l = Longhorn::new("ldp-check-dark");
    // Non-default, and no entry owns `f:value`.
    l.state("setting.json", &setting(RELAXED, None));
    let o = l.run(&["--check"], &[]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "the watch alarms on a failed run: {out}"
    );
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(
        watch_judges(&out).is_none(),
        "no verdict line from a run that could not read: {out}"
    );

    // A dark Longhorn API is no default either.
    let l = Longhorn::new("ldp-check-noapi");
    let o = l.run(&["--check"], &[("STUB_EMPTY", "--raw")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(watch_judges(&out).is_none(), "{out}");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("set-longhorn-drain-policy");
    let p = verb("plan-a-longhorn-drain-policy");
    let c = verb("check-longhorn-drain-policy");
    for (name, v) in [
        ("set-longhorn-drain-policy", &w),
        ("plan-a-longhorn-drain-policy", &p),
        ("check-longhorn-drain-policy", &c),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
    }
    assert_eq!(p["argv"], json!([SCRIPT, "--plan", "{1}"]));
    assert_eq!(c["argv"], json!([SCRIPT, "--check"]));
    assert_eq!(c["params"], json!([]), "the check takes nothing");
    for (name, v) in [("plan", &p), ("check", &c)] {
        assert!(
            !v["about"].as_str().unwrap_or_default().contains("MUTATING"),
            "{name} is read-only"
        );
        assert!(v.get("requires_approval").is_none(), "{name}");
    }
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-longhorn-drain-policy");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["argv"], json!([SCRIPT, "{1}", "{2}"]));
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 90 + 60,
        "the verb's timeout outlasts the script's own 90 s read-back and its reads"
    );
    // The value pattern is only a shape: the script's bound is Longhorn's
    // live list. No setting is a param.
    let pat = w["params"][0]["pattern"].as_str().expect("a pattern");
    assert_eq!(p["params"][0]["pattern"], w["params"][0]["pattern"]);
    for ok in [DEFAULT, RELAXED, "always-allow"] {
        assert!(jq_test(pat, ok), "{ok}");
    }
    for bad in [
        "-x",
        "a b",
        "Allow",
        "allow-",
        "allow,always-allow",
        "node-drain-policy=always-allow",
        "",
    ] {
        assert!(!jq_test(pat, bad), "the value pattern admits {bad:?}");
    }
    assert!(
        !std::fs::read_to_string(repo_root().join(SCRIPT))
            .expect("the script")
            .contains("OPTIONS=\"block"),
        "the options are read from Longhorn, never typed into the script"
    );
}
