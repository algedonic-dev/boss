//! The two ops verbs that grow an instance's volume from the forge
//! (backlog ebbb923f, incident d3c0a67c): the read-only
//! `plan-an-instance-volume-expansion` (`infra/forge/expand-instance-volume.sh
//! --plan`) and the passkey-approved `expand-instance-volume`.
//!
//! WHY. On 2026-10-01 the system of record's database volume
//! (boss/pgdata-postgres-0) filled, and the only way to grow it was David's
//! hand `kubectl patch`. His first patch, 20Gi -> 40Gi, was REFUSED by
//! Longhorn's admission webhook — one replica of the volume sits on a
//! ~110 GiB disk whose scheduling ledger had ~10.7 GiB of room:
//!
//! ```text
//! CheckReplicasSizeExpansion for volume pvc-93e11a6e-… cannot schedule
//! 21474836480 more bytes to disk bf045701-eeec-47ec-899b-4c987ef122ce
//! (StorageMaximum 117656518656, StorageReserved 35296955596,
//! StorageScheduled 70866960384, OverProvisioningPercentage 100,
//! MinimalAvailablePercentage 25): ScheduledTotal 92341796864 >
//! ProvisionedLimit 82359563060
//! ```
//!
//! — and 30Gi went through. The plan here must compute that refusal
//! BEFORE any write, from every replica's disk, and name the largest size
//! that fits. The fixtures below are those numbers, so the arithmetic is
//! held to the webhook's own: 20 GiB more is refused, 10 GiB more fits.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small cluster behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: the PVC, its StorageClass, the Longhorn volume, the
//! replica and Longhorn node lists, and the two Longhorn settings, each in
//! the shape the API serves. Every door call lands in `argv`, so a
//! refusal can be shown to have patched nothing. Nothing here reaches a
//! cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/expand-instance-volume.sh";
const NS: &str = "boss";
const PVC: &str = "pgdata-postgres-0";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
const GIB: u64 = 1024 * 1024 * 1024;

// The refusal's own disk, byte for byte.
const SMALL_DISK_UUID: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";
const SMALL_MAX: u64 = 117_656_518_656;
const SMALL_RESERVED: u64 = 35_296_955_596;
const SMALL_SCHEDULED: u64 = 70_866_960_384;

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `pvc-<ns>-<name>.json`, `sc-<name>.json`, `vol-<name>.json`,
/// `setting-<name>.json`, and the `replicas.json` / `lhnodes.json` lists.
/// A pvc patch applies the JSON patch's `test` + `replace` of
/// spec.resources.requests.storage, then resizes: STUB_RESIZE unset sets
/// the PVC's status.capacity and the Longhorn volume's spec.size to the
/// new size; `pending` grows only the Longhorn volume and leaves the PVC
/// at FileSystemResizePending; `never` grows nothing.
/// STUB_FORBIDDEN=1 answers every read as RBAC does; STUB_PATCH_FAIL=1
/// refuses the patch the way the Longhorn webhook does; STUB_EMPTY=<resource>
/// answers a get of that resource with exit 0 and no bytes.
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
    echo "Error from server (Forbidden): $2 is forbidden: User \"boss-ops\" cannot get resource \"$2\"" >&2
    exit 1
fi
# The namespace of a namespaced call: the word after -n.
ns=""
prev=""
for a in "$@"; do [ "$prev" = -n ] && ns="$a"; prev="$a"; done
notfound() { echo "Error from server (NotFound): $1 \"$2\" not found" >&2; exit 1; }
case "$1 $2" in
"get pvc")
    f="$S/pvc-$ns-$3.json"; [ -f "$f" ] || notfound persistentvolumeclaims "$3"; cat "$f"; exit 0 ;;
"get storageclass")
    f="$S/sc-$3.json"; [ -f "$f" ] || notfound storageclasses.storage.k8s.io "$3"; cat "$f"; exit 0 ;;
"get volumes.longhorn.io")
    f="$S/vol-$3.json"; [ -f "$f" ] || notfound volumes.longhorn.io "$3"; cat "$f"; exit 0 ;;
"get settings.longhorn.io")
    f="$S/setting-$3.json"; [ -f "$f" ] || notfound settings.longhorn.io "$3"; cat "$f"; exit 0 ;;
"get replicas.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"; exit 0 ;;
"get nodes.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/lhnodes.json")"; exit 0 ;;
"patch pvc")
    name="$3"; f="$S/pvc-$ns-$name.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server (Forbidden): admission webhook \"validator.longhorn.io\" denied the request: cannot schedule 21474836480 more bytes to disk" >&2; exit 1; }
    want=$(printf '%s' "$p" | jq -r '.[] | select(.op == "test" and .path == "/spec/resources/requests/storage") | .value')
    new=$(printf '%s' "$p" | jq -r '.[] | select(.op == "replace" and .path == "/spec/resources/requests/storage") | .value')
    cur=$(jq -r '.spec.resources.requests.storage' "$f")
    [ -n "$want" ] && [ "$want" = "$cur" ] || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
    jq --arg n "$new" '.spec.resources.requests.storage = $n' "$f" > "$f.new" && mv "$f.new" "$f"
    bytes=$(( ${new%Gi} * 1073741824 ))
    vol=$(jq -r '.spec.volumeName' "$f")
    case "${STUB_RESIZE:-}" in
    never) ;;
    pending)
        jq --arg b "$bytes" '.spec.size = $b' "$S/vol-$vol.json" > "$S/vol.new" && mv "$S/vol.new" "$S/vol-$vol.json"
        jq '.status.conditions = [{"type":"FileSystemResizePending","status":"True","message":"Waiting for user to (re-)start a pod to finish file system resize of volume on node."}]' "$f" > "$f.new" && mv "$f.new" "$f" ;;
    *)
        jq --arg b "$bytes" '.spec.size = $b' "$S/vol-$vol.json" > "$S/vol.new" && mv "$S/vol.new" "$S/vol-$vol.json"
        jq --arg n "$new" '.status.capacity.storage = $n' "$f" > "$f.new" && mv "$f.new" "$f" ;;
    esac
    echo "persistentvolumeclaim/$name patched"; exit 0 ;;
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
            "expand_instance_volume_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn pvc(request: &str, capacity: &str, phase: &str) -> Value {
    json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": {"name": PVC, "namespace": NS, "labels": {"app": "postgres"}},
        "spec": {"accessModes": ["ReadWriteOnce"], "storageClassName": "longhorn",
                 "volumeName": VOL, "volumeMode": "Filesystem",
                 "resources": {"requests": {"storage": request}}},
        "status": {"phase": phase, "accessModes": ["ReadWriteOnce"],
                   "capacity": {"storage": capacity}}
    })
}

