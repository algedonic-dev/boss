//! The inverse of add-longhorn-disk (backlog b791c5b4): the read-only
//! `plan-a-longhorn-disk-removal` (`infra/forge/remove-longhorn-disk.sh
//! --plan`) and the passkey-approved `remove-longhorn-disk`.
//!
//! WHY. Found 2026-10-19 by run cfaad67f while building the add door (item
//! 493a02b6): add-longhorn-disk places `u-<volume>` at `/var/mnt/<volume>`
//! in a node's Longhorn spec.disks, and nothing takes one back out — not
//! even a disk Longhorn never read Ready after the write, which stays in
//! spec.disks for good (Longhorn schedules nothing to it, and it is never
//! removed).
//!
//! THE BOUND each test below pins: only a disk in the add door's shape —
//! named `u-<volume>`, at `/var/mnt/<volume>` — on a worker the registry
//! and the cluster agree on; the Longhorn node Ready; and the disk holding
//! NOTHING by four readings, refused on any one: a replica whose diskID is
//! the disk's diskUUID or whose node and diskPath are the disk's, a
//! scheduled replica or backing image in Longhorn's diskStatus, a
//! storageScheduled above zero, and a backing image keeping a file on the
//! disk's UUID. The write is two compare-and-set patches — the disk made
//! unschedulable, the holdings read AGAIN, then the disk removed — and its
//! effect is read back: the disk gone from spec.disks and from Longhorn's
//! diskStatus, every other disk as the plan read it.
//!
//! HOW THIS IS MEASURED. The script runs for real; `sudo` is stubbed on
//! PATH as a small cluster behind the kubectl door — the Kubernetes Node,
//! the Longhorn node, its replicas and backing images — and the estate
//! registry is served from a file. The stub applies the JSON patch's own
//! operations (test, replace, remove), so a compare-and-set that would
//! fail on the cluster fails here. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/remove-longhorn-disk.sh";
const GIB: u64 = 1024 * 1024 * 1024;
const NEW_UUID: &str = "0b0b0b0b-1111-4222-8333-444455556666";
const DEFAULT_UUID: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `ip-<node>` its InternalIP, `cp-<node>` a control-plane label,
/// `lh-<node>.json` the Longhorn node, `replicas.json` and `bi.json` the
/// replica and backing-image items.
/// A patch of nodes.longhorn.io applies each operation of the JSON patch
/// in order — `test` (the value must equal), `replace` and `remove` (the
/// path must exist) — or refuses the whole patch as the API server does.
/// A `remove` of /spec/disks/<k> also drops status.diskStatus/<k>, as
/// Longhorn's node controller does. STUB_DISK bends ONE fact, one per
/// read-back conjunct and per race: `statusstays` (the diskStatus entry
/// outlives the removal), `otherchanged` (another disk's spec moves with
/// it), `notremoved` (the API answers patched and the disk stays) and
/// `placed` (Longhorn places a replica on the disk while the scheduling
/// patch lands, before the scheduler sees it).
/// STUB_FORBIDDEN=1 answers every longhorn.io read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses every patch.
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
forbid() {
    [ -z "${STUB_FORBIDDEN:-}" ] || { echo "Error from server (Forbidden): $1 is forbidden" >&2; exit 1; }
}
case "$1 $2" in
"get node")
    n="$3"
    [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
    labels='"kubernetes.io/hostname":"'"$n"'"'
    [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
    printf '{"kind":"Node","metadata":{"name":"%s","uid":"uid-%s","labels":{%s}},"spec":{},"status":{"addresses":[{"type":"InternalIP","address":"%s"}],"conditions":[{"type":"Ready","status":"True"}]}}\n' "$n" "$n" "$labels" "$(cat "$S/ip-$n")"
    exit 0 ;;
"get nodes.longhorn.io")
    forbid "nodes.longhorn.io \"$3\""
    f="$S/lh-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get replicas.longhorn.io")
    forbid replicas.longhorn.io
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json" 2>/dev/null || echo '[]')"; exit 0 ;;
"get backingimages.longhorn.io")
    forbid backingimages.longhorn.io
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/bi.json" 2>/dev/null || echo '[]')"; exit 0 ;;
"patch nodes.longhorn.io")
    n="$3"; f="$S/lh-$n.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    python3 - "$f" "$p" "${STUB_DISK:-}" "$S" <<'PY' || exit 1
