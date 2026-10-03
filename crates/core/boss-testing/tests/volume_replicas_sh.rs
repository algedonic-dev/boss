//! The two ops verbs that raise a Longhorn volume's replica count from
//! the forge (backlog f5f182fc): the read-only
//! `plan-a-volume-replica-change` (`infra/forge/volume-replicas.sh
//! --plan`) and the passkey-approved `set-volume-replicas`.
//!
//! WHY. The dev pod's /work volume (pvc-dd7b5ac3…, 40 GiB, class
//! longhorn-dev-disposable) has ONE replica, on w-1, and w-1 drains on
//! 2026-09-30 for its second NVMe. Under Longhorn's default
//! node-drain-policy (block-if-contains-last-replica) that one replica
//! holds the drain. The StorageClass's numberOfReplicas applies only at
//! creation; the live change is `volumes.longhorn.io`
//! `spec.numberOfReplicas`, and the dev session cannot read or write
//! longhorn.io (Forbidden) — so it is a forge verb over ops_kubectl, in
//! the reclaim-gcp-root / shutdown-node shape: a rendered plan a passkey
//! signs, re-rendered and compared before the one patch.
//!
//! THE EFFECT IS READ BACK: the write prints the line its verb file
//! declares as `effect` only once n replicas report healthy
//! (`spec.healthyAt` set, `spec.failedAt` empty — Longhorn's own test,
//! the one node-status reads) on n DISTINCT nodes. The tests judge that
//! line with the runner's own engine (jq `test`).
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: one volume object, the replica list and the
//! Longhorn node list, each in the shape the API serves
//! (longhorn.io/v1beta2). Every door call lands in `argv`, so a refusal
//! can be shown to have patched nothing. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/volume-replicas.sh";
const VOL: &str = "pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091";
const GIB: u64 = 1024 * 1024 * 1024;

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `vol-<name>.json` a volume, `replicas.json` and `lhnodes.json` lists.
/// A patch applies a JSON patch's `test` + `replace` of
/// spec.numberOfReplicas, then adds one replica per node named in
/// STUB_NEW_NODES (default w-2), healthy unless STUB_REBUILD=never.
/// STUB_FORBIDDEN=1 answers every longhorn.io read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses the patch; STUB_EMPTY=<resource> answers a
/// get of that resource with exit 0 and no bytes at all.
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
    echo "Error from server (Forbidden): $2 is forbidden: User \"system:serviceaccount:boss-dev:boss-dev\" cannot list resource \"$2\" in API group \"longhorn.io\" in the namespace \"longhorn-system\"" >&2
    exit 1
fi
case "$1 $2" in
"get volumes.longhorn.io")
    f="$S/vol-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): volumes.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get replicas.longhorn.io")
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
    jq --argjson n "$new" '.spec.numberOfReplicas = $n' "$f" > "$f.new" && mv "$f.new" "$f"
    i=0
    for node in ${STUB_NEW_NODES:-w-2}; do
        i=$((i + 1))
        h='"2026-09-30T05:00:00Z"'
        [ "${STUB_REBUILD:-}" = never ] && h='""'
        d=$(jq -r --arg n "$node" '[.[] | select(.metadata.name == $n) | .status.diskStatus[].diskUUID] | .[0] // "none"' "$S/lhnodes.json")
        jq --arg v "$v" --arg n "$node" --arg i "$i" --arg d "$d" --argjson h "$h" \
            '. + [{"apiVersion":"longhorn.io/v1beta2","kind":"Replica","metadata":{"name":"\($v)-r-new\($i)","namespace":"longhorn-system","labels":{"longhornvolume":$v}},"spec":{"volumeName":$v,"nodeID":$n,"diskID":$d,"healthyAt":$h,"failedAt":""},"status":{"currentState":"running"}}]' \
            "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    done
    echo "volume.longhorn.io/$v patched"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

struct Longhorn {
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
            "volume_replicas_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn volume(replicas: u64) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": VOL, "namespace": "longhorn-system"},
        "spec": {"numberOfReplicas": replicas, "size": (40 * GIB).to_string(),
                 "dataLocality": "best-effort", "nodeID": "w-1"},
        "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "w-1",
                   "kubernetesStatus": {"namespace": "boss-dev", "pvcName": "boss-dev-work",
                                        "pvName": VOL}}
    })
}

