//! Exercise the signed selector-setting door through the real ops wrapper.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-selector-policy.sh";
const SETTING: &str = "allow-empty-disk-selector-volume";
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0;;
'-n docker container inspect '*) exit 1;;
esac
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break;; esac
    shift
done
printf '%s\n' "$*" >> "$STUB_STATE/argv"
[ "$2" != "${STUB_EMPTY:-}" ] || exit 0
case "$1 $2" in
'get settings.longhorn.io') cat "$STUB_STATE/setting.json";;
'get --raw')
    v=$(jq -r .value "$STUB_STATE/setting.json")
    if [ -f "$STUB_STATE/lag" ]; then v=true; rm "$STUB_STATE/lag"; fi
    jq --arg v "${STUB_API_VALUE:-$v}" --argjson a "${STUB_APPLIED:-true}" '.value=$v | .applied=$a' "$STUB_STATE/def.json";;
'get nodes.longhorn.io') cat "$STUB_STATE/nodes.json";;
'patch settings.longhorn.io')
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    test=$(printf '%s' "$p" | jq -r '.[] | select(.op=="test" and .path=="/value") | .value')
    new=$(printf '%s' "$p" | jq -r '.[] | select(.op=="replace" and .path=="/value") | .value')
    [ -z "${STUB_RACE:-}" ] || test=changed
    [ "$test" = "$(jq -r .value "$STUB_STATE/setting.json")" ] || exit 1
    jq --arg v "$new" '.value=$v' "$STUB_STATE/setting.json" > "$STUB_STATE/new.json"
    mv "$STUB_STATE/new.json" "$STUB_STATE/setting.json"
    [ -z "${STUB_API_LAG:-}" ] || touch "$STUB_STATE/lag"
    echo patched;;
*) echo "unexpected: $*" >&2; exit 2;;
esac
"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        for tool in ["jq", "sha256sum"] {
            assert!(
                Command::new(tool)
                    .arg("--version")
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let f = Self(scratch_dir(name));
        std::fs::create_dir_all(f.0.join("bin")).unwrap();
        write_exec(&f.0.join("bin/sudo"), STUB);
        f.put(
            "setting.json",
            json!({"metadata":{"name":SETTING},"value":"true"}),
        );
        f.put("def.json", json!({"id":SETTING,"definition":{"type":"bool","default":"true","readOnly":false,"description":"Allow unselected volumes on tagged disks"}}));
        f.nodes(false);
        f
    }
    fn put(&self, name: &str, value: Value) {
        write_file(&self.0.join(name), &value.to_string());
    }
    fn nodes(&self, durable_default: bool) {
        self.put("nodes.json", json!({"items":[{"metadata":{"name":"w-1","uid":"node-uid"},"spec":{"disks":{
            "default-disk-abc":{"path":"/var/lib/longhorn","tags":if durable_default {vec!["durable"]} else {vec![]}},
            "u-scratch":{"path":"/var/mnt/scratch","tags":["scratch"]}
        }}}]}));
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.0.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.0.join("absent.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.0.join("boss-ops"))
            .env("STUB_STATE", &self.0)
            .env("BOSS_SELECTOR_POLICY_WAIT_S", "0");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }
    fn plan(&self, want: &str) -> String {
        let o = self.run(&["--plan", want], &[]);
        assert!(o.status.success(), "{o:?}");
        String::from_utf8(o.stderr)
            .unwrap()
            .lines()
            .find_map(|s| s.strip_prefix("plan-sha256: ").map(str::to_owned))
            .unwrap()
    }
    fn no_patch(&self) {
        assert!(
            !std::fs::read_to_string(self.0.join("argv"))
                .unwrap_or_default()
                .contains("patch ")
        );
    }
}