import json, os, sys
f, p, mode, S = sys.argv[1], json.loads(sys.argv[2]), sys.argv[3], sys.argv[4]
doc = json.load(open(f))
def walk(path):
    parts = [x for x in path.split("/") if x]
    cur = doc
    for k in parts[:-1]:
        cur = cur[k]
    return cur, parts[-1]
removed = []
try:
    for op in p:
        parent, key = walk(op["path"])
        if op["op"] == "test":
            if parent[key] != op["value"]:
                raise KeyError("test")
        elif op["op"] == "replace":
            parent[key]  # must exist
            parent[key] = op["value"]
            if mode == "placed" and key == "allowScheduling":
                disk = op["path"].split("/")[3]
                reps = json.load(open(S + "/replicas.json")) if os.path.exists(S + "/replicas.json") else []
                reps.append({"metadata": {"name": "pvc-77-r-late"},
                             "spec": {"volumeName": "pvc-77", "nodeID": doc["metadata"]["name"],
                                      "diskID": doc["status"]["diskStatus"][disk]["diskUUID"],
                                      "diskPath": doc["spec"]["disks"][disk]["path"]}})
                json.dump(reps, open(S + "/replicas.json", "w"))
        elif op["op"] == "remove":
            parent[key]
            if mode != "notremoved":
                del parent[key]
                removed.append(key)
        else:
            raise KeyError(op["op"])
except (KeyError, TypeError):
    print("The request is invalid: the test operation failed", file=sys.stderr)
    sys.exit(1)
for k in removed:
    if mode != "statusstays":
        doc.get("status", {}).get("diskStatus", {}).pop(k, None)
    if mode == "otherchanged":
        for d in doc["spec"]["disks"].values():
            d["allowScheduling"] = False
json.dump(doc, open(f, "w"))
PY
    echo "node.longhorn.io/$n patched"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

struct Node {
    dir: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "sha256sum", "python3"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "remove_longhorn_disk_sh: no {tool} on this box — the gate image has it, and a \
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

fn ready(status: &str) -> Value {
    json!([{"type": "Ready", "status": status}, {"type": "Schedulable", "status": status}])
}

/// A Longhorn node holding its default disk and the disk add-longhorn-disk
/// placed — `u-longhorn` at /var/mnt/longhorn, untagged and schedulable,
/// read Ready with a diskUUID of its own and nothing scheduled on it.
fn lh_node(name: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {
                     "default-disk-1f2e3d4c5b6a": {"allowScheduling": true, "evictionRequested": false,
                         "path": "/var/lib/longhorn/", "storageReserved": 35 * GIB,
                         "tags": [], "diskType": "filesystem"},
                     "u-longhorn": {"allowScheduling": true, "evictionRequested": false,
                         "path": "/var/mnt/longhorn", "storageReserved": 0,
                         "tags": [], "diskType": "filesystem"}}},
        "status": {"conditions": ready("True"),
                   "diskStatus": {
                       "default-disk-1f2e3d4c5b6a": {"diskUUID": DEFAULT_UUID,
                           "storageMaximum": 117 * GIB, "storageScheduled": 40 * GIB,
                           "storageAvailable": 60 * GIB,
                           "scheduledReplica": {"pvc-11-r-aaaa": 40 * GIB},
                           "conditions": ready("True")},
                       "u-longhorn": {"diskUUID": NEW_UUID,
                           "storageMaximum": 999653638144_u64, "storageScheduled": 0,
                           "storageAvailable": 999000000000_u64,
                           "scheduledReplica": {},
                           "conditions": ready("True")}}}
    })
}