fn storage_class(name: &str, expand: Option<bool>, provisioner: &str) -> Value {
    let mut sc = json!({
        "apiVersion": "storage.k8s.io/v1", "kind": "StorageClass",
        "metadata": {"name": name},
        "provisioner": provisioner,
        "parameters": {"numberOfReplicas": "2"},
        "reclaimPolicy": "Delete"
    });
    if let Some(e) = expand {
        sc["allowVolumeExpansion"] = json!(e);
    }
    sc
}

fn volume(size_gib: u64, robustness: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": VOL, "namespace": "longhorn-system"},
        "spec": {"numberOfReplicas": 2, "size": (size_gib * GIB).to_string()},
        "status": {"state": "attached", "robustness": robustness, "currentNodeID": "cp-1",
                   "kubernetesStatus": {"namespace": NS, "pvcName": PVC, "pvName": VOL}}
    })
}

fn replica(name: &str, node: &str, disk_uuid: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system",
                     "labels": {"longhornvolume": VOL}},
        "spec": {"volumeName": VOL, "nodeID": node, "diskID": disk_uuid,
                 "healthyAt": "2026-09-12T10:00:00Z", "failedAt": ""},
        "status": {"currentState": "running"}
    })
}

/// A Longhorn node with one disk, in bytes. `available` is the live free
/// space, which moves every second on a busy disk.
fn lh_node(
    name: &str,
    uuid: &str,
    max: u64,
    reserved: u64,
    scheduled: u64,
    available: u64,
) -> Value {
    let disk = format!("default-disk-{name}");
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {disk.clone(): {"allowScheduling": true, "evictionRequested": false,
                                          "path": "/var/lib/longhorn/",
                                          "storageReserved": reserved}}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {disk: {"diskUUID": uuid,
                                         "storageMaximum": max,
                                         "storageScheduled": scheduled,
                                         "storageAvailable": available,
                                         "conditions": [{"type": "Ready", "status": "True"},
                                                        {"type": "Schedulable", "status": "True"}]}}}
    })
}

fn setting(name: &str, value: &str) -> Value {
    json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
           "metadata": {"name": name, "namespace": "longhorn-system"},
           "value": value})
}

const BIG_DISK_UUID: &str = "0c0ffee0-1111-2222-3333-444455556666";

