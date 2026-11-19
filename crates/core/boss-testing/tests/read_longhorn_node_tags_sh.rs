//! Execute the fixed forge reader through a native-shaped kubectl door.
//! No cluster, credentials or node-converge is used by these fixtures.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-node-tags.sh";
const VERB: &str = "infra/ops/verbs/read-longhorn-node-tags.json";
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
while [ "$#" -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
printf '%s\n' "$*" >> "$STUB_ARGV"
[ "${1:-}" = get ] || { echo 'non-GET refused' >&2; exit 90; }
case "$*" in
'get namespace longhorn-system -o json --request-timeout=20s') f=namespace ;;
'get nodes.longhorn.io -n longhorn-system -o json --request-timeout=20s --chunk-size=64') f=nodes ;;
*) echo "unexpected argv: $*" >&2; exit 91 ;;
esac
if [ "${STUB_FAIL:-}" = "$f" ]; then
    echo 'Forbidden: read failed; unrelated diagnostic contains NotFound' >&2
    exit 1
fi
[ "${STUB_EMPTY:-}" != "$f" ] || exit 0
cat "$STUB_STATE/$f.json"
"#;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        write_exec(&dir.join("bin/sudo"), STUB);
        let fixture = Self { dir };
        fixture.state(
            "namespace",
            &json!({"apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":"longhorn-system","uid":"11111111-1111-4111-8111-111111111111"}}),
        );
        fixture.state("nodes", &nodes());
        fixture
    }

    fn state(&self, name: &str, value: &Value) {
        write_file(
            &self.dir.join("state").join(format!("{name}.json")),
            &value.to_string(),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default()
    }
}

