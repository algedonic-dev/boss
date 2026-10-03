//! The four ops verbs a worker-node maintenance window runs from the
//! forge (backlog f0aaa72f; design 8457c07b, the w-1 second-NVMe window
//! of 2026-09-30): `cordon-node` and `uncordon-node`
//! (`infra/forge/cordon-node.sh`), `plan-a-node-shutdown` and
//! `shutdown-node` (`infra/forge/shutdown-node.sh`), and the read-only
//! `talos-get` (`infra/forge/talos-get.sh`) and `node-status`
//! (`infra/forge/node-status.sh`, backlog 9fa9f835 — what would hold a
//! drain: the Longhorn volumes whose only healthy replica is on the node,
//! the live `node-drain-policy`, and the disruption budgets at zero).
//!
//! WHY. None existed on origin/main 06a660e3, so the window's cordon and
//! shutdown were two hand commands with the admin credentials — exactly
//! what the ops door exists to replace. Every one of them is BOUNDED by
//! the estate registry: the node must be a `talos-worker` there, and the
//! cluster must agree (no control-plane label, the same InternalIP), or
//! nothing is written. A control plane carries etcd and the system of
//! record; cordoning one is a design decision, never a packet argument.
//!
//! THE EFFECT IS READ BACK. cordon and uncordon read `spec.unschedulable`
//! after the write; shutdown reads Kubernetes' Ready condition AND the
//! Talos API until the node is both NotReady and silent. Each prints the
//! line its verb file declares as `effect` only after that read, and the
//! tests judge the line with the runner's own engine (jq `test`).
//!
//! HOW THIS IS MEASURED. The scripts run for real, with `sudo` stubbed on
//! PATH as a small stateful cluster behind the two doors
//! (`ops_kubectl`'s `docker run … kubectl --kubeconfig=/kc`, and
//! `ops_talosctl`'s `docker run … /talosctl --talosconfig=/tc`), and the
//! estate registry served from a file through `BOSS_ESTATE_NODES_URL`.
//! Every door call lands in `argv`, so a refusal can be shown to have
//! written nothing. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const CORDON: &str = "infra/forge/cordon-node.sh";
const SHUTDOWN: &str = "infra/forge/shutdown-node.sh";
const TALOS_GET: &str = "infra/forge/talos-get.sh";
const NODE_STATUS: &str = "infra/forge/node-status.sh";

