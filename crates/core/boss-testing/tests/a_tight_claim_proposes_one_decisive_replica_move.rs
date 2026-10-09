//! The no-growth fallback of read-only volume discovery (backlog 53c8cb72,
//! design ada8f698 "A tight claim proposes one evidenced replica move",
//! option A — David, 2026-10-07): when
//! `plan-the-largest-instance-volume-expansion` finds that no whole-GiB
//! growth fits a claim, `infra/forge/expand-instance-volume.sh
//! --plan-largest` asks `infra/forge/move-volume-replica.sh
//! --plan-decisive` whether EXACTLY ONE replica move would free the
//! volume to grow, and proposes that move's existing plan — or refuses,
//! saying which fact was missing.
//!
//! WHAT "DECISIVE" IS, from the design's own lines: the replica is one of
//! the claim-bound volume's healthy active copies; the existing move plan
//! admits it (every disk the new replica can land on passes the plan's
//! placement and next-growth checks); and every REMAINING replica disk
//! admits the projected next growth on the same shared arithmetic. Exactly
//! one such replica is proposed; zero or several refuse, naming every
//! candidate. Nothing ranks.
//!
//! IT IS A PROPOSAL OF A PLAN, NEVER A MOVE. Every test here asserts the
//! door saw reads only: no patch, no delete.
//!
//! HOW THIS IS MEASURED. Both scripts run for real, with `sudo` stubbed on
//! PATH as a small cluster behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`. Discovery reads the cluster TWICE — the growth read,
//! then the move read — so the stub can swap its state after N door calls
//! (STUB_SWAP_AFTER) and a test can make only the second read partial,
//! duplicated or malformed. No test here waits on a clock: these are reads.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const EXPAND: &str = "infra/forge/expand-instance-volume.sh";
const MOVE: &str = "infra/forge/move-volume-replica.sh";
const NS: &str = "boss";
const PVC: &str = "pgdata-postgres-0";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
const GIB: u64 = 1024 * 1024 * 1024;

/// w-2's disk by the webhook's own numbers of 2026-10-01, after the hand
/// patch to 30Gi: 755184436 bytes of ledger room — under 1 GiB.
const W2_DISK: &str = "bf045701-eeec-47ec-899b-4c987ef122ce";
const W2_MAX: u64 = 117_656_518_656;
const W2_RESERVED: u64 = 35_296_955_596;
const W2_SCHEDULED: u64 = 81_604_378_624;
const W1_DISK: &str = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10";
const CP1_DISK: &str = "844915d9-6a5b-4da9-9619-8f3d188d632b";
const CP2_DISK: &str = "976a5246-7a68-452c-bb25-f9e3ecb2258e";
const CP3_DISK: &str = "9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1";

/// The discovery's own reads of the growth half: the claim, its class,
/// the volume, the two lists and the two ledger settings.
const GROWTH_READS: usize = 7;

/// The stand-in behind the kubectl door: reads only. State under
/// $STUB_STATE — and, once more than $STUB_SWAP_AFTER door calls have
/// been answered, under $STUB_STATE.second. A list file that is an array
/// is wrapped as the API wraps it; one that is an object is served as it
/// stands, so a test can serve a truncated list. Anything that is not a
/// get is recorded and answered exit 2: this door has no write.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
if [ -n "${STUB_SWAP_AFTER:-}" ]; then
    n=$(wc -l < "$STUB_ARGV")
    [ "$n" -le "$STUB_SWAP_AFTER" ] || S="$STUB_STATE.second"
fi
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
ns=""
prev=""
for a in "$@"; do [ "$prev" = -n ] && ns="$a"; prev="$a"; done
notfound() { echo "Error from server (NotFound): $1 \"$2\" not found" >&2; exit 1; }
list() {
    case "$(head -c 1 "$1")" in
    '[') printf '{"apiVersion":"v1","kind":"List","metadata":{},"items":%s}\n' "$(cat "$1")" ;;
    *) cat "$1" ;;
    esac
}
case "$1 $2" in
"get pvc")
    f="$S/pvc-$ns-$3.json"; [ -f "$f" ] || notfound persistentvolumeclaims "$3"; cat "$f"; exit 0 ;;