fn nodes() -> Value {
    // Typed list: item TypeMeta is intentionally absent.
    json!({"apiVersion":"longhorn.io/v1beta2","kind":"NodeList",
        "metadata":{"resourceVersion":"123"},"items":[{
            "metadata":{"name":"w-1","namespace":"longhorn-system",
                "uid":"22222222-2222-4222-8222-222222222222",
                "annotations":{"node.longhorn.io/default-node-tags":"[\"annotation-only\"]"}},
            "spec":{"tags":["dev-build"],"allowScheduling":false,
                "disks":{"disk-z":{"tags":["disk-only"],"allowScheduling":true},
                         "disk-a":{"tags":[],"allowScheduling":false}}}}]})
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn the_real_reader_distinguishes_node_tags_from_disk_tags_and_annotations() {
    let fixture = Fixture::new("longhorn-node-tags-fields");
    let output = fixture.run(&["w-1"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["state"], "observed");
    assert_eq!(value["node"], "w-1");
    assert_eq!(value["node_uid"], "22222222-2222-4222-8222-222222222222");
    assert_eq!(value["tags"], json!(["dev-build"]));
    assert_eq!(value["allowScheduling"], false);
    assert_eq!(value["disks"][0]["name"], "disk-a");
    assert_eq!(value["disks"][1]["tags"], json!(["disk-only"]));
    assert_eq!(
        fixture.calls(),
        "get namespace longhorn-system -o json --request-timeout=20s\nget nodes.longhorn.io -n longhorn-system -o json --request-timeout=20s --chunk-size=64\n"
    );
    let mut empty = nodes();
    empty["items"][0]["spec"]["tags"] = json!([]);
    fixture.state("nodes", &empty);
    let output = fixture.run(&["w-1"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["tags"],
        json!([])
    );
}

#[test]
fn only_confirmed_complete_absence_reads_not_found() {
    let fixture = Fixture::new("longhorn-node-tags-absent");
    let output = fixture.run(&["w-2"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["state"], "NOT FOUND");
    assert_eq!(value["node"], "w-2");
    assert!(value.get("tags").is_none());
    for env in [
        [("STUB_FAIL", "nodes")],
        [("STUB_FAIL", "namespace")],
        [("STUB_EMPTY", "nodes")],
        [("STUB_EMPTY", "namespace")],
    ] {
        let output = fixture.run(&["w-2"], &env);
        assert_eq!(output.status.code(), Some(1), "{}", text(&output));
        assert!(
            output.stdout.is_empty(),
            "unread must not emit a factual result"
        );
        assert!(text(&output).contains("CANNOT ANSWER"));
    }
}

#[test]
fn kubectl_generic_list_preserves_observed_and_confirmed_absent_nodes() {
    let fixture = Fixture::new("longhorn-node-tags-kubectl-list");
    // kubectl 1.33 printGeneric rewraps collection output as v1/List.
    // Its fixed resource argv and namespace still determine the items.
    let mut value = nodes();
    value["apiVersion"] = json!("v1");
    value["kind"] = json!("List");
    fixture.state("nodes", &value);
    let output = fixture.run(&["w-1"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(observed["tags"], json!(["dev-build"]));
    assert_eq!(observed["allowScheduling"], false);
    let output = fixture.run(&["w-2"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let absent: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(absent["state"], "NOT FOUND");
    assert!(absent.get("tags").is_none());
    value["items"] = json!([]);
    fixture.state("nodes", &value);
    let output = fixture.run(&["w-1"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["state"],
        "NOT FOUND"
    );
}

#[test]
fn malformed_partial_ambiguous_and_missing_fields_never_become_empty_tags() {
    let fixture = Fixture::new("longhorn-node-tags-malformed");
    for mode in [
        "continued",
        "null-item",
        "duplicate",
        "missing-tags",
        "null-tags",
        "wrong-tags",
        "missing-scheduling",
        "wrong-namespace",
        "bad-disk-tags",
        "two-documents",
        "unsupported-list",
    ] {
        let mut value = nodes();
        match mode {
            "continued" => value["metadata"]["continue"] = json!("more"),
            "null-item" => value["items"] = json!([null]),
            "duplicate" => {
                let copy = value["items"][0].clone();
                value["items"].as_array_mut().unwrap().push(copy);
            }
            "missing-tags" => {
                value["items"][0]["spec"]
                    .as_object_mut()
                    .unwrap()
                    .remove("tags");
            }
            "null-tags" => value["items"][0]["spec"]["tags"] = Value::Null,
            "wrong-tags" => value["items"][0]["spec"]["tags"] = json!([false]),
            "missing-scheduling" => {
                value["items"][0]["spec"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowScheduling");
            }
            "wrong-namespace" => value["items"][0]["metadata"]["namespace"] = json!("other"),
            "bad-disk-tags" => {
                value["items"][0]["spec"]["disks"]["disk-a"]["tags"] = json!("dev-build")
            }
            "two-documents" => {}
            "unsupported-list" => value["kind"] = json!("PodList"),
            _ => unreachable!(),
        }
        fixture.state("nodes", &value);
        if mode == "two-documents" {
            write_file(
                &fixture.dir.join("state/nodes.json"),
                &format!("{value}\n{value}\n"),
            );
        }
        let output = fixture.run(&["w-1"], &[]);
        assert_eq!(output.status.code(), Some(1), "{mode}: {}", text(&output));
        assert!(output.stdout.is_empty(), "{mode}: no false observation");
    }
}

#[test]
fn bad_arguments_refuse_before_the_transport_opens() {
    for args in [
        vec![],
        vec!["-A"],
        vec!["w-1;id"],
        vec!["w-1\n"],
        vec!["w-1", "w-2"],
        vec!["../w-1"],
    ] {
        let fixture = Fixture::new(&format!("longhorn-node-tags-args-{:?}", args.len()));
        let output = fixture.run(&args, &[]);
        assert_eq!(output.status.code(), Some(78), "{}", text(&output));
        assert!(fixture.calls().is_empty());
    }
}

#[test]
fn the_registered_door_is_one_fixed_forge_read_with_a_bounded_node_argument() {
    let value: Value =
        serde_json::from_slice(&std::fs::read(repo_root().join(VERB)).unwrap()).unwrap();
    assert_eq!(value["hosts"], json!(["forge"]));
    assert_eq!(value["argv"], json!([SCRIPT, "{1}"]));
    assert_eq!(value["capture"], "separate-streams");
    assert_eq!(
        value["params"],
        json!([{"name":"node","pattern":"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$"}])
    );
    assert!(value.get("requires_approval").is_none());
    let source = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let direct_pattern = source
        .lines()
        .find_map(|line| line.strip_prefix("NODE_ID_RE='"))
        .and_then(|line| line.strip_suffix('\''))
        .expect("the direct script validates its node argument");
    assert_eq!(value["params"][0]["pattern"].as_str(), Some(direct_pattern));
}