/// The stand-in behind both doors. State, one file each under
/// $STUB_STATE: `ip-<node>` its InternalIP, `cp-<node>` a control-plane
/// label, `unsched-<node>` cordoned, `pods-<node>.json` the pods on it,
/// `down-<ip>` powered off, `pdbs.json` every PodDisruptionBudget,
/// `replicas.longhorn.io.json` / `volumes.longhorn.io.json` Longhorn's
/// objects, `setting-<name>` a Longhorn setting's value (absent answers
/// NotFound). Switches: STUB_CORDON=noop (the write answers and changes
/// nothing), STUB_STAYS_UP=1 (a shutdown that is accepted and never
/// happens), STUB_TALOS_FAIL=1 (talosctl cannot reach the node),
/// STUB_PDB_FAIL=1 / STUB_LONGHORN_FAIL=1 (that read cannot look).
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
node_json() {
    n="$1"
    [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
    ip=$(cat "$S/ip-$n")
    labels='"kubernetes.io/hostname":"'"$n"'"'
    [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
    spec='{}'
    [ -f "$S/unsched-$n" ] && spec='{"unschedulable":true}'
    ready=True
    [ -f "$S/down-$ip" ] && ready=Unknown
    printf '{"kind":"Node","metadata":{"name":"%s","uid":"uid-%s","labels":{%s}},"spec":%s,"status":{"addresses":[{"type":"InternalIP","address":"%s"},{"type":"Hostname","address":"%s"}],"conditions":[{"type":"MemoryPressure","status":"False"},{"type":"Ready","status":"%s"}]}}\n' "$n" "$n" "$labels" "$spec" "$ip" "$n" "$ready"
}
if [ "$tool" = kubectl ]; then
    case "$1 $2" in
    "get node")
        case "$*" in
        *"jsonpath={.spec.unschedulable}"*)
            [ -f "$S/ip-$3" ] || { echo "Error from server (NotFound): nodes \"$3\" not found" >&2; exit 1; }
            [ -f "$S/unsched-$3" ] && printf true
            exit 0 ;;
        *) node_json "$3"; exit 0 ;;
        esac ;;
    "get pods")
        n=$(printf '%s' "$*" | sed -n 's/.*spec.nodeName=\([^ ]*\).*/\1/p')
        f="$S/pods-$n.json"
        [ -f "$f" ] || echo '[]' > "$f"
        printf '{"kind":"List","items":%s}\n' "$(cat "$f")"
        exit 0 ;;
    "get pdb")
        [ -n "${STUB_PDB_FAIL:-}" ] && { echo 'error: the server could not find the requested resource (get poddisruptionbudgets.policy)' >&2; exit 1; }
        f="$S/pdbs.json"
        [ -f "$f" ] || echo '[]' > "$f"
        printf '{"kind":"List","items":%s}\n' "$(cat "$f")"
        exit 0 ;;
    "get replicas.longhorn.io" | "get volumes.longhorn.io")
        [ -n "${STUB_LONGHORN_FAIL:-}" ] && { echo "error: the server could not find the requested resource (get $2)" >&2; exit 1; }
        f="$S/$2.json"
        [ -f "$f" ] || echo '[]' > "$f"
        printf '{"kind":"List","items":%s}\n' "$(cat "$f")"
        exit 0 ;;
    "get settings.longhorn.io")
        [ -n "${STUB_LONGHORN_FAIL:-}" ] && { echo "error: the server could not find the requested resource (get $2)" >&2; exit 1; }
        [ -f "$S/setting-$3" ] || { echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1; }
        printf '{"kind":"Setting","metadata":{"name":"%s","namespace":"longhorn-system"},"value":"%s"}\n' "$3" "$(cat "$S/setting-$3")"
        exit 0 ;;
    "cordon "*)
        [ "${STUB_CORDON:-apply}" = apply ] && touch "$S/unsched-$2"
        echo "node/$2 cordoned"; exit 0 ;;
    "uncordon "*)
        [ "${STUB_CORDON:-apply}" = apply ] && rm -f "$S/unsched-$2"
        echo "node/$2 uncordoned"; exit 0 ;;
    esac
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2
fi
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    ip="$2"; shift 2
    if [ -n "${STUB_TALOS_FAIL:-}" ] || [ -f "$S/down-$ip" ]; then
        echo "error: rpc error: code = Unavailable desc = connection error: dial tcp $ip:50000: connect: no route to host" >&2
        exit 1
    fi
    case "$1" in
    shutdown)
        [ -n "${STUB_STAYS_UP:-}" ] || touch "$S/down-$ip"
        exit 0 ;;
    version)
        printf 'Client:\n\tTag: v1.13.8\nServer:\n\tNODE: %s\n\tTag: v1.13.8\n' "$ip"; exit 0 ;;
    get)
        printf 'node: %s\nmetadata:\n    type: %s\n    id: %s\nspec:\n    stub: true\n' "$ip" "$2" "${3:-all}"
        exit 0 ;;
    esac
fi
echo "stub: unexpected call: $*" >&2
exit 2
"#;

struct Estate {
    dir: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "curl", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "node_maintenance_verbs_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

impl Estate {
    /// The estate of 2026-09-29: three control planes and two workers,
    /// each declared in the registry and each a node of the cluster.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let e = Estate { dir };
        e.registry(json!([
            node("forge", "10.20.0.15", "forge", false),
            node("cp-1", "10.20.0.11", "talos-control-plane", false),
            node("cp-2", "10.20.0.12", "talos-control-plane", false),
            node("w-1", "10.20.0.14", "talos-worker", false),
            node("w-2", "10.20.0.16", "talos-worker", false),
        ]));
        for (n, ip) in [
            ("cp-1", "10.20.0.11"),
            ("cp-2", "10.20.0.12"),
            ("w-1", "10.20.0.14"),
            ("w-2", "10.20.0.16"),
        ] {
            e.state(&format!("ip-{n}"), ip);
        }
        e.state("cp-cp-1", "");
        e.state("cp-cp-2", "");
        e
    }

    fn registry(&self, nodes: Value) {
        write_file(
            &self.dir.join("nodes.json"),
            &json!({ "data": nodes }).to_string(),
        );
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn has(&self, file: &str) -> bool {
        self.dir.join("state").join(file).exists()
    }

    fn pods(&self, node: &str, pods: Value) {
        self.state(&format!("pods-{node}.json"), &pods.to_string());
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
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_TALOSCTL", self.dir.join("talosctl"))
            .env("BOSS_SHUTDOWN_WAIT_S", "4")
            .env("BOSS_SHUTDOWN_POLL_S", "1")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// Every door call, with the door's own prefix cut off: `cordon w-1`,
    /// `-n 10.20.0.14 shutdown --wait=false`.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                l.split_once("--kubeconfig=/kc ")
                    .or_else(|| l.split_once("--talosconfig=/tc "))
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .collect()
    }

    fn called(&self, what: &str) -> bool {
        self.calls().iter().any(|c| c.contains(what))
    }
}

fn node(id: &str, address: &str, role: &str, retired: bool) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": retired
    })
}