"get storageclass")
    f="$S/sc-$3.json"; [ -f "$f" ] || notfound storageclasses.storage.k8s.io "$3"; cat "$f"; exit 0 ;;
"get volumes.longhorn.io")
    f="$S/vol-$3.json"; [ -f "$f" ] || notfound volumes.longhorn.io "$3"; cat "$f"; exit 0 ;;
"get settings.longhorn.io")
    f="$S/setting-$3.json"; [ -f "$f" ] || notfound settings.longhorn.io "$3"; cat "$f"; exit 0 ;;
"get replicas.longhorn.io") list "$S/replicas.json"; exit 0 ;;
"get nodes.longhorn.io") list "$S/lhnodes.json"; exit 0 ;;
esac
echo "stub: this door only reads, and was asked: $*" >&2
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
            "a_tight_claim_proposes_one_decisive_replica_move: no {tool} on this box — the gate \
             image has it, and a trust-boundary test that cannot run must fail, never pass by \
             returning early"
        );
    }
}

fn rep(suffix: &str) -> String {
    format!("{VOL}-r-{suffix}")
}

fn replica(name: &str, node: &str, disk: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system",
                     "labels": {"longhornvolume": VOL}},
        "spec": {"volumeName": VOL, "nodeID": node, "diskID": disk,
                 "healthyAt": "2026-09-12T10:00:00Z", "failedAt": "", "active": true},
        "status": {"currentState": "running"}
    })
}

/// A Longhorn node with one disk, in bytes; half the disk is free.
fn node(name: &str, uuid: &str, max: u64, reserved: u64, scheduled: u64) -> Value {
    let disk = format!("default-disk-{name}");
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false, "tags": [],
                 "disks": {disk.clone(): {"allowScheduling": true, "evictionRequested": false,
                                          "path": "/var/lib/longhorn/", "diskType": "filesystem",
                                          "storageReserved": reserved, "tags": []}}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {disk: {"diskUUID": uuid, "storageMaximum": max,
                                         "storageScheduled": scheduled,
                                         "storageAvailable": max / 2,
                                         "conditions": [{"type": "Ready", "status": "True"},
                                                        {"type": "Schedulable", "status": "True"}]}}}
    })
}

/// A control plane's disk: 500 GiB, room to spare for the volume's growth.
fn cp(name: &str, uuid: &str) -> Value {
    node(name, uuid, 500 * GIB, 150 * GIB, 100 * GIB)
}

fn w2() -> Value {
    node("w-2", W2_DISK, W2_MAX, W2_RESERVED, W2_SCHEDULED)
}

/// w-1 holds no replica of the volume: 1.8 TiB, a third reserved.
fn w1() -> Value {
    node("w-1", W1_DISK, 1800 * GIB, 540 * GIB, 300 * GIB)
}

fn volume(replicas: u64, size_gib: u64) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": VOL, "namespace": "longhorn-system"},
        "spec": {"numberOfReplicas": replicas, "size": (size_gib * GIB).to_string(),
                 "dataLocality": "disabled", "replicaSoftAntiAffinity": "ignored",
                 "replicaAutoBalance": "ignored", "dataEngine": "v1", "nodeID": "cp-1"},
        "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "cp-1",
                   "actualSize": 20 * GIB,
                   "kubernetesStatus": {"namespace": NS, "pvcName": PVC, "pvName": VOL}}
    })
}

fn pvc(size_gib: u64) -> Value {
    json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": {"name": PVC, "namespace": NS},
        "spec": {"accessModes": ["ReadWriteOnce"], "storageClassName": "longhorn",
                 "volumeName": VOL, "volumeMode": "Filesystem",
                 "resources": {"requests": {"storage": format!("{size_gib}Gi")}}},
        "status": {"phase": "Bound", "capacity": {"storage": format!("{size_gib}Gi")}}
    })
}

