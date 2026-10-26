//! The longhorn-disk family's third pair (backlog 36d2c484): the read-only
//! `plan-a-longhorn-disk-reregistration` (`infra/forge/
//! reregister-longhorn-disk.sh --plan`) and the passkey-approved
//! `reregister-longhorn-disk`.
//!
//! WHY. Read 2026-10-21T23:45Z on w-2: its default disk
//! default-disk-080400000000 at /var/lib/longhorn read Ready=False
//! DiskFilesystemChanged ("record diskUUID does not match the one on the
//! disk"), storageMaximum 0, on a 929 GiB filesystem with 886 free — the
//! record predates the 1 TB disk that replaced the system disk on
//! 2026-10-19 — and add-longhorn-disk / remove-longhorn-disk act only on
//! `u-<volume>` at `/var/mnt/<volume>`, so no door reached it.
//!
//! THE BOUND each test below pins: only the node's DEFAULT disk — the one
//! disk at /var/lib/longhorn, named default-disk-<hex>, derived and never
//! named — on a worker the registry and the cluster agree on; the
//! Longhorn node Ready; Longhorn reading the disk Ready=False with reason
//! DiskFilesystemChanged; the disk's own longhorn-disk.cfg (read through
//! the Talos door) naming a diskUUID that is not the record's; and the
//! record holding NOTHING by four readings over BOTH UUIDs, refused on any
//! one. The write is three compare-and-set patches — the record made
//! unschedulable and its holdings read again, the record removed and the
//! stale status awaited, the same record added back schedulable — and its
//! effect is read back: Ready and Schedulable by Longhorn's own
//! conditions, under the diskUUID the file names, with a storageMaximum.
//!
//! HOW THIS IS MEASURED. The script runs for real; `sudo` is stubbed on
//! PATH as a small cluster behind both doors — the Kubernetes Node, the
//! Longhorn node, its replicas and backing images, and the one file
//! Talos is asked to read — and the estate registry is served from a
//! file. The stub applies the JSON patch's own operations (test, replace,
//! remove, add), so a compare-and-set that would fail on the cluster
//! fails here, and it adopts a re-added disk the way Longhorn does: under
//! the UUID the file on the disk names. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/reregister-longhorn-disk.sh";
const GIB: u64 = 1024 * 1024 * 1024;
const DISK: &str = "default-disk-080400000000";
/// The record's diskUUID: what Longhorn wrote before the disk changed.
const OLD_UUID: &str = "6e6e6e6e-1111-4222-8333-444455556666";
/// The diskUUID in the longhorn-disk.cfg on the disk now.
const NEW_UUID: &str = "0b0b0b0b-1111-4222-8333-444455556666";
/// A second, healthy disk on the node — the one every read-back holds
/// unchanged.
const DATA_UUID: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";
const CFG: &str = "/var/lib/longhorn/longhorn-disk.cfg";
/// A third UUID: a file on the disk that is neither of the above.
const OTHER_UUID: &str = "0c0c0c0c-1111-4222-8333-444455556666";

/// The stand-in behind both doors. State under $STUB_STATE: `ip-<node>`
/// its InternalIP, `cp-<node>` a control-plane label, `lh-<node>.json` the
/// Longhorn node, `replicas.json` and `bi.json` the replica and
/// backing-image items, `cfg-<ip>` the longhorn-disk.cfg Talos reads back
/// from that address (absent: no such file).
/// A patch of nodes.longhorn.io applies each operation of the JSON patch
/// in order — `test` (the value must equal), `replace` and `remove` (the
/// path must exist), `add` — or refuses the whole patch as the API server
/// does. A `remove` of /spec/disks/<k> drops status.diskStatus/<k>, as
/// Longhorn's node controller does; an `add` of /spec/disks/<k> with no
/// status left for <k> records it Ready and Schedulable under the UUID
/// the node's cfg file names, as Longhorn adopts a disk. STUB_DISK bends
/// ONE fact, one per conjunct and per race: `placed` (a replica lands on
/// the record while the scheduling patch lands), `notremoved` (the API
/// answers patched and the record stays), `statusstays` (the stale status
/// outlives the removal), and for the re-added record `notready`,
/// `wronguuid`, `nomax` and `otherchanged` (another disk's spec moves).
/// STUB_FORBIDDEN=1 answers every longhorn.io read as RBAC does;
/// STUB_TALOS_FORBIDDEN=1 refuses the Talos read; STUB_PATCH_FAIL=1
/// refuses every patch and STUB_ADD_FAIL=1 every patch that adds.
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
    --talosconfig=/tc) tool=talosctl; shift; break ;;
    esac
    shift
