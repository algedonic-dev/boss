//! The two ops verbs that set the tags of ONE Longhorn disk (backlog
//! aa650aaf): the read-only `plan-a-longhorn-disk-tags`
//! (`infra/forge/longhorn-disk-tags.sh --plan`) and the passkey-approved
//! `set-longhorn-disk-tags`.
//!
//! WHY. Design 3c475a19 (disk placement for the incoming hardware) places
//! the system of record on w-3's u-postgres tagged [durable, postgres],
//! durable replicas on u-data [durable], and tags every current default
//! disk durable; car 4 then selects with diskSelector [postgres]. On
//! origin/main 6b2bd7c48 (2026-11-12T15:00Z, triage run c9fd27a7) the only
//! writer of a disk's tags was add-longhorn-disk, which always writes [],
//! and reregister-longhorn-disk, which re-adds the tags a record already
//! had. No door set a disk's tags, so the design could not be carried out.
//!
//! THE BOUND each test below pins: the node is a registry talos-worker
//! the cluster agrees on; the disk is one already in the node's
//! spec.disks, either `u-<volume>` at /var/mnt/<volume> or the node's ONE
//! `default-disk-<hex>` at /var/lib/longhorn; the tags are one of a fixed
//! set of spellings the script holds; Longhorn reads the disk Ready and
//! Schedulable; the tags differ from what the disk carries now; and no
//! replica on the disk belongs to a volume whose diskSelector the new
//! tags would no longer hold. The write is one compare-and-set patch, and
//! its effect is read back: the tags as planned, every other field of the
//! disk and every other disk unchanged, and Longhorn still reading the
//! disk Ready and Schedulable under the same diskUUID.
//!
//! HOW THIS IS MEASURED. The script runs for real. `sudo` is stubbed on
//! PATH as a small cluster behind the kubectl door — the Kubernetes Node,
//! the Longhorn node, the replica and volume lists — and the estate
//! registry is served from a file. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-disk-tags.sh";
const GIB: u64 = 1024 * 1024 * 1024;
/// The stub's state file for the Longhorn setting that decides whether a
/// TAGGED disk takes replicas of a volume with no diskSelector (review
/// 7469ab28 B1).
const EMPTY_SEL: &str = "setting-allow-empty-disk-selector-volume";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `ip-<node>` its InternalIP, `cp-<node>` a control-plane label,
/// `lh-<node>.json` the Longhorn node, `replicas.json` and `volumes.json`
/// the items of those lists (default none), and `setting-<name>` the value
/// of a settings.longhorn.io (absent is NotFound; `Cluster::new` writes
/// allow-empty-disk-selector-volume as Longhorn's default, true).
/// STUB_SETTING_FORBIDDEN=1 answers the setting read alone as RBAC does.
/// A patch of nodes.longhorn.io applies the JSON patch's `test` of
/// /spec/disks and its one `replace` (or `add`) of
/// /spec/disks/<disk>/tags. STUB_DISK bends ONE fact of what Longhorn then
/// holds, one per read-back conjunct of the script: `notready` (the disk
/// Ready and Schedulable False), `wrongtags` (the tags are not the
/// planned ones), `otherchanged` (another disk's spec moved with it),
/// `samechanged` (another field of the same disk moved), `nostatus` (no
/// status for the disk) and `uuidchanged` (a diskUUID other than the
/// plan's).
/// STUB_FORBIDDEN=1 answers every longhorn.io read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses the patch.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
tool=""
while [ $# -gt 0 ]; do
    case "$1" in
    --kubeconfig=/kc) tool=kubectl; shift; break ;;
    esac
    shift