struct Cluster {
    dir: PathBuf,
}

impl Cluster {
    /// THE ONE DECISIVE CASE: a healthy 30Gi volume with three replicas,
    /// on cp-1, cp-2 and w-2; w-2's disk has under 1 GiB of ledger room,
    /// so no growth fits; w-1 holds no replica and has room for the
    /// replica and its next growth; cp-1 and cp-2 admit growth to twice.
    /// Moving the w-2 replica is the single move that frees the volume.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        write_exec(&dir.join("bin/sudo"), STUB);
        let c = Cluster { dir };
        for state in ["state", "state.second"] {
            std::fs::create_dir_all(c.dir.join(state)).expect("state");
            c.put_in(state, &format!("pvc-{NS}-{PVC}.json"), pvc(30));
            c.put_in(
                state,
                "sc-longhorn.json",
                json!({"apiVersion": "storage.k8s.io/v1", "kind": "StorageClass",
                       "metadata": {"name": "longhorn"}, "provisioner": "driver.longhorn.io",
                       "allowVolumeExpansion": true}),
            );
            c.put_in(state, &format!("vol-{VOL}.json"), volume(3, 30));
            c.put_in(state, "replicas.json", Self::three());
            c.put_in(
                state,
                "lhnodes.json",
                json!([cp("cp-1", CP1_DISK), cp("cp-2", CP2_DISK), w1(), w2()]),
            );
            for (setting, value) in [
                ("storage-over-provisioning-percentage", "100"),
                ("storage-minimal-available-percentage", "25"),
                ("replica-soft-anti-affinity", "false"),
                ("replica-auto-balance", "disabled"),
            ] {
                c.put_in(
                    state,
                    &format!("setting-{setting}.json"),
                    json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
                           "metadata": {"name": setting, "namespace": "longhorn-system"},
                           "value": value}),
                );
            }
        }
        c
    }

    fn three() -> Value {
        json!([
            replica(&rep("7c1934ba"), "cp-1", CP1_DISK),
            replica(&rep("1269eb6e"), "cp-2", CP2_DISK),
            replica(&rep("ae04deab"), "w-2", W2_DISK),
            // Another volume's replica on the target node is that volume's.
            json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
                   "metadata": {"name": "pvc-11111111-2222-3333-4444-555555555555-r-0000aaaa",
                                "namespace": "longhorn-system"},
                   "spec": {"volumeName": "pvc-11111111-2222-3333-4444-555555555555",
                            "nodeID": "w-1", "diskID": W1_DISK,
                            "healthyAt": "2026-09-12T10:00:00Z", "failedAt": "", "active": true}}),
        ])
    }

    fn put_in(&self, state: &str, file: &str, v: Value) {
        write_file(&self.dir.join(state).join(file), &v.to_string());
    }

    /// Both reads see it.
    fn put(&self, file: &str, v: Value) {
        self.put_in("state", file, v.clone());
        self.put_in("state.second", file, v);
    }

    fn get(&self, file: &str) -> Value {
        let p = self.dir.join("state").join(file);
        serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("json")
    }

    fn run(&self, script: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(script))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env_remove("BOSS_RETIRE_FLOOR")
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

    /// The registered discovery, as the verb file's argv runs it.
    fn discover(&self, env: &[(&str, &str)]) -> Output {
        self.run(EXPAND, &["--plan-largest", NS, PVC], env)
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

    fn only_read(&self) {
        let calls = self.calls();
        assert!(
            calls.iter().all(|c| c.starts_with("get ")),
            "discovery reads and does nothing else: {calls:?}"
        );
    }

    /// The move read happened: the growth read alone is exactly
    /// GROWTH_READS door calls, and the move read begins with the volume.
    fn asked_the_move_read(&self) -> bool {
        let calls = self.calls();
        calls.len() > GROWTH_READS
            && calls[GROWTH_READS].starts_with(&format!("get volumes.longhorn.io {VOL} "))
    }

    /// A discovery that must propose nothing: not exit 0, no stdout at
    /// all, reads only — and its words.
    fn nothing_proposed(&self, env: &[(&str, &str)]) -> (Option<i32>, String) {
        let out = self.discover(env);
        let words = text(&out);
        assert!(!out.status.success(), "must not propose: {words}");
        assert!(out.stdout.is_empty(), "no partial proposal: {words}");
        self.only_read();
        (out.status.code(), words)
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

fn sha256(text: &str) -> String {
    let dir = scratch_dir("decisive-move-sha");
    let file = dir.join("plan");
    write_file(&file, text);
    let o = Command::new("sha256sum")
        .arg(&file)
        .output()
        .expect("sha256sum");
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()
        .expect("a hash")
        .to_string()
}

// ----------------------------------------------------------- the one proposal

#[test]
fn no_growth_with_one_decisive_move_proposes_that_moves_existing_plan() {
    let c = Cluster::new("decisive-one");
    let out = c.discover(&[]);
    assert!(out.status.success(), "one decisive move: {}", text(&out));
    let proposal: Value = serde_json::from_slice(&out.stdout).expect("one typed proposal");
    let keys: Vec<&str> = proposal
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["args", "plan", "plan_sha256", "target", "verb"],
        "exactly the typed proposal, nothing beside it"
    );
    assert_eq!(proposal["verb"], "plan-a-volume-replica-move");
    assert_eq!(proposal["args"], json!([VOL, rep("ae04deab")]));
    // The claim the discovery was asked about is conserved in the answer.
    assert_eq!(proposal["target"], json!([NS, PVC]));
    // The plan IS the existing move plan for that replica, byte for byte:
    // what plan-a-volume-replica-move renders and David's passkey signs.
    let plain = c.run(MOVE, &["--plan", VOL, &rep("ae04deab")], &[]);
    assert!(plain.status.success(), "{}", text(&plain));
    assert_eq!(
        proposal["plan"].as_str().unwrap(),
        String::from_utf8_lossy(&plain.stdout)
    );
    let hash = proposal["plan_sha256"].as_str().unwrap();
    assert_eq!(hash, sha256(proposal["plan"].as_str().unwrap()));
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains(&format!("plan-sha256: {hash}")),
        "the hash is the one the plan verb prints"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no whole-GiB growth fits"),
        "the growth read's own finding stays on the record: {}",
        text(&out)
    );
    assert!(c.asked_the_move_read());
    c.only_read();
}

