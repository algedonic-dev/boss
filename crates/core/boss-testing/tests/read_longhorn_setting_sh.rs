//! The read-only ops verb `read-longhorn-setting`
//! (`infra/forge/longhorn-setting.sh <setting>`; backlog e14a472d).
//!
//! WHY. Option C of e14a472d tags w-1 and w-2 `dev-build` in Longhorn so
//! the dev-build workers' class can place replicas only there. Review
//! 0e863731 of car 1 asked that, BEFORE the node-converge that applies
//! those tags, the live setting `allow-empty-node-selector-volume` be
//! READ: if it is false, a node carrying any tag stops taking replicas of
//! a volume whose class names no selector — /work and the gate disk — so
//! tagging the two workers would quietly move every new untagged replica
//! onto the control planes. The tree never sets it and Longhorn's default
//! is true, but a default is not a reading, and no door read it:
//! `check-longhorn-drain-policy` reads only `node-drain-policy`, fixed in
//! a script that also carries a passkey-approved write. So this is a
//! sibling verb that READS, and only reads, one setting from a fixed
//! allow-list — the drain-policy script and its daily check are untouched.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc` (the stand-in of longhorn_drain_policy_sh.rs, cut
//! down to the two reads): the Setting object, and Longhorn's manager-API
//! answer behind `get --raw`. The definition's shape is the one that
//! file measured from Longhorn v1.11.3 on 2026-09-30; this setting's
//! words and default are Longhorn's source's, NOT read from the live
//! cluster — reading them live is what the verb is for. Every door call
//! lands in `argv`, so the tests can show the verb only ever ran `get`.
//! Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-setting.sh";
const VERB: &str = "read-longhorn-setting";
const SETTING: &str = "allow-empty-node-selector-volume";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `setting.json` (the Setting object; a get naming another setting is
/// NotFound) and `def.json` (Longhorn's API answer, served only for
/// `/v1/settings/<its id>`; its `value` is the stored one unless
/// STUB_API_VALUE). STUB_FORBIDDEN=1 answers every read as RBAC does;
/// STUB_EMPTY=<word> answers a get whose second word is that with exit 0
/// and no bytes at all. Anything but those two gets is exit 2, named.
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
    if [ ! -f "$S/setting.json" ] || [ "$(jq -r .metadata.name "$S/setting.json")" != "$3" ]; then
        echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1
    fi
    cat "$S/setting.json"; exit 0 ;;
"get --raw")
    id=$(jq -r .id "$S/def.json")
    case "$3" in
    */proxy/v1/settings/"$id") ;;
    *) echo "Error from server (NotFound): the server could not find the requested resource" >&2; exit 1 ;;
    esac
    v=$(jq -r .value "$S/setting.json")
    [ -z "${STUB_API_VALUE:-}" ] || v="$STUB_API_VALUE"
    jq --arg v "$v" '.value = $v' "$S/def.json"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// Longhorn's manager-API answer for the setting: the v1.11.3 shape, with
/// this setting's words and default as Longhorn's source declares them.
fn definition(id: &str) -> Value {
    json!({
        "actions": {}, "applied": true, "id": id, "name": id,
        "type": "setting", "value": "true",
        "definition": {
            "category": "scheduling", "default": "true",
            "displayName": "Allow Scheduling Empty Node Selector Volumes To Any Node",
            "description": "Allow replica of the volume without node selector to be scheduled on node with tags, default true\n",
            "readOnly": false, "required": true, "type": "bool"
        }
    })
}

fn setting(name: &str, value: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "value": value,
        "status": {"applied": true}
    })
}

struct Longhorn {
    dir: PathBuf,
}

fn needs_tools() {
    let ok = Command::new("jq")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok,
        "read_longhorn_setting_sh: no jq on this box — the gate image has it, and a test \
         that cannot run must fail, never pass by returning early"
    );
}