fn replica(vol: &str, name: &str, node: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system",
                     "labels": {"longhornvolume": vol}},
        "spec": {"volumeName": vol, "nodeID": node, "diskID": disk_uuid(node),
                 "healthyAt": if healthy { "2026-09-12T10:00:00Z" } else { "" },
                 "failedAt": if failed { "2026-09-20T10:00:00Z" } else { "" }},
        "status": {"currentState": if failed { "stopped" } else { "running" }}
    })
}

/// The UUID of `node`'s one disk. A live replica names its disk by UUID
/// (spec.diskID = status.diskStatus[<disk>].diskUUID), never by the disk's
/// name, and a fixture that used the name could not catch a script that
/// joined the two wrongly (backlog ab39a34e: the live replicas read through
/// plan-a-volume-replica-change name disks like bf045701-eeec-…).
fn disk_uuid(node: &str) -> String {
    let n: u32 = node.bytes().map(u32::from).sum();
    format!("{n:08x}-eeec-47ec-899b-4c987ef122ce")
}

/// A Longhorn node with one disk; sizes in GiB. `available` is the live
/// free space, which moves every second on a busy disk.
fn lh_node(
    name: &str,
    allow: bool,
    max: u64,
    reserved: u64,
    scheduled: u64,
    available: u64,
) -> Value {
    let disk = format!("default-disk-{name}");
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": allow, "evictionRequested": false,
                 "disks": {disk.clone(): {"allowScheduling": true, "evictionRequested": false,
                                          "path": "/var/lib/longhorn/",
                                          "storageReserved": reserved * GIB}}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {disk: {"diskUUID": disk_uuid(name),
                                         "storageMaximum": max * GIB,
                                         "storageScheduled": scheduled * GIB,
                                         "storageAvailable": available * GIB,
                                         "conditions": [{"type": "Ready", "status": "True"},
                                                        {"type": "Schedulable", "status": "True"}]}}}
    })
}

impl Longhorn {
    /// The estate of 2026-09-30: the dev /work volume with one healthy
    /// replica on w-1; w-2 has room for a 40 GiB replica, cp-1 is short,
    /// cp-2 does not allow scheduling.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.volume(volume(1));
        l.replicas(json!([
            replica(VOL, &format!("{VOL}-r-0a1b2c3d"), "w-1", true, false),
            replica(
                "pvc-11111111-2222-3333-4444-555555555555",
                "pvc-1111-r-x",
                "w-2",
                true,
                false
            ),
        ]));
        l.nodes(json!([
            lh_node("cp-1", true, 100, 30, 60, 35),
            lh_node("cp-2", false, 100, 30, 0, 90),
            lh_node("w-1", true, 900, 270, 300, 500),
            lh_node("w-2", true, 500, 150, 100, 380),
        ]));
        l
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn volume(&self, v: Value) {
        let name = v["metadata"]["name"]
            .as_str()
            .expect("a named volume")
            .to_string();
        self.state(&format!("vol-{name}.json"), &v.to_string());
    }