#[test]
fn the_listing_order_does_not_choose() {
    let forward = Cluster::new("decisive-order-forward");
    let reversed = Cluster::new("decisive-order-reversed");
    for file in ["replicas.json", "lhnodes.json"] {
        let mut rows = reversed.get(file);
        rows.as_array_mut().unwrap().reverse();
        reversed.put(file, rows);
    }
    let a = forward.discover(&[]);
    let b = reversed.discover(&[]);
    assert!(a.status.success() && b.status.success(), "{}", text(&b));
    assert_eq!(
        a.stdout, b.stdout,
        "the same facts in another order are the same proposal"
    );
}

// ------------------------------------------------------------- the refusals

#[test]
fn two_short_disks_have_no_single_decisive_move() {
    // The design's own worked case: two replica disks each unable to admit
    // the next growth. Moving either leaves the other short, so neither
    // move is decisive — option A stops, and names both.
    let c = Cluster::new("decisive-two-bottlenecks");
    c.put(
        "lhnodes.json",
        json!([
            cp("cp-1", CP1_DISK),
            node("cp-2", CP2_DISK, W2_MAX, W2_RESERVED, W2_SCHEDULED),
            w1(),
            w2()
        ]),
    );
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("no replica move is proposed for boss/pgdata-postgres-0"),
        "{words}"
    );
    assert!(
        words.contains("2 replica disks are short") && words.contains("no single move"),
        "{words}"
    );
    for candidate in [rep("1269eb6e"), rep("ae04deab")] {
        assert!(words.contains(&candidate), "names {candidate}: {words}");
    }
}