done
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    ip="$2"; shift 2
    if [ "$1" = read ] && [ $# -eq 2 ]; then
        [ -z "${STUB_TALOS_FORBIDDEN:-}" ] || { echo "rpc error: code = PermissionDenied desc = not authorized" >&2; exit 1; }
        f="$S/cfg-$ip"
        [ -f "$f" ] || { echo "rpc error: code = Unknown desc = open $2: no such file or directory" >&2; exit 1; }
        cat "$f"; exit 0
    fi
    echo "stub: unexpected talosctl call: $*" >&2
    exit 2
fi
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
    if [ -n "${STUB_ADD_FAIL:-}" ] && printf '%s' "$p" | grep -q '"op":"add"'; then
        echo "Error from server: admission webhook \"validator.longhorn.io\" denied the add" >&2; exit 1
    fi
    python3 - "$f" "$p" "${STUB_DISK:-}" "$S" "$S/cfg-$(cat "$S/ip-$n")" <<'PY' || exit 1
import json, os, sys
f, p, mode, S, cfg = sys.argv[1], json.loads(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5]
doc = json.load(open(f))
def walk(path):
    parts = [x for x in path.split("/") if x]
    cur = doc
    for k in parts[:-1]:
        cur = cur[k]
    return cur, parts[-1]
removed, added = [], []
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
                                      "diskID": "",
                                      "diskPath": doc["spec"]["disks"][disk]["path"]}})
                json.dump(reps, open(S + "/replicas.json", "w"))
        elif op["op"] == "remove":
            parent[key]
            if mode != "notremoved":
                del parent[key]
                removed.append(key)
        elif op["op"] == "add":
            parent[key] = op["value"]
            added.append(key)
        else:
            raise KeyError(op["op"])
except (KeyError, TypeError):
    print("The request is invalid: the test operation failed", file=sys.stderr)
    sys.exit(1)
status = doc.setdefault("status", {}).setdefault("diskStatus", {})
for k in removed:
    if mode != "statusstays":
        status.pop(k, None)
for k in added:
    if k in status:
        continue  # the stale status stands: Longhorn keeps its record
    uuid = json.load(open(cfg))["diskUUID"]
    if mode == "wronguuid":
        uuid = "99999999-1111-4222-8333-444455556666"
    r = "False" if mode == "notready" else "True"
    status[k] = {"diskUUID": uuid,
                 "storageMaximum": 0 if mode == "nomax" else 997653638144,
                 "storageScheduled": 0, "storageAvailable": 951000000000,
                 "scheduledReplica": {},
                 "conditions": [{"type": "Ready", "status": r,
                                 "reason": "" if r == "True" else "DiskFilesystemChanged"},
                                {"type": "Schedulable", "status": r,
                                 "reason": "" if r == "True" else "DiskNotReady"}]}
    if mode == "otherchanged":
        for name, d in doc["spec"]["disks"].items():
            if name != k:
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
            "reregister_longhorn_disk_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn registry_row(id: &str, address: &str, role: &str) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 929, "notes": null, "retired": false
    })
}

fn conds(status: &str, reason: &str) -> Value {
    json!([{"type": "Ready", "status": status, "reason": reason},
           {"type": "Schedulable", "status": status, "reason": reason}])
}

