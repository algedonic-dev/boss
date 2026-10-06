//! The two ops verbs that retire one replica of a Longhorn volume from the
//! forge (backlog 5ef9d2d9, incident d3c0a67c): the read-only
//! `plan-a-volume-replica-retirement` (`infra/forge/
//! retire-volume-replica.sh --plan`) and the passkey-approved
//! `retire-volume-replica`.
//!
//! WHY. The system of record's database volume (pvc-93e11a6e…, 30 GiB)
//! holds four replicas, on cp-1, cp-2, cp-3 and w-2, and the w-2 one sits
//! on disk bf045701-… whose scheduling ledger has ~755 MB left — so
//! Longhorn's admission webhook refuses any growth of the volume. David
//! (2026-10-01) chose to retire that replica and run at three rather than
//! move it, and no verb lowered a replica count (set-volume-replicas only
//! raises). Retiring is lower the count, then delete the named replica BY
//! NAME: Longhorn v1.11.3 trims nothing on a lowered count unless data
//! locality, replica auto-balance or an eviction asks it to
//! (volume_controller.go:1129-1157, adversarial review run 091904d3), and
//! the plan refuses all three, so the delete is the only removal. It is
//! also the door back down after a move that raised a count and stopped.
//!
//! THE ESTATE in these tests is that one, by its own numbers: w-2's disk
//! is the one the webhook named (StorageMaximum 117656518656,
//! StorageReserved 35296955596, and a storageScheduled that leaves
//! 755184436 bytes of room), and replicas name their disks by UUID as
//! live replicas do (spec.diskID = diskStatus[].diskUUID).
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc` — move_volume_replica_sh's stand-in, with what a
//! retirement meets added. It behaves as Longhorn does where the order
//! matters: a deleted replica stays LISTED, terminating, for one read
//! (its finalizer), and counts toward the replica count until it goes
//! (getReplenishReplicasCount); once it goes, if fewer replicas stand than
//! the count, Longhorn replenishes one onto the node it left. Every door
//! call lands in `argv`, so the order of the patch and the delete, and
//! their absence on a refusal, are read from the record. Nothing here
//! reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/retire-volume-replica.sh";
const LIB: &str = "infra/lib/longhorn-ledger.sh";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
const REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab";
/// The replica a stalled move raised onto w-1.
const NEW: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-e0e0e0e1";
const GIB: u64 = 1024 * 1024 * 1024;
const SIZE: u64 = 30 * GIB;
/// w-2's disk, by the webhook's own numbers of 2026-10-01.
const W2_DISK: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";
const W2_MAX: u64 = 117_656_518_656;
const W2_RESERVED: u64 = 35_296_955_596;
const W2_SCHEDULED: u64 = 81_604_378_624;
const W1_DISK: &str = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10";
const CP1_DISK: &str = "844915d9-6a5b-4da9-9619-8f3d188d632b";
const CP2_DISK: &str = "976a5246-7a68-452c-bb25-f9e3ecb2258e";
const CP3_DISK: &str = "9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `vol-<name>.json`, `setting-<name>.json`, and the `replicas.json` and
/// `lhnodes.json` lists (arrays).
///
/// A delete stamps the replica's deletionTimestamp; the next list of
/// replicas shows it so, and then its finalizer clears (unless
/// STUB_FINALIZER_STUCK=1): it is removed, its disk credited, and if fewer
/// replicas of its volume then stand than the count, one is replenished
/// onto the SAME node and disk. After a patch and after a removal the
/// volume's robustness is recomputed: healthy when its healthy replicas
/// meet the count, degraded otherwise — unless STUB_DEGRADE_ON_DELETE=1
/// has pinned it degraded at the delete. STUB_SQUEEZE_ON_DELETE=<uuid>
/// drops that disk's storageAvailable to 1 GiB at the delete.
///
/// A LOWER of spec.numberOfReplicas trims nothing — Longhorn v1.11.3 under
/// the settings the plan admits — unless STUB_TRIM names a replica,
/// standing in for a foreign act: removed at once, or with
/// STUB_TRIM_MARK=1 stamped terminating. STUB_FAIL marks one failed.
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
reps() { # <jq program> — rewrite the replica list
    jq "$1" "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
}
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
robust() { # <volume>
    f="$S/vol-$1.json"
    [ -f "$S/pinned-degraded" ] && r=degraded || r=$(jq -r --arg v "$1" --argjson c "$(jq '.spec.numberOfReplicas' "$f")" \
        '[.[] | select(.spec.volumeName == $v and (.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "" and .spec.active == true and (.metadata.deletionTimestamp // "") == "")] | if length >= $c then "healthy" else "degraded" end' \
        "$S/replicas.json")
    jq --arg r "$r" '.status.robustness = $r' "$f" > "$f.new" && mv "$f.new" "$f"
}
finalize() { # every replica already shown terminating goes
    [ -n "${STUB_FINALIZER_STUCK:-}" ] && return 0
    for r in $(jq -r '.[] | select((.metadata.deletionTimestamp // "") != "") | .metadata.name' "$S/replicas.json"); do
        row=$(jq -c --arg r "$r" '.[] | select(.metadata.name == $r)' "$S/replicas.json")
        v=$(printf '%s' "$row" | jq -r '.spec.volumeName')
        node=$(printf '%s' "$row" | jq -r '.spec.nodeID')
        disk=$(printf '%s' "$row" | jq -r '.spec.diskID')
        f="$S/vol-$v.json"
        size=$(jq -r '.spec.size' "$f")
        reps "map(select(.metadata.name != \"$r\"))"
        charge "$disk" "-$size"
        left=$(jq --arg v "$v" '[.[] | select(.spec.volumeName == $v)] | length' "$S/replicas.json")
        if [ "$left" -lt "$(jq '.spec.numberOfReplicas' "$f")" ]; then
            add_replica "$v" "$v-r-feedface" "$node" "$disk" "2026-10-01T18:05:00Z"
            charge "$disk" "$size"
        fi
        robust "$v"
    done
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
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"
    finalize; exit 0 ;;
"get nodes.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/lhnodes.json")"; exit 0 ;;
"patch volumes.longhorn.io")
    v="$3"; f="$S/vol-$v.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    want=$(printf '%s' "$p" | jq -c '.[] | select(.op == "test" and .path == "/spec/numberOfReplicas") | .value')
    new=$(printf '%s' "$p" | jq -c '.[] | select(.op == "replace" and .path == "/spec/numberOfReplicas") | .value')
    if [ -n "${STUB_COUNT_RACE:-}" ]; then
        jq '.spec.numberOfReplicas += 1' "$f" > "$f.new" && mv "$f.new" "$f"
    fi
    cur=$(jq -c '.spec.numberOfReplicas' "$f")
    [ -n "$want" ] && [ "$want" = "$cur" ] || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    size=$(jq -r '.spec.size' "$f")
    if [ -n "$new" ]; then
        jq --argjson n "$new" '.spec.numberOfReplicas = $n' "$f" > "$f.new" && mv "$f.new" "$f"
    fi
    if [ -n "$new" ] && [ "$new" -lt "$cur" ]; then
        if [ -n "${STUB_TRIM:-}" ] && [ -n "${STUB_TRIM_MARK:-}" ]; then
            reps "map(if .metadata.name == \"$STUB_TRIM\" then .metadata.deletionTimestamp = \"2026-10-01T18:10:00Z\" else . end)"
        elif [ -n "${STUB_TRIM:-}" ]; then
            d=$(jq -r --arg r "$STUB_TRIM" '.[] | select(.metadata.name == $r) | .spec.diskID' "$S/replicas.json")
            reps "map(select(.metadata.name != \"$STUB_TRIM\"))"
            charge "$d" "-$size"
        fi
        if [ -n "${STUB_FAIL:-}" ]; then
            reps "map(if .metadata.name == \"$STUB_FAIL\" then .spec.failedAt = \"2026-10-01T18:10:00Z\" else . end)"
        fi
    fi
    robust "$v"
    echo "volume.longhorn.io/$v patched"; exit 0 ;;
"delete replicas.longhorn.io")
    r="$3"
    [ -z "${STUB_DELETE_FAIL:-}" ] || { echo "Error from server: deletion refused" >&2; exit 1; }
    row=$(jq -c --arg r "$r" '.[] | select(.metadata.name == $r)' "$S/replicas.json")
    [ -n "$row" ] || { echo "Error from server (NotFound): replicas.longhorn.io \"$r\" not found" >&2; exit 1; }
    reps "map(if .metadata.name == \"$r\" then .metadata.deletionTimestamp = (.metadata.deletionTimestamp // \"2026-10-01T18:12:00Z\") else . end)"
    v=$(printf '%s' "$row" | jq -r '.spec.volumeName')
    if [ -n "${STUB_DEGRADE_ON_DELETE:-}" ]; then
        : > "$S/pinned-degraded"
        robust "$v"
    fi
    if [ -n "${STUB_SQUEEZE_ON_DELETE:-}" ]; then
        jq --arg u "$STUB_SQUEEZE_ON_DELETE" \
            'map(.status.diskStatus |= with_entries(if .value.diskUUID == $u then .value.storageAvailable = 1073741824 else . end))' \
            "$S/lhnodes.json" > "$S/lhnodes.new" && mv "$S/lhnodes.new" "$S/lhnodes.json"
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
            "retire_volume_replica_sh: no {tool} on this box — the gate image has it, and a \
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
                 "replicaAutoBalance": "ignored",
                 "dataEngine": "v1", "nodeID": "cp-1"},
        "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "cp-1",
                   "kubernetesStatus": {"namespace": "boss", "pvcName": "pgdata-postgres-0",
                                        "pvName": VOL}}
    })
}

fn replica(vol: &str, name: &str, node: &str, disk: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system",
                     "labels": {"longhornvolume": vol}},
        "spec": {"volumeName": vol, "nodeID": node, "diskID": disk, "active": true,
                 "healthyAt": if healthy { "2026-09-12T10:00:00Z" } else { "" },
                 "failedAt": if failed { "2026-09-20T10:00:00Z" } else { "" }},
        "status": {"currentState": if failed { "stopped" } else { "running" }}
    })
}

/// One Longhorn disk: its name, UUID and ledger in bytes. `available` is
/// the live free space.
#[derive(Clone)]
struct Disk {
    name: &'static str,
    uuid: &'static str,
    max: u64,
    reserved: u64,
    scheduled: u64,
    available: u64,
    evict: bool,
}

fn disk(name: &'static str, uuid: &'static str, max: u64, reserved: u64, scheduled: u64) -> Disk {
    Disk {
        name,
        uuid,
        max,
        reserved,
        scheduled,
        available: max / 2,
        evict: false,
    }
}

fn lh_node(name: &str, disks: &[Disk]) -> Value {
    let mut spec = serde_json::Map::new();
    let mut status = serde_json::Map::new();
    for d in disks {
        spec.insert(
            d.name.to_string(),
            json!({"allowScheduling": true, "evictionRequested": d.evict,
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

/// The control planes' disks: 500 GiB, 150 reserved, 100 scheduled — 250
/// GiB of ledger room; 250 GiB free against a floor of 125.
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

/// The estate, with cp-3's disk replaced when given.
fn estate(cp3: Option<Disk>) -> Value {
    json!([
        lh_node("cp-1", &[cp_disk("default-disk-cp1", CP1_DISK)]),
        lh_node("cp-2", &[cp_disk("default-disk-cp2", CP2_DISK)]),
        lh_node(
            "cp-3",
            &[cp3.unwrap_or_else(|| cp_disk("default-disk-cp3", CP3_DISK))]
        ),
        lh_node(
            "w-1",
            &[disk("nvme1", W1_DISK, 1800 * GIB, 540 * GIB, 300 * GIB)]
        ),
        lh_node("w-2", &[w2_disk()]),
    ])
}

struct Longhorn {
    dir: PathBuf,
}

impl Longhorn {
    /// The estate of 2026-10-01: four healthy replicas on cp-1, cp-2,
    /// cp-3 and w-2, the w-2 one on the disk the webhook named; w-1 holds
    /// only another volume's replica.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.volume(volume(4));
        l.replicas(Self::four());
        l.nodes(estate(None));
        l.setting("storage-over-provisioning-percentage", "100");
        l.setting("storage-minimal-available-percentage", "25");
        l.setting("replica-auto-balance", "disabled");
        l
    }

    fn cp(node: &str) -> Value {
        let (name, disk) = match node {
            "cp-1" => (format!("{VOL}-r-7c1934ba"), CP1_DISK),
            "cp-2" => (format!("{VOL}-r-1269eb6e"), CP2_DISK),
            _ => (format!("{VOL}-r-03f4dac5"), CP3_DISK),
        };
        replica(VOL, &name, node, disk, true, false)
    }

    fn four() -> Value {
        json!([
            Self::cp("cp-1"),
            Self::cp("cp-2"),
            Self::cp("cp-3"),
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

    /// What a stalled move leaves: the count raised to 5 and a fifth
    /// replica on w-1, in the state given.
    fn raised(&self, healthy: bool, failed: bool) {
        let mut v = volume(5);
        if !healthy {
            v["status"]["robustness"] = json!("degraded");
        }
        self.volume(v);
        let mut r = Self::four();
        r.as_array_mut()
            .expect("an array")
            .push(replica(VOL, NEW, "w-1", W1_DISK, healthy, failed));
        self.replicas(r);
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

    /// This volume's replicas now, `<name> on <node>`, sorted.
    fn replicas_now(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .read_state("replicas.json")
            .as_array()
            .expect("an array")
            .iter()
            .filter(|r| r["spec"]["volumeName"] == VOL)
            .map(|r| {
                format!(
                    "{} on {}",
                    r["metadata"]["name"].as_str().unwrap_or_default(),
                    r["spec"]["nodeID"].as_str().unwrap_or_default()
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
            .env_remove("BOSS_RETIRE_FLOOR")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_RETIRE_SETTLE_S", "1")
            .env("BOSS_RETIRE_READBACK_S", "3")
            .env("BOSS_RETIRE_POLL_S", "1")
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

    fn plan_of(&self, rep: &str, env: &[(&str, &str)]) -> (String, String) {
        let o = self.run(&["--plan", VOL, rep], env);
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

    fn plan(&self) -> (String, String) {
        self.plan_of(REP, &[])
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
    let re = verb("retire-volume-replica")["effect"]
        .as_str()
        .expect("retire-volume-replica declares an `effect`")
        .to_string();
    out.lines()
        .filter(|line| jq_test(&re, line))
        .map(str::to_string)
        .collect()
}

fn the_three() -> Vec<String> {
    let mut v = vec![
        format!("{VOL}-r-03f4dac5 on cp-3"),
        format!("{VOL}-r-1269eb6e on cp-2"),
        format!("{VOL}-r-7c1934ba on cp-1"),
    ];
    v.sort();
    v
}

// ------------------------------------------------------------------- the lib

/// Runs `program` over one disk object with the ledger lib's definitions.
fn ledger(disk: Value, program: &str) -> Value {
    let o = Command::new("bash")
        .args([
            "-c",
            r#". "$1"; jq -cn --argjson d "$2" "$LONGHORN_LEDGER_JQ $3""#,
            "ledger",
        ])
        .arg(repo_root().join(LIB))
        .arg(disk.to_string())
        .arg(program)
        .output()
        .expect("bash runs");
    assert!(o.status.success(), "{}", text(&o));
    serde_json::from_slice(&o.stdout).expect("json")
}

/// The lib's growth check is Longhorn v1.11.3's, not a stricter guess:
/// CheckReplicasSizeExpansion calls IsSchedulableToDisk(required, 0, …)
/// ("requiredStorage = 0 is intentional", replica_scheduler.go:1460), so
/// the floor check on storageAvailable takes no growth, and the physical
/// bound is ValidateDiskAvailableForExpansion's, which credits
/// storageReserved. The disk below is the one review run 091904d3 fed the
/// lib (finding B1): Longhorn admits 30 GiB more there, and the lib used to
/// refuse it.
#[test]
fn the_ledger_lib_is_longhorns_growth_arithmetic() {
    needs_tools();
    let b1 = json!({"max": 117_656_518_656_u64, "reserved": 35_296_955_596_u64,
                    "scheduled": 32_212_254_720_u64, "available": 50_000_000_000_u64});
    assert_eq!(
        ledger(b1.clone(), "$d | lh_physical(32212254720; 100; 25)"),
        json!([]),
        "Longhorn admits this growth"
    );
    // ProvisionedLimit 82359563060 - physicalUsed 32359563060 = 50000000000;
    // 50000000000 + 35296955596 - floor 29414129664 = 55882825932.
    assert_eq!(
        ledger(b1.clone(), "$d | lh_physical_room(100; 25)"),
        json!(50_000_000_000_u64)
    );
    // At the floor, IsSchedulableToDisk refuses any growth at all.
    let mut at_floor = b1.clone();
    at_floor["available"] = json!(29_414_129_664_u64);
    let why = ledger(at_floor.clone(), "$d | lh_physical(1; 100; 25)");
    assert!(
        why.to_string().contains("IsSchedulableToDisk")
            && why.as_array().is_some_and(|a| a.len() == 1),
        "{why}"
    );
    assert_eq!(ledger(at_floor, "$d | lh_physical_room(100; 25)"), json!(0));
    // Past ValidateDiskAvailableForExpansion's floor: nothing reserved, so
    // the growth itself takes the disk under it.
    let tight = json!({"max": 200 * GIB, "reserved": 0, "scheduled": 100 * GIB,
                       "available": 50 * GIB + GIB / 2});
    let why = ledger(tight.clone(), &format!("$d | lh_physical({GIB}; 200; 25)"));
    assert!(
        why.to_string()
            .contains("ValidateDiskAvailableForExpansion")
            && !why.to_string().contains("IsSchedulableToDisk"),
        "{why}"
    );
    assert_eq!(
        ledger(tight.clone(), "$d | lh_physical_room(200; 25)"),
        json!(GIB / 2)
    );
    // ValidateDiskAvailableForExpansion returns nil for required <= 0
    // (replica_scheduler.go:1226): at 50% over-provisioning this disk's
    // physicalUsed is already over its limit, and a zero growth is still
    // admitted, judged by IsSchedulableToDisk's live half alone.
    assert_eq!(
        ledger(tight.clone(), "$d | lh_physical(0; 50; 25)"),
        json!([])
    );
    // Go reports the FIRST failure, Validate before IsSchedulableToDisk:
    // under its floor both ways, the answer names Validate only.
    let mut both = tight;
    both["available"] = json!(49 * GIB);
    let why = ledger(both, &format!("$d | lh_physical({GIB}; 200; 25)"));
    assert!(
        why.as_array().is_some_and(|a| a.len() == 1)
            && why
                .to_string()
                .contains("ValidateDiskAvailableForExpansion"),
        "{why}"
    );
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_replica_the_count_the_floor_and_what_remains() {
    let l = Longhorn::new("rvr-plan");
    let (plan, hash) = l.plan();
    for want in [
        "plan: retire-volume-replica".to_string(),
        format!("volume: {VOL}"),
        "claim: boss/pgdata-postgres-0".to_string(),
        "size: 32212254720 bytes (30 GiB)".to_string(),
        "state: attached on cp-1".to_string(),
        "robustness: healthy".to_string(),
        "data locality: disabled".to_string(),
        "replica auto-balance: disabled (the volume says ignored, the setting \
         replica-auto-balance says disabled)"
            .to_string(),
        "eviction: none requested on any replica of the volume, its node or its disk".to_string(),
        format!("retire: replica {REP} on w-2 (disk {W2_DISK}): healthy"),
        "numberOfReplicas: 4 -> 3 — the floor is 3, and never below 2".to_string(),
        "remain: 3 healthy replicas on 3 distinct nodes (cp-1, cp-2, cp-3)".to_string(),
        // The retired replica's disk, by the webhook's own arithmetic: 755 MB.
        format!(
            "disk {W2_DISK} (default-disk-fd0100000000 on w-2): the retired replica's disk — \
             ProvisionedLimit (117656518656 - 35296955596) x 100% = 82359563060; \
             storageScheduled 81604378624; room 755184436 bytes"
        ),
        format!(
            "disk {CP3_DISK} (default-disk-cp3 on cp-3): ProvisionedLimit (536870912000 - \
             161061273600) x 100% = 375809638400; storageScheduled 107374182400; room \
             268435456000 bytes — admits growth of 268435456000 bytes"
        ),
        "after: every remaining replica's disk admits the volume's growth by at least \
         1073741824 bytes (1 GiB); their ledgers admit growth to 300647710720 bytes (280 GiB)"
            .to_string(),
        format!(
            "kubectl patch volumes.longhorn.io {VOL} -n longhorn-system --type=json — test \
             that spec.numberOfReplicas is still 4, replace it with 3"
        ),
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
    let mut busy = cp_disk("default-disk-cp3", CP3_DISK);
    busy.available = 300 * GIB;
    l.nodes(estate(Some(busy)));
    assert_eq!(
        l.plan().1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
}

#[test]
fn the_plan_refuses_what_the_argv_bound_does_not_admit_and_opens_no_door() {
    let l = Longhorn::new("rvr-argv");
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
fn a_degraded_or_detached_volume_retires_nothing() {
    let l = Longhorn::new("rvr-unhealthy");
    let plan = ["--plan", VOL, REP];

    // Degraded with every replica healthy: the volume says a copy is
    // short, and retiring a healthy one on top is how it reaches one.
    let mut v = volume(4);
    v["status"]["robustness"] = json!("degraded");
    l.volume(v);
    assert!(l.refused(&plan, &[]).contains("robustness degraded"));
    let mut v = volume(4);
    v["status"]["state"] = json!("detached");
    l.volume(v);
    assert!(l.refused(&plan, &[]).contains("not attached"));
    l.volume(volume(4));

    // Another replica still rebuilding, or failed: retiring a healthy one
    // then leaves fewer healthy than the count says.
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

    // Too few copies, or more than one extra, cannot resume.
    l.replicas(Longhorn::four());
    l.volume(volume(5));
    assert!(
        l.refused(&plan, &[])
            .contains("spec.numberOfReplicas 5 and has 4")
    );
    l.volume(volume(2));
    assert!(
        l.refused(&plan, &[])
            .contains("spec.numberOfReplicas 2 and has 4")
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

/// Longhorn counts a replica healthy only when it is also ACTIVE
/// (isHealthyAndActiveReplica, volume_controller.go:943-958): one left
/// inactive by a migration or an engine upgrade carries healthyAt and is
/// not a copy the volume serves from. Counted, the retirement would leave
/// fewer real copies than the plan read.
#[test]
fn a_healthy_but_inactive_replica_is_not_a_healthy_copy() {
    let l = Longhorn::new("rvr-inactive");
    let mut r = Longhorn::four();
    r[2]["spec"]["active"] = json!(false);
    l.replicas(r);
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(
        out.contains("3 healthy") && out.contains("not active"),
        "{out}"
    );
    // A replica with no `active` at all is not active either.
    let mut r = Longhorn::four();
    r[2]["spec"]
        .as_object_mut()
        .expect("an object")
        .remove("active");
    l.replicas(r);
    assert!(l.refused(&["--plan", VOL, REP], &[]).contains("3 healthy"));
}

#[test]
fn a_count_at_the_floor_is_refused_and_the_floor_is_never_below_two() {
    let l = Longhorn::new("rvr-floor");
    // Three replicas, the w-2 one among them: 3 -> 2 is under the floor of 3.
    let three = || {
        json!([
            Longhorn::cp("cp-1"),
            Longhorn::cp("cp-2"),
            replica(VOL, REP, "w-2", W2_DISK, true, false),
        ])
    };
    l.volume(volume(3));
    l.replicas(three());
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(
        out.contains("3 -> 2 is under the floor of 3"),
        "names the count and the floor: {out}"
    );

    // A declared floor of 2 admits it, and says so in the signed bytes.
    let (plan, _) = l.plan_of(REP, &[("BOSS_RETIRE_FLOOR", "2")]);
    assert!(
        plan.contains("numberOfReplicas: 3 -> 2 — the floor is 2, and never below 2"),
        "{plan}"
    );

    // Two replicas: 2 -> 1 is under any floor this verb takes.
    l.volume(volume(2));
    l.replicas(json!([
        Longhorn::cp("cp-1"),
        replica(VOL, REP, "w-2", W2_DISK, true, false),
    ]));
    let out = l.refused(&["--plan", VOL, REP], &[("BOSS_RETIRE_FLOOR", "2")]);
    assert!(out.contains("2 -> 1 is under the floor of 2"), "{out}");

    // A floor under 2, or not a number, is refused before any door opens.
    let l = Longhorn::new("rvr-floor-bad");
    for bad in ["1", "0", "two", ""] {
        let o = l.run(&["--plan", VOL, REP], &[("BOSS_RETIRE_FLOOR", bad)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "floor {bad:?}: {out}");
        assert!(out.contains("never below 2"), "floor {bad:?}: {out}");
    }
    assert!(l.calls().is_empty(), "no door opened: {:?}", l.calls());
}

#[test]
fn a_replica_that_is_not_the_volumes_is_refused() {
    let l = Longhorn::new("rvr-not-mine");
    let out = l.refused(&["--plan", VOL, &format!("{VOL}-r-00000000")], &[]);
    assert!(out.contains("is not a replica of"), "{out}");
    assert!(out.contains(REP), "names the replicas it has: {out}");
}

/// Longhorn v1.11.3 removes a replica by itself on a lowered count only
/// through cleanupEvictionRequestedReplicas, cleanupDataLocalityReplicas
/// (best-effort) or cleanupAutoBalancedReplicas — and then picks one this
/// plan cannot name (review run 091904d3, finding B2). Each is refused.
#[test]
fn a_volume_longhorn_may_trim_by_itself_is_refused() {
    let l = Longhorn::new("rvr-trims");
    let plan = ["--plan", VOL, REP];

    for (loc, aim) in [
        ("best-effort", "cleanupDataLocalityReplicas"),
        ("strict-local", "data locality strict-local"),
    ] {
        let mut v = volume(4);
        v["spec"]["dataLocality"] = json!(loc);
        l.volume(v);
        let out = l.refused(&plan, &[]);
        assert!(
            out.contains(&format!("data locality {loc}")) && out.contains(aim),
            "{loc}: {out}"
        );
    }

    // Auto-balance: the volume's own word, else the setting's.
    let mut v = volume(4);
    v["spec"]["replicaAutoBalance"] = json!("best-effort");
    l.volume(v);
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains("replica auto-balance best-effort")
            && out.contains("cleanupAutoBalancedReplicas"),
        "{out}"
    );
    l.volume(volume(4));
    l.setting("replica-auto-balance", "least-effort");
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains("replica auto-balance least-effort (the volume says ignored, the setting replica-auto-balance says least-effort)"),
        "{out}"
    );
    let mut v = volume(4);
    v["spec"]["replicaAutoBalance"] = json!("disabled");
    l.volume(v);
    let (p, _) = l.plan();
    assert!(
        p.contains("replica auto-balance: disabled (the volume says disabled, the setting replica-auto-balance says least-effort)"),
        "the volume's word wins: {p}"
    );
    l.volume(volume(4));
    l.setting("replica-auto-balance", "disabled");

    // An eviction requested on a replica, on its node, or on its disk.
    let mut r = Longhorn::four();
    r[1]["spec"]["evictionRequested"] = json!(true);
    l.replicas(r);
    let out = l.refused(&plan, &[]);
    assert!(
        out.contains(&format!("replica {VOL}-r-1269eb6e"))
            && out.contains("cleanupEvictionRequestedReplicas"),
        "{out}"
    );
    l.replicas(Longhorn::four());
    let mut n = estate(None);
    n[1]["spec"]["evictionRequested"] = json!(true);
    l.nodes(n);
    assert!(l.refused(&plan, &[]).contains("node cp-2"));
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.evict = true;
    l.nodes(estate(Some(cp3)));
    assert!(
        l.refused(&plan, &[])
            .contains(&format!("disk {CP3_DISK} on cp-3"))
    );
    // Another volume's node, evicting, is not this volume's concern.
    let mut n = estate(None);
    n[3]["spec"]["evictionRequested"] = json!(true);
    l.nodes(n);
    l.plan();

    // An unreadable auto-balance setting is no answer at all.
    std::fs::remove_file(l.dir.join("state/setting-replica-auto-balance.json")).expect("rm");
    let o = l.run(&plan, &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
}

#[test]
fn a_remaining_disk_without_ledger_room_is_refused() {
    let l = Longhorn::new("rvr-ledger");
    // cp-3's ledger has half a GiB left: the volume could not grow there,
    // so retiring w-2's copy would cost a replica and free nothing.
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.scheduled = (500 - 150) * GIB - GIB / 2;
    l.nodes(estate(Some(cp3)));
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(
        out.contains(CP3_DISK)
            && out.contains("room 536870912 bytes")
            && out.contains("short of 1073741824"),
        "{out}"
    );
}

/// The live half of the growth check, as Longhorn v1.11.3 runs it: a
/// remaining disk at its floor (IsSchedulableToDisk, requiredStorage 0) or
/// one the growth would take under it (ValidateDiskAvailableForExpansion)
/// is refused, and one Longhorn admits is admitted — including the shape
/// the old lib refused.
#[test]
fn a_remaining_disk_without_live_free_space_is_refused() {
    let l = Longhorn::new("rvr-physical");
    // 124 GiB free against a floor of 125 (500 x 25%).
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.available = 124 * GIB;
    l.nodes(estate(Some(cp3.clone())));
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(
        out.contains(CP3_DISK)
            && out.contains("physical space as well as on the ledger")
            && out.contains("is not above the floor 134217728000")
            && out.contains("IsSchedulableToDisk"),
        "{out}"
    );

    // Over-provisioning 200 and nothing reserved: the floor check passes,
    // and only ValidateDiskAvailableForExpansion sees the GiB would take
    // the disk under it.
    l.setting("storage-over-provisioning-percentage", "200");
    let mut small = disk("default-disk-cp3", CP3_DISK, 200 * GIB, 0, 100 * GIB);
    small.available = 50 * GIB + GIB / 2;
    l.nodes(estate(Some(small)));
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(
        out.contains("ValidateDiskAvailableForExpansion")
            && out.contains(&format!("is under the floor {}", 50 * GIB))
            && !out.contains("IsSchedulableToDisk"),
        "{out}"
    );
    l.setting("storage-over-provisioning-percentage", "100");

    // Half a GiB above the floor with 150 GiB reserved: Longhorn admits
    // the growth (finding B1's shape), and so does this plan.
    let mut near = cp_disk("default-disk-cp3", CP3_DISK);
    near.available = 125 * GIB + GIB / 2;
    l.nodes(estate(Some(near)));
    l.plan();

    // Signed while the disk had room, refused when the write re-renders
    // and it no longer has: the live half is judged at both.
    l.nodes(estate(None));
    let (_, hash) = l.plan();
    l.nodes(estate(Some(cp3)));
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("physical space as well as on the ledger"),
        "{out}"
    );
    assert!(l.writes().is_empty(), "{:?}", l.writes());
}

#[test]
fn a_missing_volume_is_refused_and_a_read_that_cannot_look_cannot_answer() {
    let l = Longhorn::new("rvr-dark");
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
fn a_remaining_replica_disk_no_node_reports_cannot_answer() {
    let l = Longhorn::new("rvr-unknown-disk");
    let mut r = Longhorn::four();
    r[2] = replica(
        VOL,
        &format!("{VOL}-r-03f4dac5"),
        "cp-3",
        "default-disk-cp3",
        true,
        false,
    );
    l.replicas(r);
    let o = l.run(&["--plan", VOL, REP], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("disk default-disk-cp3 on cp-3"),
        "{out}"
    );
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_lowers_then_deletes_the_named_replica_and_reads_the_effect_back() {
    let l = Longhorn::new("rvr-write");
    let (plan, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );

    // In order: lower 4 -> 3, and only then the delete, BY NAME. A delete
    // under a count of 4 is replenished onto the node it left once its
    // finalizer clears (the stub does what Longhorn does), and the
    // retirement would undo itself.
    let w = l.writes();
    assert_eq!(w.len(), 2, "one compare-and-set and one delete: {w:?}");
    assert!(
        w[0].starts_with(&format!(
            "patch volumes.longhorn.io {VOL} -n longhorn-system --type=json"
        )) && w[0].contains(r#"{"op":"test","path":"/spec/numberOfReplicas","value":4}"#)
            && w[0].contains(r#"{"op":"replace","path":"/spec/numberOfReplicas","value":3}"#),
        "{w:?}"
    );
    assert!(
        w[1].starts_with(&format!(
            "delete replicas.longhorn.io {REP} -n longhorn-system"
        )),
        "{w:?}"
    );

    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 3);
    assert_eq!(l.replicas_now(), the_three(), "no replica replenished");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("(cp-1, cp-2, cp-3)") && hits[0].contains(REP),
        "names the nodes and the replica: {hits:?}"
    );
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let l = Longhorn::new("rvr-drift");
    let (_, hash) = l.plan();
    // The ledger moved after the plan was signed: another volume
    // scheduled on cp-3.
    let mut cp3 = cp_disk("default-disk-cp3", CP3_DISK);
    cp3.scheduled += 5 * GIB;
    l.nodes(estate(Some(cp3)));
    // Refused with today's plan printed beside it — and nothing written.
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
    assert_eq!(
        l.writes().len(),
        2,
        "no second retirement: {:?}",
        l.writes()
    );
}

#[test]
fn a_replica_removed_by_anything_else_on_the_lowered_count_stops_the_delete() {
    let cp3 = format!("{VOL}-r-03f4dac5");
    // Removed at once, or still listed with its deletionTimestamp set.
    for (name, mark) in [("rvr-trim-other", ""), ("rvr-trim-other-mark", "1")] {
        let l = Longhorn::new(name);
        let (_, hash) = l.plan();
        let o = l.run(
            &[VOL, REP, &hash],
            &[("STUB_TRIM", &cp3), ("STUB_TRIM_MARK", mark)],
        );
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{name}: {out}");
        assert!(
            out.contains(&format!("removed (or is removing) {cp3}"))
                && out.contains(&format!("{REP} is NOT deleted")),
            "{name}: names what went, and that the named replica stands: {out}"
        );
        let w = l.writes();
        assert_eq!(w.len(), 1, "{name}: the lower, never the delete: {w:?}");
        assert!(
            l.replicas_now().iter().any(|r| r.starts_with(REP)),
            "{name}: the named replica stands"
        );
        assert!(effect_lines(&out).is_empty(), "{name}: {out}");
    }
}

#[test]
fn the_named_replica_removed_by_anything_else_is_not_deleted_again() {
    // Gone at once, or terminating for one read and then gone: nothing to
    // delete either way, and the read-back waits for it to vanish.
    for (name, mark) in [("rvr-trim-mine", ""), ("rvr-trim-mine-mark", "1")] {
        let l = Longhorn::new(name);
        let (_, hash) = l.plan();
        let o = l.run(
            &[VOL, REP, &hash],
            &[("STUB_TRIM", REP), ("STUB_TRIM_MARK", mark)],
        );
        let out = text(&o);
        assert!(o.status.success(), "{name}: {out}");
        assert!(out.contains("nothing to delete"), "{name}: {out}");
        assert_eq!(l.writes().len(), 1, "{name}: {:?}", l.writes());
        assert_eq!(l.replicas_now(), the_three(), "{name}");
        assert_eq!(effect_lines(&out).len(), 1, "{name}: {out}");
    }
}

#[test]
fn a_replica_whose_finalizer_never_clears_is_not_the_effect() {
    let l = Longhorn::new("rvr-stuck");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_FINALIZER_STUCK", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("NOT proven") && out.contains(&format!("{REP} still present")),
        "{out}"
    );
    assert_eq!(
        l.writes().len(),
        2,
        "one delete, never a second: {:?}",
        l.writes()
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_replica_that_fails_while_the_count_settles_stops_the_delete() {
    let l = Longhorn::new("rvr-fail-settle");
    let (_, hash) = l.plan();
    let cp2 = format!("{VOL}-r-1269eb6e");
    let o = l.run(&[VOL, REP, &hash], &[("STUB_FAIL", &cp2)]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("{REP} is NOT deleted")) && out.contains(&cp2),
        "{out}"
    );
    assert_eq!(l.writes().len(), 1, "{:?}", l.writes());
    assert!(l.replicas_now().iter().any(|r| r.starts_with(REP)));
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_read_back_that_does_not_settle_is_not_the_effect() {
    let l = Longhorn::new("rvr-readback");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_DEGRADE_ON_DELETE", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("NOT proven") && out.contains("robustness degraded"),
        "{out}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_remaining_disk_that_lost_its_room_by_the_read_back_is_not_the_effect() {
    let l = Longhorn::new("rvr-squeezed");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_SQUEEZE_ON_DELETE", CP3_DISK)]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("{REP} is gone"))
            && out.contains("NOT every remaining replica's disk")
            && out.contains(CP3_DISK),
        "{out}"
    );
    assert_eq!(l.replicas_now(), the_three(), "the retirement stands");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_patch_the_server_refuses_is_a_failure_with_its_words() {
    let l = Longhorn::new("rvr-patch-refused");
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

// --------------------------------------------- the door back down after a move

/// A move that raised the count to 5 and stalled leaves a fifth replica
/// rebuilding, or failed, and the volume degraded. Retiring THAT replica
/// starts with no healthy copy on that replica, so a degraded volume
/// admits — and the remaining disks' room is not its bound, because the
/// full w-2 disk the move was leaving is among them.
#[test]
fn a_stalled_move_backs_down_by_retiring_its_unhealthy_replica() {
    for (name, failed) in [("rvr-back-rebuilding", false), ("rvr-back-failed", true)] {
        let l = Longhorn::new(name);
        l.raised(false, failed);
        let (plan, hash) = l.plan_of(NEW, &[]);
        for want in [
            "robustness: degraded".to_string(),
            format!(
                "retire: replica {NEW} on w-1 (disk {W1_DISK}): {} — the one replica not healthy now; \
                 it may rebuild healthy before the delete, which then costs that healthy copy",
                if failed {
                    "failed"
                } else {
                    "not yet healthy (rebuilding, or never finished)"
                }
            ),
            "numberOfReplicas: 5 -> 4 — the floor is 3, and never below 2".to_string(),
            "remain: 4 healthy replicas on 4 distinct nodes (cp-1, cp-2, cp-3, w-2)".to_string(),
            "after: the retired replica is not healthy, so the room on the remaining disks is \
             not a bound"
                .to_string(),
        ] {
            assert!(
                plan.contains(&want),
                "{name}: the plan says {want:?}:\n{plan}"
            );
        }
        let o = l.run(&[VOL, NEW, &hash], &[]);
        let out = text(&o);
        assert!(o.status.success(), "{name}: {out}");
        let w = l.writes();
        assert_eq!(w.len(), 2, "{name}: {w:?}");
        assert!(
            w[0].contains(r#""value":5}"#)
                && w[0].contains(r#""replace","path":"/spec/numberOfReplicas","value":4"#),
            "{name}: {w:?}"
        );
        assert!(
            w[1].starts_with(&format!("delete replicas.longhorn.io {NEW} ")),
            "{name}: {w:?}"
        );
        assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
        assert!(
            !l.replicas_now().iter().any(|r| r.starts_with(NEW)),
            "{name}"
        );
        let hits = effect_lines(&out);
        assert_eq!(hits.len(), 1, "{name}: {out}");
        assert!(
            hits[0].contains("(cp-1, cp-2, cp-3, w-2)"),
            "{name}: {hits:?}"
        );
    }
}

#[test]
fn a_degraded_volume_retires_only_its_one_unhealthy_replica() {
    let l = Longhorn::new("rvr-back-wrong");
    l.raised(false, false);
    // The healthy w-2 copy while the fifth still rebuilds: that costs a
    // copy the volume already lacks.
    let out = l.refused(&["--plan", VOL, REP], &[]);
    assert!(out.contains("4 healthy") && out.contains(NEW), "{out}");

    // Two replicas not healthy: retiring one still leaves the volume short.
    let mut r = Longhorn::four();
    r[2] = replica(
        VOL,
        &format!("{VOL}-r-03f4dac5"),
        "cp-3",
        CP3_DISK,
        true,
        true,
    );
    r.as_array_mut()
        .expect("an array")
        .push(replica(VOL, NEW, "w-1", W1_DISK, false, false));
    l.replicas(r);
    let out = l.refused(&["--plan", VOL, NEW], &[]);
    assert!(out.contains("3 healthy"), "{out}");

    // Faulted is not degraded.
    l.raised(false, false);
    let mut v = volume(5);
    v["status"]["robustness"] = json!("faulted");
    l.volume(v);
    assert!(
        l.refused(&["--plan", VOL, NEW], &[])
            .contains("robustness faulted")
    );
}

/// A move that raised the count and placed the new replica healthy but
/// not where its plan said leaves 5 healthy: an ordinary retirement.
#[test]
fn a_raised_count_with_every_replica_healthy_retires_the_full_disks_copy() {
    let l = Longhorn::new("rvr-back-healthy");
    l.raised(true, false);
    let (plan, hash) = l.plan();
    assert!(
        plan.contains("remain: 4 healthy replicas on 4 distinct nodes (cp-1, cp-2, cp-3, w-1)"),
        "{plan}"
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "{out}");
    assert!(hits[0].contains("(cp-1, cp-2, cp-3, w-1)"), "{hits:?}");
}

// ------------------------------------------------------------------ verbs

/// Stop through the real write path, then use current state and a NEW approval.
#[test]
fn a_stopped_retirement_resumes_only_with_a_fresh_plan_and_test_only_cas() {
    let l = Longhorn::new("rvr-resume-stopped");
    let (_, old_hash) = l.plan();
    let stopped = l.run(&[VOL, REP, &old_hash], &[("STUB_DELETE_FAIL", "1")]);
    assert_eq!(stopped.status.code(), Some(1), "{}", text(&stopped));
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 3);
    assert_eq!(l.replicas_now().len(), 4);
    assert!(effect_lines(&text(&stopped)).is_empty());
    let writes_before = l.writes().len();
    let old = l.run(&[VOL, REP, &old_hash], &[]);
    assert_eq!(old.status.code(), Some(78), "{}", text(&old));
    assert_eq!(l.writes().len(), writes_before);
    let (plan, hash) = l.plan();
    assert!(
        plan.contains("resume: count already lowered to 3"),
        "{plan}"
    );
    assert!(plan.contains("numberOfReplicas: 3 -> 3"), "{plan}");
    assert!(plan.contains("replicas before: 4"), "{plan}");
    assert!(plan.contains("test-only"), "{plan}");
    assert_ne!(hash, old_hash);
    let resumed = l.run(&[VOL, REP, &hash], &[]);
    assert!(resumed.status.success(), "{}", text(&resumed));
    assert!(
        !text(&resumed).contains("numberOfReplicas 4 -> 3"),
        "the resume did not lower again: {}",
        text(&resumed)
    );
    let writes = l.writes();
    assert_eq!(writes.len(), writes_before + 2, "{writes:?}");
    assert!(
        writes[writes_before].contains(r#""op":"test","path":"/spec/numberOfReplicas","value":3"#)
    );
    assert!(!writes[writes_before].contains("replace"));
    assert!(writes[writes_before + 1].starts_with(&format!("delete replicas.longhorn.io {REP} ")));
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 3);
    assert_eq!(l.replicas_now().len(), 3);
    assert!(!l.replicas_now().iter().any(|r| r == REP));
    assert_eq!(effect_lines(&text(&resumed)).len(), 1);
    let spent = l.run(&[VOL, REP, &hash], &[]);
    assert_eq!(spent.status.code(), Some(78));
    assert_eq!(l.writes(), writes);
}

fn after_lower(l: &Longhorn, hook: &str) {
    let marker = "    echo \"volume.longhorn.io/$v patched\"; exit 0 ;;";
    assert!(STUB.contains(marker));
    let replacement = format!(
        "    if [ -n \"$new\" ] && [ \"$new\" -lt \"$cur\" ]; then\n{hook}\n    fi\n{marker}"
    );
    write_exec(&l.dir.join("bin/sudo"), &STUB.replace(marker, &replacement));
}

/// Return the supplied resource response only AFTER the approved test-only CAS.
/// Initial reads still describe the state the new resume plan was signed for.
fn resume_snapshot_response(l: &Longhorn, resource: &str, body: &str, hook: &str) {
    l.state("snapshot-response", body);
    let read_marker = "if [ -n \"${STUB_FORBIDDEN:-}\" ]; then";
    assert!(STUB.contains(read_marker));
    let intercept = format!(
        "if [ \"$1\" = get ] && [ \"$2\" = \"{resource}\" ] && [ -f \"$S/snapshot-ready\" ]; then\n    cat \"$S/snapshot-response\"; exit 0\nfi\n{read_marker}"
    );
    let patch_marker = "    echo \"volume.longhorn.io/$v patched\"; exit 0 ;;";
    assert!(STUB.contains(patch_marker));
    let after_cas = format!(
        "    if [ -z \"$new\" ]; then\n{hook}\n        : > \"$S/snapshot-ready\"\n    fi\n{patch_marker}"
    );
    write_exec(
        &l.dir.join("bin/sudo"),
        &STUB
            .replace(read_marker, &intercept)
            .replace(patch_marker, &after_cas),
    );
}

fn assert_snapshot_refuses_before_delete(l: &Longhorn, hash: &str, label: &str) {
    let o = l.run(&[VOL, REP, hash], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{label}: {out}");
    let writes = l.writes();
    assert_eq!(writes.len(), 1, "{label}: {writes:?}\n{out}");
    assert!(writes[0].starts_with("patch volumes.longhorn.io "));
    assert!(writes[0].contains(r#""op":"test""#));
    assert!(!writes[0].contains("replace"), "{label}: {writes:?}");
    assert!(effect_lines(&out).is_empty(), "{label}: {out}");
    assert_eq!(l.replicas_now().len(), 4, "{label}: replica retained");
}

#[test]
fn contradictory_volume_snapshot_cannot_delete_before_readback_refusal() {
    let l = Longhorn::new("rvr-snapshot-contradictory");
    l.volume(volume(3));
    let (_, hash) = l.plan();
    let stale = volume(3);
    let mut current = volume(4);
    current["spec"]["dataLocality"] = json!("best-effort");
    resume_snapshot_response(
        &l,
        "volumes.longhorn.io",
        &format!("{stale}\n{current}\n"),
        "        jq '.spec.numberOfReplicas = 4 | .spec.dataLocality = \"best-effort\"' \"$f\" > \"$f.new\" && mv \"$f.new\" \"$f\"",
    );
    assert_snapshot_refuses_before_delete(&l, &hash, "stale-safe first/current-unsafe second");
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    assert_eq!(l.volume_now()["spec"]["dataLocality"], "best-effort");
}

#[test]
fn a_single_wrong_volume_snapshot_cannot_authorize_a_replica_delete() {
    for kind in ["different", "missing", "null", "numeric"] {
        let l = Longhorn::new(&format!("rvr-snapshot-volume-identity-{kind}"));
        l.volume(volume(3));
        let (_, hash) = l.plan();
        let mut stale = volume(3);
        match kind {
            "different" => {
                stale["metadata"]["name"] = json!("pvc-00000000-0000-0000-0000-000000000000");
            }
            "missing" => {
                stale["metadata"].as_object_mut().unwrap().remove("name");
            }
            "null" => stale["metadata"]["name"] = json!(null),
            _ => stale["metadata"]["name"] = json!(7),
        }
        resume_snapshot_response(
            &l,
            "volumes.longhorn.io",
            &format!("{stale}\n"),
            "        jq '.spec.numberOfReplicas = 4 | .spec.dataLocality = \"best-effort\"' \"$f\" > \"$f.new\" && mv \"$f.new\" \"$f\"",
        );
        assert_snapshot_refuses_before_delete(&l, &hash, kind);
        assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
        assert_eq!(l.volume_now()["spec"]["dataLocality"], "best-effort");
    }
}

#[test]
fn a_single_matching_volume_snapshot_still_finishes_a_fresh_resume() {
    let l = Longhorn::new("rvr-snapshot-volume-identity-valid");
    l.volume(volume(3));
    let (_, hash) = l.plan();
    resume_snapshot_response(
        &l,
        "volumes.longhorn.io",
        &volume(3).to_string(),
        "        :",
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(effect_lines(&text(&o)).len(), 1);
    let writes = l.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes[0].contains(r#""op":"test""#));
    assert!(!writes[0].contains("replace"));
    assert!(writes[1].starts_with(&format!("delete replicas.longhorn.io {REP} ")));
}

#[test]
fn a_wrong_volume_readback_cannot_certify_a_completed_retirement() {
    let l = Longhorn::new("rvr-readback-volume-identity");
    l.volume(volume(3));
    let (_, hash) = l.plan();
    let mut wrong = volume(3);
    wrong["metadata"]["name"] = json!("pvc-00000000-0000-0000-0000-000000000000");
    l.state("snapshot-response", &wrong.to_string());
    let read_marker = "if [ -n \"${STUB_FORBIDDEN:-}\" ]; then";
    let intercept = format!(
        "if [ \"$1\" = get ] && [ \"$2\" = volumes.longhorn.io ] && [ -f \"$S/identity-after-delete\" ]; then\n    cat \"$S/snapshot-response\"; exit 0\nfi\n{read_marker}"
    );
    let delete_marker = "    echo \"replica.longhorn.io \\\"$r\\\" deleted\"; exit 0 ;;";
    assert!(STUB.contains(read_marker) && STUB.contains(delete_marker));
    write_exec(
        &l.dir.join("bin/sudo"),
        &STUB.replace(read_marker, &intercept).replace(
            delete_marker,
            &format!("    : > \"$S/identity-after-delete\"\n{delete_marker}"),
        ),
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(effect_lines(&text(&o)).is_empty(), "{}", text(&o));
    let writes = l.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes[1].starts_with(&format!("delete replicas.longhorn.io {REP} ")));
}

#[test]
fn ambiguous_or_malformed_resource_snapshots_refuse_before_delete() {
    for resource in [
        "volumes.longhorn.io",
        "replicas.longhorn.io",
        "nodes.longhorn.io",
        "settings.longhorn.io",
    ] {
        for kind in [
            "duplicate",
            "empty",
            "whitespace",
            "null",
            "array",
            "wrong-shape",
            "malformed",
        ] {
            let l = Longhorn::new(&format!("rvr-snapshot-{resource}-{kind}"));
            l.volume(volume(3));
            let (_, hash) = l.plan();
            let correct = match resource {
                "volumes.longhorn.io" => volume(3),
                "replicas.longhorn.io" => json!({"items": Longhorn::four()}),
                "nodes.longhorn.io" => json!({"items": estate(None)}),
                _ => json!({"value": "disabled"}),
            };
            let body = match kind {
                "duplicate" => format!("{correct}\n{correct}\n"),
                "empty" => String::new(),
                "whitespace" => " \n\t".to_string(),
                "null" => "null\n".to_string(),
                "array" => "[]\n".to_string(),
                "wrong-shape" => "{}\n".to_string(),
                _ => "{\"broken\":\n".to_string(),
            };
            resume_snapshot_response(&l, resource, &body, "        :");
            assert_snapshot_refuses_before_delete(&l, &hash, &format!("{resource}/{kind}"));
        }
    }
}

#[test]
fn the_delete_refuses_the_named_replica_moving_from_its_signed_node_or_disk() {
    for (name, hook) in [
        (
            "named-node",
            "        reps 'map(if .metadata.name == \"'\"$v-r-ae04deab\"'\" then .spec.nodeID = \"w-1\" else . end)'",
        ),
        (
            "named-disk",
            "        reps 'map(if .metadata.name == \"'\"$v-r-ae04deab\"'\" then .spec.diskID = \"844915d9-6a5b-4da9-9619-8f3d188d632b\" else . end)'",
        ),
    ] {
        let l = Longhorn::new(&format!("rvr-fresh-{name}"));
        let (plan, hash) = l.plan();
        assert!(plan.contains(REP));
        assert!(plan.contains("w-2"));
        after_lower(
            &l,
            &format!(
                "{hook}\n        jq --arg r \"$v-r-ae04deab\" '.[] | select(.metadata.name == $r)' \"$S/replicas.json\" > \"$S/named-after-lower.json\""
            ),
        );
        let o = l.run(&[VOL, REP, &hash], &[]);
        let moved = l.read_state("named-after-lower.json");
        assert_eq!(moved["metadata"]["name"], REP);
        if name == "named-node" {
            assert_eq!(moved["spec"]["nodeID"], "w-1");
        } else {
            assert_eq!(moved["spec"]["diskID"], CP1_DISK);
        }
        assert_eq!(o.status.code(), Some(1), "{name}: {}", text(&o));
        assert!(
            l.writes().iter().all(|w| !w.starts_with("delete ")),
            "{name}: {:?}",
            l.writes()
        );
        assert!(effect_lines(&text(&o)).is_empty(), "{name}: {}", text(&o));
    }
}

#[test]
fn the_delete_rechecks_trim_settings_robustness_and_count_after_lowering() {
    for (name, hook) in [
        (
            "autobalance",
            "        printf '%s' '{\"value\":\"best-effort\"}' > \"$S/setting-replica-auto-balance.json\"",
        ),
        (
            "locality",
            "        jq '.spec.dataLocality = \"best-effort\"' \"$f\" > \"$f.new\" && mv \"$f.new\" \"$f\"",
        ),
        (
            "robustness",
            "        jq '.status.robustness = \"faulted\"' \"$f\" > \"$f.new\" && mv \"$f.new\" \"$f\"",
        ),
        (
            "count",
            "        jq '.spec.numberOfReplicas += 1' \"$f\" > \"$f.new\" && mv \"$f.new\" \"$f\"",
        ),
        (
            "retained-placement",
            "        reps 'map(if .spec.nodeID == \"cp-3\" then .spec.nodeID = \"w-1\" | .spec.diskID = \"5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10\" else . end)'",
        ),
        (
            "remaining-distinct",
            "        reps 'map(if .spec.nodeID == \"cp-3\" then .spec.nodeID = \"cp-2\" else . end)'",
        ),
        (
            "setting-read",
            "        rm \"$S/setting-replica-auto-balance.json\"",
        ),
        (
            "replica-eviction",
            "        reps 'map(.spec.evictionRequested = true)'",
        ),
        (
            "node-eviction",
            "        jq 'map(.spec.evictionRequested = true)' \"$S/lhnodes.json\" > \"$S/lhnodes.new\" && mv \"$S/lhnodes.new\" \"$S/lhnodes.json\"",
        ),
        (
            "disk-eviction",
            "        jq 'map(.spec.disks |= with_entries(.value.evictionRequested = true))' \"$S/lhnodes.json\" > \"$S/lhnodes.new\" && mv \"$S/lhnodes.new\" \"$S/lhnodes.json\"",
        ),
    ] {
        let l = Longhorn::new(&format!("rvr-fresh-{name}"));
        let (_, hash) = l.plan();
        after_lower(&l, hook);
        let o = l.run(&[VOL, REP, &hash], &[]);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", text(&o));
        assert!(
            l.writes().iter().all(|w| !w.starts_with("delete ")),
            "{name}: {:?}",
            l.writes()
        );
        assert!(effect_lines(&text(&o)).is_empty(), "{name}: {}", text(&o));
    }
}

#[test]
fn a_settle_jq_error_cannot_reach_the_delete() {
    let l = Longhorn::new("rvr-jq-settle");
    let (_, hash) = l.plan();
    after_lower(&l, "        : > \"$S/fail-settle-jq\"");
    write_exec(
        &l.dir.join("bin/jq"),
        r#"#!/bin/sh
if [ -f "$STUB_STATE/fail-settle-jq" ]; then
    case "$*" in *'.others_gone'*'length'*) echo 'injected jq safety-check error' >&2; exit 2 ;; esac
fi
exec /usr/bin/jq "$@"
"#,
    );
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("injected jq safety-check error"));
    assert!(
        l.writes().iter().all(|w| !w.starts_with("delete ")),
        "{:?}",
        l.writes()
    );
    assert!(effect_lines(&text(&o)).is_empty());
}

#[test]
fn resume_requires_exactly_one_extra_healthy_active_distinct_copy_above_the_floor() {
    for kind in [
        "unhealthy",
        "inactive",
        "deleting",
        "shared-node",
        "degraded",
        "floor",
        "missing-name",
        "missing-node",
        "missing-disk",
    ] {
        let l = Longhorn::new(&format!("rvr-resume-bound-{kind}"));
        l.volume(volume(3));
        let mut r = Longhorn::four();
        match kind {
            "unhealthy" => r[3]["spec"]["failedAt"] = json!("2026-10-02T03:00:00Z"),
            "inactive" => r[3]["spec"]["active"] = json!(false),
            "deleting" => r[3]["metadata"]["deletionTimestamp"] = json!("2026-10-02T03:00:00Z"),
            "shared-node" => r[2]["spec"]["nodeID"] = json!("cp-2"),
            "missing-node" => r[3]["spec"]["nodeID"] = json!(""),
            "missing-disk" => r[3]["spec"]["diskID"] = json!(""),
            "degraded" => {
                let mut v = volume(3);
                v["status"]["robustness"] = json!("degraded");
                l.volume(v);
            }
            "floor" => {
                l.volume(volume(2));
                r.as_array_mut()
                    .unwrap()
                    .retain(|x| x["spec"]["nodeID"] != "cp-3");
            }
            "missing-name" => r
                .as_array_mut()
                .unwrap()
                .retain(|x| x["metadata"]["name"] != REP),
            _ => unreachable!(),
        }
        l.replicas(r);
        l.refused(&["--plan", VOL, REP], &[]);
    }
}

#[test]
fn resume_count_drift_is_refused_by_the_test_only_patch() {
    let l = Longhorn::new("rvr-resume-cas");
    l.volume(volume(3));
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_COUNT_RACE", "1")]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("test operation failed"));
    assert_eq!(l.writes().len(), 1);
    assert!(!l.writes()[0].contains("replace"));
    assert!(effect_lines(&text(&o)).is_empty());
    assert_eq!(l.replicas_now().len(), 4);
}

#[test]
fn a_back_down_can_remove_a_copy_that_rebuilt_during_the_wait() {
    let l = Longhorn::new("rvr-back-rebuilt");
    l.raised(false, false);
    let (plan, hash) = l.plan_of(NEW, &[]);
    assert!(!plan.contains("costs no healthy copy"), "{plan}");
    assert!(
        plan.contains("may rebuild healthy before the delete"),
        "{plan}"
    );
    after_lower(
        &l,
        &format!(
            "        reps 'map(if .metadata.name == \"{NEW}\" then .spec.healthyAt = \"2026-10-02T03:00:00Z\" else . end)'\n        robust \"$v\""
        ),
    );
    let o = l.run(&[VOL, NEW, &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    assert!(!l.replicas_now().iter().any(|r| r == NEW));
    assert_eq!(effect_lines(&text(&o)).len(), 1);
}

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("retire-volume-replica");
    let p = verb("plan-a-volume-replica-retirement");
    for (name, v) in [
        ("retire-volume-replica", &w),
        ("plan-a-volume-replica-retirement", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert_eq!(v["params"][0]["name"], "volume", "{name}");
        assert_eq!(v["params"][1]["name"], "replica", "{name}");
    }
    assert_eq!(p["argv"][1], "--plan");
    assert!(
        p["timeout"].as_u64().unwrap_or(0) > 120 + 20 + 30 + 30 + 3 * 20,
        "the plan timeout covers the bounded image pull and all six reads"
    );
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert!(
        !about.contains("at no moment"),
        "the healthy count can drop in the instant before the delete, and the about text \
         must not claim otherwise: {about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-volume-replica-retirement");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 30 + 180,
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