#[test]
fn a_sole_bottleneck_whose_remaining_disks_are_short_of_the_next_growth_is_refused() {
    // w-2 is the one disk that admits no growth at all, but cp-2 has room
    // for only 10 GiB of a 30 GiB projected growth: the move would not
    // free the volume, so it is not decisive.
    let c = Cluster::new("decisive-remaining-short");
    c.put(
        "lhnodes.json",
        json!([
            cp("cp-1", CP1_DISK),
            node("cp-2", CP2_DISK, 500 * GIB, 150 * GIB, 340 * GIB),
            w1(),
            w2()
        ]),
    );
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(words.contains("no single move"), "{words}");
    assert!(words.contains(CP2_DISK), "names the short disk: {words}");
}

#[test]
fn a_destination_that_does_not_admit_the_volume_is_refused_by_the_move_plans_own_sums() {
    // Ledger: w-1 has room for the replica but not for twice the volume.
    let c = Cluster::new("decisive-target-ledger");
    c.put(
        "lhnodes.json",
        json!([
            cp("cp-1", CP1_DISK),
            cp("cp-2", CP2_DISK),
            node("w-1", W1_DISK, 500 * GIB, 150 * GIB, 310 * GIB),
            w2()
        ]),
    );
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("without ledger headroom for twice the volume") && words.contains(W1_DISK),
        "{words}"
    );
    // Live: Longhorn's own placement test on the disk's free space.
    let c = Cluster::new("decisive-target-live");
    let mut nodes = c.get("lhnodes.json");
    nodes[2]["status"]["diskStatus"]["default-disk-w-1"]["storageAvailable"] = json!(460 * GIB);
    c.put("lhnodes.json", nodes);
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("fails Longhorn v1.11.3's live test")
            && words.contains("IsSchedulableToDisk"),
        "{words}"
    );
    // No destination at all: every node already holds a replica.
    let c = Cluster::new("decisive-no-target");
    c.put(
        "lhnodes.json",
        json!([cp("cp-1", CP1_DISK), cp("cp-2", CP2_DISK), w2()]),
    );
    let mut reps = c.get("replicas.json");
    reps.as_array_mut().unwrap().pop();
    c.put("replicas.json", reps);
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("no disk on a node without a replica"),
        "{words}"
    );
}