/// w-2's Longhorn node as it read on 2026-10-21: its default disk at
/// /var/lib/longhorn whose record carries the OLD diskUUID and reads
/// DiskFilesystemChanged with storageMaximum 0, beside a healthy second
/// disk (`u-data`) that every read-back holds unchanged.
fn lh_node(name: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {
                     DISK: {"allowScheduling": true, "evictionRequested": false,
                         "path": "/var/lib/longhorn/", "storageReserved": 35325304832_u64,
                         "tags": [], "diskType": "filesystem"},
                     "u-data": {"allowScheduling": true, "evictionRequested": false,
                         "path": "/var/mnt/data", "storageReserved": 0,
                         "tags": [], "diskType": "filesystem"}}},
        "status": {"conditions": conds("True", ""),
                   "diskStatus": {
                       DISK: {"diskUUID": OLD_UUID,
                           "storageMaximum": 0, "storageScheduled": 0, "storageAvailable": 0,
                           "scheduledReplica": {},
                           "conditions": [
                               {"type": "Ready", "status": "False", "reason": "DiskFilesystemChanged",
                                "message": "record diskUUID does not match the one on the disk"},
                               {"type": "Schedulable", "status": "False", "reason": "DiskNotReady"}]},
                       "u-data": {"diskUUID": DATA_UUID,
                           "storageMaximum": 999653638144_u64, "storageScheduled": 40 * GIB,
                           "storageAvailable": 900000000000_u64,
                           "scheduledReplica": {"pvc-11-r-aaaa": 40 * GIB},
                           "conditions": conds("True", "")}}}
    })
}

/// A replica of `vol` on `node`'s disk `uuid` at `path`.
fn replica(name: &str, vol: &str, node: &str, uuid: &str, path: &str) -> Value {
    json!({"metadata": {"name": name, "namespace": "longhorn-system"},
           "spec": {"volumeName": vol, "nodeID": node, "diskID": uuid, "diskPath": path}})
}

fn cfg(uuid: &str) -> String {
    let v = json!({"diskName": DISK, "diskUUID": uuid, "diskDriver": ""});
    v.to_string()
}

