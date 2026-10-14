//! The read-only ops verb `node-disk-usage` (`infra/forge/node-disk-usage.sh`,
//! backlog ebcddea5): what occupies ONE cluster node's disk, named.
//!
//! WHY IT EXISTS. On 2026-10-09 the estate alarm `disk_tight:w-2` fired for
//! the second time in three days: w-2's /var fell from 73 GiB free to 31 GiB
//! in 59 hours, against a 39 GiB floor. The measure step (agent-run
//! 34723794) could read THAT it was filling — the talos-nodefs series, the
//! Longhorn disk sums — and not WHAT was filling it, because no door names
//! what occupies a node's disk: `node-status` lists only volumes whose ONLY
//! replica is on the node, `plan-a-node-shutdown` refuses an uncordoned
//! node, the replica plan verbs need a volume name already known, and
//! `disk-report` serves the forge and boss-gcp, not cluster nodes. Both
//! alarms on w-2 were answered by moving a replica without knowing the
//! grower. This verb is that read: the kubelet's own filesystem figures,
//! every pod's ephemeral storage, the node's largest images, and every
//! Longhorn replica on the node with its volume, claim and actual size.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small cluster behind the forge's `ops_kubectl` door, and the
//! estate registry served from a file through `BOSS_ESTATE_NODES_URL` —
//! the harness `node_maintenance_verbs_sh.rs` uses for `node-status`. Every
//! door call lands in `argv`, so the read can be shown to have written
//! nothing and a refusal to have opened no door. Nothing here reaches a
//! cluster.

use boss_testing::test_values::test_net_1;
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/node-disk-usage.sh";
const GIB: u64 = 1_073_741_824;

/// The stand-in behind the door. State, one file each under $STUB_STATE:
/// `node-<n>.json` the Node (absent answers NotFound), `summary-<n>.json`
/// the kubelet's /stats/summary, `lhnode-<n>.json` the Longhorn node
/// (absent answers NotFound), `replicas.longhorn.io.json` and
/// `volumes.longhorn.io.json` Longhorn's lists. Switches:
/// STUB_SUMMARY_FAIL=1 (the summary read cannot look), STUB_SUMMARY_EMPTY=1
/// (it answers exit 0 and no bytes), STUB_LONGHORN_FAIL=1 (every Longhorn
/// read cannot look).
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
lh_fail() { [ -n "${STUB_LONGHORN_FAIL:-}" ] && { echo "error: the server could not find the requested resource (get $1)" >&2; exit 1; }; }
case "$1 $2" in
"get node")
    [ -f "$S/node-$3.json" ] || { echo "Error from server (NotFound): nodes \"$3\" not found" >&2; exit 1; }
    cat "$S/node-$3.json"; exit 0 ;;
"get --raw")
    n=$(printf '%s' "$3" | sed -n 's#^/api/v1/nodes/\([^/]*\)/proxy/stats/summary$#\1#p')
    [ -n "$n" ] || { echo "stub: unexpected raw path: $3" >&2; exit 2; }
    [ -n "${STUB_SUMMARY_FAIL:-}" ] && { echo 'Error from server (ServiceUnavailable): the server is currently unable to handle the request' >&2; exit 1; }
    [ -n "${STUB_SUMMARY_EMPTY:-}" ] && exit 0
    cat "$S/summary-$n.json"; exit 0 ;;
"get replicas.longhorn.io" | "get volumes.longhorn.io")
    lh_fail "$2"
    f="$S/$2.json"
    [ -f "$f" ] || echo '[]' > "$f"
    printf '{"kind":"List","items":%s}\n' "$(cat "$f")"
    exit 0 ;;
"get nodes.longhorn.io")
    lh_fail "$2"
    [ -f "$S/lhnode-$3.json" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$S/lhnode-$3.json"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

struct Estate {
    dir: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "curl"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "node_disk_usage_sh: no {tool} on this box — the gate image has it, and a test \
             that cannot run must fail, never pass by returning early"
        );
    }
}

fn reg_node(id: &str, address: &str, role: &str, retired: bool) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 110, "notes": null, "retired": retired
    })
}