#[test]
fn a_volume_that_is_not_wholly_healthy_or_that_longhorn_would_trim_is_refused() {
    type Change = fn(&Cluster);
    let cases: [(&str, Change, &str); 6] = [
        (
            "inactive-replica",
            |c| {
                let mut reps = c.get("replicas.json");
                reps[1]["spec"]["active"] = json!(false);
                c.put("replicas.json", reps);
            },
            "only a volume whose every declared replica is healthy",
        ),
        (
            "detached",
            |c| {
                let mut v = c.get(&format!("vol-{VOL}.json"));
                v["status"]["state"] = json!("detached");
                c.put(&format!("vol-{VOL}.json"), v);
            },
            "not attached",
        ),
        (
            "node-eviction",
            |c| {
                let mut nodes = c.get("lhnodes.json");
                nodes[0]["spec"]["evictionRequested"] = json!(true);
                c.put("lhnodes.json", nodes);
            },
            "an eviction is requested",
        ),
        (
            "auto-balance",
            |c| {
                c.put(
                    "setting-replica-auto-balance.json",
                    json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
                           "metadata": {"name": "replica-auto-balance",
                                        "namespace": "longhorn-system"},
                           "value": "best-effort"}),
                );
            },
            "replica auto-balance is on",
        ),
        (
            "data-locality",
            |c| {
                let mut v = c.get(&format!("vol-{VOL}.json"));
                v["spec"]["dataLocality"] = json!("best-effort");
                c.put(&format!("vol-{VOL}.json"), v);
            },
            "data locality best-effort",
        ),
        (
            "soft-anti-affinity",
            |c| {
                c.put(
                    "setting-replica-soft-anti-affinity.json",
                    json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
                           "metadata": {"name": "replica-soft-anti-affinity",
                                        "namespace": "longhorn-system"},
                           "value": "true"}),
                );
            },
            "may place two replicas on one node",
        ),
    ];
    for (name, change, want) in cases {
        let c = Cluster::new(&format!("decisive-unsafe-{name}"));
        change(&c);
        let (code, words) = c.nothing_proposed(&[]);
        assert_eq!(code, Some(78), "{name}: {words}");
        assert!(
            words.contains("no replica move is proposed for boss/pgdata-postgres-0")
                && words.contains(want),
            "{name} says {want:?}: {words}"
        );
        assert!(c.asked_the_move_read(), "{name}");
    }
    // A volume Longhorn itself reads degraded never reaches the move read:
    // the growth read refuses it first, as it always did.
    let c = Cluster::new("decisive-unsafe-degraded");
    let mut v = c.get(&format!("vol-{VOL}.json"));
    v["status"]["robustness"] = json!("degraded");
    c.put(&format!("vol-{VOL}.json"), v);
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(words.contains("only a healthy volume grows"), "{words}");
    assert!(!c.asked_the_move_read());
}

#[test]
fn more_replicas_than_the_retirement_floor_is_a_persons_choice_between_retiring_and_moving() {
    // The estate of 2026-10-06: boss/pgdata-postgres-0 with FOUR replicas,
    // one on w-2's short disk. A retirement to three is admissible by
    // count, and David chose it; a move is then not the one decisive act.
    let c = Cluster::new("decisive-above-the-floor");
    c.put(&format!("vol-{VOL}.json"), volume(4, 30));
    let mut reps = c.get("replicas.json");
    reps.as_array_mut()
        .unwrap()
        .push(replica(&rep("03f4dac5"), "cp-3", CP3_DISK));
    c.put("replicas.json", reps);
    let mut nodes = c.get("lhnodes.json");
    nodes.as_array_mut().unwrap().push(cp("cp-3", CP3_DISK));
    c.put("lhnodes.json", nodes);
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    for want in [
        "no replica move is proposed for boss/pgdata-postgres-0",
        "holds 4 replicas, above the retirement floor of 3",
        "plan-a-volume-replica-retirement",
        &rep("ae04deab"),
    ] {
        assert!(words.contains(want), "says {want:?}: {words}");
    }
}

#[test]
fn a_ceiling_is_not_a_placement_bound_and_asks_no_move() {
    // At the script's ceiling nothing a replica move changes could admit a
    // growth, so the move read is never made.
    let c = Cluster::new("decisive-ceiling");
    c.put(&format!("pvc-{NS}-{PVC}.json"), pvc(100));
    c.put(&format!("vol-{VOL}.json"), volume(3, 100));
    let (code, words) = c.nothing_proposed(&[]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("the ceiling 100Gi") && words.contains("no replica move would change that"),
        "{words}"
    );
    assert!(!c.asked_the_move_read(), "{:?}", c.calls());
}