impl Node {
    /// w-2 a worker the registry and the cluster agree on, its default
    /// disk's record drifted, the file on the disk naming NEW_UUID, and
    /// one replica — on the OTHER disk.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("reregister-longhorn-disk-{name}"));
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
        n.state("cfg-10.20.0.16", &cfg(NEW_UUID));
        n.state("cfg-10.20.0.11", &cfg(NEW_UUID));
        n.replicas(json!([replica(
            "pvc-11-r-aaaa",
            "pvc-11",
            "w-2",
            DATA_UUID,
            "/var/mnt/data/"
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

    fn plan(&self, node: &str) -> (String, String) {
        let o = self.run(&["--plan", node], &[]);
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
    let re = verb("reregister-longhorn-disk")["effect"]
        .as_str()
        .expect("reregister-longhorn-disk declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_renders_the_drifted_default_disk_and_patches_nothing() {
    let n = Node::new("plan");
    let (plan, hash) = n.plan("w-2");
    for want in [
        "plan: reregister-longhorn-disk",
        "node: w-2",
        "address: 10.20.0.16",
        "disk default-disk-080400000000: path /var/lib/longhorn/",
        "disk u-data: path /var/mnt/data",
        "record: default-disk-080400000000 at /var/lib/longhorn",
        "DiskFilesystemChanged",
        OLD_UUID,
        NEW_UUID,
        "holds: nothing",
        "kubectl patch nodes.longhorn.io w-2 -n longhorn-system --type=json",
        "allowScheduling false",
        "add spec.disks/default-disk-080400000000",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(n.patches().is_empty(), "the plan patches nothing");
    assert!(effect_lines(&plan).is_empty(), "{plan}");
    assert!(
        n.argv().contains(&format!("-n 10.20.0.16 read {CFG}")),
        "the file on the disk is read through the Talos door, at the registry's address: {}",
        n.argv()
    );

    // Deterministic, and blind to the live free space.
    let (again, hash2) = n.plan("w-2");
    assert_eq!((&plan, &hash), (&again, &hash2));
    let mut moved = lh_node("w-2");
    moved["status"]["diskStatus"]["u-data"]["storageAvailable"] = json!(12 * GIB);
    n.lh("w-2", moved);
    let free_moved = n.plan("w-2").1;
    assert_eq!(free_moved, hash, "free space is not signed");

    // The UUID on the disk IS in the signed bytes: a different file is a
    // different plan.
    n.state("cfg-10.20.0.16", &cfg(OTHER_UUID));
    assert_ne!(n.plan("w-2").1, hash, "the file's diskUUID is signed");
}

/// Each reading of "something names the record" refuses on its own, with
/// nothing patched: one refusal per reading, so deleting any one of them
/// turns a test red.
fn holding_refuses(name: &str, bend: impl Fn(&Node), says: &str) {
    let n = Node::new(&format!("holds-{name}"));
    bend(&n);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("named by"), "{name}: {out}");
    assert!(out.contains(says), "{name} names {says:?}: {out}");
}

#[test]
fn a_replica_on_the_records_uuid_is_refused() {
    holding_refuses(
        "old-uuid",
        |n| {
            n.replicas(json!([replica(
                "pvc-22-r-bbbb",
                "pvc-22",
                "w-1",
                OLD_UUID,
                "/var/lib/longhorn/"
            )]))
        },
        "pvc-22-r-bbbb",
    );
}

#[test]
fn a_replica_on_the_uuid_of_the_disk_under_it_is_refused() {
    // Re-adoption would bring it back into Longhorn's books: a choice the
    // plan does not make.
    holding_refuses(
        "new-uuid",
        |n| {
            n.replicas(json!([replica(
                "pvc-23-r-bbbb",
                "pvc-23",
                "w-1",
                NEW_UUID,
                "/var/lib/longhorn/"
            )]))
        },
        "pvc-23-r-bbbb",
    );
}

#[test]
fn a_replica_on_the_records_node_and_path_is_refused() {
    holding_refuses(
        "path",
        |n| {
            n.replicas(json!([replica(
                "pvc-33-r-cccc",
                "pvc-33",
                "w-2",
                "",
                "/var/lib/longhorn/"
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
            v["status"]["diskStatus"][DISK]["scheduledReplica"] =
                json!({"pvc-44-r-dddd": 10 * GIB});
            n.lh("w-2", v);
        },
        "pvc-44-r-dddd",
    );
}

#[test]
fn storage_scheduled_on_the_record_is_refused() {
    holding_refuses(
        "storage-scheduled",
        |n| {
            let mut v = lh_node("w-2");
            v["status"]["diskStatus"][DISK]["storageScheduled"] = json!(5 * GIB);
            n.lh("w-2", v);
        },
        "storageScheduled",
    );
}

#[test]
fn a_backing_image_on_either_uuid_is_refused() {
    holding_refuses(
        "backing-image-new",
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
        "backing-image-old",
        |n| {
            n.state(
                "bi.json",
                &json!([{"metadata": {"name": "talos-old"},
                         "spec": {}, "status": {"diskFileStatusMap": {OLD_UUID: {}}}}])
                .to_string(),
            )
        },
        "talos-old",
    );
    holding_refuses(
        "scheduled-backing-image",
        |n| {
            let mut v = lh_node("w-2");
            v["status"]["diskStatus"][DISK]["scheduledBackingImage"] = json!({"talos-base": 1});
            n.lh("w-2", v);
        },
        "talos-base",
    );
}

#[test]
fn only_one_default_disk_at_the_default_path_is_reached() {
    // None at the default path: nothing to re-register.
    let n = Node::new("shape-none");
    let mut v = lh_node("w-2");
    v["spec"]["disks"].as_object_mut().unwrap().remove(DISK);
    v["status"]["diskStatus"]
        .as_object_mut()
        .unwrap()
        .remove(DISK);
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("has no disk at /var/lib/longhorn"), "{out}");

    // Two at the default path: which record is stale is not its choice.
    let n = Node::new("shape-two");
    let mut v = lh_node("w-2");
    v["spec"]["disks"]["default-disk-aaaa"] = v["spec"]["disks"][DISK].clone();
    v["spec"]["disks"]["default-disk-aaaa"]["path"] = json!("/var/lib/longhorn");
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("more than one disk"), "{out}");

    // At the default path under a name Longhorn never gives its default.
    let n = Node::new("shape-name");
    let mut v = lh_node("w-2");
    let d = v["spec"]["disks"]
        .as_object_mut()
        .unwrap()
        .remove(DISK)
        .unwrap();
    v["spec"]["disks"]["u-longhorn"] = d;
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("not Longhorn's default-disk-<hex>"), "{out}");
}

#[test]
fn a_record_that_has_not_drifted_is_refused() {
    // Longhorn reads the disk Ready: there is nothing to repair.
    let n = Node::new("ready");
    let mut v = lh_node("w-2");
    v["status"]["diskStatus"][DISK]["conditions"] = conds("True", "");
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(
        out.contains("not Ready=False (DiskFilesystemChanged)"),
        "{out}"
    );

    // Not Ready for another reason: not the drift this verb repairs.
    let n = Node::new("other-reason");
    let mut v = lh_node("w-2");
    v["status"]["diskStatus"][DISK]["conditions"] = conds("False", "DiskNotFound");
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("DiskNotFound"), "{out}");

    // No status at all for the record: nothing to compare.
    let n = Node::new("no-status");
    let mut v = lh_node("w-2");
    v["status"]["diskStatus"]
        .as_object_mut()
        .unwrap()
        .remove(DISK);
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("reports no status"), "{out}");

    // The file on the disk agrees with the record.
    let n = Node::new("same-uuid");
    n.state("cfg-10.20.0.16", &cfg(OLD_UUID));
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("nothing drifted"), "{out}");

    // No file on the disk: the re-add would mint a new UUID.
    let n = Node::new("no-cfg");
    let cfg_file = n.dir.join("state/cfg-10.20.0.16");
    std::fs::remove_file(cfg_file).unwrap();
    let out = n.refused(&["--plan", "w-2"]);
    assert!(
        out.contains("carries no /var/lib/longhorn/longhorn-disk.cfg"),
        "{out}"
    );
}

#[test]
fn what_the_bound_does_not_admit_is_refused_before_any_door() {
    let n = Node::new("bounds");
    let zeros = "0".repeat(64);
    for args in [
        vec![],
        vec!["--plan"],
        vec!["--plan", "w-2", "extra"],
        vec!["--plan", "W-2"],
        vec!["--plan", "-w"],
        vec!["--plan", "w-2,cp-1"],
        vec!["--plan", "10.20.0.16"],
        vec!["w-2"],
        vec!["w-2", "not-a-hash"],
        vec!["w-2", zeros.as_str(), "extra"],
    ] {
        n.refused(&args);
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());

    // A control plane is refused by the registry before any door opens,
    // even with a drifted default disk.
    let o = n.run(&["--plan", "cp-1"], &[]);
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
    let out = n.refused(&["--plan", "w-2"]);
    assert!(out.contains("does not read Ready=True"), "{out}");
    assert!(out.contains("ManagerPodDown"), "{out}");
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    for (name, var, says) in [
        ("dark-longhorn", "STUB_FORBIDDEN", "Forbidden"),
        ("dark-talos", "STUB_TALOS_FORBIDDEN", "PermissionDenied"),
    ] {
        let n = Node::new(name);
        let o = n.run(&["--plan", "w-2"], &[(var, "1")]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{name}: {out}");
        assert!(out.contains(says), "{name}: {out}");
        assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
        assert!(n.patches().is_empty(), "{}", n.argv());
    }
    // A file that is not Longhorn's, or names no UUID, is not evidence.
    for (name, body) in [("cfg-garbage", "not json"), ("cfg-no-uuid", "{}")] {
        let n = Node::new(name);
        n.state("cfg-10.20.0.16", body);
        let o = n.run(&["--plan", "w-2"], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{name}: {out}");
        assert!(n.patches().is_empty(), "{}", n.argv());
    }
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_the_record_back_adopted() {
    let n = Node::new("write");
    let before = n.lh_now("w-2")["spec"]["disks"].clone();
    let (plan, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let p = n.patches();
    assert_eq!(p.len(), 3, "three patches: {p:?}");
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
    let unschedule =
        format!(r#"{{"op":"replace","path":"/spec/disks/{DISK}/allowScheduling","value":false}}"#);
    assert!(
        p[0].contains(&unschedule),
        "first the record is made unschedulable: {}",
        p[0]
    );
    let remove = format!(r#"{{"op":"remove","path":"/spec/disks/{DISK}"}}"#);
    assert!(p[1].contains(&remove), "then it is removed: {}", p[1]);
    let add = format!(r#"{{"op":"add","path":"/spec/disks/{DISK}","#);
    assert!(p[2].contains(&add), "then it is added back: {}", p[2]);
    // The disks read as before: the same record, schedulable, and u-data
    // untouched.
    let now = n.lh_now("w-2");
    assert_eq!(now["spec"]["disks"], before, "the disks as before");
    assert_eq!(now["status"]["diskStatus"][DISK]["diskUUID"], NEW_UUID);
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains(NEW_UUID), "{hits:?}");
    assert!(
        hits[0].contains("its 1 other disk(s) unchanged"),
        "{hits:?}"
    );
}

#[test]
fn a_record_already_unschedulable_is_removed_and_re_added_schedulable() {
    let n = Node::new("unschedulable");
    let mut v = lh_node("w-2");
    v["spec"]["disks"][DISK]["allowScheduling"] = json!(false);
    n.lh("w-2", v);
    let (plan, hash) = n.plan("w-2");
    assert!(
        plan.contains("already reads allowScheduling false"),
        "{plan}"
    );
    let o = n.run(&["w-2", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let p = n.patches();
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[0].contains(r#""op":"remove""#), "{}", p[0]);
    assert!(p[1].contains(r#""op":"add""#), "{}", p[1]);
    let now = n.lh_now("w-2");
    assert_eq!(now["spec"]["disks"][DISK]["allowScheduling"], true, "{now}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let n = Node::new("drift");
    let (_, hash) = n.plan("w-2");
    // Another disk appeared after the plan was signed.
    let mut v = lh_node("w-2");
    v["spec"]["disks"]["other"] = json!({"path": "/srv/other", "allowScheduling": false,
        "evictionRequested": false, "storageReserved": 0, "tags": [], "diskType": "filesystem"});
    n.lh("w-2", v);
    let out = n.refused(&["w-2", &hash]);
    assert!(out.contains(&hash), "names the approved hash: {out}");
    n.refused(&["w-2", &"0".repeat(64)]);

    // Applied once, the same approval is spent: Longhorn reads the record
    // Ready, so the re-render refuses.
    n.lh("w-2", lh_node("w-2"));
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = n.run(&["w-2", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(n.patches().len(), 3, "no fourth patch: {}", n.argv());
}

/// The race the scheduling patch exists for: a replica lands on the
/// record between the plan's read and the patch. The second read finds
/// it, and the record is left unschedulable and NOT removed.
#[test]
fn a_replica_placed_before_the_record_went_unschedulable_stops_the_removal() {
    let n = Node::new("placed");
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_DISK", "placed")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("pvc-77-r-late"), "names the replica: {out}");
    assert!(out.contains("nothing was removed"), "{out}");
    assert_eq!(n.patches().len(), 1, "no removal patch: {}", n.argv());
    let now = n.lh_now("w-2");
    assert_eq!(
        now["spec"]["disks"][DISK]["allowScheduling"], false,
        "{now}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_refused_patch_changes_nothing_and_is_not_an_effect() {
    let n = Node::new("patch-refused");
    let before = n.lh_now("w-2");
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(n.lh_now("w-2"), before);
}

/// The record went and could not come back: the run says the node has no
/// default disk record, after three tries, and claims nothing.
#[test]
fn a_refused_re_add_is_tried_three_times_and_named() {
    let n = Node::new("add-refused");
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_ADD_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("could NOT be added back"), "{out}");
    assert!(out.contains("attempt 3 of 3"), "{out}");
    // Two patches, then three tries of the add.
    assert_eq!(n.patches().len(), 5, "{}", n.argv());
    assert!(
        n.lh_now("w-2")["spec"]["disks"].get(DISK).is_none(),
        "the stub refused every add"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

/// Longhorn never dropped the stale status: the record goes back all the
/// same — the node reads as it did before — and the run fails.
#[test]
fn a_stale_status_that_outlives_the_removal_is_put_back_and_not_an_effect() {
    let n = Node::new("statusstays");
    let before = n.lh_now("w-2");
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_DISK", "statusstays")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("added back all the same"), "{out}");
    assert!(out.contains("NOT proven"), "{out}");
    assert_eq!(n.patches().len(), 3, "{}", n.argv());
    let after = n.lh_now("w-2");
    assert_eq!(after, before, "the node reads as it did before the run");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_record_the_api_kept_is_not_re_added() {
    let n = Node::new("notremoved");
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_DISK", "notremoved")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("spec.disks still holds {DISK}")),
        "{out}"
    );
    assert!(out.contains("Nothing was re-added"), "{out}");
    assert_eq!(n.patches().len(), 2, "no add: {}", n.argv());
    assert!(effect_lines(&out).is_empty(), "{out}");
}

/// Every conjunct of the read-back is a claim the effect line makes, so
/// each is shown refusing: the record went back, ONE fact of what
/// Longhorn then holds is wrong, and the run is NOT proven with that fact
/// named and no effect line printed.
fn read_back_refuses(mode: &str, says: &str) {
    let n = Node::new(&format!("readback-{mode}"));
    let (_, hash) = n.plan("w-2");
    let o = n.run(&["w-2", &hash], &[("STUB_DISK", mode)]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{mode}: {out}");
    assert!(out.contains("NOT proven"), "{mode}: {out}");
    assert!(out.contains(says), "{mode} names {says:?}: {out}");
    assert!(effect_lines(&out).is_empty(), "{mode}: {out}");
}

#[test]
fn a_record_longhorn_does_not_read_ready_is_not_an_effect() {
    read_back_refuses("notready", "Ready=False");
}

#[test]
fn a_record_under_a_uuid_not_the_disks_is_not_an_effect() {
    read_back_refuses("wronguuid", &format!("not {NEW_UUID}, the one on the disk"));
}

#[test]
fn a_record_with_no_storage_maximum_is_not_an_effect() {
    read_back_refuses("nomax", "no storageMaximum");
}

#[test]
fn another_disk_that_changed_with_the_re_add_is_not_an_effect() {
    read_back_refuses("otherchanged", "another disk on the node changed");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("reregister-longhorn-disk");
    let p = verb("plan-a-longhorn-disk-reregistration");
    let removal = verb("remove-longhorn-disk");
    let node_pattern = &removal["params"][0]["pattern"];
    for (name, v) in [
        ("reregister-longhorn-disk", &w),
        ("plan-a-longhorn-disk-reregistration", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        // The node is the only thing a packet names: no disk, no path.
        assert_eq!(&v["params"][0]["pattern"], node_pattern, "{name}");
    }
    assert_eq!(p["argv"], json!([SCRIPT, "--plan", "{1}"]));
    assert_eq!(p["params"].as_array().map(Vec::len), Some(1));
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_ne!(p["requires_approval"], true);
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-longhorn-disk-reregistration");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["argv"], json!([SCRIPT, "{1}", "{2}"]));
    assert_eq!(w["params"][1]["name"], "plan_sha256");
    assert_eq!(w["params"].as_array().map(Vec::len), Some(2));
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 300 + 15 + 300 + 300,
        "outlasts the plan's reads, the settle, the wait for the stale status and the read-back"
    );
}