impl Cluster {
    /// The estate of 2026-10-01 before David's hand patch: the SoR's
    /// database volume at 20Gi, one replica on cp-1's big disk and one on
    /// w-2's ~110 GiB disk — the refusal's disk, byte for byte.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let c = Cluster { dir };
        c.put(
            &format!("pvc-{NS}-{PVC}.json"),
            pvc("20Gi", "20Gi", "Bound"),
        );
        c.put(
            "sc-longhorn.json",
            storage_class("longhorn", Some(true), "driver.longhorn.io"),
        );
        c.put(&format!("vol-{VOL}.json"), volume(20, "healthy"));
        c.replicas(json!([
            replica(&format!("{VOL}-r-aaaa1111"), "cp-1", BIG_DISK_UUID),
            replica(&format!("{VOL}-r-bbbb2222"), "w-2", SMALL_DISK_UUID),
            // Another volume's replica on the small disk is that volume's.
            json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
                   "metadata": {"name": "pvc-1111-r-x", "namespace": "longhorn-system"},
                   "spec": {"volumeName": "pvc-11111111-2222-3333-4444-555555555555",
                            "nodeID": "w-2", "diskID": SMALL_DISK_UUID,
                            "healthyAt": "2026-09-12T10:00:00Z", "failedAt": ""}}),
        ]));
        c.small_disk(60_000_000_000);
        c.put(
            "setting-storage-over-provisioning-percentage.json",
            setting("storage-over-provisioning-percentage", "100"),
        );
        c.put(
            "setting-storage-minimal-available-percentage.json",
            setting("storage-minimal-available-percentage", "25"),
        );
        c
    }

    /// The Longhorn nodes, with the small disk's live free space set.
    fn small_disk(&self, available: u64) {
        self.nodes(json!([
            lh_node(
                "cp-1",
                BIG_DISK_UUID,
                500 * GIB,
                150 * GIB,
                100 * GIB,
                380 * GIB
            ),
            lh_node(
                "w-2",
                SMALL_DISK_UUID,
                SMALL_MAX,
                SMALL_RESERVED,
                SMALL_SCHEDULED,
                available
            ),
        ]));
    }

    fn put(&self, file: &str, v: Value) {
        write_file(&self.dir.join("state").join(file), &v.to_string());
    }

    fn get(&self, file: &str) -> Value {
        let p = self.dir.join("state").join(file);
        serde_json::from_str(&std::fs::read_to_string(&p).expect("state")).expect("json")
    }

    fn pvc_now(&self) -> Value {
        self.get(&format!("pvc-{NS}-{PVC}.json"))
    }

    fn replicas(&self, r: Value) {
        write_file(&self.dir.join("state/replicas.json"), &r.to_string());
    }

    fn nodes(&self, n: Value) {
        write_file(&self.dir.join("state/lhnodes.json"), &n.to_string());
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_script(repo_root().join(SCRIPT), args, env)
    }

    fn run_script(&self, script: PathBuf, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(script)
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_EXPAND_WAIT_S", "3")
            .env("BOSS_EXPAND_POLL_S", "1")
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

    fn patched(&self) -> bool {
        self.calls().iter().any(|c| c.starts_with("patch "))
    }

    fn plan(&self, size: &str) -> (String, String) {
        let o = self.run(&["--plan", NS, PVC, size], &[]);
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

    /// A plan that must be refused: exit 78, nothing patched; its words.
    fn refused(&self, size: &str) -> String {
        let o = self.run(&["--plan", NS, PVC, size], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{size} is refused: {out}");
        assert!(
            !out.lines().any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        assert!(!self.patched(), "{:?}", self.calls());
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
    let re = verb("expand-instance-volume")["effect"]
        .as_str()
        .expect("expand-instance-volume declares an `effect`")
        .to_string();
    out.lines()
        .filter(|line| jq_test(&re, line))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------- the webhook's sums

#[test]
fn the_refusals_own_numbers_twenty_more_gib_is_refused_and_ten_more_fits() {
    let c = Cluster::new("eiv-webhook");
    // 20Gi -> 40Gi is 21474836480 more bytes on the small disk: refused,
    // in the webhook's own figures, naming the disk and the largest fit.
    let out = c.refused("40Gi");
    for want in [
        SMALL_DISK_UUID,
        "ScheduledTotal 92341796864 > ProvisionedLimit 82359563060",
        "(117656518656 - 35296955596) x 100%",
        "the largest size that fits every replica's disk is 30Gi",
    ] {
        assert!(out.contains(want), "the refusal says {want:?}:\n{out}");
    }
    // 11 GiB more is 11811160064 bytes; the disk has 11492602676.
    let out = c.refused("31Gi");
    assert!(
        out.contains("ScheduledTotal 82678120448 > ProvisionedLimit 82359563060"),
        "{out}"
    );

    // 20Gi -> 30Gi is 10737418240 more bytes: fits, and says by how much.
    let (plan, hash) = c.plan("30Gi");
    for want in [
        "plan: expand-instance-volume",
        &format!("claim: {NS}/{PVC}"),
        &format!("volume: {VOL}"),
        "size: 20Gi (21474836480 bytes) -> 30Gi (32212254720 bytes), +10737418240 bytes",
        "storage class: longhorn (driver.longhorn.io, allowVolumeExpansion true)",
        "robustness: healthy",
        "storage-over-provisioning-percentage 100",
        &format!(
            "disk {SMALL_DISK_UUID} (default-disk-w-2 on w-2): 1 replica(s) of this volume; \
             ProvisionedLimit (117656518656 - 35296955596) x 100% = 82359563060; \
             ScheduledTotal 70866960384 + 1 x 10737418240 = 81604378624 <= 82359563060 — fits"
        ),
        &format!("disk {BIG_DISK_UUID} (default-disk-cp-1 on cp-1)"),
        "the largest size the replica disks' ledgers admit is 30Gi",
        &format!("replica {VOL}-r-bbbb2222 on w-2"),
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        !plan.contains("pvc-1111-r-x"),
        "another volume's replica is not this plan's: {plan}"
    );
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(!c.patched(), "the plan patches nothing: {:?}", c.calls());
    assert!(effect_lines(&plan).is_empty(), "{plan}");
}

#[test]
fn after_the_hand_patch_no_growth_fits_until_a_replica_moves() {
    // The estate David's 30Gi left: the small disk's ledger grew by the
    // 10 GiB he added, so it has 755184436 bytes of room — under 1 GiB.
    let c = Cluster::new("eiv-after");
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("30Gi", "30Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(30, "healthy"));
    c.nodes(json!([
        lh_node(
            "cp-1",
            BIG_DISK_UUID,
            500 * GIB,
            150 * GIB,
            110 * GIB,
            370 * GIB
        ),
        lh_node(
            "w-2",
            SMALL_DISK_UUID,
            SMALL_MAX,
            SMALL_RESERVED,
            SMALL_SCHEDULED + 10 * GIB,
            50_000_000_000,
        ),
    ]));
    let out = c.refused("31Gi");
    assert!(
        out.contains("ScheduledTotal 82678120448 > ProvisionedLimit 82359563060"),
        "{out}"
    );
    assert!(
        out.contains("the largest size that fits every replica's disk is 30Gi")
            && out.contains("a replica must move off that disk first"),
        "{out}"
    );
}

/// Longhorn v1.11.3's CheckReplicasSizeExpansion runs
/// ValidateDiskAvailableForExpansion on every disk BEFORE the ledger sum:
/// physicalUsed = max - available - reserved, and it refuses when
/// physicalUsed + required > ProvisionedLimit, or when
/// max - (physicalUsed + required) < the minimal-available floor. Found
/// by adversarial review adc832e2: the ledger fits here, the disk does not.
#[test]
fn a_growth_the_ledger_admits_and_the_disk_cannot_hold_is_refused() {
    let c = Cluster::new("eiv-physical");
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("30Gi", "30Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(30, "healthy"));
    c.nodes(json!([
        lh_node(
            "cp-1",
            BIG_DISK_UUID,
            500 * GIB,
            150 * GIB,
            100 * GIB,
            380 * GIB
        ),
        lh_node(
            "w-2",
            SMALL_DISK_UUID,
            SMALL_MAX,
            SMALL_RESERVED,
            40 * GIB,
            31_000_000_000,
        ),
    ]));
    // 30Gi -> 60Gi: the ledger fits (75161927680 <= 82359563060), and
    // 31000000000 is above the floor, but 32212254720 more bytes do not.
    let out = c.refused("60Gi");
    for want in [
        SMALL_DISK_UUID,
        "ValidateDiskAvailableForExpansion",
        "physical used after 83571817780 is over ProvisionedLimit 82359563060",
        // Physical room min(31000000000, 36882825932) -> 28 GiB; the
        // ledger alone would say 66Gi.
        "the largest size that fits every replica's disk is 58Gi",
    ] {
        assert!(out.contains(want), "the refusal says {want:?}:\n{out}");
    }
    let (plan, _) = c.plan("58Gi");
    assert!(
        plan.contains("the largest size the replica disks' ledgers admit is 66Gi"),
        "{plan}"
    );
}

#[test]
fn over_provisioned_the_minimal_available_form_binds() {
    // At 200% the limit is 164719126120, so neither the ledger nor the
    // physical-limit form binds; what the disk must keep free does:
    // max - (physicalUsed + required) >= floor(max x 25%) = 29414129664.
    let c = Cluster::new("eiv-physical-floor");
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("40Gi", "40Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(40, "healthy"));
    c.put(
        "setting-storage-over-provisioning-percentage.json",
        setting("storage-over-provisioning-percentage", "200"),
    );
    c.small_disk(31_000_000_000);
    // +40 GiB = 42949672960; the disk can give 31000000000 + 35296955596
    // - 29414129664 = 36882825932 (34 whole GiB).
    let out = c.refused("80Gi");
    assert!(
        out.contains("ValidateDiskAvailableForExpansion")
            && out.contains("is under the floor 29414129664"),
        "{out}"
    );
    assert!(
        out.contains("the largest size that fits every replica's disk is 74Gi"),
        "{out}"
    );
    c.plan("74Gi");
}

#[test]
fn two_replicas_on_one_disk_count_the_growth_twice() {
    // The webhook multiplies the growth by the replicas a disk holds.
    let c = Cluster::new("eiv-twice");
    c.replicas(json!([
        replica(&format!("{VOL}-r-1"), "w-2", SMALL_DISK_UUID),
        replica(&format!("{VOL}-r-2"), "w-2", SMALL_DISK_UUID),
    ]));
    let out = c.refused("30Gi");
    assert!(
        out.contains("ScheduledTotal 92341796864 > ProvisionedLimit 82359563060"),
        "2 x 10737418240 more on one disk: {out}"
    );
    // floor(11492602676 / 2) = 5746301338 bytes, 5 whole GiB.
    assert!(
        out.contains("the largest size that fits every replica's disk is 25Gi"),
        "{out}"
    );
    c.plan("25Gi");
}

fn largest_fit_cluster(name: &str) -> Cluster {
    let c = Cluster::new(name);
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("30Gi", "30Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(30, "healthy"));
    c.replicas(json!([
        replica(&format!("{VOL}-r-1"), "w-2", SMALL_DISK_UUID),
        replica(&format!("{VOL}-r-2"), "w-2", SMALL_DISK_UUID),
    ]));
    c.nodes(json!([lh_node(
        "w-2",
        SMALL_DISK_UUID,
        100 * GIB,
        0,
        76 * GIB,
        70 * GIB
    )]));
    c
}

#[test]
fn largest_fit_discovers_explicit_args_and_the_existing_signed_plan_without_writing() {
    let c = largest_fit_cluster("eiv-largest-aggregate");
    let (plan, hash) = c.plan("42Gi");
    c.refused("43Gi");
    let out = c.run(&["--plan-largest", NS, PVC], &[]);
    assert!(out.status.success(), "largest fit renders: {}", text(&out));
    let proposal: Value = serde_json::from_slice(&out.stdout).expect("one typed proposal");
    assert_eq!(proposal["verb"], "plan-an-instance-volume-expansion");
    assert_eq!(proposal["args"], json!([NS, PVC, "42Gi"]));
    assert_eq!(proposal["plan"], plan);
    assert_eq!(proposal["plan_sha256"], hash);
    assert!(!c.patched(), "read-only discovery: {:?}", c.calls());
    assert!(effect_lines(&text(&out)).is_empty());
}

#[test]
fn largest_fit_holds_physical_space_two_times_and_ceiling_without_changing_approval() {
    for (name, max, scheduled, available, expected) in [
        ("physical", 100, 0, 41, "38Gi"),
        ("twice", 300, 0, 250, "60Gi"),
    ] {
        let c = largest_fit_cluster(&format!("eiv-largest-{name}"));
        c.nodes(json!([lh_node(
            "w-2",
            SMALL_DISK_UUID,
            max * GIB,
            0,
            scheduled * GIB,
            available * GIB
        )]));
        let out = c.run(&["--plan-largest", NS, PVC], &[]);
        assert!(out.status.success(), "{name}: {}", text(&out));
        let proposal: Value = serde_json::from_slice(&out.stdout).expect("proposal");
        assert_eq!(proposal["args"], json!([NS, PVC, expected]));
        let (plan, hash) = c.plan(expected);
        assert_eq!(proposal["plan"], plan);
        assert_eq!(proposal["plan_sha256"], hash);
        // A later admissible physical-space change leaves these EXPLICIT args
        // and their signed plan unchanged, even if fresh discovery could grow more.
        c.nodes(json!([lh_node(
            "w-2",
            SMALL_DISK_UUID,
            max * GIB,
            0,
            scheduled * GIB,
            (available + 1) * GIB
        )]));
        assert_eq!(c.plan(expected).1, hash);
        assert!(!c.patched());
    }
    let c = largest_fit_cluster("eiv-largest-ceiling");
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("60Gi", "60Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(60, "healthy"));
    c.nodes(json!([lh_node(
        "w-2",
        SMALL_DISK_UUID,
        500 * GIB,
        0,
        0,
        400 * GIB
    )]));
    let out = c.run(&["--plan-largest", NS, PVC], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let proposal: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(proposal["args"], json!([NS, PVC, "100Gi"]));
    assert!(!c.patched());
}

#[test]
fn largest_fit_refuses_ambiguous_malformed_overflow_and_unread_inputs_without_a_proposal() {
    for case in [
        "duplicate-node",
        "duplicate-disk",
        "duplicate-replica",
        "wrong-volume",
        "wrong-kind",
        "wrong-setting",
        "null-size",
        "negative-disk",
        "overflow",
        "fraction",
        "numeric-exponent",
        "bad-setting",
        "multi-document",
        "null-document",
        "empty-read",
        "forbidden",
        "no-growth",
        "physical-floor",
    ] {
        let c = largest_fit_cluster(&format!("eiv-largest-refuse-{case}"));
        let mut nodes = c.get("lhnodes.json");
        let mut v = c.get(&format!("vol-{VOL}.json"));
        let mut env = vec![];
        match case {
            "duplicate-node" => {
                let extra = nodes[0].clone();
                nodes.as_array_mut().unwrap().push(extra);
                c.nodes(nodes);
            }
            "duplicate-disk" => {
                let d = nodes[0]["status"]["diskStatus"]["default-disk-w-2"].clone();
                nodes[0]["status"]["diskStatus"]["other"] = d;
                c.nodes(nodes);
            }
            "duplicate-replica" => {
                let mut r = c.get("replicas.json");
                let extra = r[0].clone();
                r.as_array_mut().unwrap().push(extra);
                c.replicas(r);
            }
            "wrong-volume" => {
                v["metadata"]["name"] = json!("pvc-11111111-2222-3333-4444-555555555555");
                c.put(&format!("vol-{VOL}.json"), v);
            }
            "wrong-kind" => {
                v["kind"] = json!("DifferentResource");
                c.put(&format!("vol-{VOL}.json"), v);
            }
            "wrong-setting" => c.put(
                "setting-storage-over-provisioning-percentage.json",
                setting("different-setting", "100"),
            ),
            "null-size" => {
                v["spec"]["size"] = Value::Null;
                c.put(&format!("vol-{VOL}.json"), v);
            }
            "negative-disk" | "overflow" | "fraction" => {
                nodes[0]["status"]["diskStatus"]["default-disk-w-2"]["storageScheduled"] =
                    match case {
                        "negative-disk" => json!(-1),
                        "overflow" => json!("9007199254740992"),
                        _ => json!("1.5"),
                    };
                c.nodes(nodes);
            }
            "numeric-exponent" => {
                nodes[0]["status"]["diskStatus"]["default-disk-w-2"]["storageScheduled"] =
                    json!("8e10");
                c.nodes(nodes);
            }
            "bad-setting" => c.put(
                "setting-storage-minimal-available-percentage.json",
                setting("storage-minimal-available-percentage", "101"),
            ),
            "multi-document" => {
                let p = pvc("30Gi", "30Gi", "Bound").to_string();
                write_file(
                    &c.dir.join(format!("state/pvc-{NS}-{PVC}.json")),
                    &format!("{p}\n{p}"),
                );
            }
            "null-document" => c.put(&format!("pvc-{NS}-{PVC}.json"), Value::Null),
            "empty-read" => env.push(("STUB_EMPTY", "settings.longhorn.io")),
            "forbidden" => env.push(("STUB_FORBIDDEN", "1")),
            "no-growth" => {
                nodes[0]["status"]["diskStatus"]["default-disk-w-2"]["storageScheduled"] =
                    json!(100 * GIB);
                c.nodes(nodes);
            }
            "physical-floor" => {
                nodes[0]["status"]["diskStatus"]["default-disk-w-2"]["storageAvailable"] =
                    json!(25 * GIB);
                c.nodes(nodes);
            }
            _ => unreachable!(),
        }
        let out = c.run(&["--plan-largest", NS, PVC], &env);
        assert!(!out.status.success(), "{case} must refuse: {}", text(&out));
        assert!(
            out.stdout.is_empty(),
            "{case}: no partial proposal: {}",
            text(&out)
        );
        assert!(!c.patched(), "{case}: {:?}", c.calls());
        assert!(effect_lines(&text(&out)).is_empty());
    }
}

#[test]
fn largest_fit_registry_is_read_only_and_preserves_the_explicit_write_contract() {
    let p = verb("plan-the-largest-instance-volume-expansion");
    assert_eq!(p["hosts"], json!(["forge"]));
    assert_eq!(p["argv"], json!([SCRIPT, "--plan-largest", "{1}", "{2}"]));
    assert_eq!(p["params"].as_array().unwrap().len(), 2);
    assert_eq!(p["params"][0]["name"], "namespace");
    assert_eq!(p["params"][1]["name"], "pvc");
    assert_ne!(p["requires_approval"], true);
    assert!(p["effect"].is_null());
    let w = verb("expand-instance-volume");
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-an-instance-volume-expansion");
    assert_eq!(w["params"][2]["name"], "size");
    assert_eq!(w["params"][3]["name"], "plan_sha256");
    assert!(!jq_test(
        w["params"][2]["pattern"].as_str().unwrap(),
        "auto"
    ));
    let c = largest_fit_cluster("eiv-largest-no-auto-write");
    let out = c.run(&[NS, PVC, "auto", &"0".repeat(64)], &[]);
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(!c.patched());
}

#[test]
fn the_over_provisioning_setting_scales_the_limit() {
    let c = Cluster::new("eiv-overprov");
    c.put(
        "setting-storage-over-provisioning-percentage.json",
        setting("storage-over-provisioning-percentage", "200"),
    );
    let (plan, _) = c.plan("40Gi");
    assert!(
        plan.contains("ProvisionedLimit (117656518656 - 35296955596) x 200% = 164719126120"),
        "{plan}"
    );
}

#[test]
fn a_disk_under_the_minimal_available_floor_admits_no_growth() {
    // StorageAvailable must stay above StorageMaximum x 25% (29414129664).
    let c = Cluster::new("eiv-pressure");
    c.small_disk(29_000_000_000);
    let out = c.refused("30Gi");
    assert!(
        out.contains("storage-minimal-available-percentage") && out.contains(SMALL_DISK_UUID),
        "{out}"
    );
}

#[test]
fn the_plan_is_blind_to_live_free_space() {
    let c = Cluster::new("eiv-determinism");
    let (plan, hash) = c.plan("30Gi");
    assert_eq!(c.plan("30Gi"), (plan, hash.clone()));
    c.small_disk(61_234_567_890);
    assert_eq!(
        c.plan("30Gi").1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
}

fn physical_limited_cluster(name: &str) -> Cluster {
    let c = Cluster::new(name);
    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("30Gi", "30Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(30, "healthy"));
    c.nodes(json!([
        lh_node(
            "cp-1",
            BIG_DISK_UUID,
            500 * GIB,
            150 * GIB,
            100 * GIB,
            380 * GIB
        ),
        lh_node(
            "w-2",
            SMALL_DISK_UUID,
            SMALL_MAX,
            SMALL_RESERVED,
            40 * GIB,
            31_000_000_000
        ),
    ]));
    c
}

fn available_now(c: &Cluster, available: u64) {
    let mut nodes = c.get("lhnodes.json");
    nodes[1]["status"]["diskStatus"]["default-disk-w-2"]["storageAvailable"] = json!(available);
    c.nodes(nodes);
}

#[test]
fn physical_room_changes_never_change_the_signed_ledger_plan() {
    let c = physical_limited_cluster("eiv-stable-ledger-plan");
    let first = c.run(&["--plan", NS, PVC, "50Gi"], &[]);
    assert!(first.status.success(), "{}", text(&first));
    available_now(&c, 33_000_000_000);
    let second = c.run(&["--plan", NS, PVC, "50Gi"], &[]);
    assert!(second.status.success(), "{}", text(&second));
    assert_ne!(
        first.stderr, second.stderr,
        "the live physical observation changes"
    );
    assert_eq!(
        first.stdout, second.stdout,
        "physical room must not change any signed byte"
    );
    assert_eq!(
        c.plan("50Gi"),
        (
            String::from_utf8(first.stdout).unwrap(),
            String::from_utf8_lossy(&first.stderr)
                .lines()
                .find_map(|l| l.strip_prefix("plan-sha256: "))
                .unwrap()
                .into()
        )
    );
    let plan = String::from_utf8(second.stdout).unwrap();
    assert!(
        plan.contains("the largest size the replica disks' ledgers admit is 66Gi"),
        "{plan}"
    );
    assert!(
        plan.contains("physical space is re-checked before any write"),
        "{plan}"
    );
    assert!(!c.patched());
}

#[test]
fn an_admissible_physical_change_keeps_the_approval_usable() {
    let c = physical_limited_cluster("eiv-stable-ledger-write");
    let (_, hash) = c.plan("50Gi");
    available_now(&c, 33_000_000_000);
    let out = c.run(&[NS, PVC, "50Gi", &hash], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(c.patched());
    assert_eq!(effect_lines(&text(&out)).len(), 1);
}

#[test]
fn an_approved_growth_is_refused_when_live_physical_space_no_longer_fits() {
    let c = physical_limited_cluster("eiv-physical-recheck");
    let (_, hash) = c.plan("58Gi");
    available_now(&c, 30_000_000_000); // Still above the floor; less than the growth.
    let out = c.run(&[NS, PVC, "58Gi", &hash], &[]);
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(text(&out).contains("ValidateDiskAvailableForExpansion"));
    assert!(!c.patched());
    assert!(effect_lines(&text(&out)).is_empty());
}

#[test]
fn expansion_reads_the_shared_physical_function_instead_of_an_inline_copy() {
    let c = Cluster::new("eiv-shared-arithmetic-door");
    let infra = c.dir.join("source/infra");
    std::fs::create_dir_all(infra.join("forge")).unwrap();
    std::fs::create_dir_all(infra.join("lib")).unwrap();
    for dir in ["cluster", "estate"] {
        std::os::unix::fs::symlink(repo_root().join("infra").join(dir), infra.join(dir)).unwrap();
    }
    std::os::unix::fs::symlink(repo_root().join("examples"), c.dir.join("source/examples"))
        .unwrap();
    for file in ["sor.sh", "sor-reader.sh", "jq.sh"] {
        write_file(
            &infra.join("lib").join(file),
            &std::fs::read_to_string(repo_root().join("infra/lib").join(file)).unwrap(),
        );
    }
    let library =
        std::fs::read_to_string(repo_root().join("infra/lib/longhorn-ledger.sh")).unwrap();
    write_file(
        &infra.join("lib/longhorn-ledger.sh"),
        &format!(
            "{library}\nLONGHORN_LEDGER_JQ+='def lh_physical($required; $opct; $mpct): if $required <= 0 then [] else [\"shared-physical-refusal-sentinel\"] end;'\n"
        ),
    );
    let script = infra.join("forge/expand-instance-volume.sh");
    write_file(
        &script,
        &std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap(),
    );
    let out = c.run_script(script, &["--plan", NS, PVC, "30Gi"], &[]);
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(
        text(&out).contains("shared-physical-refusal-sentinel"),
        "{}",
        text(&out)
    );
    assert!(!c.patched());
}

#[test]
fn expansion_and_the_shared_library_judge_the_same_disk_fixtures() {
    let cases = [
        (50_000_000_000, 55, true), // A growth never subtracts requiredStorage at the floor.
        (31_000_000_000, 58, true),
        (31_000_000_000, 60, false), // The scheduling ledger fits; physical space refuses.
        (30_000_000_000, 58, false),
        (SMALL_MAX / 4, 31, false), // Equality at the live floor is a refusal.
    ];
    for (available, size, fits) in cases {
        let c = physical_limited_cluster("eiv-shared-fixtures");
        available_now(&c, available);
        let out = Command::new("bash")
            .args([
                "-c",
                r#"
                set -euo pipefail
                . "$1"
                jq -n --slurpfile nodes "$2" --argjson growth "$3" "$LONGHORN_LEDGER_JQ"'
                    lh_disks($nodes[0]) | map(select(.node == "w-2")) | .[0] |
                    {limit: lh_limit(100), room: lh_room(100),
                     physical: lh_physical($growth; 100; 25),
                     physical_room: lh_physical_room(100; 25)}'
            "#,
                "fixture",
            ])
            .arg(repo_root().join("infra/lib/longhorn-ledger.sh"))
            .arg(c.dir.join("state/lhnodes.json"))
            .arg(((size - 30) * GIB).to_string())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["limit"], 82_359_563_060_u64);
        let growth = (size - 30) * GIB;
        let shared_fits = answer["physical"].as_array().unwrap().is_empty()
            && growth <= answer["room"].as_u64().unwrap();
        assert_eq!(shared_fits, fits, "{available}, {size}: {answer}");
        let out = c.run(&["--plan", NS, PVC, &format!("{size}Gi")], &[]);
        assert_eq!(out.status.success(), shared_fits, "{}", text(&out));
        if !shared_fits {
            assert_eq!(out.status.code(), Some(78), "{}", text(&out));
        }
        assert!(!c.patched());
    }
}

#[test]
fn replicas_sharing_a_disk_multiply_the_live_physical_growth() {
    let c = physical_limited_cluster("eiv-shared-disk-physical");
    c.replicas(json!([
        replica(&format!("{VOL}-r-1"), "w-2", SMALL_DISK_UUID),
        replica(&format!("{VOL}-r-2"), "w-2", SMALL_DISK_UUID),
    ]));
    let mut nodes = c.get("lhnodes.json");
    nodes[1]["status"]["diskStatus"]["default-disk-w-2"]["storageScheduled"] = json!(20 * GIB);
    c.nodes(nodes);
    c.plan("44Gi"); // Two 14Gi growths fit in 31 billion available bytes.
    let out = c.refused("45Gi"); // Two 15Gi growths do not; the ledger still fits.
    assert!(out.contains("ValidateDiskAvailableForExpansion"), "{out}");
    assert!(
        out.contains("physicalUsed 51359563060 + 32212254720"),
        "{out}"
    );
    assert!(
        out.contains("the largest size that fits every replica's disk is 44Gi"),
        "{out}"
    );
    assert!(!c.patched());
}

#[test]
fn malformed_or_missing_disk_figures_cannot_render_a_plan() {
    let c = Cluster::new("eiv-unreported-disk-figures");
    let nodes = c.get("lhnodes.json");
    for field in [
        "storageMaximum",
        "storageScheduled",
        "storageAvailable",
        "storageReserved",
    ] {
        for bad in [Value::Null, json!("not-a-byte-count")] {
            let mut altered = nodes.clone();
            if field == "storageReserved" {
                altered[1]["spec"]["disks"]["default-disk-w-2"][field] = bad;
            } else {
                altered[1]["status"]["diskStatus"]["default-disk-w-2"][field] = bad;
            }
            c.nodes(altered);
            let out = c.run(&["--plan", NS, PVC, "30Gi"], &[]);
            assert_eq!(out.status.code(), Some(1), "{field}: {}", text(&out));
            assert!(text(&out).contains("CANNOT ANSWER"), "{}", text(&out));
            assert!(out.stdout.is_empty());
            assert!(!c.patched());
        }
    }
}

// ------------------------------------------------------------- the bounds

#[test]
fn a_malformed_or_foreign_request_is_refused_before_any_door_opens() {
    let c = Cluster::new("eiv-shape");
    for args in [
        vec!["--plan", NS, PVC, "30G"],
        vec!["--plan", NS, PVC, "30"],
        vec!["--plan", NS, PVC, "-30Gi"],
        vec!["--plan", NS, PVC, "030Gi"],
        vec!["--plan", NS, PVC, "30Ti"],
        vec!["--plan", NS, "-pvc", "30Gi"],
        vec!["--plan", NS, "Pgdata", "30Gi"],
        vec!["--plan", NS, PVC],
        vec!["--plan", NS, PVC, "30Gi", "extra"],
        // Not an instance: the pipeline's namespace, a system one.
        vec!["--plan", "boss-dev", PVC, "30Gi"],
        vec!["--plan", "kube-system", PVC, "30Gi"],
        vec!["--plan", "longhorn-system", PVC, "30Gi"],
        // The write without a hash, or with one that is not one.
        vec![NS, PVC, "30Gi"],
        vec![NS, PVC, "30Gi", "not-a-hash"],
    ] {
        let o = c.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(c.calls().is_empty(), "no door opened: {:?}", c.calls());
}

#[test]
fn it_grows_only_by_at_most_double_and_under_the_ceiling() {
    let c = Cluster::new("eiv-growth");
    for size in ["20Gi", "10Gi"] {
        let out = c.refused(size);
        assert!(out.contains("grows only"), "{size}: {out}");
    }
    // Room for anything, so the bound refusing is the verb's own.
    c.put(
        "setting-storage-over-provisioning-percentage.json",
        setting("storage-over-provisioning-percentage", "1000"),
    );
    let out = c.refused("41Gi");
    assert!(out.contains("at most 2x") && out.contains("40Gi"), "{out}");
    c.plan("40Gi");

    c.put(
        &format!("pvc-{NS}-{PVC}.json"),
        pvc("80Gi", "80Gi", "Bound"),
    );
    c.put(&format!("vol-{VOL}.json"), volume(80, "healthy"));
    let out = c.refused("120Gi");
    assert!(out.contains("ceiling") && out.contains("100Gi"), "{out}");
    c.plan("100Gi");
}

#[test]
fn the_claim_class_and_volume_must_each_admit_an_expansion() {
    let c = Cluster::new("eiv-state");
    let ok_pvc = pvc("20Gi", "20Gi", "Bound");
    let cases: Vec<(&str, Box<dyn Fn(&Cluster)>, &str)> = vec![
        (
            "a claim that is not Bound",
            Box::new(|c: &Cluster| {
                c.put(
                    &format!("pvc-{NS}-{PVC}.json"),
                    pvc("20Gi", "20Gi", "Pending"),
                )
            }),
            "Bound",
        ),
        (
            "an expansion already in flight",
            Box::new(|c: &Cluster| {
                c.put(
                    &format!("pvc-{NS}-{PVC}.json"),
                    pvc("30Gi", "20Gi", "Bound"),
                )
            }),
            "in flight",
        ),
        (
            "a class that does not allow expansion",
            Box::new(|c: &Cluster| {
                c.put(
                    "sc-longhorn.json",
                    storage_class("longhorn", Some(false), "driver.longhorn.io"),
                )
            }),
            "allowVolumeExpansion",
        ),
        (
            "a class that does not say",
            Box::new(|c: &Cluster| {
                c.put(
                    "sc-longhorn.json",
                    storage_class("longhorn", None, "driver.longhorn.io"),
                )
            }),
            "allowVolumeExpansion",
        ),
        (
            "a class that is not Longhorn's",
            Box::new(|c: &Cluster| {
                c.put(
                    "sc-longhorn.json",
                    storage_class("longhorn", Some(true), "rancher.io/local-path"),
                )
            }),
            "driver.longhorn.io",
        ),
        (
            "a degraded volume",
            Box::new(|c: &Cluster| c.put(&format!("vol-{VOL}.json"), volume(20, "degraded"))),
            "robustness",
        ),
        (
            "a Longhorn size that is not the claim's",
            Box::new(|c: &Cluster| c.put(&format!("vol-{VOL}.json"), volume(25, "healthy"))),
            "spec.size",
        ),
    ];
    for (what, setup, says) in cases {
        setup(&c);
        let out = c.refused("30Gi");
        assert!(out.contains(says), "{what}: says {says:?}: {out}");
        // Back to the healthy estate for the next case.
        c.put(&format!("pvc-{NS}-{PVC}.json"), ok_pvc.clone());
        c.put(
            "sc-longhorn.json",
            storage_class("longhorn", Some(true), "driver.longhorn.io"),
        );
        c.put(&format!("vol-{VOL}.json"), volume(20, "healthy"));
    }
    // A Longhorn volume bound to another claim is not this claim's.
    let mut v = volume(20, "healthy");
    v["status"]["kubernetesStatus"]["pvcName"] = json!("someone-else");
    c.put(&format!("vol-{VOL}.json"), v);
    let out = c.refused("30Gi");
    assert!(out.contains("someone-else"), "{out}");
}

#[test]
fn a_missing_claim_is_refused_and_an_unread_cluster_cannot_answer() {
    let c = Cluster::new("eiv-dark");
    let o = c.run(&["--plan", NS, "no-such-claim", "30Gi"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("NotFound"), "{out}");

    let o = c.run(&["--plan", NS, PVC, "30Gi"], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a read that could not look is no plan: {out}"
    );
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );

    // No document at all — exit 0, no bytes — is not an empty answer.
    for resource in [
        "pvc",
        "storageclass",
        "volumes.longhorn.io",
        "replicas.longhorn.io",
        "nodes.longhorn.io",
        "settings.longhorn.io",
    ] {
        let o = c.run(&["--plan", NS, PVC, "30Gi"], &[("STUB_EMPTY", resource)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{resource}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{resource}: {out}");
    }
    // A replica on a disk no Longhorn node reports: the webhook could
    // not judge it either, and neither can the plan.
    c.replicas(json!([replica(
        &format!("{VOL}-r-1"),
        "w-2",
        "ffffffff-0000-0000-0000-000000000000"
    )]));
    let o = c.run(&["--plan", NS, PVC, "30Gi"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("ffffffff-0000"),
        "{out}"
    );
    assert!(!c.patched(), "{:?}", c.calls());
}

// --------------------------------------------------------------- the write

#[test]
fn the_write_patches_the_signed_request_and_reads_the_capacity_back() {
    let c = Cluster::new("eiv-write");
    let (plan, hash) = c.plan("30Gi");
    let o = c.run(&[NS, PVC, "30Gi", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let patches: Vec<String> = c
        .calls()
        .into_iter()
        .filter(|l| l.starts_with("patch "))
        .collect();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains(&format!("patch pvc {PVC} -n {NS} --type=json")),
        "{p}"
    );
    assert!(
        p.contains(r#"{"op":"test","path":"/spec/resources/requests/storage","value":"20Gi"}"#)
            && p.contains(
                r#"{"op":"replace","path":"/spec/resources/requests/storage","value":"30Gi"}"#
            ),
        "a compare-and-set on the request and nothing else: {p}"
    );
    assert_eq!(c.pvc_now()["status"]["capacity"]["storage"], "30Gi");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains("32212254720"), "{hits:?}");
}

#[test]
fn the_write_refuses_a_plan_that_moved_and_a_second_run() {
    let c = Cluster::new("eiv-drift");
    let (_, hash) = c.plan("30Gi");
    // Another volume was scheduled onto the small disk after the signature.
    c.nodes(json!([
        lh_node(
            "cp-1",
            BIG_DISK_UUID,
            500 * GIB,
            150 * GIB,
            100 * GIB,
            380 * GIB
        ),
        // 100 MiB more scheduled: 30Gi still fits (by 650 MiB), so the
        // refusal is the hash's, not the webhook sum's.
        lh_node(
            "w-2",
            SMALL_DISK_UUID,
            SMALL_MAX,
            SMALL_RESERVED,
            SMALL_SCHEDULED + 100 * 1024 * 1024,
            60_000_000_000,
        ),
    ]));
    let o = c.run(&[NS, PVC, "30Gi", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    let o = c.run(&[NS, PVC, "30Gi", &"0".repeat(64)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(!c.patched(), "{:?}", c.calls());

    // Applied once, the same approval is spent: the size before is in
    // the signed bytes, and it moved.
    c.small_disk(60_000_000_000);
    let (_, hash) = c.plan("30Gi");
    assert!(c.run(&[NS, PVC, "30Gi", &hash], &[]).status.success());
    let o = c.run(&[NS, PVC, "30Gi", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(
        c.calls().iter().filter(|l| l.starts_with("patch ")).count(),
        1
    );
}

#[test]
fn a_resize_that_stalls_is_reported_and_is_not_an_effect() {
    for (mode, says) in [
        ("pending", "FileSystemResizePending"),
        ("never", "spec.size 21474836480"),
    ] {
        let c = Cluster::new(&format!("eiv-stall-{mode}"));
        let (_, hash) = c.plan("30Gi");
        let o = c.run(&[NS, PVC, "30Gi", &hash], &[("STUB_RESIZE", mode)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{mode}: {out}");
        assert!(out.contains("NOT proven"), "{mode}: {out}");
        assert!(out.contains(says), "{mode}: names what it read: {out}");
        assert!(effect_lines(&out).is_empty(), "{mode}: {out}");
    }
}

#[test]
fn a_patch_the_webhook_refuses_is_a_failure_with_its_words() {
    let c = Cluster::new("eiv-patch-refused");
    let (_, hash) = c.plan("30Gi");
    let o = c.run(&[NS, PVC, "30Gi", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(
        c.pvc_now()["spec"]["resources"]["requests"]["storage"],
        "20Gi"
    );
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("expand-instance-volume");
    let p = verb("plan-an-instance-volume-expansion");
    for (name, v) in [
        ("expand-instance-volume", &w),
        ("plan-an-instance-volume-expansion", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        for (i, n) in ["namespace", "pvc", "size"].iter().enumerate() {
            assert_eq!(v["params"][i]["name"], *n, "{name}");
        }
    }
    assert_eq!(p["argv"][1], "--plan");
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-an-instance-volume-expansion");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["params"][3]["name"], "plan_sha256");
    assert!(w["effect"].is_string());
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 600,
        "the verb's timeout outlasts the script's own read-back"
    );
    let pat = |i: usize| {
        w["params"][i]["pattern"]
            .as_str()
            .expect("a pattern")
            .to_string()
    };
    for ok in ["30Gi", "1Gi", "100Gi"] {
        assert!(jq_test(&pat(2), ok), "{ok}");
    }
    for bad in [
        "30G", "30", "0Gi", "030Gi", "-30Gi", "30Ti", "30Gi ", "1.5Gi", "",
    ] {
        assert!(!jq_test(&pat(2), bad), "the size pattern admits {bad:?}");
    }
    assert!(jq_test(&pat(1), PVC));
    for bad in ["-pvc", "Pgdata", "a b", "pvc/x", ""] {
        assert!(!jq_test(&pat(1), bad), "the pvc pattern admits {bad:?}");
    }
}