#[test]
fn a_partial_duplicated_or_malformed_second_read_cannot_answer() {
    let swap = GROWTH_READS.to_string();
    type Change = fn(&Cluster);
    let cases: [(&str, Change); 6] = [
        ("truncated-replica-list", |c| {
            let items = c.get("replicas.json");
            c.put_in(
                "state.second",
                "replicas.json",
                json!({"apiVersion": "longhorn.io/v1beta2", "kind": "ReplicaList",
                       "metadata": {"continue": "eyJ2IjoibWV0YS5rOHMuaW8vdjEifQ"},
                       "items": items}),
            );
        }),
        ("duplicate-replica", |c| {
            let mut reps = c.get("replicas.json");
            let extra = reps[2].clone();
            reps.as_array_mut().unwrap().push(extra);
            c.put_in("state.second", "replicas.json", reps);
        }),
        ("duplicate-disk-identity", |c| {
            let mut nodes = c.get("lhnodes.json");
            let d = nodes[3]["status"]["diskStatus"]["default-disk-w-2"].clone();
            nodes[3]["status"]["diskStatus"]["other"] = d;
            c.put_in("state.second", "lhnodes.json", nodes);
        }),
        ("fractional-ledger", |c| {
            let mut nodes = c.get("lhnodes.json");
            nodes[3]["status"]["diskStatus"]["default-disk-w-2"]["storageScheduled"] =
                json!("8.16e10");
            c.put_in("state.second", "lhnodes.json", nodes);
        }),
        ("another-volume-answers", |c| {
            let mut v = c.get(&format!("vol-{VOL}.json"));
            v["metadata"]["name"] = json!("pvc-11111111-2222-3333-4444-555555555555");
            c.put_in("state.second", &format!("vol-{VOL}.json"), v);
        }),
        ("setting-unreadable", |c| {
            std::fs::remove_file(
                c.dir
                    .join("state.second/setting-replica-soft-anti-affinity.json"),
            )
            .expect("the setting file");
        }),
    ];
    for (name, change) in cases {
        let c = Cluster::new(&format!("decisive-second-read-{name}"));
        change(&c);
        let (code, words) = c.nothing_proposed(&[("STUB_SWAP_AFTER", &swap)]);
        assert_eq!(code, Some(1), "{name} cannot answer: {words}");
        assert!(
            words.contains("CANNOT ANSWER")
                && words.contains("no replica move is proposed for boss/pgdata-postgres-0"),
            "{name}: {words}"
        );
        assert!(c.asked_the_move_read(), "{name}");
    }
}

#[test]
fn a_population_that_changed_between_the_two_reads_is_judged_as_it_now_stands() {
    // Between the growth read and the move read the short replica left
    // w-2 for w-1 (someone moved it). Nothing is short any more, so there
    // is nothing for a move to free: no proposal from stale evidence.
    let c = Cluster::new("decisive-population-changed");
    let mut reps = c.get("replicas.json");
    reps[2]["spec"]["nodeID"] = json!("w-1");
    reps[2]["spec"]["diskID"] = json!(W1_DISK);
    c.put_in("state.second", "replicas.json", reps);
    let swap = GROWTH_READS.to_string();
    let (code, words) = c.nothing_proposed(&[("STUB_SWAP_AFTER", &swap)]);
    assert_eq!(code, Some(78), "{words}");
    assert!(
        words.contains("no replica disk is short of the volume's next growth"),
        "{words}"
    );
}

// ------------------------------------------------- the move script's own door

#[test]
fn the_decisive_read_holds_the_volume_to_the_claim_it_was_asked_about() {
    let c = Cluster::new("decisive-claim-mismatch");
    let out = c.run(MOVE, &["--plan-decisive", VOL, "boss-playground", PVC], &[]);
    let words = text(&out);
    assert_eq!(out.status.code(), Some(78), "{words}");
    assert!(out.stdout.is_empty(), "{words}");
    assert!(
        words.contains("is bound to boss/pgdata-postgres-0, not boss-playground/pgdata-postgres-0"),
        "{words}"
    );
    c.only_read();
}

#[test]
fn the_decisive_read_refuses_malformed_words_before_any_door_opens() {
    let c = Cluster::new("decisive-argv");
    for args in [
        vec!["--plan-decisive", VOL, NS],
        vec!["--plan-decisive", "pvc-not-a-volume", NS, PVC],
        vec!["--plan-decisive", VOL, "Boss", PVC],
        vec!["--plan-decisive", VOL, NS, "pg data"],
        vec!["--plan-decisive", VOL, NS, PVC, "extra"],
    ] {
        let out = c.run(MOVE, &args, &[]);
        assert_eq!(out.status.code(), Some(78), "{args:?}: {}", text(&out));
        assert!(out.stdout.is_empty());
        assert!(c.calls().is_empty(), "{args:?} opened the door");
    }
}