done
[ "$tool" = kubectl ] || { echo "stub: unexpected call: $*" >&2; exit 2; }
case "$1 $2" in
"get node")
    n="$3"
    [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
    labels='"kubernetes.io/hostname":"'"$n"'"'
    [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
    printf '{"kind":"Node","metadata":{"name":"%s","uid":"uid-%s","labels":{%s}},"spec":{},"status":{"addresses":[{"type":"InternalIP","address":"%s"}],"conditions":[{"type":"Ready","status":"True"}]}}\n' "$n" "$n" "$labels" "$(cat "$S/ip-$n")"
    exit 0 ;;
"get nodes.longhorn.io"|"get replicas.longhorn.io"|"get volumes.longhorn.io"|"get settings.longhorn.io")
    if [ -n "${STUB_FORBIDDEN:-}" ]; then
        echo "Error from server (Forbidden): $2 is forbidden" >&2
        exit 1
    fi
    case "$2" in
    settings.longhorn.io)
        if [ -n "${STUB_SETTING_FORBIDDEN:-}" ]; then
            echo "Error from server (Forbidden): settings.longhorn.io \"$3\" is forbidden" >&2
            exit 1
        fi
        f="$S/setting-$3"
        [ -f "$f" ] || { echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1; }
        printf '{"apiVersion":"longhorn.io/v1beta2","kind":"Setting","metadata":{"name":"%s","namespace":"longhorn-system"},"value":"%s"}\n' "$3" "$(cat "$f")" ;;
    nodes.longhorn.io)
        f="$S/lh-$3.json"
        [ -f "$f" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
        cat "$f" ;;
    replicas.longhorn.io)
        printf '{"apiVersion":"longhorn.io/v1beta2","kind":"List","items":%s}\n' "$(cat "$S/replicas.json" 2>/dev/null || echo '[]')" ;;
    volumes.longhorn.io)
        printf '{"apiVersion":"longhorn.io/v1beta2","kind":"List","items":%s}\n' "$(cat "$S/volumes.json" 2>/dev/null || echo '[]')" ;;
    esac
    exit 0 ;;
"patch nodes.longhorn.io")
    n="$3"; f="$S/lh-$n.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    [ "$(printf '%s' "$p" | jq 'length')" = 2 ] || { echo "stub: a patch of two operations is expected: $p" >&2; exit 2; }
    want=$(printf '%s' "$p" | jq -c '.[0] | select(.op == "test" and .path == "/spec/disks") | .value')
    [ -n "$want" ] && jq -e --argjson w "$want" '.spec.disks == $w' "$f" > /dev/null \
        || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    op=$(printf '%s' "$p" | jq -r '.[1].op')
    path=$(printf '%s' "$p" | jq -r '.[1].path')
    val=$(printf '%s' "$p" | jq -c '.[1].value')
    key=${path#/spec/disks/}; key=${key%/tags}
    [ "$path" = "/spec/disks/$key/tags" ] || { echo "stub: not a tags path: $path" >&2; exit 2; }
    case "$op" in
    replace) jq -e --arg k "$key" '.spec.disks[$k] | has("tags")' "$f" > /dev/null \
                 || { echo "The request is invalid: replace of a missing member" >&2; exit 1; } ;;
    add) ;;
    *) echo "stub: unexpected op $op" >&2; exit 2 ;;
    esac
    jq --arg k "$key" --argjson v "$val" --arg mode "${STUB_DISK:-}" '
        .spec.disks[$k].tags = $v
        | if $mode == "wrongtags" then .spec.disks[$k].tags = ["scratch"] else . end
        | if $mode == "samechanged" then .spec.disks[$k].allowScheduling = false else . end
        | if $mode == "otherchanged"
          then .spec.disks |= with_entries(if .key == $k then . else .value.allowScheduling = false end)
          else . end
        | if $mode == "nostatus" then del(.status.diskStatus[$k]) else . end
        | if $mode == "uuidchanged" then .status.diskStatus[$k].diskUUID = "0e0e0e0e-1111-4222-8333-444455556666" else . end
        | if $mode == "notready"
          then .status.diskStatus[$k].conditions = [
                 {type: "Ready", status: "False", reason: "DiskFilesystemChanged", message: "disk \($k) has a different file system than the one recorded"},
                 {type: "Schedulable", status: "False", reason: "DiskNotReady"}]
          else . end' "$f" > "$f.new" && mv "$f.new" "$f"
    echo "node.longhorn.io/$n patched"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

struct Cluster {
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
            "longhorn_disk_tags_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn registry_row(id: &str, address: &str, role: &str) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": false
    })
}

fn disk_spec(path: &str, reserved: u64) -> Value {
    json!({"allowScheduling": true, "evictionRequested": false, "path": path,
           "storageReserved": reserved, "tags": [], "diskType": "filesystem"})
}

fn disk_status(uuid: &str) -> Value {
    json!({"diskUUID": uuid, "storageMaximum": 900 * GIB, "storageScheduled": 0,
           "storageAvailable": 880 * GIB,
           "conditions": [{"type": "Ready", "status": "True"},
                          {"type": "Schedulable", "status": "True"}]})
}