    fn volume_now(&self) -> Value {
        let p = self.dir.join("state").join(format!("vol-{VOL}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).expect("volume")).expect("json")
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
            .env("BOSS_REPLICAS_WAIT_S", "3")
            .env("BOSS_REPLICAS_POLL_S", "1")
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

    fn plan(&self, n: &str) -> (String, String) {
        let o = self.run(&["--plan", VOL, n], &[]);
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

/// The lines of `out` the write's declared `effect` matches, judged by
/// the runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("set-volume-replicas")["effect"]
        .as_str()
        .expect("set-volume-replicas declares an `effect`")
        .to_string();
    out.lines()
        .filter(|line| {
            let o = Command::new("jq")
                .args([
                    "-n",
                    "--arg",
                    "re",
                    &re,
                    "--arg",
                    "l",
                    line,
                    "$l | test($re)",
                ])
                .output()
                .expect("jq runs");
            assert!(o.status.success(), "jq judges {re:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        })
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_volume_its_replicas_and_the_nodes_that_can_take_one() {
    let l = Longhorn::new("vr-plan");
    let (plan, hash) = l.plan("2");
    for want in [
        &format!("volume: {VOL}"),
        "claim: boss-dev/boss-dev-work",
        "size: 42949672960 bytes (40 GiB)",
        "state: attached on w-1",
        "robustness: healthy",
        "numberOfReplicas: 1 -> 2",
        &format!(
            "replica {VOL}-r-0a1b2c3d on w-1 (disk {}): healthy",
            disk_uuid("w-1")
        ),
        "node w-1: holds a replica of this volume",
        "node w-2: can take a replica",
        "node cp-1: cannot take a replica",
        "node cp-2: cannot take a replica",
        "spec.allowScheduling is false",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        !plan.contains("pvc-1111-r-x"),
        "another volume's replica is not this plan's: {plan}"
    );
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(!l.patched(), "the plan patches nothing: {:?}", l.calls());
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to the live free space: a busy disk's
    // storageAvailable moves every second, and a plan that signed it
    // could never be run.
    let (again, hash2) = l.plan("2");
    assert_eq!((&plan, &hash), (&again, &hash2));
    l.nodes(json!([
        lh_node("cp-1", true, 100, 30, 60, 11),
        lh_node("cp-2", false, 100, 30, 0, 12),
        lh_node("w-1", true, 900, 270, 300, 13),
        lh_node("w-2", true, 500, 150, 100, 14),
    ]));
    assert_eq!(
        l.plan("2").1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
}

#[test]
fn the_plan_refuses_what_the_bound_does_not_admit_and_patches_nothing() {
    let l = Longhorn::new("vr-bounds");
    for args in [
        vec!["--plan", "boss-dev-work", "2"],
        vec!["--plan", "pvc-DD7B5AC3-884e-485f-8c73-92b87ce77091", "2"],
        vec!["--plan", "-pvc", "2"],
        vec!["--plan", VOL, "0"],
        vec!["--plan", VOL, "4"],
        vec!["--plan", VOL, "two"],
        vec!["--plan", VOL],
        vec!["--plan", VOL, "2", "extra"],
    ] {
        let o = l.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(l.calls().is_empty(), "no door opened: {:?}", l.calls());

    // Lowering, or setting what already stands: this verb never deletes,
    // and what a lowered count leaves over is Longhorn's to trim or keep.
    for (current, n) in [(1, "1"), (3, "2")] {
        l.volume(volume(current));
        let o = l.run(&["--plan", VOL, n], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{out}");
        assert!(out.contains("only raises"), "{out}");
        assert!(out.contains("trim or keep"), "{out}");
        assert!(!out.contains("Longhorn deletes the replicas"), "{out}");
        assert!(
            out.contains("healthy on w-1"),
            "the refusal reads what stands: {out}"
        );
    }
    l.volume(volume(1));

    // No healthy replica: nothing to rebuild from.
    l.replicas(json!([replica(VOL, "r-dead", "w-1", true, true)]));
    let o = l.run(&["--plan", VOL, "2"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("no healthy replica"), "{out}");

    // Three replicas need two more nodes, and only w-2 can take one.
    l.replicas(json!([replica(VOL, "r-1", "w-1", true, false)]));
    let o = l.run(&["--plan", VOL, "3"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("w-2") && out.contains("cp-1"),
        "names what it read: {out}"
    );
    assert!(!l.patched(), "{:?}", l.calls());
}

#[test]
fn a_missing_volume_is_refused_and_a_forbidden_read_cannot_answer() {
    let l = Longhorn::new("vr-dark");
    let o = l.run(
        &["--plan", "pvc-00000000-0000-0000-0000-000000000000", "2"],
        &[],
    );
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(78),
        "a volume that is not there: {out}"
    );
    assert!(out.contains("NotFound"), "{out}");

    let o = l.run(&["--plan", VOL, "2"], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a read that could not look is a failure, never a refusal or a plan: {out}"
    );
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );
    assert!(
        !out.lines().any(|l| l.starts_with("plan: ")),
        "no plan is rendered from an unread cluster: {out}"
    );

    // An answer carrying no document at all — exit 0, no bytes — is not
    // an empty cluster: on jq-1.6 `jq -e` alone would pass it (d96e38ab).
    for resource in [
        "replicas.longhorn.io",
        "nodes.longhorn.io",
        "volumes.longhorn.io",
    ] {
        let o = l.run(&["--plan", VOL, "2"], &[("STUB_EMPTY", resource)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{resource}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{resource}: {out}");
    }
    assert!(!l.patched(), "{:?}", l.calls());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_n_healthy_replicas_on_distinct_nodes() {
    let l = Longhorn::new("vr-write");
    let (plan, hash) = l.plan("2");
    let o = l.run(&[VOL, "2", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );
    let patches: Vec<String> = l
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch "))
        .collect();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains(&format!(
            "patch volumes.longhorn.io {VOL} -n longhorn-system --type=json"
        )),
        "{p}"
    );
    assert!(
        p.contains(r#"{"op":"test","path":"/spec/numberOfReplicas","value":1}"#)
            && p.contains(r#"{"op":"replace","path":"/spec/numberOfReplicas","value":2}"#),
        "a compare-and-set on numberOfReplicas and nothing else: {p}"
    );
    assert!(
        !l.calls().iter().any(|c| c.contains("delete")),
        "nothing is deleted: {:?}",
        l.calls()
    );
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 2);
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains("w-1, w-2"), "names the nodes: {hits:?}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let l = Longhorn::new("vr-drift");
    let (_, hash) = l.plan("2");
    // The replica set moved after the plan was signed.
    l.replicas(json!([
        replica(VOL, &format!("{VOL}-r-0a1b2c3d"), "w-1", true, false),
        replica(VOL, "r-stale", "cp-1", false, true),
    ]));
    let o = l.run(&[VOL, "2", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    for bad in ["0".repeat(64), "not-a-hash".to_string()] {
        let o = l.run(&[VOL, "2", &bad], &[]);
        assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    }
    assert!(!l.patched(), "{:?}", l.calls());

    // Applied once, the same approval is spent.
    let (_, hash) = l.plan("2");
    let o = l.run(&[VOL, "2", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = l.run(&[VOL, "2", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    let patches = l.calls().iter().filter(|c| c.starts_with("patch ")).count();
    assert_eq!(patches, 1, "no second patch: {:?}", l.calls());
}

#[test]
fn a_rebuild_that_never_finishes_is_not_an_effect() {
    let l = Longhorn::new("vr-rebuilding");
    let (_, hash) = l.plan("2");
    let o = l.run(&[VOL, "2", &hash], &[("STUB_REBUILD", "never")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("NOT proven"), "{out}");
    assert!(
        out.contains("r-new1 on w-2"),
        "names the replica it waited on: {out}"
    );
}

#[test]
fn a_second_replica_on_the_same_node_is_not_an_effect() {
    let l = Longhorn::new("vr-same-node");
    let (_, hash) = l.plan("2");
    let o = l.run(&[VOL, "2", &hash], &[("STUB_NEW_NODES", "w-1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        effect_lines(&out).is_empty(),
        "two replicas on one node survive nothing a drain does: {out}"
    );
}

#[test]
fn a_patch_the_server_refuses_is_a_failure_with_its_words() {
    let l = Longhorn::new("vr-patch-refused");
    let (_, hash) = l.plan("2");
    let o = l.run(&[VOL, "2", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 1);
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("set-volume-replicas");
    let p = verb("plan-a-volume-replica-change");
    for (name, v) in [
        ("set-volume-replicas", &w),
        ("plan-a-volume-replica-change", &p),
    ] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "volume", "{name}");
        assert_eq!(v["params"][1]["name"], "replicas", "{name}");
    }
    assert_eq!(p["argv"][1], "--plan");
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    let about = w["about"].as_str().unwrap_or_default();
    assert!(
        about.contains("MUTATING") && about.contains("David"),
        "{about}"
    );
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-volume-replica-change");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert!(w["effect"].is_string());
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 1500,
        "the verb's timeout outlasts the script's own read-back"
    );
    let vol = w["params"][0]["pattern"].as_str().expect("a pattern");
    let n = w["params"][1]["pattern"].as_str().expect("a pattern");
    let fits = |pat: &str, s: &str| {
        let o = Command::new("jq")
            .args(["-n", "--arg", "re", pat, "--arg", "l", s, "$l | test($re)"])
            .output()
            .expect("jq runs");
        String::from_utf8_lossy(&o.stdout).trim() == "true"
    };
    assert!(fits(vol, VOL));
    for bad in [
        "boss-dev-work",
        "pvc-x",
        "-pvc",
        "pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091,pvc-dd7b5ac3-884e-485f-8c73-92b87ce77092",
        "pvc-DD7B5AC3-884e-485f-8c73-92b87ce77091",
    ] {
        assert!(!fits(vol, bad), "the volume pattern admits {bad:?}");
    }
    for ok in ["1", "2", "3"] {
        assert!(fits(n, ok), "{ok}");
    }
    for bad in ["0", "4", "10", "-1", "2 ", ""] {
        assert!(!fits(n, bad), "the replicas pattern admits {bad:?}");
    }
}