/// A replica of `vol` on `node`'s disk `uuid` at `path`.
fn replica(name: &str, vol: &str, node: &str, uuid: &str, path: &str) -> Value {
    json!({"metadata": {"name": name, "namespace": "longhorn-system"},
           "spec": {"volumeName": vol, "nodeID": node, "diskID": uuid, "diskPath": path}})
}

impl Node {
    /// w-2 a worker the registry and the cluster agree on, with the disk
    /// add-longhorn-disk placed and one replica — on the DEFAULT disk.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("remove-longhorn-disk-{name}"));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let n = Node { dir };
        write_file(
            &n.dir.join("nodes.json"),
            &json!({"data": [
                registry_row("forge", "10.20.0.15", "forge"),
                registry_row("cp-1", "10.20.0.11", "talos-control-plane"),
                registry_row("w-1", "10.20.0.14", "talos-worker"),
                registry_row("w-2", "10.20.0.16", "talos-worker"),
            ]})
            .to_string(),
        );
        n.state("ip-cp-1", "10.20.0.11");
        n.state("cp-cp-1", "");
        n.state("ip-w-1", "10.20.0.14");
        n.state("ip-w-2", "10.20.0.16");
        n.lh("w-2", lh_node("w-2"));
        n.lh("cp-1", lh_node("cp-1"));
        n.replicas(json!([replica(
            "pvc-11-r-aaaa",
            "pvc-11",
            "w-2",
            DEFAULT_UUID,
            "/var/lib/longhorn/"
        )]));
        n.state("bi.json", "[]");
        n
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn lh(&self, node: &str, v: Value) {
        self.state(&format!("lh-{node}.json"), &v.to_string());
    }

