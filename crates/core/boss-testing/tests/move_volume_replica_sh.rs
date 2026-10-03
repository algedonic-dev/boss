//! The two ops verbs that move one replica of a Longhorn volume off its
//! disk from the forge (backlog ab39a34e, incident d3c0a67c): the
//! read-only `plan-a-volume-replica-move` (`infra/forge/
//! move-volume-replica.sh --plan`) and the passkey-approved
//! `move-volume-replica`.
//!
//! WHY. The system of record's database volume (pvc-93e11a6e…, 30 GiB)
//! holds four replicas, on cp-1, cp-2, cp-3 and w-2, and the w-2 one sits
//! on disk bf045701-… (StorageMaximum 117656518656) whose scheduling
//! ledger has ~755 MB left — so Longhorn's admission webhook refuses any
//! growth of the volume until that replica moves. Moving a replica is
//! raise the count, wait for the new one, lower the count, delete the old
//! one; the bound is that the volume never holds fewer healthy replicas
//! than it declared, and that the disk the new one lands on can itself
//! hold the volume's growth.
//!
//! THE ESTATE in these tests is that one, by its own numbers: w-2's disk
//! is the one the webhook named (StorageMaximum 117656518656,
//! StorageReserved 35296955596, and a storageScheduled that leaves
//! 755184436 bytes of room), the replicas name their disks by UUID as
//! live replicas do (spec.diskID = diskStatus[].diskUUID, read through
//! plan-a-volume-replica-change, ops-request 5b61b067), and w-1 — the
//! one node holding no replica — has a large disk.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`. The stub behaves as Longhorn does where the order
//! matters: a raised count schedules a new replica on the node holding
//! none; a replica deleted while the count is above what remains is
//! REPLENISHED at once onto the node it left (no failed replica to wait
//! for). Every door call lands in `argv`, so the order of the patches and
//! the delete, and their absence on a refusal, are read from the record.
//! Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/move-volume-replica.sh";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
const REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab";
const GIB: u64 = 1024 * 1024 * 1024;
const SIZE: u64 = 30 * GIB;
/// w-2's disk, by the webhook's own numbers of 2026-10-01.
const W2_DISK: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";
const W2_MAX: u64 = 117_656_518_656;
const W2_RESERVED: u64 = 35_296_955_596;
/// 70866960384 before David's hand patch, plus the 10 GiB it scheduled.
const W2_SCHEDULED: u64 = 81_604_378_624;
const W1_DISK: &str = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10";
const CP1_DISK: &str = "844915d9-6a5b-4da9-9619-8f3d188d632b";
const CP2_DISK: &str = "976a5246-7a68-452c-bb25-f9e3ecb2258e";
const CP3_DISK: &str = "9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `vol-<name>.json`, `setting-<name>.json`, and the `replicas.json` and
/// `lhnodes.json` lists (arrays). A raise of spec.numberOfReplicas adds
/// one replica per step on STUB_NEW_NODE (default w-1), on STUB_NEW_DISK
/// (default that node's first disk UUID), healthy unless
/// STUB_REBUILD=never, and charges the disk's storageScheduled. A lower
/// removes STUB_TRIM, if named — a replica removed by another hand at that
/// moment (under the plan's settings Longhorn trims nothing itself). A
/// delete removes the replica and credits its disk; if fewer replicas then
/// stand than the count, Longhorn replenishes one onto the SAME node and
/// disk. With STUB_TERMINATING=1 a removal (the trim or the delete) first
/// sets metadata.deletionTimestamp, and the replica stays LISTED for one
/// more `get replicas` before it is gone, as a finalizer holds it.
/// STUB_FORBIDDEN=1 answers every read as RBAC does; STUB_PATCH_FAIL=1
/// refuses every patch; STUB_EMPTY=<resource> answers its get with exit 0
/// and no bytes.
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
    echo "Error from server (Forbidden): $2 is forbidden: User \"system:serviceaccount:boss-dev:boss-dev\" cannot get resource \"$2\" in API group \"longhorn.io\"" >&2
    exit 1
fi
vol() { ls "$S"/vol-*.json | head -n 1; }
charge() { # <disk uuid> <bytes, signed>
    jq --arg u "$1" --argjson d "$2" \
        'map(.status.diskStatus |= with_entries(if .value.diskUUID == $u then .value.storageScheduled += $d else . end))' \
        "$S/lhnodes.json" > "$S/lhnodes.new" && mv "$S/lhnodes.new" "$S/lhnodes.json"
}
add_replica() { # <volume> <name> <node> <disk> <healthyAt>
    jq --arg v "$1" --arg r "$2" --arg n "$3" --arg d "$4" --arg h "$5" \
        '. + [{"apiVersion":"longhorn.io/v1beta2","kind":"Replica","metadata":{"name":$r,"namespace":"longhorn-system","labels":{"longhornvolume":$v}},"spec":{"volumeName":$v,"nodeID":$n,"diskID":$d,"healthyAt":$h,"failedAt":"","active":true},"status":{"currentState":"running"}}]' \
        "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
}
remove_replica() { # <name> — gone now, or terminating for one more list
    if [ -n "${STUB_TERMINATING:-}" ]; then
        jq --arg r "$1" 'map(if .metadata.name == $r then .metadata.deletionTimestamp = "2026-10-01T18:10:00Z" else . end)' \
            "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    else
        jq --arg r "$1" 'map(select(.metadata.name != $r))' "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    fi
}
case "$1 $2" in
"get volumes.longhorn.io")
    f="$S/vol-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): volumes.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get settings.longhorn.io")
    f="$S/setting-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get replicas.longhorn.io")
    # A terminating replica is listed once more, then its finalizer clears.
    jq 'map(select(.metadata.labels.listed_terminating != "yes"))
        | map(if (.metadata.deletionTimestamp // "") != "" then .metadata.labels.listed_terminating = "yes" else . end)' \
        "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"; exit 0 ;;
"get nodes.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/lhnodes.json")"; exit 0 ;;
"patch volumes.longhorn.io")
    v="$3"; f="$S/vol-$v.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    want=$(printf '%s' "$p" | jq -c '.[] | select(.op == "test" and .path == "/spec/numberOfReplicas") | .value')
    new=$(printf '%s' "$p" | jq -c '.[] | select(.op == "replace" and .path == "/spec/numberOfReplicas") | .value')
    cur=$(jq -c '.spec.numberOfReplicas' "$f")
    [ -n "$want" ] && [ "$want" = "$cur" ] || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    size=$(jq -r '.spec.size' "$f")
    jq --argjson n "$new" '.spec.numberOfReplicas = $n' "$f" > "$f.new" && mv "$f.new" "$f"
    if [ "$new" -gt "$cur" ]; then
        node="${STUB_NEW_NODE:-w-1}"
        disk="${STUB_NEW_DISK:-$(jq -r --arg n "$node" '.[] | select(.metadata.name == $n) | [.status.diskStatus[].diskUUID] | .[0]' "$S/lhnodes.json")}"
        h="2026-10-01T18:00:00Z"
        [ "${STUB_REBUILD:-}" = never ] && h=""
        add_replica "$v" "$v-r-e0e0e0e1" "$node" "$disk" "$h"
        charge "$disk" "$size"
    elif [ -n "${STUB_TRIM:-}" ]; then
        d=$(jq -r --arg r "$STUB_TRIM" '.[] | select(.metadata.name == $r) | .spec.diskID' "$S/replicas.json")
        remove_replica "$STUB_TRIM"
        charge "$d" "-$size"
    fi
    echo "volume.longhorn.io/$v patched"; exit 0 ;;