/// w-3 as design 3c475a19 leaves it after add-longhorn-disk ran twice: its
/// default disk on the system disk, and u-postgres and u-data untagged.
fn w3() -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": "w-3", "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {
                     "default-disk-3c3c3c3c3c3c": disk_spec("/var/lib/longhorn/", 35 * GIB),
                     "u-postgres": disk_spec("/var/mnt/postgres", 0),
                     "u-data": disk_spec("/var/mnt/data", 0)}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {
                       "default-disk-3c3c3c3c3c3c": disk_status("33333333-1111-4222-8333-444455556666"),
                       "u-postgres": disk_status("0b0b0b0b-1111-4222-8333-444455556666"),
                       "u-data": disk_status("0d0d0d0d-1111-4222-8333-444455556666")}}
    })
}

/// w-1 with its one default disk.
fn w1() -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": "w-1", "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {"default-disk-9a8b7c6d5e4f": disk_spec("/var/lib/longhorn/", 35 * GIB)}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {"default-disk-9a8b7c6d5e4f": disk_status("bf045701-eeec-47ec-899b-4c987ef122ce")}}
    })
}

fn replica(name: &str, volume: &str, node: &str, disk_uuid: &str, path: &str) -> Value {
    json!({"metadata": {"name": name, "namespace": "longhorn-system"},
           "spec": {"volumeName": volume, "nodeID": node, "diskID": disk_uuid, "diskPath": path}})
}

fn volume(name: &str, disk_selector: Value) -> Value {
    json!({"metadata": {"name": name, "namespace": "longhorn-system"},
           "spec": {"numberOfReplicas": 2, "diskSelector": disk_selector}})
}