impl Estate {
    /// w-2 as the alarm of 2026-10-09 found it: 110 GiB of /var, 31 free,
    /// a gate pod with a large emptyDir, two Longhorn replicas on the node
    /// (one of a claimed volume, one failed and unclaimed) and one replica
    /// elsewhere that must not be counted.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let e = Estate { dir };
        write_file(
            &e.dir.join("nodes.json"),
            &json!({"data": [
                reg_node("forge", &test_net_1(40).to_string(), "forge", false),
                reg_node("cp-1", &test_net_1(11).to_string(), "talos-control-plane", false),
                reg_node("w-1", &test_net_1(14).to_string(), "talos-worker", false),
                reg_node("w-2", &test_net_1(16).to_string(), "talos-worker", false),
                reg_node("w-3", &test_net_1(17).to_string(), "talos-worker", true),
            ]})
            .to_string(),
        );
        for (n, ip) in [
            ("cp-1", test_net_1(11)),
            ("w-1", test_net_1(14)),
            ("w-2", test_net_1(16)),
        ] {
            e.state(
                &format!("node-{n}.json"),
                &json!({
                    "kind": "Node",
                    "metadata": {"name": n},
                    "status": {
                        "addresses": [{"type": "InternalIP", "address": ip.to_string()}],
                        "images": [
                            {"names": ["reg.test/boss@sha256:aaaa", "reg.test/boss:v1"], "sizeBytes": 3 * GIB},
                            {"names": ["reg.test/gate-runner:v9"], "sizeBytes": 5 * GIB},
                            {"names": ["reg.test/pause@sha256:bbbb"], "sizeBytes": 1_000_000}
                        ]
                    }
                })
                .to_string(),
            );
            e.state(
                &format!("summary-{n}.json"),
                &json!({
                    "node": {
                        "nodeName": n,
                        "fs": {"capacityBytes": 110 * GIB, "usedBytes": 79 * GIB, "availableBytes": 31 * GIB},
                        "runtime": {"imageFs": {"capacityBytes": 110 * GIB, "usedBytes": 20 * GIB, "availableBytes": 31 * GIB}}
                    },
                    "pods": [
                        {
                            "podRef": {"namespace": "kube-system", "name": "kube-proxy-x"},
                            "containers": [{"name": "kube-proxy", "rootfs": {"usedBytes": GIB / 10}, "logs": {"usedBytes": 0}}],
                            "volume": [],
                            "ephemeral-storage": {"usedBytes": GIB / 10}
                        },
                        {
                            "podRef": {"namespace": "boss-gates", "name": "gate-abc"},
                            "containers": [{"name": "gate", "rootfs": {"usedBytes": GIB / 2}, "logs": {"usedBytes": 2 * GIB}}],
                            "volume": [
                                {"name": "scratch", "usedBytes": 10 * GIB},
                                {"name": "work", "usedBytes": 7 * GIB, "pvcRef": {"namespace": "boss-gates", "name": "work"}}
                            ],
                            "ephemeral-storage": {"usedBytes": 12 * GIB + GIB / 2}
                        },
                        {
                            "podRef": {"namespace": "boss", "name": "sim-1"},
                            "containers": [{"name": "sim", "rootfs": {"usedBytes": GIB}, "logs": {"usedBytes": GIB}}],
                            "volume": [],
                            "ephemeral-storage": {"usedBytes": 2 * GIB}
                        }
                    ]
                })
                .to_string(),
            );
        }
        e.state(
            "replicas.longhorn.io.json",
            &json!([
                replica("pvc-a-r-1", "pvc-a", "w-2", true, false),
                replica("pvc-b-r-2", "pvc-b", "w-2", false, true),
                replica("pvc-a-r-3", "pvc-a", "w-1", true, false),
            ])
            .to_string(),
        );
        e.state(
            "volumes.longhorn.io.json",
            &json!([
                lh_volume(
                    "pvc-a",
                    30 * GIB,
                    21 * GIB,
                    Some(("boss", "pgdata-postgres-0"))
                ),
                lh_volume("pvc-b", 10 * GIB, 2 * GIB, None),
            ])
            .to_string(),
        );
        e.state(
            "lhnode-w-2.json",
            &json!({
                "metadata": {"name": "w-2", "namespace": "longhorn-system"},
                "spec": {"disks": {"default-disk-0804": {"path": "/var/lib/longhorn/", "storageReserved": 33 * GIB}}},
                "status": {"diskStatus": {"default-disk-0804": {
                    "storageMaximum": 110 * GIB, "storageScheduled": 40 * GIB, "storageAvailable": 31 * GIB
                }}}
            })
            .to_string(),
        );
        e
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
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
            .env("BOSS_MACHINE_TOKEN_DIR", self.dir.join("no-machine-token"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// Every door call, with the door's own prefix cut off.
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
}

fn replica(name: &str, volume: &str, node: &str, healthy: bool, failed: bool) -> Value {
    json!({
        "metadata": {"namespace": "longhorn-system", "name": name},
        "spec": {
            "volumeName": volume, "nodeID": node, "diskID": "disk-uuid-1",
            "healthyAt": if healthy { "2026-10-07T10:00:00Z" } else { "" },
            "failedAt": if failed { "2026-10-08T11:00:00Z" } else { "" }
        }
    })
}

fn lh_volume(name: &str, size: u64, actual: u64, pvc: Option<(&str, &str)>) -> Value {
    let mut v = json!({
        "metadata": {"namespace": "longhorn-system", "name": name},
        "spec": {"size": size.to_string()},
        "status": {"state": "attached", "actualSize": actual}
    });
    if let Some((ns, claim)) = pvc {
        v["status"]["kubernetesStatus"] = json!({"namespace": ns, "pvcName": claim});
    }
    v
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The lines of one `== <title> ... ==` section, up to the next blank line.
fn section(stdout: &str, title: &str) -> Vec<String> {
    stdout
        .split("\n\n")
        .find(|block| block.starts_with(&format!("== {title}")))
        .unwrap_or_else(|| panic!("a {title:?} section:\n{stdout}"))
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

#[test]
fn the_synthetic_estate_uses_reserved_addresses() {
    let estate = Estate::new("node-disk-reserved-addresses");
    let rows: Value =
        serde_json::from_str(&std::fs::read_to_string(estate.dir.join("nodes.json")).unwrap())
            .unwrap();
    for row in rows["data"].as_array().unwrap() {
        let address: std::net::Ipv4Addr = row["address"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            &address.octets()[..3],
            &[192, 0, 2],
            "synthetic node {} must not carry this instance's LAN address",
            row["id"]
        );
    }
}

#[test]
fn node_disk_usage_names_what_occupies_a_nodes_disk() {
    let e = Estate::new("node-disk-usage");
    let o = e.run(&["w-2"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    for want in [
        "node: w-2",
        "estate role: talos-worker",
        "nodefs: capacity 110.0 GiB, used 79.0 GiB, available 31.0 GiB",
        "imagefs: capacity 110.0 GiB, used 20.0 GiB, available 31.0 GiB",
        "longhorn disk default-disk-0804 (/var/lib/longhorn/): maximum 110.0 GiB, reserved 33.0 GiB, scheduled 40.0 GiB, available 31.0 GiB",
        "node-disk-usage: w-2 — nodefs used 79.0 of 110.0 GiB (31.0 available); imagefs used 20.0 GiB; 3 pod(s) use 14.6 GiB ephemeral; 2 longhorn replica(s) hold 23.0 GiB actual",
    ] {
        assert!(stdout.contains(want), "says {want:?}:\n{stdout}");
    }
    // Pods, largest first; a claim's volume is not ephemeral storage.
    assert_eq!(
        section(&stdout, "pods on w-2 by ephemeral storage"),
        vec![
            "pod boss-gates/gate-abc: ephemeral 12.5 GiB (container rootfs 0.5 GiB, logs 2.0 GiB, local volumes 10.0 GiB)",
            "pod boss/sim-1: ephemeral 2.0 GiB (container rootfs 1.0 GiB, logs 1.0 GiB, local volumes 0.0 GiB)",
            "pod kube-system/kube-proxy-x: ephemeral 0.1 GiB (container rootfs 0.1 GiB, logs 0.0 GiB, local volumes 0.0 GiB)",
        ]
    );
    // Images, largest first, named by a tag where the Node lists one.
    assert_eq!(
        section(&stdout, "images on w-2 by size"),
        vec![
            "image reg.test/gate-runner:v9: 5.0 GiB",
            "image reg.test/boss:v1: 3.0 GiB",
            "image reg.test/pause@sha256:bbbb: 0.0 GiB",
        ]
    );
    // Only the replicas whose node is w-2, largest actual first.
    assert_eq!(
        section(&stdout, "longhorn replicas on w-2"),
        vec![
            "replica pvc-a-r-1: volume pvc-a (pvc boss/pgdata-postgres-0), size 30.0 GiB, actual 21.0 GiB, disk disk-uuid-1, healthy",
            "replica pvc-b-r-2: volume pvc-b (no claim), size 10.0 GiB, actual 2.0 GiB, disk disk-uuid-1, failed",
        ]
    );
    // READ-ONLY: every door call is a get, and the summary is the node's own.
    let calls = e.calls();
    assert!(
        calls.iter().all(|c| c.starts_with("get ")),
        "node-disk-usage only reads: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("get --raw /api/v1/nodes/w-2/proxy/stats/summary")),
        "{calls:?}"
    );
    // Byte-identical across two reads of one state: no clock in stdout.
    let again = e.run(&["w-2"], &[]);
    assert_eq!(o.stdout, again.stdout);
}

#[test]
fn node_disk_usage_refuses_a_node_outside_the_cluster_before_any_door() {
    let e = Estate::new("node-disk-usage-refuse");
    for args in [
        vec!["forge"],
        vec!["w-9"],
        vec!["w-3"],
        vec!["W-2"],
        vec!["w-2", "extra"],
        vec![],
    ] {
        let o = e.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(
        e.calls().is_empty(),
        "a refusal opens no door: {:?}",
        e.calls()
    );
    let o = e.run(&["cp-1"], &[]);
    assert!(
        o.status.success(),
        "a read may look at a control plane: {}",
        text(&o)
    );
}

#[test]
fn node_disk_usage_that_cannot_read_a_section_is_not_a_clean_answer() {
    // No evidence is not a pass: a read that could not look must not
    // render as a node with nothing on it.
    for (switch, what, still) in [
        ("STUB_SUMMARY_FAIL", "stats/summary", "replica pvc-a-r-1"),
        ("STUB_SUMMARY_EMPTY", "stats/summary", "replica pvc-a-r-1"),
        (
            "STUB_LONGHORN_FAIL",
            "replicas.longhorn.io",
            "nodefs: capacity",
        ),
    ] {
        let e = Estate::new(&format!("node-disk-usage-{switch}"));
        let o = e.run(&["w-2"], &[(switch, "1")]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{switch}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{switch}: {out}");
        assert!(
            out.contains(what),
            "{switch} names what could not be read: {out}"
        );
        assert!(
            out.contains(still),
            "{switch}: the sections that could be read are still printed: {out}"
        );
    }
}

#[test]
fn node_disk_usage_says_so_when_longhorn_holds_no_such_node() {
    let e = Estate::new("node-disk-usage-no-lhnode");
    let o = e.run(&["w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("longhorn node: NOT FOUND — nodes.longhorn.io holds no w-1"),
        "{out}"
    );
    assert!(out.contains("replica pvc-a-r-3"), "{out}");
}

#[test]
fn the_verb_file_bounds_what_a_packet_can_ask() {
    let p = repo_root().join("infra/ops/verbs/node-disk-usage.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).expect("the verb file"))
        .expect("the verb file is JSON");
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(v["argv"], json!([SCRIPT, "{1}"]), "the node, nothing else");
    assert_eq!(v["params"][0]["name"], "node");
    assert!(v["params"][0].get("default").is_none(), "names its node");
    assert!(v["timeout"].as_u64().is_some());
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
    assert!(v["effect"].is_null(), "a read declares no effect");
}