"delete replicas.longhorn.io")
    r="$3"
    row=$(jq -c --arg r "$r" '.[] | select(.metadata.name == $r)' "$S/replicas.json")
    [ -n "$row" ] || { echo "Error from server (NotFound): replicas.longhorn.io \"$r\" not found" >&2; exit 1; }
    v=$(printf '%s' "$row" | jq -r '.spec.volumeName')
    node=$(printf '%s' "$row" | jq -r '.spec.nodeID')
    disk=$(printf '%s' "$row" | jq -r '.spec.diskID')
    f="$S/vol-$v.json"
    size=$(jq -r '.spec.size' "$f")
    remove_replica "$r"
    charge "$disk" "-$size"
    left=$(jq --arg v "$v" '[.[] | select(.spec.volumeName == $v and (.metadata.deletionTimestamp // "") == "")] | length' "$S/replicas.json")
    if [ "$left" -lt "$(jq '.spec.numberOfReplicas' "$f")" ]; then
        add_replica "$v" "$v-r-feedface" "$node" "$disk" "2026-10-01T18:05:00Z"
        charge "$disk" "$size"
    fi
    echo "replica.longhorn.io \"$r\" deleted"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

fn needs_tools() {
    for tool in ["jq", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "move_volume_replica_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn volume(replicas: u64) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": VOL, "namespace": "longhorn-system"},
        "spec": {"numberOfReplicas": replicas, "size": SIZE.to_string(),
                 "dataLocality": "disabled", "replicaSoftAntiAffinity": "ignored",
                 "replicaAutoBalance": "ignored", "dataEngine": "v1", "nodeID": "cp-1"},
        "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "cp-1",
                   "actualSize": 20 * GIB,
                   "kubernetesStatus": {"namespace": "boss", "pvcName": "pgdata-postgres-0",
                                        "pvName": VOL}}
    })
}

fn replica(vol: &str, name: &str, node: &str, disk: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system",
                     "labels": {"longhornvolume": vol}},
        "spec": {"volumeName": vol, "nodeID": node, "diskID": disk,
                 "healthyAt": if healthy { "2026-09-12T10:00:00Z" } else { "" },
                 "failedAt": if failed { "2026-09-20T10:00:00Z" } else { "" },
                 "active": true},
        "status": {"currentState": if failed { "stopped" } else { "running" }}
    })
}

/// One Longhorn disk: its name, UUID and ledger in bytes, and whether it
/// allows scheduling. `available` is the live free space.
#[derive(Clone)]
struct Disk {
    name: &'static str,
    uuid: &'static str,
    max: u64,
    reserved: u64,
    scheduled: u64,
    available: u64,
    allow: bool,
}

fn disk(name: &'static str, uuid: &'static str, max: u64, reserved: u64, scheduled: u64) -> Disk {
    Disk {
        name,
        uuid,
        max,
        reserved,
        scheduled,
        available: max / 2,
        allow: true,
    }
}

fn lh_node(name: &str, disks: &[Disk]) -> Value {
    let mut spec = serde_json::Map::new();
    let mut status = serde_json::Map::new();
    for d in disks {
        spec.insert(
            d.name.to_string(),
            json!({"allowScheduling": d.allow, "evictionRequested": false,
                   "path": "/var/lib/longhorn/", "diskType": "filesystem",
                   "storageReserved": d.reserved, "tags": []}),
        );
        status.insert(
            d.name.to_string(),
            json!({"diskUUID": d.uuid, "storageMaximum": d.max,
                   "storageScheduled": d.scheduled, "storageAvailable": d.available,
                   "conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}]}),
        );
    }
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false, "tags": [],
                 "disks": spec},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": status}
    })
}

/// The control planes' disks: 500 GiB, room to spare for the volume's growth.
fn cp_disk(name: &'static str, uuid: &'static str) -> Disk {
    disk(name, uuid, 500 * GIB, 150 * GIB, 100 * GIB)
}

fn w2_disk() -> Disk {
    disk(
        "default-disk-fd0100000000",
        W2_DISK,
        W2_MAX,
        W2_RESERVED,
        W2_SCHEDULED,
    )
}

/// w-1's disk: 1.8 TiB, a third reserved, 300 GiB scheduled.
fn w1_disk() -> Disk {
    disk("nvme1", W1_DISK, 1800 * GIB, 540 * GIB, 300 * GIB)
}

struct Longhorn {
    dir: PathBuf,
}