#[test]
fn approved_patch_reads_back_and_replay_refuses() {
    let f = Fixture::new("selector-approved");
    let h = f.plan("false");
    f.no_patch();
    let o = f.run(&["false", &h], &[]);
    assert!(o.status.success(), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stdout)
            .contains("allow-empty-disk-selector-volume reads back false")
    );
    let verb: Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/ops/verbs/set-longhorn-selector-policy.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let effect = regex::Regex::new(verb["effect"].as_str().unwrap()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|line| effect.is_match(line))
            .count(),
        1
    );
    assert!(
        !effect
            .is_match("set-longhorn-selector-policy: FAILED — the patch stands but is NOT proven")
    );
    assert!(!effect.is_match("set-longhorn-selector-policy: REFUSED — stale plan"));
    assert_eq!(f.run(&["false", &h], &[]).status.code(), Some(78));
    let h = f.plan("true");
    assert!(f.run(&["true", &h], &[]).status.success());
}

#[test]
fn signed_inventory_and_current_value_are_rechecked() {
    let f = Fixture::new("selector-stale");
    let h = f.plan("false");
    f.nodes(true);
    assert_eq!(f.run(&["false", &h], &[]).status.code(), Some(78));
    f.no_patch();
    let f = Fixture::new("selector-changed-disk");
    let h = f.plan("false");
    let mut n: Value =
        serde_json::from_str(&std::fs::read_to_string(f.0.join("nodes.json")).unwrap()).unwrap();
    n["items"][0]["spec"]["disks"]["u-scratch"]["tags"] = json!(["scratch", "other"]);
    f.put("nodes.json", n);
    assert_eq!(f.run(&["false", &h], &[]).status.code(), Some(78));
    f.no_patch();
    let f = Fixture::new("selector-cas");
    let h = f.plan("false");
    assert_eq!(
        f.run(&["false", &h], &[("STUB_RACE", "1")]).status.code(),
        Some(1)
    );
}

#[test]
fn unknown_reads_definitions_and_unapplied_writes_never_pass() {
    let f = Fixture::new("selector-unknown");
    for resource in ["settings.longhorn.io", "--raw", "nodes.longhorn.io"] {
        let o = f.run(&["--plan", "false"], &[("STUB_EMPTY", resource)]);
        assert_eq!(o.status.code(), Some(1), "{resource}: {o:?}");
        f.no_patch();
    }
    assert_eq!(
        f.run(&["--plan", "false"], &[("STUB_API_VALUE", "false")])
            .status
            .code(),
        Some(1)
    );
    let h = f.plan("false");
    assert_eq!(
        f.run(&["false", &h], &[("STUB_APPLIED", "false")])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn fixed_boolean_bound_and_plan_registry_contract() {
    let f = Fixture::new("selector-bound");
    assert_eq!(
        f.run(&["--plan", "node-drain-policy"], &[]).status.code(),
        Some(78)
    );
    f.no_patch();
    for (name, approved) in [
        ("plan-a-longhorn-selector-policy", false),
        ("set-longhorn-selector-policy", true),
    ] {
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(repo_root().join(format!("infra/ops/verbs/{name}.json")))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["requires_approval"].as_bool().unwrap_or(false), approved);
        assert_eq!(v["params"][0]["one_of"], json!(["true", "false"]));
        if approved {
            assert_eq!(v["plan_verb"], "plan-a-longhorn-selector-policy");
            assert_eq!(v["approvers"], json!(["emp-david"]));
        }
    }
}

#[test]
fn malformed_inventory_and_readonly_definition_refuse_without_patch() {
    let f = Fixture::new("selector-malformed");
    for nodes in [
        json!({"items": []}),
        json!({"items": [{}]}),
        json!({"items": null}),
    ] {
        f.put("nodes.json", nodes);
        assert_eq!(f.run(&["--plan", "false"], &[]).status.code(), Some(1));
        f.no_patch();
    }
    f.nodes(false);
    f.put(
        "def.json",
        json!({"id": SETTING, "definition": {"type": "bool", "default": "true", "readOnly": true}}),
    );
    assert_eq!(f.run(&["--plan", "false"], &[]).status.code(), Some(78));
    f.no_patch();
}

#[test]
fn readback_waits_through_a_lagging_manager_api() {
    let f = Fixture::new("selector-api-lag");
    let h = f.plan("false");
    let o = f.run(
        &["false", &h],
        &[("STUB_API_LAG", "1"), ("BOSS_SELECTOR_POLICY_WAIT_S", "5")],
    );
    assert!(o.status.success(), "{o:?}");
}