fn pod(ns: &str, name: &str, owner: &str, phase: &str) -> Value {
    let mut p = json!({
        "metadata": {"namespace": ns, "name": name, "uid": format!("uid-{name}")},
        "spec": {"nodeName": "w-1"},
        "status": {"phase": phase}
    });
    if owner == "mirror" {
        p["metadata"]["annotations"] = json!({"kubernetes.io/config.mirror": "abc"});
    } else if let Some((kind, oname)) = owner.split_once('/') {
        p["metadata"]["ownerReferences"] = json!([{"kind": kind, "name": oname}]);
    }
    p
}

fn labelled(mut p: Value, labels: Value) -> Value {
    p["metadata"]["labels"] = labels;
    p
}

/// A PodDisruptionBudget as the API states it: the selector as written,
/// and the controller's `status.disruptionsAllowed`.
fn pdb(ns: &str, name: &str, allowed: u32, selector: Value) -> Value {
    json!({
        "metadata": {"namespace": ns, "name": name},
        "spec": {"selector": selector},
        "status": {"disruptionsAllowed": allowed}
    })
}

/// A Longhorn replica. Healthy is Longhorn's own test: `spec.healthyAt`
/// set and `spec.failedAt` empty.
fn replica(name: &str, volume: &str, node: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "metadata": {"namespace": "longhorn-system", "name": name},
        "spec": {
            "volumeName": volume, "nodeID": node,
            "healthyAt": if healthy { "2026-09-29T10:00:00Z" } else { "" },
            "failedAt": if failed { "2026-09-29T11:00:00Z" } else { "" }
        },
        "status": {"currentState": if failed { "error" } else { "running" }}
    })
}

fn lh_volume(name: &str, state: &str, pvc: Option<(&str, &str)>) -> Value {
    let mut v = json!({
        "metadata": {"namespace": "longhorn-system", "name": name},
        "status": {"state": state, "robustness": "healthy"}
    });
    if let Some((ns, claim)) = pvc {
        v["status"]["kubernetesStatus"] = json!({"namespace": ns, "pvcName": claim});
    }
    v
}