impl Longhorn {
    /// The estate of 2026-10-01: four healthy replicas on cp-1, cp-2,
    /// cp-3 and w-2, the w-2 one on the disk the webhook named; w-1 holds
    /// none (only another volume's replica) and has room.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.volume(volume(4));
        l.replicas(Self::four());
        l.nodes(json!([
            lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
            lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
            lh_node("cp-3", &[cp_disk("default-disk-cp3", CP3_DISK)]),
            lh_node("w-1", &[w1_disk()]),
            lh_node("w-2", &[w2_disk()]),
        ]));
        l.setting("storage-over-provisioning-percentage", "100");
        l.setting("storage-minimal-available-percentage", "25");
        l.setting("replica-soft-anti-affinity", "false");
        l.setting("replica-auto-balance", "disabled");
        l
    }

    fn four() -> Value {
        json!([
            replica(
                VOL,
                &format!("{VOL}-r-7c1934ba"),
                "cp-1",
                CP1_DISK,
                true,
                false
            ),
            replica(
                VOL,
                &format!("{VOL}-r-1269eb6e"),
                "cp-2",
                CP2_DISK,
                true,
                false
            ),
            replica(
                VOL,
                &format!("{VOL}-r-03f4dac5"),
                "cp-3",
                CP3_DISK,
                true,
                false
            ),
            replica(VOL, REP, "w-2", W2_DISK, true, false),
            replica(
                "pvc-11111111-2222-3333-4444-555555555555",
                "pvc-11111111-2222-3333-4444-555555555555-r-0000aaaa",
                "w-1",
                W1_DISK,
                true,
                false
            ),
        ])
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn volume(&self, v: Value) {
        self.state(&format!("vol-{VOL}.json"), &v.to_string());
    }

    fn setting(&self, name: &str, value: &str) {
        self.state(
            &format!("setting-{name}.json"),
            &json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
                    "metadata": {"name": name, "namespace": "longhorn-system"},
                    "value": value})
            .to_string(),
        );
    }

    fn read_state(&self, file: &str) -> Value {
        let p = self.dir.join("state").join(file);
        serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("json")
    }

    fn volume_now(&self) -> Value {
        self.read_state(&format!("vol-{VOL}.json"))
    }

    /// This volume's replicas now, `<name> on <node> (<disk>)`.
    fn replicas_now(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .read_state("replicas.json")
            .as_array()
            .expect("an array")
            .iter()
            .filter(|r| r["spec"]["volumeName"] == VOL)
            .map(|r| {
                format!(
                    "{} on {} ({})",
                    r["metadata"]["name"].as_str().unwrap_or_default(),
                    r["spec"]["nodeID"].as_str().unwrap_or_default(),
                    r["spec"]["diskID"].as_str().unwrap_or_default()
                )
            })
            .collect();
        v.sort();
        v
    }

    fn replicas(&self, r: Value) {
        self.state("replicas.json", &r.to_string());
    }

    fn nodes(&self, n: Value) {
        self.state("lhnodes.json", &n.to_string());
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
            .env("BOSS_MOVE_REBUILD_S", "3")
            .env("BOSS_MOVE_SETTLE_S", "1")
            .env("BOSS_MOVE_READBACK_S", "3")
            .env("BOSS_MOVE_POLL_S", "1")
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

    /// The mutating calls, in order.
    fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("patch ") || c.starts_with("delete "))
            .collect()
    }

    fn plan(&self) -> (String, String) {
        let o = self.run(&["--plan", VOL, REP], &[]);
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

    /// A plan that must refuse: exit 78, no plan, no write.
    fn refused(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let o = self.run(args, env);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{args:?} is refused: {out}");
        assert!(
            !out.lines().any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        assert!(self.writes().is_empty(), "{:?}", self.writes());
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
    let re = verb("move-volume-replica")["effect"]
        .as_str()
        .expect("move-volume-replica declares an `effect`")
        .to_string();
    out.lines()
        .filter(|line| jq_test(&re, line))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_replica_its_target_disk_and_the_webhook_headroom() {
    let l = Longhorn::new("mvr-plan");
    let (plan, hash) = l.plan();
    for want in [
        format!("volume: {VOL}"),
        "claim: boss/pgdata-postgres-0".to_string(),
        "size: 32212254720 bytes (30 GiB)".to_string(),
        "robustness: healthy".to_string(),
        format!("move: replica {REP} on w-2 (disk {W2_DISK})"),
        "numberOfReplicas: 4 -> 5 -> 4 — the count never below 4, and \
         pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab deleted only after a read of 5 healthy replicas"
            .to_string(),
        "trims: none of Longhorn's own — data locality disabled; replica auto-balance: the \
         volume says ignored, the setting replica-auto-balance says disabled; it resolves to \
         disabled; no eviction requested"
            .to_string(),
        format!("twice the volume (64424509440 bytes): {W1_DISK}"),
        // The disk the webhook named, by its own arithmetic: 755 MB.
        format!(
            "disk {W2_DISK} (default-disk-fd0100000000 on w-2): 1 replica(s) of this volume; \
             ProvisionedLimit (117656518656 - 35296955596) x 100% = 82359563060; \
             storageScheduled 81604378624; room 755184436 bytes"
        ),
        "SHORT: admits growth of 755184436 bytes".to_string(),
        format!("disk {W1_DISK} (nvme1 on w-1): TARGET"),
        "headroom 1030792151040 bytes >= 2 x 32212254720 = 64424509440".to_string(),
        format!(
            "disk {CP3_DISK} (default-disk-cp3 on cp-3): not a target — node cp-3 holds a replica of this volume"
        ),
        "after: every replica's disk admits growth to twice the volume".to_string(),
        "live: each target passes Longhorn v1.11.3's placement test, \
         IsSchedulableToDisk(size, actualSize), and the volume's next growth there"
            .to_string(),
        format!("kubectl delete replicas.longhorn.io {REP} -n longhorn-system"),
    ] {
        assert!(plan.contains(&want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        !plan.contains("pvc-11111111"),
        "another volume's replica is not this plan's: {plan}"
    );
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(
        l.writes().is_empty(),
        "the plan writes nothing: {:?}",
        l.calls()
    );
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to live free space: a busy disk's
    // storageAvailable moves every second, and a plan that signed it
    // could never be run.
    assert_eq!(l.plan(), (plan.clone(), hash.clone()));
    let mut busy = w1_disk();
    busy.available = 700 * GIB;
    let mut w2 = w2_disk();
    w2.available = 3 * GIB;
    l.nodes(json!([
        lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
        lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
        lh_node("cp-3", &[cp_disk("default-disk-cp3", CP3_DISK)]),
        lh_node("w-1", &[busy]),
        lh_node("w-2", &[w2]),
    ]));
    assert_eq!(
        l.plan().1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
    let mut v = volume(4);
    v["status"]["actualSize"] = json!(25 * GIB);
    l.volume(v);
    assert_eq!(l.plan().1, hash, "nor is the volume's actualSize");
}

#[test]
fn the_plan_refuses_what_the_argv_bound_does_not_admit_and_opens_no_door() {
    let l = Longhorn::new("mvr-argv");
    let other = "pvc-11111111-2222-3333-4444-555555555555-r-0000aaaa";
    let upper = "pvc-93E11A6E-6999-41a8-9df3-622f36b7ff56-r-ae04deab";
    for args in [
        vec!["--plan", "pgdata-postgres-0", REP],
        vec!["--plan", VOL, "ae04deab"],
        vec!["--plan", VOL, upper],
        vec!["--plan", VOL, "-r"],
        vec!["--plan", VOL, &format!("{VOL}-r-ae04dea")],
        vec!["--plan", VOL, other],
        vec!["--plan", VOL],
        vec!["--plan", VOL, REP, "extra"],
        vec![VOL, REP],
        vec![VOL, REP, "not-a-hash"],
    ] {
        let o = l.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(l.calls().is_empty(), "no door opened: {:?}", l.calls());
}

#[test]
fn a_volume_that_is_not_healthy_moves_nothing() {
    let l = Longhorn::new("mvr-unhealthy");
    let plan = ["--plan", VOL, REP];

    // Degraded, or detached: a rebuild needs a healthy, attached volume.
    let mut v = volume(4);
    v["status"]["robustness"] = json!("degraded");
    l.volume(v);
    assert!(l.refused(&plan, &[]).contains("robustness degraded"));
    let mut v = volume(4);
    v["status"]["state"] = json!("detached");
    l.volume(v);
    assert!(l.refused(&plan, &[]).contains("not attached"));
    l.volume(volume(4));

    // A replica still rebuilding, a failed one, one being deleted.
    for (i, (healthy, failed)) in [(false, false), (true, true)].into_iter().enumerate() {
        let mut r = Longhorn::four();
        r[2] = replica(
            VOL,
            &format!("{VOL}-r-03f4dac5"),
            "cp-3",
            CP3_DISK,
            healthy,
            failed,
        );
        l.replicas(r);
        let out = l.refused(&plan, &[]);
        assert!(out.contains("3 healthy"), "case {i}: {out}");
    }
    let mut r = Longhorn::four();
    r[2]["metadata"]["deletionTimestamp"] = json!("2026-10-01T17:00:00Z");
    l.replicas(r);
    assert!(l.refused(&plan, &[]).contains("being deleted"));
    // Healthy but not active (a live-upgrade leftover): Longhorn's
    // isHealthyAndActiveReplica does not count it, and neither does this.
    let mut r = Longhorn::four();
    r[2]["spec"]["active"] = json!(false);
    l.replicas(r);
    assert!(l.refused(&plan, &[]).contains("healthy but not active"));

    // The count and the replicas disagree, both ways.
    l.replicas(Longhorn::four());
    l.volume(volume(5));
    assert!(
        l.refused(&plan, &[])
            .contains("spec.numberOfReplicas 5 and has 4")
    );
    l.volume(volume(3));
    assert!(
        l.refused(&plan, &[])
            .contains("spec.numberOfReplicas 3 and has 4")
    );
    l.volume(volume(4));

    // Two replicas on one node: four replicas, three nodes.
    let mut r = Longhorn::four();
    r[2] = replica(
        VOL,
        &format!("{VOL}-r-03f4dac5"),
        "cp-2",
        CP2_DISK,
        true,
        false,
    );
    l.replicas(r);
    assert!(l.refused(&plan, &[]).contains("on 3 distinct node(s)"));
}

#[test]
fn a_replica_that_is_not_the_volumes_is_refused() {
    let l = Longhorn::new("mvr-not-mine");
    let out = l.refused(&["--plan", VOL, &format!("{VOL}-r-00000000")], &[]);
    assert!(out.contains("is not a replica of"), "{out}");
    assert!(out.contains(REP), "names the replicas it has: {out}");
}

#[test]
fn a_target_disk_that_would_fail_the_webhook_is_refused() {
    let l = Longhorn::new("mvr-short-target");
    let plan = ["--plan", VOL, REP];
    let nodes = |w1: &[Disk]| {
        json!([
            lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
            lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
            lh_node("cp-3", &[cp_disk("default-disk-cp3", CP3_DISK)]),
            lh_node("w-1", w1),
            lh_node("w-2", &[w2_disk()]),
        ])
    };
    // Room for the replica but not its growth: 45 GiB, against 2 x 30.
    let mut tight = w1_disk();
    tight.scheduled = (1800 - 540 - 45) * GIB;
    l.nodes(nodes(&[tight.clone()]));
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains(&format!("disk {W1_DISK} (nvme1 on w-1): TARGET"))
            && out.contains("< 2 x 32212254720")
            && out.contains("would fail the webhook"),
        "{out}"
    );

    // A second disk on w-1: Longhorn picks between them, so the short one
    // is refused even though the big one would do.
    let mut second = tight.clone();
    second.name = "nvme2";
    second.uuid = "0f0f0f0f-1111-4222-8333-444444444444";
    l.nodes(nodes(&[w1_disk(), second.clone()]));
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains("0f0f0f0f-1111-4222-8333-444444444444"),
        "{out}"
    );

    // The short disk does not allow scheduling: Longhorn cannot pick it,
    // so it is no target, and the plan stands on the big one.
    second.allow = false;
    l.nodes(nodes(&[w1_disk(), second]));
    let (p, _) = l.plan();
    assert!(
        p.contains("0f0f0f0f-1111-4222-8333-444444444444 (nvme2 on w-1): not a target — disk: allowScheduling is false"),
        "{p}"
    );

    // No room for the replica at all, and no other node: nothing to move to.
    let mut full = w1_disk();
    full.scheduled = (1800 - 540 - 10) * GIB;
    l.nodes(nodes(&[full]));
    let out = l.refused(&plan, &[]);
    assert!(out.contains("no disk on a node without a replica"), "{out}");
}

/// The estate with w-1's disk replaced, and cp-3's when given.
fn estate(w1: Disk, cp3: Option<Disk>) -> Value {
    json!([
        lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
        lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
        lh_node(
            "cp-3",
            &[cp3.unwrap_or_else(|| cp_disk("default-disk-cp3", CP3_DISK))]
        ),
        lh_node("w-1", &[w1]),
        lh_node("w-2", &[w2_disk()]),
    ])
}

/// Longhorn v1.11.3 refuses on live free space as well as on the ledger.
/// A target whose ledger fits and whose free space does not is refused,
/// with the figures, and nothing is written — by the scheduler's own
/// placement test, IsSchedulableToDisk(size, actualSize), or by
/// CheckReplicasSizeExpansion on the disk as it would stand with the
/// replica on it (review 091904d3 read both from the v1.11.3 source).
#[test]
fn a_target_whose_live_free_space_does_not_fit_is_refused() {
    let l = Longhorn::new("mvr-physical");
    let plan = ["--plan", VOL, REP];

    // The ledger has 960 GiB of room; the disk has 465 GiB free, and the
    // rebuild writes the volume's 20 GiB actualSize: 445 GiB left is not
    // above the floor of 450 (1800 x 25%). The scheduler would not place
    // here at all.
    let mut w1 = w1_disk();
    w1.available = 465 * GIB;
    l.nodes(estate(w1, None));
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains(W1_DISK)
            && out.contains("fails Longhorn v1.11.3's live test")
            && out.contains(&format!(
                "placement: Actual space usage condition failed: CurrentAvailable = {} \
                 (StorageAvailable {} - Required {}) is less than or equal to MinimalAvailable = {}",
                445 * GIB,
                465 * GIB,
                20 * GIB,
                450 * GIB
            )),
        "{out}"
    );

    // Over-provisioning 200 and no reservation: the replica fits (85 - 20
    // GiB free stays above the 50 GiB floor), and the ledger allows 400
    // GiB; the NEXT growth would not — physicalUsed 135 GiB + 30 leaves 35
    // GiB, under the floor (ValidateDiskAvailableForExpansion).
    l.setting("storage-over-provisioning-percentage", "200");
    let mut small = disk("nvme1", W1_DISK, 200 * GIB, 0, 100 * GIB);
    small.available = 85 * GIB;
    l.nodes(estate(small, None));
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains(
            "the next growth, with the replica on it: physical free space would drop below minimal"
        ) && out.contains(&format!("left={} < minimal={}", 35 * GIB, 50 * GIB)),
        "{out}"
    );
    l.setting("storage-over-provisioning-percentage", "100");

    // Signed while the disk had room, refused when the write re-renders
    // and it no longer has: the live half is judged at both.
    l.nodes(estate(w1_disk(), None));
    let (_, hash) = l.plan();
    let mut w1 = w1_disk();
    w1.available = 465 * GIB;
    l.nodes(estate(w1, None));
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("fails Longhorn v1.11.3's live test"), "{out}");
    assert!(l.writes().is_empty(), "{:?}", l.writes());
}

/// Review 091904d3's repro, as a remaining replica's disk: 50000000000
/// free, 35296955596 reserved, 30 GiB scheduled. Longhorn admits the
/// volume's next 30 GiB there (Validate counts the reserve as free; the
/// floor test takes required 0), and the first library refused it, so a
/// finished move read "NOT every replica's disk has room".
#[test]
fn a_remaining_disk_longhorn_would_grow_on_is_not_called_short() {
    let l = Longhorn::new("mvr-reserve-is-free");
    let mut cp3 = disk("default-disk-cp3", CP3_DISK, W2_MAX, W2_RESERVED, 30 * GIB);
    cp3.available = 50_000_000_000;
    l.nodes(estate(w1_disk(), Some(cp3)));
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!out.contains("would refuse the volume's growth"), "{out}");
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}

#[test]
fn a_replica_left_on_a_disk_without_live_free_space_is_not_the_effect() {
    let l = Longhorn::new("mvr-physical-after");
    // cp-3's ledger has room; its disk has 120 GiB free against a 125 GiB
    // floor, so IsSchedulableToDisk refuses ANY growth there.
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.available = 120 * GIB;
    l.nodes(estate(w1_disk(), Some(cp3)));
    let o = l.run(&["--plan", VOL, REP], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains(
            "a remaining replica disk that would refuse the volume's growth to twice today"
        ) && out.contains(CP3_DISK),
        "the plan says so on stderr: {out}"
    );
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("NOT every replica's disk")
            && out.contains("Actual space usage condition failed")
            && out.contains(CP3_DISK),
        "{out}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

/// Under the plan's settings Longhorn trims nothing when the count is
/// lowered (v1.11.3 cleanupExtraHealthyReplicas trims only for eviction,
/// best-effort data locality, or auto-balance). Each of the three is a
/// refusal, resolved as Longhorn resolves it.
#[test]
fn a_volume_longhorn_would_trim_on_its_own_is_refused() {
    let l = Longhorn::new("mvr-trims");
    let plan = ["--plan", VOL, REP];

    for locality in ["best-effort", "strict-local"] {
        let mut v = volume(4);
        v["spec"]["dataLocality"] = json!(locality);
        l.volume(v);
        let out = l.refused(&plan, &[]);
        assert!(
            out.contains(&format!("data locality {locality}"))
                && out.contains("cleanupDataLocalityReplicas"),
            "{out}"
        );
    }
    let mut v = volume(4);
    v["spec"]["dataLocality"] = json!("");
    l.volume(v);
    l.plan();

    // Auto-balance: the volume's word unless "" or ignored, then the
    // setting, with the setting's ignored read as disabled.
    l.volume(volume(4));
    for setting in ["least-effort", "best-effort"] {
        l.setting("replica-auto-balance", setting);
        let out = l.refused(&plan, &[]);
        assert!(
            out.contains(&format!("it resolves to {setting}"))
                && out.contains("cleanupAutoBalancedReplicas"),
            "{out}"
        );
    }
    let mut v = volume(4);
    v["spec"]["replicaAutoBalance"] = json!("disabled");
    l.volume(v);
    l.plan();
    let mut v = volume(4);
    v["spec"]["replicaAutoBalance"] = json!("best-effort");
    l.volume(v);
    l.setting("replica-auto-balance", "disabled");
    l.refused(&plan, &[]);
    l.volume(volume(4));
    l.setting("replica-auto-balance", "ignored");
    l.plan();
    l.setting("replica-auto-balance", "disabled");

    // An eviction on a replica, on the moving replica's node, or on a
    // replica's disk.
    let mut r = Longhorn::four();
    r[3]["spec"]["evictionRequested"] = json!(true);
    l.replicas(r);
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains(&format!("replica {REP} has evictionRequested")),
        "{out}"
    );
    l.replicas(Longhorn::four());
    let mut n = estate(w1_disk(), None);
    n[4]["spec"]["evictionRequested"] = json!(true);
    l.nodes(n);
    let out = l.refused(&plan, &[]);
    assert!(out.contains("node w-2, which holds"), "{out}");
    let mut n = estate(w1_disk(), None);
    n[0]["spec"]["disks"]["default-disk-cp1"]["evictionRequested"] = json!(true);
    l.nodes(n);
    let out = l.refused(&plan, &[]);
    assert!(out.contains(&format!("disk {CP1_DISK}")), "{out}");
}

#[test]
fn a_volume_that_may_put_two_replicas_on_one_node_is_refused() {
    let l = Longhorn::new("mvr-soft");
    let plan = ["--plan", VOL, REP];
    l.setting("replica-soft-anti-affinity", "true");
    assert!(
        l.refused(&plan, &[])
            .contains("may place two replicas on one node")
    );
    // The volume's own word wins over the setting, both ways.
    let mut v = volume(4);
    v["spec"]["replicaSoftAntiAffinity"] = json!("disabled");
    l.volume(v);
    l.plan();
    l.setting("replica-soft-anti-affinity", "false");
    let mut v = volume(4);
    v["spec"]["replicaSoftAntiAffinity"] = json!("enabled");
    l.volume(v);
    l.refused(&plan, &[]);
}

#[test]
fn a_missing_volume_is_refused_and_a_read_that_cannot_look_cannot_answer() {
    let l = Longhorn::new("mvr-dark");
    let other = "pvc-00000000-0000-0000-0000-000000000000";
    let out = l.refused(&["--plan", other, &format!("{other}-r-00000000")], &[]);
    assert!(out.contains("NotFound"), "{out}");

    let o = l.run(&["--plan", VOL, REP], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a forbidden read is a failure: {out}"
    );
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");

    // An answer carrying no document — exit 0, no bytes — is not an empty
    // cluster: on jq-1.6 `jq -e` alone would pass it (d96e38ab).
    for resource in [
        "volumes.longhorn.io",
        "replicas.longhorn.io",
        "nodes.longhorn.io",
        "settings.longhorn.io",
    ] {
        let o = l.run(&["--plan", VOL, REP], &[("STUB_EMPTY", resource)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{resource}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{resource}: {out}");
    }
    assert!(l.writes().is_empty(), "{:?}", l.calls());
}

#[test]
fn a_replica_disk_no_node_reports_cannot_answer() {
    // A replica names its disk by UUID. One no node reports — or a
    // fixture that names a disk by its NAME, as volume_replicas_sh's did
    // — is a ledger nothing can sum.
    let l = Longhorn::new("mvr-unknown-disk");
    let mut r = Longhorn::four();
    r[3] = replica(VOL, REP, "w-2", "default-disk-fd0100000000", true, false);
    l.replicas(r);
    let o = l.run(&["--plan", VOL, REP], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("disk default-disk-fd0100000000 on w-2"),
        "{out}"
    );
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_raises_waits_lowers_then_deletes_and_reads_the_effect_back() {
    let l = Longhorn::new("mvr-write");
    let (plan, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );

    // In order: raise 4 -> 5, lower 5 -> 4, and only then the delete. A
    // delete under a count of 5 is replenished onto the node it left (the
    // stub does what Longhorn does), and the move would undo itself.
    let w = l.writes();
    assert_eq!(w.len(), 3, "two compare-and-sets and one delete: {w:?}");
    assert!(
        w[0].starts_with(&format!(
            "patch volumes.longhorn.io {VOL} -n longhorn-system --type=json"
        )) && w[0].contains(r#"{"op":"test","path":"/spec/numberOfReplicas","value":4}"#)
            && w[0].contains(r#"{"op":"replace","path":"/spec/numberOfReplicas","value":5}"#),
        "{w:?}"
    );
    assert!(
        w[1].contains(r#"{"op":"test","path":"/spec/numberOfReplicas","value":5}"#)
            && w[1].contains(r#"{"op":"replace","path":"/spec/numberOfReplicas","value":4}"#),
        "{w:?}"
    );
    assert!(
        w[2].starts_with(&format!(
            "delete replicas.longhorn.io {REP} -n longhorn-system"
        )),
        "{w:?}"
    );

    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    let now = l.replicas_now();
    assert!(!now.iter().any(|r| r.starts_with(REP)), "{now:?}");
    assert!(
        now.contains(&format!("{VOL}-r-e0e0e0e1 on w-1 ({W1_DISK})")),
        "{now:?}"
    );
    assert_eq!(now.len(), 4, "{now:?}");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("(cp-1, cp-2, cp-3, w-1)") && hits[0].contains(REP),
        "names the nodes and the replica: {hits:?}"
    );
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let l = Longhorn::new("mvr-drift");
    let (_, hash) = l.plan();
    // The ledger moved after the plan was signed: another volume
    // scheduled on w-1.
    let mut w1 = w1_disk();
    w1.scheduled += 5 * GIB;
    l.nodes(json!([
        lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
        lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
        lh_node("cp-3", &[cp_disk("default-disk-cp3", CP3_DISK)]),
        lh_node("w-1", &[w1]),
        lh_node("w-2", &[w2_disk()]),
    ]));
    // Refused with today's plan printed beside it, so the operator sees
    // what moved — and nothing written.
    for bad in [hash.clone(), "0".repeat(64)] {
        let o = l.run(&[VOL, REP, &bad], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{out}");
        assert!(out.contains(&bad), "names the approved hash: {out}");
        assert!(l.writes().is_empty(), "{:?}", l.writes());
    }

    // Applied once, the approval is spent: the replica is gone.
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(l.writes().len(), 3, "no second move: {:?}", l.writes());
}

#[test]
fn a_stalled_rebuild_leaves_the_count_raised_and_deletes_nothing() {
    let l = Longhorn::new("mvr-stall");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_REBUILD", "never")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(
        out.contains("STALLED") && out.contains("STAYS 5") && out.contains("NOT deleted"),
        "{out}"
    );
    assert!(
        out.contains("r-e0e0e0e1 on w-1"),
        "names the replica it waited on: {out}"
    );
    assert_eq!(
        l.writes().len(),
        1,
        "the raise and nothing after: {:?}",
        l.writes()
    );
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 5);
    assert!(l.replicas_now().iter().any(|r| r.starts_with(REP)));
}

#[test]
fn a_new_replica_that_lands_off_the_plan_deletes_nothing() {
    let l = Longhorn::new("mvr-off-plan");
    let (_, hash) = l.plan();
    let o = l.run(
        &[VOL, REP, &hash],
        &[("STUB_NEW_DISK", "0f0f0f0f-1111-4222-8333-444444444444")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("did not name as a target") && out.contains("NOT deleted"),
        "{out}"
    );
    assert_eq!(l.writes().len(), 1, "{:?}", l.writes());
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 5);
    assert!(effect_lines(&out).is_empty(), "{out}");
}

/// Another replica removed during the settle — gone, or still listed and
/// terminating — stops the run before the delete, which would leave N-1.
#[test]
fn another_replica_removed_during_the_settle_stops_the_delete() {
    for terminating in ["", "1"] {
        let l = Longhorn::new("mvr-trim-other");
        let (_, hash) = l.plan();
        let cp3 = format!("{VOL}-r-03f4dac5");
        let o = l.run(
            &[VOL, REP, &hash],
            &[("STUB_TRIM", &cp3), ("STUB_TERMINATING", terminating)],
        );
        let out = text(&o);
        assert_eq!(
            o.status.code(),
            Some(1),
            "terminating={terminating:?}: {out}"
        );
        assert!(out.contains("NOT deleted") && out.contains(&cp3), "{out}");
        let w = l.writes();
        assert_eq!(w.len(), 2, "raise and lower, never the delete: {w:?}");
        assert!(
            l.replicas_now().iter().any(|r| r.starts_with(REP)),
            "the named replica stands"
        );
        assert!(effect_lines(&out).is_empty(), "{out}");
    }
}

/// The named replica removed by another hand during the settle is the move
/// done — whether it is already gone or still listed with a
/// deletionTimestamp while its finalizer clears (review 091904d3 B2: read
/// as present, it failed the run as "removed one that is not" it).
#[test]
fn the_named_replica_removed_during_the_settle_is_the_move() {
    for terminating in ["", "1"] {
        let l = Longhorn::new("mvr-trim-mine");
        let (_, hash) = l.plan();
        let o = l.run(
            &[VOL, REP, &hash],
            &[("STUB_TRIM", REP), ("STUB_TERMINATING", terminating)],
        );
        let out = text(&o);
        assert!(o.status.success(), "terminating={terminating:?}: {out}");
        assert_eq!(
            l.writes().len(),
            2,
            "nothing left to delete: {:?}",
            l.writes()
        );
        assert!(out.contains("removed by another hand"), "{out}");
        assert_eq!(effect_lines(&out).len(), 1, "{out}");
    }
}

/// The delete's own finalizer: the replica stays listed, terminating, and
/// the read-back waits for it to go before it prints the effect.
#[test]
fn the_read_back_waits_for_the_deleted_replica_to_go() {
    let l = Longhorn::new("mvr-finalizer");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_TERMINATING", "1")]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert_eq!(l.writes().len(), 3, "{:?}", l.writes());
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
    assert!(!l.replicas_now().iter().any(|r| r.starts_with(REP)));
}

#[test]
fn a_replica_left_on_a_short_disk_is_not_the_effect() {
    let l = Longhorn::new("mvr-two-short");
    // cp-3's disk is short too: 20 GiB of room for a 30 GiB replica's growth.
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.scheduled = (500 - 150 - 20) * GIB;
    l.nodes(json!([
        lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
        lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
        lh_node("cp-3", &[cp3]),
        lh_node("w-1", &[w1_disk()]),
        lh_node("w-2", &[w2_disk()]),
    ]));
    let (plan, hash) = l.plan();
    assert!(
        plan.contains("SHORT of twice the volume") && plan.contains(CP3_DISK),
        "the plan says so before it is signed: {plan}"
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("{REP} is gone")) && out.contains("NOT every replica's disk"),
        "{out}"
    );
    assert!(out.contains(CP3_DISK), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_patch_the_server_refuses_is_a_failure_with_its_words() {
    let l = Longhorn::new("mvr-patch-refused");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(
        !l.writes().iter().any(|c| c.starts_with("delete ")),
        "{:?}",
        l.writes()
    );
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    assert!(effect_lines(&out).is_empty(), "{out}");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("move-volume-replica");
    let p = verb("plan-a-volume-replica-move");
    for (name, v) in [
        ("move-volume-replica", &w),
        ("plan-a-volume-replica-move", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert_eq!(v["params"][0]["name"], "volume", "{name}");
        assert_eq!(v["params"][1]["name"], "replica", "{name}");
    }
    assert_eq!(p["argv"][1], "--plan");
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-volume-replica-move");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 1200 + 30 + 180,
        "the verb's timeout outlasts the script's own waits"
    );
    let vol = w["params"][0]["pattern"].as_str().expect("a pattern");
    let rep = w["params"][1]["pattern"].as_str().expect("a pattern");
    assert_eq!(Some(vol), p["params"][0]["pattern"].as_str());
    assert_eq!(Some(rep), p["params"][1]["pattern"].as_str());
    assert!(jq_test(vol, VOL));
    assert!(jq_test(rep, REP));
    for bad in [
        "pgdata-postgres-0",
        "-pvc",
        REP,
        "pvc-93E11A6E-6999-41a8-9df3-622f36b7ff56",
    ] {
        assert!(!jq_test(vol, bad), "the volume pattern admits {bad:?}");
    }
    for bad in [
        VOL,
        "ae04deab",
        "-r",
        "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04dea",
        "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-AE04DEAB",
        "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab x",
    ] {
        assert!(!jq_test(rep, bad), "the replica pattern admits {bad:?}");
    }
}

#[test]
fn final_readback_refuses_a_failed_pinned_replica_and_short_reserved_growth() {
    let l = Longhorn::new("review-8f52f401-failed-new");
    let mut w1 = w1_disk();
    // Initially 90 GiB of ledger room admits the approved 30 GiB replica
    // and its next 30 GiB growth. Two retained replica CRs later reserve
    // 60 GiB, leaving only 30 GiB for their combined 60 GiB growth.
    w1.scheduled = (1800 - 540 - 90) * GIB;
    l.nodes(estate(w1, None));
    let (_, hash) = l.plan();
    let hook = r#"
    if [ -n "${STUB_FAIL_NEW_AFTER_DELETE:-}" ]; then
        jq --arg r "$v-r-e0e0e0e1" 'map(if .metadata.name == $r then .spec.failedAt = "2026-10-02T02:20:00Z" else . end)' \
            "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
        add_replica "$v" "$v-r-bad00bad" "w-1" "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10" "2026-10-02T02:20:01Z"
        charge "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10" "$size"
    fi
"#;
    let marker = "    echo \"replica.longhorn.io \\\"$r\\\" deleted\"; exit 0 ;;";
    assert!(STUB.contains(marker));
    write_exec(
        &l.dir.join("bin/sudo"),
        &STUB.replace(marker, &format!("{hook}\n{marker}")),
    );
    let o = l.run(&[VOL, REP, &hash], &[("STUB_FAIL_NEW_AFTER_DELETE", "1")]);
    let out = text(&o);
    let reps = l.read_state("replicas.json");
    let retained: Vec<&Value> = reps
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["spec"]["volumeName"] == VOL && r["spec"]["diskID"] == W1_DISK)
        .collect();
    assert_eq!(
        retained.len(),
        2,
        "two assigned replica CRs remain on the disk"
    );
    assert!(retained.iter().any(
        |r| r["metadata"]["name"] == format!("{VOL}-r-e0e0e0e1") && r["spec"]["failedAt"] != ""
    ));
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    let nodes = l.read_state("lhnodes.json");
    let scheduled = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["metadata"]["name"] == "w-1")
        .unwrap()["status"]["diskStatus"]["nvme1"]["storageScheduled"]
        .as_u64()
        .unwrap();
    assert_eq!((1800 - 540) * GIB - scheduled, 30 * GIB);
    assert!(
        !o.status.success() && effect_lines(&out).is_empty(),
        "a failed pinned new replica and a disk whose two CRs cannot both grow must not be reported proven: {out}"
    );
}

#[test]
fn final_readback_counts_growth_reserved_by_retained_failed_replicas() {
    let l = Longhorn::new("review-8f52f401-failed-other");
    let mut nodes = estate(w1_disk(), None);
    nodes[0]["status"]["diskStatus"]["default-disk-cp1"]["storageScheduled"] =
        json!((500 - 150 - 60) * GIB);
    l.nodes(nodes);
    let (_, hash) = l.plan();
    let hook = r#"
    if [ -n "${STUB_FAIL_OTHER_AFTER_DELETE:-}" ]; then
        jq --arg r "$v-r-7c1934ba" 'map(if .metadata.name == $r then .spec.failedAt = "2026-10-02T02:20:00Z" else . end)' \
            "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
        add_replica "$v" "$v-r-bad00bad" "cp-1" "844915d9-6a5b-4da9-9619-8f3d188d632b" "2026-10-02T02:20:01Z"
        charge "844915d9-6a5b-4da9-9619-8f3d188d632b" "$size"
    fi
"#;
    let marker = "    echo \"replica.longhorn.io \\\"$r\\\" deleted\"; exit 0 ;;";
    assert!(STUB.contains(marker));
    write_exec(
        &l.dir.join("bin/sudo"),
        &STUB.replace(marker, &format!("{hook}\n{marker}")),
    );
    let o = l.run(&[VOL, REP, &hash], &[("STUB_FAIL_OTHER_AFTER_DELETE", "1")]);
    let out = text(&o);
    let reps = l.read_state("replicas.json");
    assert_eq!(
        reps.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["spec"]["volumeName"] == VOL && r["spec"]["diskID"] == CP1_DISK)
            .count(),
        2
    );
    assert!(
        reps.as_array()
            .unwrap()
            .iter()
            .any(|r| r["metadata"]["name"] == format!("{VOL}-r-e0e0e0e1")
                && r["spec"]["failedAt"] == "")
    );
    let nodes = l.read_state("lhnodes.json");
    let scheduled = nodes[0]["status"]["diskStatus"]["default-disk-cp1"]["storageScheduled"]
        .as_u64()
        .unwrap();
    assert_eq!((500 - 150) * GIB - scheduled, 30 * GIB);
    assert!(
        !o.status.success() && effect_lines(&out).is_empty(),
        "the pinned new replica is healthy, but two retained CRs need 60 GiB growth with only 30 GiB room: {out}"
    );
}

fn after_delete(l: &Longhorn, hook: &str) {
    let marker = "    echo \"replica.longhorn.io \\\"$r\\\" deleted\"; exit 0 ;;";
    assert!(STUB.contains(marker));
    write_exec(
        &l.dir.join("bin/sudo"),
        &STUB.replace(marker, &format!("{hook}\n{marker}")),
    );
}

#[test]
fn final_readback_requires_the_pinned_new_replica_healthy_even_with_room() {
    let l = Longhorn::new("pinned-new-failed-with-room");
    l.nodes(estate(w1_disk(), None));
    let (_, hash) = l.plan();
    after_delete(
        &l,
        r#"
    jq --arg r "$v-r-e0e0e0e1" 'map(if .metadata.name == $r then .spec.failedAt = "2026-10-02T02:20:00Z" else . end)' \
        "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    add_replica "$v" "$v-r-bad00bad" "w-1" "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10" "2026-10-02T02:20:01Z"
    charge "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10" "$size"
"#,
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    let reps = l.read_state("replicas.json");
    assert!(
        reps.as_array()
            .unwrap()
            .iter()
            .any(|r| r["metadata"]["name"] == format!("{VOL}-r-e0e0e0e1")
                && r["spec"]["failedAt"] != "")
    );
    let nodes = l.read_state("lhnodes.json");
    let scheduled = nodes[3]["status"]["diskStatus"]["nvme1"]["storageScheduled"]
        .as_u64()
        .unwrap();
    assert!((1800 - 540) * GIB - scheduled >= 60 * GIB);
    assert!(
        !o.status.success() && effect_lines(&out).is_empty(),
        "a replacement's health does not prove the pinned replica healthy: {out}"
    );
}

#[test]
fn final_readback_requires_the_pinned_new_replica_on_its_observed_disk() {
    let l = Longhorn::new("pinned-new-changed-disk");
    let mut other = w1_disk();
    other.name = "nvme2";
    other.uuid = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f11";
    let mut nodes = estate(w1_disk(), None);
    nodes[3] = lh_node("w-1", &[w1_disk(), other]);
    l.nodes(nodes);
    let (_, hash) = l.plan();
    after_delete(
        &l,
        r#"
    jq --arg r "$v-r-e0e0e0e1" 'map(if .metadata.name == $r then .spec.diskID = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f11" else . end)' \
        "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    charge "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10" "-$size"
    charge "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f11" "$size"
"#,
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    let reps = l.read_state("replicas.json");
    assert!(
        reps.as_array()
            .unwrap()
            .iter()
            .any(|r| r["metadata"]["name"] == format!("{VOL}-r-e0e0e0e1")
                && r["spec"]["diskID"] == "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f11"
                && r["spec"]["failedAt"] == "")
    );
    assert!(
        !o.status.success() && effect_lines(&out).is_empty(),
        "the effect must prove the exact disk it names: {out}"
    );
}

#[test]
fn final_readback_admits_retained_failed_replicas_when_all_growth_fits() {
    let l = Longhorn::new("retained-failed-growth-fits");
    l.nodes(estate(w1_disk(), None));
    let (_, hash) = l.plan();
    after_delete(
        &l,
        r#"
    add_replica "$v" "$v-r-bad00bad" "cp-1" "844915d9-6a5b-4da9-9619-8f3d188d632b" "2026-10-02T02:20:01Z"
    jq --arg r "$v-r-bad00bad" 'map(if .metadata.name == $r then .spec.failedAt = "2026-10-02T02:20:02Z" else . end)' \
        "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    charge "844915d9-6a5b-4da9-9619-8f3d188d632b" "$size"
"#,
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    let reps = l.read_state("replicas.json");
    assert_eq!(
        reps.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["spec"]["volumeName"] == VOL && r["spec"]["diskID"] == CP1_DISK)
            .count(),
        2
    );
    assert!(
        o.status.success(),
        "retaining a failed CR does not itself forbid a proven move: {out}"
    );
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}