#[test]
fn a_volume_nothing_blocks_is_not_proposed_a_move_when_asked_directly() {
    // Asked of a volume whose every replica disk admits growth, the read
    // must not name a replica merely because it could move.
    let c = Cluster::new("decisive-nothing-short");
    c.put(
        "lhnodes.json",
        json!([
            cp("cp-1", CP1_DISK),
            cp("cp-2", CP2_DISK),
            w1(),
            node("w-2", W2_DISK, 500 * GIB, 150 * GIB, 100 * GIB)
        ]),
    );
    let out = c.run(MOVE, &["--plan-decisive", VOL, NS, PVC], &[]);
    let words = text(&out);
    assert_eq!(out.status.code(), Some(78), "{words}");
    assert!(out.stdout.is_empty(), "{words}");
    assert!(
        words.contains("no replica disk is short of the volume's next growth"),
        "{words}"
    );
    c.only_read();
}

#[test]
fn the_explicit_move_plan_and_its_hash_are_unchanged_by_the_new_read() {
    // The signed bytes are the existing plan's. A plan rendered for the
    // named replica reads exactly as it did before this read existed:
    // its first line, its move line and its count's path.
    let c = Cluster::new("decisive-plan-unchanged");
    let out = c.run(MOVE, &["--plan", VOL, &rep("ae04deab")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let plan = String::from_utf8_lossy(&out.stdout);
    for want in [
        "plan: move-volume-replica\n",
        &format!(
            "move: replica {} on w-2 (disk {W2_DISK})\n",
            rep("ae04deab")
        ),
        "numberOfReplicas: 3 -> 4 -> 3",
        "after: every replica's disk admits growth to twice the volume in its ledger",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        !plan.contains("decisive"),
        "nothing of the discovery enters the signed bytes:\n{plan}"
    );
}

// ---------------------------------------------------------------- the registry

#[test]
fn the_registry_names_the_fallback_as_a_read_only_plan_a_passkey_verb_owns() {
    let expand = verb("expand-instance-volume");
    let entry = &expand["discovery_remedies"][0];
    assert_eq!(entry["no_growth_plan_verb"], "plan-a-volume-replica-move");
    let plan = verb("plan-a-volume-replica-move");
    assert!(plan["about"].as_str().unwrap().starts_with("READ-ONLY"));
    assert_ne!(plan["requires_approval"], true);
    assert_eq!(plan["argv"], json!([MOVE, "--plan", "{1}", "{2}"]));
    let write = verb("move-volume-replica");
    assert_eq!(write["requires_approval"], true);
    assert_eq!(write["plan_verb"], "plan-a-volume-replica-move");
    assert_eq!(write["approvers"], json!(["emp-david"]));
    // No verb reaches --plan-decisive by itself: it is asked only by the
    // registered growth discovery, after that read found no growth.
    let verbs = repo_root().join("infra/ops/verbs");
    for entry in std::fs::read_dir(&verbs).expect("the verbs directory") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|e| e == "json") {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("a verb"))
                .expect("json");
            let argv = v["argv"].to_string();
            assert!(
                !argv.contains("--plan-decisive"),
                "{} runs --plan-decisive directly",
                path.display()
            );
        }
    }
    let discovery = verb("plan-the-largest-instance-volume-expansion");
    assert_eq!(
        discovery["argv"],
        json!([EXPAND, "--plan-largest", "{1}", "{2}"])
    );
    assert!(
        discovery["about"]
            .as_str()
            .unwrap()
            .contains("plan-a-volume-replica-move"),
        "the discovery verb says what else it may propose"
    );
}