/// w-1 as the morning of 2026-09-30 is expected to find it (review
/// bc33ff50 M1): the dev /work volume's one replica is on w-1, the system
/// of record's volume is replicated elsewhere too, and Longhorn's
/// instance-manager pod on w-1 is held by a budget at zero.
fn a_node_a_drain_would_wait_on(name: &str) -> Estate {
    let e = Estate::new(name);
    e.pods(
        "w-1",
        json!([
            labelled(
                pod(
                    "longhorn-system",
                    "instance-manager-aaaa",
                    "InstanceManager/instance-manager-aaaa",
                    "Running"
                ),
                json!({"longhorn.io/component": "instance-manager", "longhorn.io/node": "w-1"})
            ),
            labelled(
                pod(
                    "boss-dev",
                    "boss-dev-7c9f-abcde",
                    "ReplicaSet/boss-dev-7c9f",
                    "Running"
                ),
                json!({"app": "boss-dev"})
            ),
            labelled(
                pod(
                    "longhorn-system",
                    "longhorn-manager-x1",
                    "DaemonSet/longhorn-manager",
                    "Running"
                ),
                json!({"app": "longhorn-manager"})
            ),
            labelled(
                pod("boss-dev", "gate-run-9-q", "Job/gate-run-9", "Succeeded"),
                json!({"app": "gate"})
            ),
        ]),
    );
    e.state(
        "pdbs.json",
        &json!([
            // Holds the drain: at zero, selects the instance manager on w-1.
            pdb(
                "longhorn-system",
                "instance-manager-aaaa",
                0,
                json!({"matchLabels": {"longhorn.io/component": "instance-manager", "longhorn.io/node": "w-1"}})
            ),
            // At zero, by an expression the dev pod's labels satisfy.
            pdb(
                "boss-dev",
                "boss-dev",
                0,
                json!({"matchExpressions": [{"key": "app", "operator": "In", "values": ["boss-dev", "other"]}]})
            ),
            // Allows one: not a hold.
            pdb("boss-dev", "roomy", 1, json!({"matchLabels": {"app": "boss-dev"}})),
            // At zero, the right labels, the wrong namespace.
            pdb("boss", "elsewhere", 0, json!({"matchLabels": {"app": "boss-dev"}})),
            // At zero, but it selects only a DaemonSet's pod, which a drain leaves.
            pdb("longhorn-system", "manager", 0, json!({"matchLabels": {"app": "longhorn-manager"}})),
            // At zero, but it selects only a finished pod.
            pdb("boss-dev", "gates", 0, json!({"matchLabels": {"app": "gate"}})),
            // At zero, excluded by NotIn.
            pdb(
                "boss-dev",
                "not-dev",
                0,
                json!({"matchExpressions": [{"key": "app", "operator": "NotIn", "values": ["boss-dev"]}]})
            ),
        ])
        .to_string(),
    );
    e.state(
        "replicas.longhorn.io.json",
        &json!([
            // The dev /work volume: one replica, on w-1.
            replica("pvc-work-r-1", "pvc-work", "w-1", true, false),
            // The system of record's volume: healthy on w-1 AND w-2.
            replica("pvc-sor-r-1", "pvc-sor", "w-1", true, false),
            replica("pvc-sor-r-2", "pvc-sor", "w-2", true, false),
            // Healthy on w-1; its other replica failed: w-1 holds the last one.
            replica("pvc-logs-r-1", "pvc-logs", "w-1", true, false),
            replica("pvc-logs-r-2", "pvc-logs", "w-2", true, true),
            // Failed on w-1, healthy on w-2: w-1 holds nothing that matters.
            replica("pvc-cache-r-1", "pvc-cache", "w-1", true, true),
            replica("pvc-cache-r-2", "pvc-cache", "w-2", true, false),
            // Rebuilding on w-1 (never healthy yet), healthy on cp-1.
            replica("pvc-new-r-1", "pvc-new", "w-1", false, false),
            replica("pvc-new-r-2", "pvc-new", "cp-1", true, false),
        ])
        .to_string(),
    );
    e.state(
        "volumes.longhorn.io.json",
        &json!([
            lh_volume("pvc-work", "attached", Some(("boss-dev", "work"))),
            lh_volume("pvc-sor", "attached", Some(("boss", "postgres-data"))),
            lh_volume("pvc-logs", "detached", None),
        ])
        .to_string(),
    );
    e.state(
        "setting-node-drain-policy",
        "block-if-contains-last-replica",
    );
    e
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

/// The lines of `out` the verb's declared `effect` matches, judged by the
/// runner's own engine (jq's `test`).
fn effect_lines(name: &str, out: &str) -> Vec<String> {
    let re = verb(name)["effect"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} declares an `effect`"))
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

// ---------------------------------------------------------------- cordon

#[test]
fn cordon_node_cordons_a_worker_and_reads_spec_unschedulable_back() {
    let e = Estate::new("cordon-worker");
    let o = e.run(CORDON, &["cordon", "w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(e.called("cordon w-1"), "{:?}", e.calls());
    assert!(e.has("unsched-w-1"), "the stub cluster holds the cordon");
    let hits = effect_lines("cordon-node", &out);
    assert_eq!(hits.len(), 1, "exactly one effect line: {out}");
    assert!(hits[0].contains("w-1"), "{hits:?}");
    // The read-back is a read made AFTER the write, not the write's answer.
    let calls = e.calls();
    let write = calls.iter().position(|c| c.starts_with("cordon w-1"));
    let read = calls
        .iter()
        .rposition(|c| c.contains("jsonpath={.spec.unschedulable}"));
    assert!(
        matches!((write, read), (Some(w), Some(r)) if r > w),
        "spec.unschedulable is read after the cordon: {calls:?}"
    );
}

#[test]
fn a_cordon_that_changed_nothing_is_not_an_effect() {
    let e = Estate::new("cordon-noop");
    let o = e.run(CORDON, &["cordon", "w-1"], &[("STUB_CORDON", "noop")]);
    let out = text(&o);
    assert!(
        !o.status.success(),
        "a cordon that did not hold exits nonzero: {out}"
    );
    assert!(effect_lines("cordon-node", &out).is_empty(), "{out}");
    assert!(out.contains("FAILED"), "{out}");
}

#[test]
fn cordon_node_refuses_a_control_plane_by_the_estate_registry() {
    let e = Estate::new("cordon-cp");
    let o = e.run(CORDON, &["cordon", "cp-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "a refusal is exit 78: {out}");
    assert!(
        out.contains("talos-control-plane"),
        "names the role it read: {out}"
    );
    assert!(
        !e.called("cordon"),
        "nothing reached the cluster's write: {:?}",
        e.calls()
    );
    assert!(effect_lines("cordon-node", &out).is_empty(), "{out}");
}

#[test]
fn cordon_node_refuses_when_the_cluster_calls_the_worker_a_control_plane() {
    // The registry and the cluster must AGREE the node is a worker: a
    // registry row that drifted (or was edited) cannot aim the verb at a
    // node that carries the control-plane label.
    let e = Estate::new("cordon-label");
    e.state("cp-w-1", "");
    let o = e.run(CORDON, &["cordon", "w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("node-role.kubernetes.io/control-plane"),
        "{out}"
    );
    assert!(!e.called("cordon w-1"), "{:?}", e.calls());
}

#[test]
fn cordon_node_refuses_when_the_registry_and_the_cluster_disagree_on_the_address() {
    let e = Estate::new("cordon-ip");
    e.state("ip-w-1", "10.20.0.99");
    let o = e.run(CORDON, &["cordon", "w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("10.20.0.99") && out.contains("10.20.0.14"),
        "names both: {out}"
    );
    assert!(!e.called("cordon w-1"), "{:?}", e.calls());
}

#[test]
fn cordon_node_refuses_a_node_the_registry_does_not_hold_or_has_retired() {
    let e = Estate::new("cordon-unknown");
    let o = e.run(CORDON, &["cordon", "w-9"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    e.registry(json!([node("w-1", "10.20.0.14", "talos-worker", true)]));
    let o = e.run(CORDON, &["cordon", "w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("retired"), "{out}");
    assert!(!e.called("cordon"), "{:?}", e.calls());
}

#[test]
fn cordon_node_cannot_answer_without_the_registry_and_writes_nothing() {
    let e = Estate::new("cordon-dark");
    std::fs::remove_file(e.dir.join("nodes.json")).expect("rm");
    let o = e.run(CORDON, &["cordon", "w-1"], &[]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a registry that did not answer is a failure, not a refusal: {out}"
    );
    assert!(out.contains("estate registry"), "{out}");
    assert!(e.calls().is_empty(), "no door was opened: {:?}", e.calls());
}

#[test]
fn cordon_node_refuses_a_malformed_node_or_action_before_anything_is_read() {
    let e = Estate::new("cordon-args");
    for args in [
        vec!["cordon", "-w-1"],
        vec!["cordon", "W-1"],
        vec!["cordon", "w-1", "extra"],
        vec!["drain", "w-1"],
        vec!["cordon"],
    ] {
        let o = e.run(CORDON, &args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(e.calls().is_empty(), "{:?}", e.calls());
}

#[test]
fn uncordon_node_makes_a_worker_schedulable_and_reads_it_back() {
    let e = Estate::new("uncordon-worker");
    e.state("unsched-w-1", "");
    let o = e.run(CORDON, &["uncordon", "w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(e.called("uncordon w-1"), "{:?}", e.calls());
    assert!(!e.has("unsched-w-1"));
    assert_eq!(effect_lines("uncordon-node", &out).len(), 1, "{out}");
    assert!(
        effect_lines("cordon-node", &out).is_empty(),
        "an uncordon never reads as a cordon: {out}"
    );
}

#[test]
fn uncordon_node_refuses_a_control_plane_and_a_noop_is_not_an_effect() {
    let e = Estate::new("uncordon-cp");
    e.state("unsched-cp-2", "");
    let o = e.run(CORDON, &["uncordon", "cp-2"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(!e.called("uncordon"), "{:?}", e.calls());

    e.state("unsched-w-1", "");
    let o = e.run(CORDON, &["uncordon", "w-1"], &[("STUB_CORDON", "noop")]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(effect_lines("uncordon-node", &out).is_empty(), "{out}");
}

// -------------------------------------------------------------- shutdown

fn cordoned_worker(name: &str) -> Estate {
    let e = Estate::new(name);
    e.state("unsched-w-1", "");
    e.pods(
        "w-1",
        json!([
            pod(
                "boss-dev",
                "boss-dev-7c9f-abcde",
                "ReplicaSet/boss-dev-7c9f",
                "Running"
            ),
            pod(
                "longhorn-system",
                "longhorn-manager-x1",
                "DaemonSet/longhorn-manager",
                "Running"
            ),
            pod("kube-system", "kube-proxy-w-1", "mirror", "Running"),
            pod(
                "boss-dev",
                "gate-run-1234-q",
                "Job/gate-run-1234",
                "Succeeded"
            ),
        ]),
    );
    e
}

fn plan_of(e: &Estate) -> (String, String) {
    let o = e.run(SHUTDOWN, &["--plan", "w-1"], &[]);
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

#[test]
fn the_shutdown_plan_names_the_node_its_address_and_the_pods_it_will_evict() {
    let e = cordoned_worker("shutdown-plan");
    let (plan, hash) = plan_of(&e);
    for want in [
        "node: w-1",
        "address: 10.20.0.14",
        "estate role: talos-worker",
        "boss-dev/boss-dev-7c9f-abcde",
        "talosctl -n 10.20.0.14 shutdown",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    let evict = plan
        .split("== pods that stay")
        .next()
        .expect("an eviction section");
    assert!(evict.contains("boss-dev/boss-dev-7c9f-abcde"), "{plan}");
    assert!(
        !evict.contains("longhorn-manager-x1") && !evict.contains("kube-proxy-w-1"),
        "a DaemonSet or static pod is not evicted by a drain: {plan}"
    );
    assert!(
        !plan.contains("gate-run-1234-q"),
        "a finished pod runs nothing and is not in the signed bytes: {plan}"
    );
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(
        !e.called("shutdown"),
        "the plan shuts nothing down: {:?}",
        e.calls()
    );
    assert!(effect_lines("shutdown-node", &plan).is_empty(), "{plan}");
    // Deterministic: a second render of one true state is byte-identical.
    let (again, hash2) = plan_of(&e);
    assert_eq!((plan, hash), (again, hash2));
}

#[test]
fn the_shutdown_plan_names_the_budgets_that_would_hold_the_drain() {
    // Review bc33ff50 M1: a budget at zero holds Talos's drain, and the
    // write then reads NOT proven down after 720 s. The passkey signs a
    // plan that says so beforehand.
    let e = a_node_a_drain_would_wait_on("shutdown-plan-budgets");
    e.state("unsched-w-1", "");
    let (plan, _) = plan_of(&e);
    let budgets = plan
        .split("== disruption budgets")
        .nth(1)
        .unwrap_or_else(|| panic!("a budgets section:\n{plan}"));
    assert!(budgets.contains("(2) =="), "{plan}");
    assert!(
        budgets.contains(
            "pdb longhorn-system/instance-manager-aaaa (disruptionsAllowed 0) selects longhorn-system/instance-manager-aaaa"
        ),
        "{plan}"
    );
    assert!(!budgets.contains("roomy"), "{plan}");

    // A budget read that cannot look renders no plan.
    let o = e.run(SHUTDOWN, &["--plan", "w-1"], &[("STUB_PDB_FAIL", "1")]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(o.stdout.is_empty(), "{}", text(&o));
}

#[test]
fn the_shutdown_plan_refuses_a_control_plane_and_a_node_not_cordoned() {
    let e = cordoned_worker("shutdown-plan-refusals");
    e.state("unsched-cp-1", "");
    let o = e.run(SHUTDOWN, &["--plan", "cp-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("talos-control-plane"), "{out}");

    let o = e.run(SHUTDOWN, &["--plan", "w-2"], &[]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(78),
        "an uncordoned worker is refused: {out}"
    );
    assert!(
        out.contains("cordon-node"),
        "names the verb that comes first: {out}"
    );
    assert!(!e.called("shutdown"), "{:?}", e.calls());
}

#[test]
fn shutdown_node_runs_only_the_signed_plan_and_reads_the_node_down() {
    let e = cordoned_worker("shutdown-write");
    let (plan, hash) = plan_of(&e);
    let o = e.run(SHUTDOWN, &["w-1", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let calls = e.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("-n 10.20.0.14 shutdown") && !c.contains("--force")),
        "a graceful Talos shutdown of the registry's address, never --force: {calls:?}"
    );
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );
    let hits = effect_lines("shutdown-node", &out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains("w-1"), "{hits:?}");
}

#[test]
fn shutdown_node_refuses_a_hash_that_is_not_todays_plan() {
    let e = cordoned_worker("shutdown-drift");
    let (_, hash) = plan_of(&e);
    // A new eviction target appeared after the plan was signed: the
    // passkey never saw it, so the signature does not cover it.
    e.pods(
        "w-1",
        json!([
            pod(
                "boss-dev",
                "boss-dev-7c9f-abcde",
                "ReplicaSet/boss-dev-7c9f",
                "Running"
            ),
            pod("boss", "boss-6d-zzz", "ReplicaSet/boss-6d", "Running"),
        ]),
    );
    let o = e.run(SHUTDOWN, &["w-1", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    assert!(!e.called("shutdown"), "{:?}", e.calls());

    let o = e.run(SHUTDOWN, &["w-1", &"0".repeat(64)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    let o = e.run(SHUTDOWN, &["w-1", "not-a-hash"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(!e.called("shutdown"), "{:?}", e.calls());
}

#[test]
fn shutdown_node_refuses_a_control_plane_even_with_a_hash() {
    let e = cordoned_worker("shutdown-cp");
    e.state("unsched-cp-1", "");
    let o = e.run(SHUTDOWN, &["cp-1", &"a".repeat(64)], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(!e.called("shutdown"), "{:?}", e.calls());
}

#[test]
fn a_second_run_of_an_applied_plan_is_refused() {
    let e = cordoned_worker("shutdown-twice");
    let (_, hash) = plan_of(&e);
    let o = e.run(SHUTDOWN, &["w-1", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let before = e.calls().len();
    let o = e.run(SHUTDOWN, &["w-1", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(
        !e.calls()[before..].iter().any(|c| c.contains(" shutdown")),
        "no second shutdown: {:?}",
        e.calls()
    );
}

#[test]
fn a_shutdown_the_node_never_took_is_not_an_effect() {
    let e = cordoned_worker("shutdown-stays-up");
    let (_, hash) = plan_of(&e);
    let o = e.run(SHUTDOWN, &["w-1", &hash], &[("STUB_STAYS_UP", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines("shutdown-node", &out).is_empty(), "{out}");
    assert!(out.contains("NOT proven down"), "{out}");
}

// -------------------------------------------------------------- talos-get

#[test]
fn talos_get_reads_a_fixed_resource_of_a_registry_node() {
    let e = Estate::new("talos-get");
    let o = e.run(TALOS_GET, &["w-1", "machinestatus"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        e.calls()
            .iter()
            .any(|c| c == "-n 10.20.0.14 get machinestatus -o yaml"),
        "{:?}",
        e.calls()
    );
    assert!(
        out.contains("type: machinestatus"),
        "talosctl's answer is printed: {out}"
    );

    // Read-only, so a control plane may be read.
    let o = e.run(TALOS_GET, &["cp-1", "disks"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = e.run(TALOS_GET, &["w-1", "volumestatus", "u-gate"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        e.called("-n 10.20.0.14 get volumestatus u-gate -o yaml"),
        "{:?}",
        e.calls()
    );
}

#[test]
fn talos_get_refuses_a_resource_outside_its_list_and_a_node_outside_the_cluster() {
    let e = Estate::new("talos-get-refusals");
    for args in [
        vec!["w-1", "machineconfig"],
        vec!["w-1", "secrets"],
        vec!["w-1", "disks", "-o"],
        vec!["forge", "disks"],
        vec!["w-9", "disks"],
    ] {
        let o = e.run(TALOS_GET, &args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(e.calls().is_empty(), "talosctl never ran: {:?}", e.calls());
}

#[test]
fn talos_get_says_what_talosctl_said_when_it_cannot_read() {
    let e = Estate::new("talos-get-dark");
    let o = e.run(TALOS_GET, &["w-1", "disks"], &[("STUB_TALOS_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("no route to host"), "{out}");
}

// ------------------------------------------------------------ node-status

#[test]
fn node_status_names_what_would_hold_a_drain_of_the_node() {
    let e = a_node_a_drain_would_wait_on("node-status");
    e.state("unsched-w-1", "");
    let o = e.run(NODE_STATUS, &["w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    for want in [
        "node: w-1",
        "estate role: talos-worker",
        "cordoned: yes (spec.unschedulable=true)",
        "condition Ready: True",
        "condition MemoryPressure: False",
        "node-drain-policy: block-if-contains-last-replica",
        "volume pvc-logs (detached)",
        "volume pvc-work (pvc boss-dev/work, attached)",
        "pdb boss-dev/boss-dev (disruptionsAllowed 0) selects boss-dev/boss-dev-7c9f-abcde",
        "pdb longhorn-system/instance-manager-aaaa (disruptionsAllowed 0) selects longhorn-system/instance-manager-aaaa",
    ] {
        assert!(
            stdout.contains(want),
            "node-status says {want:?}:\n{stdout}"
        );
    }
    let (vols, budgets) = {
        let after = stdout
            .split("== longhorn volumes whose only healthy replica is on w-1")
            .nth(1)
            .expect("a volumes section");
        let mut halves = after.split("== disruption budgets");
        (
            halves.next().unwrap_or_default().to_string(),
            halves.next().expect("a budgets section").to_string(),
        )
    };
    assert!(vols.starts_with(" (2) =="), "two volumes: {vols}");
    for not in ["pvc-sor", "pvc-cache", "pvc-new"] {
        assert!(
            !vols.contains(not),
            "{not} has a healthy replica on another node: {vols}"
        );
    }
    assert!(budgets.contains("(2) =="), "two budgets: {budgets}");
    for not in ["roomy", "elsewhere", "/manager", "gates", "not-dev"] {
        assert!(
            !budgets.contains(not),
            "{not} does not hold the drain: {budgets}"
        );
    }
    // Sorted, so two reads of one state are byte-identical.
    let lines: Vec<&str> = budgets.lines().filter(|l| l.starts_with("pdb ")).collect();
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted);
    // READ-ONLY: every door call is a get.
    let calls = e.calls();
    assert!(
        calls.iter().all(|c| c.starts_with("get ")),
        "node-status only reads: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("get settings.longhorn.io node-drain-policy")),
        "the LIVE setting is read, never assumed: {calls:?}"
    );
}

#[test]
fn node_status_reads_a_node_with_nothing_to_hold_its_drain_and_says_none() {
    let e = Estate::new("node-status-clear");
    e.state("setting-node-drain-policy", "always-allow");
    let o = e.run(NODE_STATUS, &["w-2"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("cordoned: no"), "{out}");
    assert!(out.contains("node-drain-policy: always-allow"), "{out}");
    assert!(
        out.contains("on w-2 (0) ==\nnone"),
        "an empty section says none: {out}"
    );
}

#[test]
fn node_status_reads_a_control_plane_and_refuses_a_node_outside_the_cluster() {
    let e = a_node_a_drain_would_wait_on("node-status-cp");
    let o = e.run(NODE_STATUS, &["cp-1"], &[]);
    let out = text(&o);
    assert!(
        o.status.success(),
        "a read may look at a control plane: {out}"
    );
    assert!(out.contains("estate role: talos-control-plane"), "{out}");
    for args in [
        vec!["forge"],
        vec!["w-9"],
        vec!["W-1"],
        vec!["w-1", "extra"],
        vec![],
    ] {
        let o = e.run(NODE_STATUS, &args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
}

#[test]
fn node_status_that_cannot_read_a_section_is_not_a_clean_status() {
    // No evidence is not a pass: a read that could not look must not
    // render as a node with nothing on it.
    for (switch, what) in [
        ("STUB_LONGHORN_FAIL", "replicas.longhorn.io"),
        ("STUB_PDB_FAIL", "poddisruptionbudgets"),
    ] {
        let e = a_node_a_drain_would_wait_on(&format!("node-status-{switch}"));
        let o = e.run(NODE_STATUS, &["w-1"], &[(switch, "1")]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{switch}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{switch}: {out}");
        assert!(out.contains(what), "names what could not be read: {out}");
        assert!(
            out.contains("condition Ready: True"),
            "the sections that could be read are still printed: {out}"
        );
    }
}

#[test]
fn node_status_says_so_when_the_drain_policy_setting_does_not_exist() {
    let e = a_node_a_drain_would_wait_on("node-status-no-setting");
    std::fs::remove_file(e.dir.join("state/setting-node-drain-policy")).expect("rm");
    let o = e.run(NODE_STATUS, &["w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("node-drain-policy: NOT FOUND"),
        "an absent setting is named, never read as the default: {out}"
    );
}

// ----------------------------------------------------------------- verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    for (name, script) in [
        ("cordon-node", CORDON),
        ("uncordon-node", CORDON),
        ("shutdown-node", SHUTDOWN),
        ("plan-a-node-shutdown", SHUTDOWN),
        ("talos-get", TALOS_GET),
        ("node-status", NODE_STATUS),
    ] {
        let v = verb(name);
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], script, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        assert!(
            v["params"][0].get("default").is_none(),
            "{name} names its node"
        );
    }
    for name in ["cordon-node", "uncordon-node", "shutdown-node"] {
        let v = verb(name);
        assert!(
            v["about"].as_str().unwrap_or_default().contains("MUTATING"),
            "{name}"
        );
        assert!(v["effect"].is_string(), "{name} declares its effect");
    }
    assert_eq!(verb("cordon-node")["argv"][1], "cordon");
    assert_eq!(verb("uncordon-node")["argv"][1], "uncordon");
    let s = verb("shutdown-node");
    assert_eq!(s["requires_approval"], true);
    assert_eq!(s["plan_verb"], "plan-a-node-shutdown");
    assert_eq!(s["approvers"], json!(["emp-david"]));
    let p = verb("plan-a-node-shutdown");
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_eq!(p["argv"][1], "--plan");
    let n = verb("node-status");
    assert!(!n["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert!(n["effect"].is_null(), "a read declares no effect");
    assert_eq!(
        n["argv"],
        json!([NODE_STATUS, "{1}"]),
        "the node, nothing else"
    );
    let t = verb("talos-get");
    assert!(!t["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_eq!(
        t["params"][1]["one_of"],
        json!(["machinestatus", "disks", "volumestatus"]),
        "a fixed list: never machineconfig, whose bytes carry the cluster's keys"
    );
}