impl Cluster {
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("longhorn-disk-tags-{name}"));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let c = Cluster { dir };
        write_file(
            &c.dir.join("nodes.json"),
            &json!({"data": [
                registry_row("cp-1", "10.20.0.11", "talos-control-plane"),
                registry_row("w-1", "10.20.0.14", "talos-worker"),
                registry_row("w-3", "10.20.0.17", "talos-worker"),
            ]})
            .to_string(),
        );
        c.state("ip-cp-1", "10.20.0.11");
        c.state("cp-cp-1", "");
        c.state("ip-w-1", "10.20.0.14");
        c.state("ip-w-3", "10.20.0.17");
        c.lh("w-3", w3());
        c.lh("w-1", w1());
        c.state(EMPTY_SEL, "true");
        c
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn unstate(&self, file: &str) {
        std::fs::remove_file(self.dir.join("state").join(file)).expect("a state file to remove");
    }

    fn lh(&self, node: &str, v: Value) {
        self.state(&format!("lh-{node}.json"), &v.to_string());
    }

    fn lh_now(&self, node: &str) -> Value {
        let p = self.dir.join("state").join(format!("lh-{node}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).expect("longhorn node")).expect("json")
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
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_LONGHORN_DISK_WAIT_S", "2")
            .env("BOSS_LONGHORN_DISK_POLL_S", "1")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default()
    }

    fn patches(&self) -> Vec<String> {
        self.argv()
            .lines()
            .filter(|l| l.contains("kubectl --kubeconfig=/kc patch "))
            .map(str::to_string)
            .collect()
    }

    fn plan(&self, node: &str, disk: &str, tags: &str) -> (String, String) {
        let o = self.run(&["--plan", node, disk, tags], &[]);
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

    /// A refusal: exit 78, no patch, no plan on stdout, and the text it said.
    fn refused(&self, args: &[&str]) -> String {
        let o = self.run(args, &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{args:?} is refused: {out}");
        assert!(
            self.patches().is_empty(),
            "a refusal patches nothing: {}",
            self.argv()
        );
        assert!(
            !String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        out
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

/// The lines of `out` the write's declared `effect` matches, judged by
/// the runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("set-longhorn-disk-tags")["effect"]
        .as_str()
        .expect("set-longhorn-disk-tags declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_renders_w3s_postgres_disk_tagged_durable_postgres_and_patches_nothing() {
    let c = Cluster::new("plan");
    let (plan, hash) = c.plan("w-3", "u-postgres", "durable,postgres");
    for want in [
        "plan: set-longhorn-disk-tags",
        "node: w-3",
        "address: 10.20.0.17",
        "estate role: talos-worker",
        "kubernetes node uid: uid-w-3",
        "disk default-disk-3c3c3c3c3c3c: path /var/lib/longhorn/",
        "disk u-data: path /var/mnt/data",
        "disk: u-postgres at /var/mnt/postgres, diskUUID 0b0b0b0b-1111-4222-8333-444455556666",
        "tags now: []",
        "tags after: [durable, postgres]",
        "kubectl patch nodes.longhorn.io w-3 -n longhorn-system --type=json",
        "A volume with no diskSelector",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(
        c.patches().is_empty(),
        "the plan patches nothing: {}",
        c.argv()
    );
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to the live free space.
    let (again, hash2) = c.plan("w-3", "u-postgres", "durable,postgres");
    assert_eq!((&plan, &hash), (&again, &hash2));
    let mut moved = w3();
    moved["status"]["diskStatus"]["u-postgres"]["storageAvailable"] = json!(12 * GIB);
    c.lh("w-3", moved);
    assert_eq!(
        c.plan("w-3", "u-postgres", "durable,postgres").1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
    // Other tags are another plan.
    assert_ne!(c.plan("w-3", "u-postgres", "durable").1, hash);
}

#[test]
fn the_default_disk_is_reached_by_its_own_name() {
    let c = Cluster::new("default");
    let (plan, _) = c.plan("w-1", "default-disk-9a8b7c6d5e4f", "durable");
    assert!(
        plan.contains("disk: default-disk-9a8b7c6d5e4f at /var/lib/longhorn/"),
        "{plan}"
    );
    assert!(plan.contains("tags after: [durable]"), "{plan}");
}

#[test]
fn what_the_bound_does_not_admit_is_refused_before_any_door() {
    let c = Cluster::new("bounds");
    for args in [
        vec!["--plan", "w-3", "u-postgres"],
        vec!["--plan", "w-3", "u-postgres", "durable", "extra"],
        vec!["--plan", "W-3", "u-postgres", "durable"],
        vec!["--plan", "-w", "u-postgres", "durable"],
        vec!["--plan", "w-3", "postgres", "durable"],
        vec!["--plan", "w-3", "U-postgres", "durable"],
        vec!["--plan", "w-3", "u-../x", "durable"],
        vec!["--plan", "w-3", "u-", "durable"],
        vec!["--plan", "w-3", "default-disk-XYZ", "durable"],
        vec!["--plan", "w-3", "default-disk-", "durable"],
        vec!["--plan", "w-3", "-x", "durable"],
        // Tags outside the fixed spellings.
        vec!["--plan", "w-3", "u-postgres", ""],
        vec!["--plan", "w-3", "u-postgres", "fast"],
        vec!["--plan", "w-3", "u-postgres", "postgres"],
        vec!["--plan", "w-3", "u-postgres", "postgres,durable"],
        vec!["--plan", "w-3", "u-postgres", "durable,scratch"],
        vec!["--plan", "w-3", "u-postgres", "durable,durable"],
        vec!["--plan", "w-3", "u-postgres", "durable durable,postgres"],
        vec!["--plan", "w-3", "u-postgres", "Durable"],
        vec!["--plan", "w-3", "u-postgres", "-durable"],
        vec!["w-3", "u-postgres", "durable"],
        vec!["w-3", "u-postgres", "durable", "not-a-hash"],
    ] {
        c.refused(&args);
    }
    assert!(c.argv().is_empty(), "no door opened: {}", c.argv());

    // A control plane is refused by the registry before any door opens: a
    // mutating node verb acts on workers (the-controls-are-bounded-verbs).
    let out = c.refused(&["--plan", "cp-1", "default-disk-9a8b7c6d5e4f", "durable"]);
    assert!(out.contains("talos-worker"), "{out}");
    assert!(c.argv().is_empty(), "no door opened: {}", c.argv());
}

#[test]
fn a_disk_the_node_does_not_hold_is_refused() {
    let c = Cluster::new("absent");
    let out = c.refused(&["--plan", "w-3", "u-scratch", "scratch"]);
    assert!(out.contains("u-scratch"), "{out}");
    assert!(
        out.contains("u-postgres"),
        "names the disks it holds: {out}"
    );
    let out = c.refused(&["--plan", "w-1", "default-disk-3c3c3c3c3c3c", "durable"]);
    assert!(out.contains("default-disk-9a8b7c6d5e4f"), "{out}");
}

#[test]
fn a_disk_whose_path_is_not_its_names_mount_is_refused() {
    let c = Cluster::new("path");
    let mut v = w3();
    v["spec"]["disks"]["u-postgres"]["path"] = json!("/srv/postgres");
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable,postgres"]);
    assert!(out.contains("/srv/postgres"), "{out}");
    assert!(out.contains("/var/mnt/postgres"), "{out}");

    let mut v = w3();
    v["spec"]["disks"]["default-disk-3c3c3c3c3c3c"]["path"] = json!("/var/mnt/data");
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "default-disk-3c3c3c3c3c3c", "durable"]);
    assert!(out.contains("/var/lib/longhorn"), "{out}");
}

#[test]
fn a_node_with_two_default_disks_is_refused() {
    let c = Cluster::new("two-defaults");
    let mut v = w3();
    v["spec"]["disks"]["default-disk-4d4d4d4d4d4d"] = disk_spec("/var/lib/longhorn", 0);
    v["status"]["diskStatus"]["default-disk-4d4d4d4d4d4d"] =
        disk_status("44444444-1111-4222-8333-444455556666");
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "default-disk-3c3c3c3c3c3c", "durable"]);
    assert!(out.contains("default-disk-4d4d4d4d4d4d"), "{out}");
}

#[test]
fn tags_the_disk_already_carries_are_refused() {
    let c = Cluster::new("same");
    let mut v = w3();
    v["spec"]["disks"]["u-data"]["tags"] = json!(["durable"]);
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "u-data", "durable"]);
    assert!(out.contains("already"), "{out}");
    // `none` is the way back to what add-longhorn-disk writes.
    let (plan, _) = c.plan("w-3", "u-data", "none");
    assert!(plan.contains("tags now: [durable]"), "{plan}");
    assert!(plan.contains("tags after: []"), "{plan}");
}

#[test]
fn a_disk_or_node_longhorn_does_not_read_ready_is_refused() {
    let c = Cluster::new("not-ready");
    let mut v = w3();
    v["status"]["diskStatus"]["u-postgres"]["conditions"][0] =
        json!({"type": "Ready", "status": "False", "reason": "DiskFilesystemChanged"});
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable,postgres"]);
    assert!(out.contains("DiskFilesystemChanged"), "{out}");

    let mut v = w3();
    v["status"]["diskStatus"]["u-postgres"]["conditions"][1] =
        json!({"type": "Schedulable", "status": "False", "reason": "DiskPressure"});
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable,postgres"]);
    assert!(out.contains("DiskPressure"), "{out}");

    let mut v = w3();
    v["status"]["conditions"][0] =
        json!({"type": "Ready", "status": "False", "reason": "ManagerPodDown"});
    c.lh("w-3", v);
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable,postgres"]);
    assert!(out.contains("ManagerPodDown"), "{out}");
}

/// Tags a replica's volume selects must stay: a disk retagged out from
/// under a placed replica strands it, because Longhorn moves nothing when
/// tags change and will not rebuild there.
#[test]
fn a_replica_whose_volume_would_no_longer_select_the_disk_is_refused() {
    let c = Cluster::new("stranded");
    let mut v = w3();
    v["spec"]["disks"]["u-postgres"]["tags"] = json!(["durable", "postgres"]);
    c.lh("w-3", v);
    c.state(
        "volumes.json",
        &json!([
            volume("pvc-sor", json!(["postgres"])),
            volume("pvc-any", json!(null))
        ])
        .to_string(),
    );
    // By diskID, and (a second shape) by node and path alone.
    c.state(
        "replicas.json",
        &json!([
            replica(
                "pvc-sor-r-1",
                "pvc-sor",
                "w-3",
                "0b0b0b0b-1111-4222-8333-444455556666",
                "/var/mnt/postgres"
            ),
            replica("pvc-any-r-1", "pvc-any", "w-3", "", "/var/mnt/postgres/"),
        ])
        .to_string(),
    );
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable"]);
    assert!(out.contains("pvc-sor-r-1"), "{out}");
    assert!(out.contains("pvc-sor"), "{out}");
    assert!(
        !out.contains("pvc-any-r-1"),
        "with allow-empty-disk-selector-volume read true, a volume with no selector \
         strands nothing: {out}"
    );

    // Tags that still hold the selector are a plan.
    c.lh("w-3", w3());
    let (plan, _) = c.plan("w-3", "u-postgres", "durable,postgres");
    assert!(plan.contains("replicas:"), "{plan}");

    // A replica naming a volume the list does not hold cannot be judged.
    c.state(
        "replicas.json",
        &json!([replica(
            "ghost-r-1",
            "ghost",
            "w-3",
            "0b0b0b0b-1111-4222-8333-444455556666",
            "/var/mnt/postgres"
        )])
        .to_string(),
    );
    let o = c.run(&["--plan", "w-3", "u-postgres", "durable,postgres"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("ghost"), "{out}");
}

/// A replica of a volume with no diskSelector on w-3's u-postgres, by
/// diskID: design 3c475a19's "tag every current default disk durable"
/// shape, where most of the cluster's volumes name no selector.
fn an_unselected_replica_on_u_postgres(c: &Cluster) {
    c.state(
        "volumes.json",
        &json!([
            volume("pvc-any", json!(null)),
            volume("pvc-sor", json!(["postgres"]))
        ])
        .to_string(),
    );
    c.state(
        "replicas.json",
        &json!([replica(
            "pvc-any-r-1",
            "pvc-any",
            "w-3",
            "0b0b0b0b-1111-4222-8333-444455556666",
            "/var/mnt/postgres"
        )])
        .to_string(),
    );
}

/// Review 7469ab28 B1. Longhorn's allow-empty-disk-selector-volume decides
/// whether a TAGGED disk takes replicas of a volume with no diskSelector:
/// false, it does not, so tagging a disk that holds one strands it exactly
/// as a selector the tags stop holding does. The setting is READ, never
/// assumed from Longhorn's default.
#[test]
fn an_unselected_replica_is_stranded_when_empty_selectors_place_only_on_untagged_disks() {
    let c = Cluster::new("empty-sel-false");
    an_unselected_replica_on_u_postgres(&c);
    c.state(EMPTY_SEL, "false");
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable"]);
    assert!(out.contains("pvc-any-r-1"), "{out}");
    assert!(out.contains("allow-empty-disk-selector-volume"), "{out}");
    // The write re-judges it, whatever hash it is handed.
    let out = c.refused(&["w-3", "u-postgres", "durable", &"0".repeat(64)]);
    assert!(out.contains("pvc-any-r-1"), "{out}");

    // Untagging is the one change such a replica survives.
    let mut v = w3();
    v["spec"]["disks"]["u-postgres"]["tags"] = json!(["durable"]);
    c.lh("w-3", v);
    let (plan, _) = c.plan("w-3", "u-postgres", "none");
    assert!(
        plan.contains("allow-empty-disk-selector-volume: false"),
        "{plan}"
    );

    // The same replica with the setting read true is a plan, and says why.
    c.lh("w-3", w3());
    c.state(EMPTY_SEL, "true");
    let (plan, _) = c.plan("w-3", "u-postgres", "durable");
    assert!(
        plan.contains("allow-empty-disk-selector-volume: true"),
        "{plan}"
    );
    // N2: the plan names what it judged — the volume and its selector.
    assert!(plan.contains("volume pvc-any"), "{plan}");
}

/// The setting is in the signed bytes, so a plan signed under one value
/// does not run under the other.
#[test]
fn the_plan_binds_the_setting_it_read() {
    let c = Cluster::new("empty-sel-bound");
    c.state(
        "volumes.json",
        &json!([volume("pvc-sor", json!(["postgres"]))]).to_string(),
    );
    let (plan, hash) = c.plan("w-3", "u-postgres", "durable,postgres");
    assert!(
        plan.contains("allow-empty-disk-selector-volume: true"),
        "{plan}"
    );
    // N3: a tag widens the disk too, and the plan names whom it admits.
    assert!(
        plan.contains("newly admit volume pvc-sor (diskSelector [\"postgres\"])"),
        "{plan}"
    );
    c.state(EMPTY_SEL, "false");
    let (plan_false, hash_false) = c.plan("w-3", "u-postgres", "durable,postgres");
    assert!(
        plan_false.contains("allow-empty-disk-selector-volume: false"),
        "{plan_false}"
    );
    assert_ne!(hash, hash_false, "the setting is in the signed bytes");
    let out = c.refused(&["w-3", "u-postgres", "durable,postgres", &hash]);
    assert!(out.contains("not the approved plan"), "{out}");
}

/// No evidence is not a pass: a setting that could not be read, or reads
/// as neither true nor false, is CANNOT ANSWER; one Longhorn does not hold
/// is refused, named — never taken as its default.
#[test]
fn a_setting_that_cannot_be_read_is_not_assumed() {
    let c = Cluster::new("empty-sel-dark");
    let o = c.run(
        &["--plan", "w-3", "u-postgres", "durable,postgres"],
        &[("STUB_SETTING_FORBIDDEN", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("allow-empty-disk-selector-volume"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    assert!(c.patches().is_empty(), "{}", c.argv());

    c.state(EMPTY_SEL, "maybe");
    let o = c.run(&["--plan", "w-3", "u-postgres", "durable,postgres"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("maybe"), "{out}");

    c.unstate(EMPTY_SEL);
    let out = c.refused(&["--plan", "w-3", "u-postgres", "durable,postgres"]);
    assert!(out.contains("NotFound"), "{out}");
    assert!(out.contains("allow-empty-disk-selector-volume"), "{out}");
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    let c = Cluster::new("dark");
    let o = c.run(
        &["--plan", "w-3", "u-postgres", "durable,postgres"],
        &[("STUB_FORBIDDEN", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    assert!(c.patches().is_empty(), "{}", c.argv());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_the_tags_back() {
    let c = Cluster::new("write");
    let before = c.lh_now("w-3")["spec"]["disks"].clone();
    let (plan, hash) = c.plan("w-3", "u-postgres", "durable,postgres");
    let o = c.run(&["w-3", "u-postgres", "durable,postgres", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let patches = c.patches();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains("patch nodes.longhorn.io w-3 -n longhorn-system --type=json"),
        "{p}"
    );
    assert!(
        p.contains(r#"{"op":"test","path":"/spec/disks","value":"#),
        "a compare-and-set on the disks the plan read: {p}"
    );
    assert!(
        p.contains(r#"{"op":"replace","path":"/spec/disks/u-postgres/tags","value":["durable","postgres"]}"#),
        "{p}"
    );
    let now = c.lh_now("w-3");
    assert_eq!(
        now["spec"]["disks"]["u-postgres"]["tags"],
        json!(["durable", "postgres"])
    );
    let mut was = before.clone();
    was["u-postgres"]["tags"] = json!(["durable", "postgres"]);
    assert_eq!(now["spec"]["disks"], was, "nothing else moved");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("u-postgres tagged durable,postgres — read back"),
        "{hits:?}"
    );
}

/// A disk record with no `tags` key is given one by `add`: `replace`
/// would fail on a missing member.
#[test]
fn a_disk_with_no_tags_key_is_given_one() {
    let c = Cluster::new("no-key");
    let mut v = w1();
    v["spec"]["disks"]["default-disk-9a8b7c6d5e4f"]
        .as_object_mut()
        .unwrap()
        .remove("tags");
    c.lh("w-1", v);
    let (plan, hash) = c.plan("w-1", "default-disk-9a8b7c6d5e4f", "durable");
    assert!(plan.contains("tags now: []"), "{plan}");
    let o = c.run(&["w-1", "default-disk-9a8b7c6d5e4f", "durable", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        c.patches()[0].contains(r#"{"op":"add","path":"/spec/disks/default-disk-9a8b7c6d5e4f/tags","value":["durable"]}"#),
        "{}",
        c.argv()
    );
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let c = Cluster::new("drift");
    let (_, hash) = c.plan("w-3", "u-data", "durable");
    // Another disk moved after the plan was signed.
    let mut v = w3();
    v["spec"]["disks"]["u-postgres"]["allowScheduling"] = json!(false);
    c.lh("w-3", v);
    let out = c.refused(&["w-3", "u-data", "durable", &hash]);
    assert!(out.contains(&hash), "names the approved hash: {out}");
    c.refused(&["w-3", "u-data", "durable", &"0".repeat(64)]);
    // The hash of one tag set does not run another.
    c.lh("w-3", w3());
    c.refused(&["w-3", "u-data", "durable,postgres", &hash]);

    // Applied once, the same approval is spent.
    let o = c.run(&["w-3", "u-data", "durable", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = c.run(&["w-3", "u-data", "durable", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(c.patches().len(), 1, "no second patch: {}", c.argv());
}

#[test]
fn a_patch_longhorn_refuses_is_not_an_effect() {
    let c = Cluster::new("patch-refused");
    let (_, hash) = c.plan("w-3", "u-postgres", "durable,postgres");
    let o = c.run(
        &["w-3", "u-postgres", "durable,postgres", &hash],
        &[("STUB_PATCH_FAIL", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(
        c.lh_now("w-3")["spec"]["disks"]["u-postgres"]["tags"],
        json!([])
    );
}

/// Every conjunct of the read-back is a claim the effect line makes, so
/// each is shown refusing: Longhorn accepted the patch, ONE fact of what
/// it then holds is wrong (STUB_DISK=`mode`), and the run is NOT proven
/// with that fact named and no effect line printed.
fn read_back_refuses(mode: &str, says: &str) {
    let c = Cluster::new(&format!("readback-{mode}"));
    let (_, hash) = c.plan("w-3", "u-postgres", "durable,postgres");
    let o = c.run(
        &["w-3", "u-postgres", "durable,postgres", &hash],
        &[("STUB_DISK", mode)],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{mode}: {out}");
    assert!(out.contains("NOT proven"), "{mode}: {out}");
    assert!(out.contains(says), "{mode} names {says:?}: {out}");
    assert!(effect_lines(&out).is_empty(), "{mode}: {out}");
}

#[test]
fn a_disk_longhorn_no_longer_reads_ready_is_not_an_effect() {
    read_back_refuses("notready", "DiskFilesystemChanged");
}

#[test]
fn tags_that_are_not_the_planned_ones_are_not_an_effect() {
    read_back_refuses("wrongtags", "spec.disks/u-postgres/tags reads [scratch]");
}

#[test]
fn another_disk_that_changed_with_the_patch_is_not_an_effect() {
    read_back_refuses("otherchanged", "another disk on the node changed");
}

#[test]
fn another_field_of_the_disk_that_changed_is_not_an_effect() {
    read_back_refuses("samechanged", "another field of u-postgres changed");
}

#[test]
fn a_disk_longhorn_reports_no_status_for_is_not_an_effect() {
    read_back_refuses("nostatus", "Longhorn reports no status for u-postgres");
}

#[test]
fn a_disk_under_another_disk_uuid_is_not_an_effect() {
    read_back_refuses(
        "uuidchanged",
        "not the planned 0b0b0b0b-1111-4222-8333-444455556666",
    );
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("set-longhorn-disk-tags");
    let p = verb("plan-a-longhorn-disk-tags");
    for (name, v) in [
        ("set-longhorn-disk-tags", &w),
        ("plan-a-longhorn-disk-tags", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        assert_eq!(v["params"][1]["name"], "disk", "{name}");
        assert_eq!(v["params"][2]["name"], "tags", "{name}");
    }
    assert_eq!(p["argv"][1], "--plan");
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_ne!(p["requires_approval"], true);
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-longhorn-disk-tags");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["params"][3]["name"], "plan_sha256");
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 300,
        "outlasts the script's read-back"
    );
    let disk = w["params"][1]["pattern"].as_str().expect("a pattern");
    for ok in ["u-postgres", "u-data", "default-disk-080400000000"] {
        assert!(jq_test(disk, ok), "{ok}");
    }
    for bad in [
        "",
        "-x",
        "postgres",
        "u-",
        "U-x",
        "u-a/b",
        "default-disk-",
        "default-disk-XY",
    ] {
        assert!(!jq_test(disk, bad), "the disk pattern admits {bad:?}");
    }
    let tags = w["params"][2]["pattern"].as_str().expect("a pattern");
    for ok in ["durable", "durable,postgres", "scratch", "none"] {
        assert!(jq_test(tags, ok), "{ok}");
    }
    for bad in ["", "-x", "a b", "durable,", ",durable", "Durable"] {
        assert!(!jq_test(tags, bad), "the tags pattern admits {bad:?}");
    }
}

/// The spellings the script admits, read from its one definition.
fn admitted() -> Vec<String> {
    let body = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    let line = body
        .lines()
        .find_map(|l| l.strip_prefix("ADMITTED_TAGS='"))
        .expect("the script defines ADMITTED_TAGS='…'");
    line.trim_end_matches('\'')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// The vocabulary is design 3c475a19's, and every Longhorn class in the
/// tree that selects disks by tag selects only from it — so no class can
/// name a tag this door cannot write. Read from the manifests, so a class
/// that lands with a new tag fails here until the door admits it.
#[test]
fn the_admitted_tags_are_the_designs_and_hold_every_class_disk_selector() {
    assert_eq!(
        admitted(),
        ["durable", "durable,postgres", "scratch", "none"],
        "design 3c475a19's spellings, and `none` for the untagged disk add-longhorn-disk writes"
    );
    let words: Vec<String> = admitted()
        .iter()
        .filter(|s| *s != "none")
        .flat_map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>())
        .collect();
    let dir = repo_root().join("infra/cluster/manifests");
    let mut seen = 0;
    for e in std::fs::read_dir(&dir).expect("the manifests") {
        let p = e.expect("an entry").path();
        if p.extension().is_none_or(|x| x != "yaml") {
            continue;
        }
        let body = std::fs::read_to_string(&p).expect("a manifest");
        for line in body.lines() {
            let Some(v) = line.trim().strip_prefix("diskSelector:") else {
                continue;
            };
            seen += 1;
            for tag in v.trim().trim_matches(['"', '\'']).split(',') {
                assert!(
                    words.iter().any(|w| w == tag.trim()),
                    "{} selects disk tag {tag:?}, which set-longhorn-disk-tags cannot write",
                    p.display()
                );
            }
        }
    }
    // Control: the reader sees the class files it scans.
    assert!(
        std::fs::read_to_string(dir.join("boss-dev.yaml"))
            .expect("boss-dev.yaml")
            .contains("kind: StorageClass"),
        "the scan reads the manifests that declare classes ({seen} diskSelector line(s) read)"
    );
}