    fn lh_now(&self, node: &str) -> Value {
        let p = self.dir.join("state").join(format!("lh-{node}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).expect("longhorn node")).expect("json")
    }

    fn replicas(&self, v: Value) {
        self.state("replicas.json", &v.to_string());
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
            .env("BOSS_TALOSCTL", self.dir.join("talosctl"))
            .env("BOSS_LONGHORN_DISK_WAIT_S", "2")
            .env("BOSS_LONGHORN_DISK_POLL_S", "1")
            .env("BOSS_LONGHORN_DISK_SETTLE_S", "0")
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

    fn plan(&self, node: &str, vol: &str) -> (String, String) {
        let o = self.run(&["--plan", node, vol], &[]);
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

    /// A refusal: exit 78, no patch, no plan on stdout, and the text.
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
    let re = verb("remove-longhorn-disk")["effect"]
        .as_str()
        .expect("remove-longhorn-disk declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_renders_the_disk_add_placed_for_removal_and_patches_nothing() {
    let n = Node::new("plan");
    let (plan, hash) = n.plan("w-2", "longhorn");
    for want in [
        "plan: remove-longhorn-disk",
        "node: w-2",
        "address: 10.20.0.16",
        "disk default-disk-1f2e3d4c5b6a: path /var/lib/longhorn/",
        "disk u-longhorn: path /var/mnt/longhorn",
        "disk to remove: u-longhorn at /var/mnt/longhorn",
        NEW_UUID,
        "holds: nothing",
        "kubectl patch nodes.longhorn.io w-2 -n longhorn-system --type=json",
        "allowScheduling false",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(n.patches().is_empty(), "the plan patches nothing");
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to the live free space.
    let (again, hash2) = n.plan("w-2", "longhorn");
    assert_eq!((&plan, &hash), (&again, &hash2));
    let mut moved = lh_node("w-2");
    moved["status"]["diskStatus"]["u-longhorn"]["storageAvailable"] = json!(12 * GIB);
    n.lh("w-2", moved);
    assert_eq!(
        n.plan("w-2", "longhorn").1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
}

/// A disk the add door placed that Longhorn never read Ready — the case
/// this verb was filed for — has no diskUUID and no status of its own,
/// and still plans: nothing can be on it, and the replicas are read by
/// its node and path.
#[test]
fn a_disk_longhorn_never_read_ready_plans_for_removal() {
    let n = Node::new("never-ready");
    let mut v = lh_node("w-2");
    v["status"]["diskStatus"]["u-longhorn"] = json!({"diskUUID": "",
        "conditions": [{"type": "Ready", "status": "False", "reason": "DiskFilesystemChanged"}]});
    n.lh("w-2", v);
    let (plan, _) = n.plan("w-2", "longhorn");
    assert!(plan.contains("no diskUUID"), "{plan}");

    let mut v = lh_node("w-2");
    v["status"]["diskStatus"]
        .as_object_mut()
        .unwrap()
        .remove("u-longhorn");
    n.lh("w-2", v);
    let (plan, hash) = n.plan("w-2", "longhorn");
    assert!(plan.contains("no diskUUID"), "{plan}");
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(effect_lines(&text(&o)).len(), 1, "{}", text(&o));
}

/// Each reading of "the disk holds something" refuses on its own, with
/// nothing patched: one refusal per reading, so deleting any one of them
/// turns a test red.
fn holding_refuses(name: &str, bend: impl Fn(&Node), says: &str) {
    let n = Node::new(&format!("holds-{name}"));
    bend(&n);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("holds"), "{name}: {out}");
    assert!(out.contains(says), "{name} names {says:?}: {out}");
}

#[test]
fn a_replica_on_the_disks_uuid_is_refused() {
    holding_refuses(
        "uuid",
        |n| {
            n.replicas(json!([replica(
                "pvc-22-r-bbbb",
                "pvc-22",
                "w-2",
                NEW_UUID,
                "/var/mnt/longhorn"
            )]))
        },
        "pvc-22-r-bbbb",
    );
}

#[test]
fn a_replica_on_the_disks_node_and_path_is_refused() {
    // No diskID yet (Longhorn names the disk by path before it records the
    // UUID): the node and the path are the disk.
    holding_refuses(
        "path",
        |n| {
            n.replicas(json!([replica(
                "pvc-33-r-cccc",
                "pvc-33",
                "w-2",
                "",
                "/var/mnt/longhorn/"
            )]))
        },
        "pvc-33-r-cccc",
    );
}

#[test]
fn a_scheduled_replica_in_longhorns_disk_status_is_refused() {
    holding_refuses(
        "scheduled",
        |n| {
            let mut v = lh_node("w-2");
            v["status"]["diskStatus"]["u-longhorn"]["scheduledReplica"] =
                json!({"pvc-44-r-dddd": 10 * GIB});
            n.lh("w-2", v);
        },
        "pvc-44-r-dddd",
    );
}

#[test]
fn storage_scheduled_on_the_disk_is_refused() {
    holding_refuses(
        "storage-scheduled",
        |n| {
            let mut v = lh_node("w-2");
            v["status"]["diskStatus"]["u-longhorn"]["storageScheduled"] = json!(5 * GIB);
            n.lh("w-2", v);
        },
        "storageScheduled",
    );
}

#[test]
fn a_backing_image_on_the_disk_is_refused() {
    holding_refuses(
        "backing-image",
        |n| {
            n.state(
                "bi.json",
                &json!([{"metadata": {"name": "talos-base"},
                         "spec": {"diskFileSpecMap": {NEW_UUID: {}}}}])
                .to_string(),
            )
        },
        "talos-base",
    );
    holding_refuses(
        "scheduled-backing-image",
        |n| {
            let mut v = lh_node("w-2");
            v["status"]["diskStatus"]["u-longhorn"]["scheduledBackingImage"] =
                json!({"talos-base": 1});
            n.lh("w-2", v);
        },
        "talos-base",
    );
}

#[test]
fn a_disk_not_in_the_add_doors_shape_is_refused() {
    let n = Node::new("shape");
    // Absent: nothing to remove.
    let out = n.refused(&["--plan", "w-2", "data"]);
    assert!(out.contains("u-data"), "{out}");
    assert!(out.contains("no Longhorn disk"), "{out}");

    // The name the add door gives, at a path it never gives.
    let mut v = lh_node("w-2");
    v["spec"]["disks"]["u-longhorn"]["path"] = json!("/var/lib/longhorn/");
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(
        out.contains("not the shape add-longhorn-disk places"),
        "{out}"
    );
}

#[test]
fn what_the_bound_does_not_admit_is_refused_before_any_door() {
    let n = Node::new("bounds");
    for args in [
        vec!["--plan", "w-2"],
        vec!["--plan", "w-2", "longhorn", "extra"],
        vec!["--plan", "W-2", "longhorn"],
        vec!["--plan", "-w", "longhorn"],
        vec!["--plan", "w-2", "Longhorn"],
        vec!["--plan", "w-2", "-x"],
        vec!["--plan", "w-2", "../gate"],
        vec!["--plan", "w-2", "default-disk/x"],
        vec!["w-2", "longhorn"],
        vec!["w-2", "longhorn", "not-a-hash"],
    ] {
        n.refused(&args);
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());

    // A control plane is refused by the registry before any door opens,
    // even with the disk in the add door's shape.
    let o = n.run(&["--plan", "cp-1", "longhorn"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("talos-worker"), "{out}");
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

#[test]
fn a_longhorn_node_that_is_not_ready_is_refused() {
    let n = Node::new("lh-not-ready");
    let mut v = lh_node("w-2");
    v["status"]["conditions"][0] =
        json!({"type": "Ready", "status": "False", "reason": "ManagerPodDown"});
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("does not read Ready=True"), "{out}");
    assert!(out.contains("ManagerPodDown"), "{out}");
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    let n = Node::new("dark");
    let o = n.run(&["--plan", "w-2", "longhorn"], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    assert!(n.patches().is_empty(), "{}", n.argv());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_the_disk_back_gone() {
    let n = Node::new("write");
    let before = n.lh_now("w-2")["spec"]["disks"].clone();
    let (plan, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let p = n.patches();
    assert_eq!(p.len(), 2, "two patches: {p:?}");
    for patch in &p {
        assert!(
            patch.contains("patch nodes.longhorn.io w-2 -n longhorn-system --type=json"),
            "{patch}"
        );
        assert!(
            patch.contains(r#"{"op":"test","path":"/spec/disks","value":"#),
            "each is a compare-and-set on the disks: {patch}"
        );
    }
    assert!(
        p[0].contains(
            r#"{"op":"replace","path":"/spec/disks/u-longhorn/allowScheduling","value":false}"#
        ),
        "first the disk is made unschedulable: {}",
        p[0]
    );
    assert!(
        p[1].contains(r#"{"op":"remove","path":"/spec/disks/u-longhorn"}"#),
        "then it is removed: {}",
        p[1]
    );
    let now = n.lh_now("w-2");
    assert!(now["spec"]["disks"].get("u-longhorn").is_none(), "{now}");
    let mut others = before.clone();
    others.as_object_mut().unwrap().remove("u-longhorn");
    assert_eq!(now["spec"]["disks"], others, "every other disk as it was");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains("u-longhorn"), "{hits:?}");
}

#[test]
fn a_disk_already_unschedulable_is_removed_in_one_patch() {
    let n = Node::new("unschedulable");
    let mut v = lh_node("w-2");
    v["spec"]["disks"]["u-longhorn"]["allowScheduling"] = json!(false);
    n.lh("w-2", v);
    let (plan, hash) = n.plan("w-2", "longhorn");
    assert!(
        plan.contains("already reads allowScheduling false"),
        "{plan}"
    );
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let p = n.patches();
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].contains(r#""op":"remove""#), "{}", p[0]);
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let n = Node::new("drift");
    let (_, hash) = n.plan("w-2", "longhorn");
    // Another disk appeared after the plan was signed.
    let mut v = lh_node("w-2");
    v["spec"]["disks"]["other"] = json!({"path": "/srv/other", "allowScheduling": false,
        "evictionRequested": false, "storageReserved": 0, "tags": [], "diskType": "filesystem"});
    n.lh("w-2", v);
    let out = n.refused(&["w-2", "longhorn", &hash]);
    assert!(out.contains(&hash), "names the approved hash: {out}");
    n.refused(&["w-2", "longhorn", &"0".repeat(64)]);

    // Applied once, the same approval is spent: the disk is gone, so the
    // re-render refuses.
    n.lh("w-2", lh_node("w-2"));
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(n.patches().len(), 2, "no third patch: {}", n.argv());
}

/// The race the two-patch shape exists for: Longhorn places a replica on
/// the disk between the plan's read and the scheduling patch. The second
/// read finds it, and the disk is left unschedulable and NOT removed.
#[test]
fn a_replica_placed_before_the_disk_went_unschedulable_stops_the_removal() {
    let n = Node::new("placed");
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[("STUB_DISK", "placed")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("pvc-77-r-late"), "names the replica: {out}");
    assert!(out.contains("nothing was removed"), "{out}");
    assert_eq!(n.patches().len(), 1, "no removal patch: {}", n.argv());
    let now = n.lh_now("w-2");
    assert_eq!(
        now["spec"]["disks"]["u-longhorn"]["allowScheduling"], false,
        "{now}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_refused_patch_changes_nothing_and_is_not_an_effect() {
    let n = Node::new("patch-refused");
    let before = n.lh_now("w-2");
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(n.lh_now("w-2"), before);
}

/// Every conjunct of the read-back is a claim the effect line makes, so
/// each is shown refusing: the API accepted the removal, ONE fact of what
/// Longhorn then holds is wrong, and the run is NOT proven with that fact
/// named and no effect line printed.
fn read_back_refuses(mode: &str, says: &str) {
    let n = Node::new(&format!("readback-{mode}"));
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[("STUB_DISK", mode)]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{mode}: {out}");
    assert!(out.contains("NOT proven"), "{mode}: {out}");
    assert!(out.contains(says), "{mode} names {says:?}: {out}");
    assert!(effect_lines(&out).is_empty(), "{mode}: {out}");
}

#[test]
fn a_disk_still_in_spec_is_not_an_effect() {
    read_back_refuses("notremoved", "spec.disks still holds u-longhorn");
}

#[test]
fn a_disk_longhorn_still_reports_status_for_is_not_an_effect() {
    read_back_refuses(
        "statusstays",
        "Longhorn still reports status for u-longhorn",
    );
}

#[test]
fn another_disk_that_changed_with_the_removal_is_not_an_effect() {
    read_back_refuses("otherchanged", "another disk on the node changed");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("remove-longhorn-disk");
    let p = verb("plan-a-longhorn-disk-removal");
    for (name, v) in [
        ("remove-longhorn-disk", &w),
        ("plan-a-longhorn-disk-removal", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        assert_eq!(v["params"][1]["name"], "volume", "{name}");
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
    assert_eq!(w["plan_verb"], "plan-a-longhorn-disk-removal");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["params"][2]["name"], "plan_sha256");
    // The add door's volume pattern, so a disk it can place is a disk
    // this can name, and nothing else.
    assert_eq!(
        w["params"][1]["pattern"],
        verb("add-longhorn-disk")["params"][1]["pattern"]
    );
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 300 + 180 + 15,
        "outlasts the plan's reads, the settle and the read-back"
    );
}