impl Longhorn {
    /// A Longhorn whose allow-empty-node-selector-volume stands at `value`.
    fn new(name: &str, value: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.state("setting.json", &setting(SETTING, value));
        l.state("def.json", &definition(SETTING));
        l
    }

    fn state(&self, file: &str, body: &Value) {
        write_file(&self.dir.join("state").join(file), &body.to_string());
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
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn verdict(out: &str) -> Option<&str> {
    out.lines().rfind(|l| l.contains(": READ — "))
}

fn script_source() -> String {
    std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script")
}

/// The script's allow-list: its one `ALLOWED="…"` line, space-separated.
fn script_allow_list() -> Vec<String> {
    let src = script_source();
    let line = src
        .lines()
        .find_map(|l| l.strip_prefix("ALLOWED=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("the script carries one ALLOWED=\"…\" line");
    line.split_whitespace().map(str::to_string).collect()
}

// ------------------------------------------------------------------- read

#[test]
fn the_read_names_the_value_longhorns_default_and_a_verdict_and_only_gets() {
    let l = Longhorn::new("rls-default", "true");
    let o = l.run(&[SETTING], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    for want in [
        "allow-empty-node-selector-volume is true (settings.longhorn.io, and Longhorn's API agrees; applied true); Longhorn's default is true; type bool",
        "Longhorn's words for allow-empty-node-selector-volume: Allow replica of the volume without node selector to be scheduled on node with tags, default true",
    ] {
        assert!(out.contains(want), "the read says {want:?}:\n{out}");
    }
    assert_eq!(
        verdict(&out),
        Some(
            "read-longhorn-setting: READ — allow-empty-node-selector-volume true, default true, non_default 0"
        ),
        "{out}"
    );
    let calls = l.calls();
    assert_eq!(calls.len(), 2, "two reads: {calls:?}");
    assert!(
        calls.iter().all(|c| c.starts_with("get ")),
        "the verb only reads: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with(
            "get --raw /api/v1/namespaces/longhorn-system/services/longhorn-backend:9500/proxy/v1/settings/allow-empty-node-selector-volume "
        )),
        "Longhorn's definition through the API server's service proxy: {calls:?}"
    );
}

#[test]
fn a_value_that_is_not_longhorns_default_reads_non_default_1() {
    // The case review 0e863731 asked about: false means a tagged node
    // takes no replica of an untagged volume.
    let l = Longhorn::new("rls-false", "false");
    let o = l.run(&[SETTING], &[]);
    let out = text(&o);
    assert!(
        o.status.success(),
        "a reading is an answer either way: {out}"
    );
    assert!(
        out.contains(
            "NON-DEFAULT — allow-empty-node-selector-volume is false, not Longhorn's default true"
        ),
        "{out}"
    );
    assert_eq!(
        verdict(&out),
        Some(
            "read-longhorn-setting: READ — allow-empty-node-selector-volume false, default true, non_default 1"
        ),
        "{out}"
    );
    assert!(l.calls().iter().all(|c| c.starts_with("get ")));
}

#[test]
fn a_setting_off_the_allow_list_is_refused_before_any_door_opens() {
    let l = Longhorn::new("rls-bounds", "true");
    for args in [
        vec![],
        vec!["node-drain-policy"],
        vec!["Allow-Empty-Node-Selector-Volume"],
        vec!["allow-empty-node-selector-volume=false"],
        vec!["-x"],
        vec![SETTING, "extra"],
        vec!["--check"],
    ] {
        let o = l.run(&args, &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {out}");
        assert!(out.contains("REFUSED"), "{args:?}: {out}");
        assert!(verdict(&out).is_none(), "{args:?}: {out}");
    }
    assert!(l.calls().is_empty(), "no door opened: {:?}", l.calls());
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    let l = Longhorn::new("rls-dark", "true");
    let o = l.run(&[SETTING], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );
    assert!(verdict(&out).is_none(), "{out}");

    // An answer carrying no document at all — exit 0, no bytes — is not a
    // default (d96e38ab).
    for word in ["settings.longhorn.io", "--raw"] {
        let o = l.run(&[SETTING], &[("STUB_EMPTY", word)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{word}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{word}: {out}");
        assert!(verdict(&out).is_none(), "{word}: {out}");
    }

    // The stored setting and Longhorn's API disagree on what stands.
    let o = l.run(&[SETTING], &[("STUB_API_VALUE", "false")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("disagree"),
        "{out}"
    );
    assert!(verdict(&out).is_none(), "{out}");

    // A definition with no default is no definition.
    let mut def = definition(SETTING);
    def["definition"]
        .as_object_mut()
        .expect("an object")
        .remove("default");
    l.state("def.json", &def);
    let o = l.run(&[SETTING], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");

    // A Longhorn that holds no such setting.
    l.state("setting.json", &setting("some-other-setting", "true"));
    let o = l.run(&[SETTING], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("NotFound"),
        "{out}"
    );
    assert!(
        l.calls().iter().all(|c| c.starts_with("get ")),
        "{:?}",
        l.calls()
    );
}

// ------------------------------------------------------------------ verb

#[test]
fn the_verb_file_offers_exactly_the_scripts_allow_list_and_reads_only() {
    let p = repo_root().join(format!("infra/ops/verbs/{VERB}.json"));
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    assert_eq!(v["hosts"], json!(["forge"]), "the forge's ops_kubectl door");
    assert_eq!(v["argv"], json!([SCRIPT, "{1}"]));
    assert!(v["timeout"].as_u64().is_some(), "declares its timeout");
    let about = v["about"].as_str().unwrap_or_default();
    assert!(
        about.starts_with("READ-ONLY") && !about.contains("MUTATING"),
        "{about}"
    );
    assert!(v.get("requires_approval").is_none());

    // One fact, two files (CLAUDE.md §9a): the packet may name only a
    // word the script itself admits, and the script admits only what the
    // verb offers.
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 1, "one param: {params:?}");
    assert_eq!(params[0]["name"], "setting");
    assert!(params[0].get("pattern").is_none(), "one_of, not a shape");
    let offered: Vec<String> = params[0]["one_of"]
        .as_array()
        .expect("a one_of list")
        .iter()
        .map(|w| w.as_str().expect("a word").to_string())
        .collect();
    assert_eq!(
        offered,
        script_allow_list(),
        "the verb's one_of and the script's ALLOWED drifted"
    );
    assert!(
        offered.iter().any(|w| w == SETTING),
        "the setting e14a472d needs read"
    );
    // Backlog 46328c3a (design 3c475a19 car 2): when this setting is
    // false, a TAGGED disk takes no replica of a volume with no disk
    // selector — every durable volume today. So it says whether w-1's
    // scratch disk is closed to durable replicas or only watched for
    // them, and it is read before the first disk tag is written.
    assert!(
        offered
            .iter()
            .any(|w| w == "allow-empty-disk-selector-volume"),
        "the setting 46328c3a needs read before any disk is tagged"
    );
    // Backlog 2b7c78f8: the instance manager on every Longhorn node
    // requests this percentage of the node's CPU, and on w-1 that is CPU
    // the capacity record never charged. Its default is a reading too.
    assert!(
        offered
            .iter()
            .any(|w| w == "guaranteed-instance-manager-cpu"),
        "the setting 2b7c78f8 needs read to charge w-1 what Longhorn holds"
    );
}

#[test]
fn the_script_runs_the_kubectl_door_only_to_get() {
    // Every use of the door is a `get`: the script has no write in it to
    // widen, and a future one has to change this line to land.
    let src = script_source();
    let uses: Vec<&str> = src
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains("$K "))
        .collect();
    assert!(!uses.is_empty(), "the script reads through the door");
    for l in &uses {
        assert!(l.contains("$K get "), "a door call that is not a get: {l}");
    }
    assert!(
        !src.contains("SETTING=node-drain-policy"),
        "a sibling read, not the drain-policy script"
    );
}
