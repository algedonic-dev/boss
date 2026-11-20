//! The two ops verbs that retire ONE live Talos user volume in place, its
//! data kept (backlog 412d504c): the read-only
//! `plan-a-user-volume-retirement` (`infra/forge/retire-user-volume.sh
//! --plan`) and the passkey-approved `retire-a-user-volume`.
//!
//! WHY. Door 1 of the volume-move protocol David asked for on 88b35891
//! and ordered on 2026-11-07 ~02:00Z: w-1's gate volume moves off the
//! Kingston SNV3S1000G onto a Micron 7400 PRO WITHOUT a reboot.
//! node-converge refuses a DRIFT (it can only add), so the provisioned
//! volume has to go first: this door deletes the live UserVolumeConfig
//! (a `$patch: delete` document, `--mode=no-reboot`), Talos unmounts
//! /var/mnt/<volume> and keeps the partition, and node-converge then
//! provisions the declared volume, ABSENT live by then, by its new
//! selector.
//!
//! THE BOUND each test below pins: the node's declaration in the tree
//! names the volume and is one node-converge would apply; the live
//! UserVolumeConfig DRIFTs from it IN ITS diskSelector (the tree wants the
//! volume on another disk — a MATCH is refused, so is a live config that
//! no longer holds it); Talos reads the volume ready at /var/mnt/<volume>
//! on a partition whose label is u-<volume>; and no pod on the node mounts
//! the path, a claim bound to a PersistentVolume at it, or anything under
//! it (a gate run in flight). The write re-renders and applies nothing
//! unless the plan still hashes to the signed one, and its effect is read
//! back three ways: the live config holds no such document, Talos holds no
//! such volume, and the partition still carries its label.
//!
//! HOW THIS IS MEASURED. The script runs for real against a FIXTURE
//! patches directory (BOSS_TALOS_PATCHES, the seam it shares with
//! check-declared.sh): a copy of the tree's declarations with w-1's gate
//! selector set to the one the selector car declares (feat/the-gate-
//! selector-keeps-the-kingston-out, B's draft on 88b35891), so every case
//! holds whether or not that car has landed. `sudo` is stubbed on PATH as
//! a small node behind both doors — the Node, the PV and pod lists, the
//! live machine config, Talos's volumestatus and discoveredvolumes, and
//! the patch — and the estate registry is served from a file. Nothing here
//! reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/retire-user-volume.sh";

/// The stand-in behind both doors. State under $STUB_STATE: `ip-<node>`
/// its InternalIP, `cp-<node>` a control-plane label, `pvs.json` the PV
/// items, `pods-<node>.json` the pod items on that node (default none),
/// `mc-<ip>.yaml` what `get machineconfig -o yaml` answers,
/// `vs-<ip>-<id>.json` / `dv-<ip>-<id>.json` what `get volumestatus` /
/// `get discoveredvolumes <id> -o json` answer (absent: NotFound).
/// A `patch machineconfig` records its `--patch=` value in `patch-arg`
/// and its mode in `patch-mode`; unless STUB_PATCH_FAIL is set it then
/// makes `mc-after-<ip>.yaml` the live config and, unless
/// STUB_VOLUME_STAYS is set, drops every u-* volumestatus of that node.
/// STUB_LABEL_GONE=1 drops the partition after the patch too (a wipe).
/// `lh-<node>.json` is Longhorn's node object (absent: NotFound);
/// STUB_LONGHORN_FORBIDDEN=1 answers its read as RBAC does.
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
if [ "$tool" = kubectl ]; then
    case "$1 $2" in
    "get node")
        n="$3"
        [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
        labels='"kubernetes.io/hostname":"'"$n"'"'
        [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
        printf '{"kind":"Node","metadata":{"name":"%s","uid":"uid-%s","labels":{%s}},"spec":{},"status":{"addresses":[{"type":"InternalIP","address":"%s"}],"conditions":[{"type":"Ready","status":"True"}]}}\n' "$n" "$n" "$labels" "$(cat "$S/ip-$n")"
        exit 0 ;;
    "get pv")
        printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/pvs.json" 2>/dev/null || echo '[]')"; exit 0 ;;
    "get pods")
        n=""
        for a in "$@"; do case "$a" in spec.nodeName=*) n="${a#spec.nodeName=}" ;; esac; done
        printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/pods-$n.json" 2>/dev/null || echo '[]')"; exit 0 ;;
    "get nodes.longhorn.io")
        [ -z "${STUB_LONGHORN_FORBIDDEN:-}" ] || { echo "Error from server (Forbidden): nodes.longhorn.io \"$3\" is forbidden" >&2; exit 1; }
        f="$S/lh-$3.json"
        [ -f "$f" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
        cat "$f"; exit 0 ;;
    esac
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2
fi
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    ip="$2"; shift 2
    case "$1 $2" in
    "get machineconfig")
        [ "$3 $4" = "-o yaml" ] || { echo "stub: get machineconfig without -o yaml" >&2; exit 2; }
        cat "$S/mc-$ip.yaml"; exit 0 ;;
    "get volumestatus" | "get discoveredvolumes")
        [ "$4 $5" = "-o json" ] || { echo "stub: get $2 without one id and -o json: $*" >&2; exit 2; }
        p=vs; [ "$2" = discoveredvolumes ] && p=dv
        f="$S/$p-$ip-$3.json"
        [ -f "$f" ] || { echo "error getting resource: rpc error: code = NotFound desc = resource $2($3@undefined) not found" >&2; exit 1; }
        cat "$f"; exit 0 ;;
    "patch machineconfig")
        for a in "$@"; do
            case "$a" in
            --mode=*) printf '%s' "${a#--mode=}" > "$S/patch-mode" ;;
            --patch=*) printf '%s' "${a#--patch=}" > "$S/patch-arg" ;;
            esac
        done
        [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "error: rpc error: code = Unavailable desc = connection refused" >&2; exit 1; }
        [ -f "$S/mc-after-$ip.yaml" ] && cp "$S/mc-after-$ip.yaml" "$S/mc-$ip.yaml"
        [ -n "${STUB_VOLUME_STAYS:-}" ] || rm -f "$S"/vs-"$ip"-u-*.json
        [ -z "${STUB_LABEL_GONE:-}" ] || rm -f "$S"/dv-"$ip"-*.json
        echo "Applied configuration without a reboot"; exit 0 ;;
    esac
fi
echo "stub: unexpected call: $*" >&2
exit 2
"#;

/// The selector w-1's gate volume was applied with on 2026-09-30 — what
/// Talos holds live — and the one the selector car declares, which keeps
/// the Kingston SNV3S1000G out so Talos picks a Micron 7400 PRO.
const LIVE_SELECTOR: &str = r#"!system_disk && disk.transport == "nvme" && disk.size > 500u * GB"#;
const NEW_SELECTOR: &str = r#"!system_disk && disk.transport == "nvme" && disk.size > 500u * GB && disk.model != "KINGSTON SNV3S1000G""#;

/// The exact document the write hands talosctl: Talos's strategic merge
/// removes the named document, and nothing else.
const DELETE_DOC: &str =
    "apiVersion: v1alpha1\nkind: UserVolumeConfig\nname: gate\n$patch: delete\n";

/// A live machine config as `talosctl get machineconfig -o yaml` prints a
/// multi-document one — every document in a block string under `spec:` —
/// carrying a token that must never reach the plan. `selector`: the gate
/// UserVolumeConfig beside v1alpha1, or None once it is deleted.
fn live_config(selector: Option<&str>) -> String {
    let mut spec = String::from(
        "version: v1alpha1\nmachine:\n    type: worker\n    token: SECRETTOKENaaaa.bbbbCCCC1234\n    network:\n        hostname: w-1\n",
    );
    if let Some(sel) = selector {
        spec.push_str(&format!(
            "---\napiVersion: v1alpha1\nkind: UserVolumeConfig\nname: gate\nprovisioning:\n    diskSelector:\n        match: '{sel}'\n    minSize: 500GB\n    grow: true\nfilesystem:\n    type: xfs\n"
        ));
    }
    let body: String = spec.lines().map(|l| format!("    {l}\n")).collect();
    format!(
        "node: 10.20.0.14\nmetadata:\n    namespace: config\n    type: MachineConfigs.config.talos.dev\n    id: v1alpha1\n    version: 3\nspec: |\n{body}"
    )
}

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
            "retire_user_volume_sh: no {tool} on this box — the gate image has it, and a \
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

/// A Talos resource the way `talosctl -o json` prints it: indented, the
/// node's address beside the resource.
fn resource(ip: &str, kind: &str, id: &str, spec: Value) -> String {
    serde_json::to_string_pretty(&json!({
        "node": ip,
        "metadata": {"namespace": "runtime", "type": kind,
                     "id": id, "version": "3", "phase": "running"},
        "spec": spec
    }))
    .expect("json")
}

/// u-gate as w-1 holds it today: the Kingston's one partition.
fn gate_volume(phase: &str, size: &str) -> Value {
    json!({"phase": phase, "type": "partition", "location": "/dev/nvme1n1p1",
           "parentLocation": "/dev/nvme1n1", "partitionIndex": 1, "filesystem": "xfs",
           "size": 1_000_202_043_392_u64, "prettySize": size,
           "partitionUUID": "6f1d7c3e-8a4b-4d2e-9f10-2b3c4d5e6f70",
           "mountSpec": {"targetPath": "gate", "parentId": "/var/mnt"}})
}

/// The partition under it, as discoveredvolumes names it.
fn gate_partition(label: &str) -> Value {
    json!({"dev_path": "/dev/nvme1n1p1", "type": "partition", "size": 1_000_202_043_392_u64,
           "pretty_size": "1.0 TB", "parent": "nvme1n1", "partition_index": 1,
           "partition_label": label, "partition_uuid": "6f1d7c3e-8a4b-4d2e-9f10-2b3c4d5e6f70",
           "name": "xfs", "label": label})
}

impl Node {
    /// w-1 on the morning of the move: its declaration (the fixture's)
    /// wants the gate volume by NEW_SELECTOR, Talos holds it live by
    /// LIVE_SELECTOR, ready on /dev/nvme1n1p1; the gate's local PV points
    /// at /var/mnt/gate, and no pod on w-1 mounts it (the gates are held).
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("retire-user-volume-{name}"));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        std::fs::create_dir_all(dir.join("patches")).expect("patches");
        write_exec(&dir.join("bin/sudo"), STUB);
        let n = Node { dir };
        let tree = repo_root().join("infra/cluster/talos/patches");
        for entry in std::fs::read_dir(&tree).expect("the tree's patches") {
            let p = entry.expect("a patch").path();
            let body = std::fs::read_to_string(&p).expect("a declaration");
            write_file(
                &n.dir.join("patches").join(p.file_name().expect("a name")),
                &body,
            );
        }
        n.declare_gate_selector(NEW_SELECTOR);
        write_file(
            &n.dir.join("nodes.json"),
            &json!({"data": [
                registry_row("forge", "10.20.0.15", "forge"),
                registry_row("cp-1", "10.20.0.11", "talos-control-plane"),
                registry_row("w-1", "10.20.0.14", "talos-worker"),
            ]})
            .to_string(),
        );
        n.state("ip-cp-1", "10.20.0.11");
        n.state("cp-cp-1", "");
        n.state("ip-w-1", "10.20.0.14");
        n.state("mc-10.20.0.14.yaml", &live_config(Some(LIVE_SELECTOR)));
        n.state("mc-after-10.20.0.14.yaml", &live_config(None));
        n.volume(gate_volume("ready", "1.0 TB"));
        n.state(
            "dv-10.20.0.14-nvme1n1p1.json",
            &resource(
                "10.20.0.14",
                "DiscoveredVolumes.block.talos.dev",
                "nvme1n1p1",
                gate_partition("u-gate"),
            ),
        );
        n.state(
            "pvs.json",
            &json!([{
                "apiVersion": "v1", "kind": "PersistentVolume",
                "metadata": {"name": "gate-w-1"},
                "spec": {"local": {"path": "/var/mnt/gate"},
                         "claimRef": {"namespace": "boss-dev", "name": "gate"},
                         "storageClassName": "gate-seed-local"}
            }, {
                "apiVersion": "v1", "kind": "PersistentVolume",
                "metadata": {"name": "pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091"},
                "spec": {"csi": {"driver": "driver.longhorn.io"},
                         "claimRef": {"namespace": "boss-dev", "name": "boss-dev-work"}}
            }])
            .to_string(),
        );
        n.longhorn_disks(json!({"default-disk-9a8b7c6d5e4f": "/var/lib/longhorn/"}));
        n
    }

    /// Longhorn's node w-1 with these disks ({name: path}) in spec.disks.
    fn longhorn_disks(&self, disks: Value) {
        let disks: serde_json::Map<String, Value> = disks
            .as_object()
            .expect("{name: path}")
            .iter()
            .map(|(k, p)| {
                (
                    k.clone(),
                    json!({"path": p, "allowScheduling": true, "evictionRequested": false,
                                   "storageReserved": 0, "tags": [], "diskType": "filesystem"}),
                )
            })
            .collect();
        self.state(
            "lh-w-1.json",
            &json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Node",
                    "metadata": {"name": "w-1", "namespace": "longhorn-system"},
                    "spec": {"allowScheduling": true, "disks": disks}})
            .to_string(),
        );
    }

    /// The fixture's w-1.yaml with its gate volume's selector set to `sel`
    /// — the one line the selector car changes; everything else is the
    /// tree's own declaration.
    fn declare_gate_selector(&self, sel: &str) {
        let tree =
            std::fs::read_to_string(repo_root().join("infra/cluster/talos/patches/w-1.yaml"))
                .expect("w-1.yaml");
        // Only the gate document's selector: w-1 declares a scratch volume
        // beside it since backlog 46328c3a.
        let mut seen = 0;
        let mut in_gate = false;
        let body: String = tree
            .lines()
            .map(|l| {
                if l == "---" || l.starts_with("name: ") {
                    in_gate = l == "name: gate";
                }
                match l.find("match: '") {
                    Some(at) if in_gate && l[..at].trim().is_empty() => {
                        seen += 1;
                        format!("{}match: '{sel}'\n", &l[..at])
                    }
                    _ => format!("{l}\n"),
                }
            })
            .collect();
        assert_eq!(seen, 1, "w-1.yaml declares one gate diskSelector:\n{tree}");
        write_file(&self.patches().join("w-1.yaml"), &body);
    }

    fn patches(&self) -> PathBuf {
        self.dir.join("patches")
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn volume(&self, spec: Value) {
        self.state(
            "vs-10.20.0.14-u-gate.json",
            &resource(
                "10.20.0.14",
                "VolumeStatuses.block.talos.dev",
                "u-gate",
                spec,
            ),
        );
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
            .env("BOSS_TALOS_PATCHES", self.patches())
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_TALOSCTL", self.dir.join("talosctl"))
            .env("BOSS_RETIRE_VOLUME_WAIT_S", "2")
            .env("BOSS_RETIRE_VOLUME_POLL_S", "1")
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

    fn patched(&self) -> bool {
        self.argv().contains("patch machineconfig") || self.dir.join("state/patch-arg").exists()
    }

    fn plan(&self) -> (String, String) {
        let o = self.run(&["--plan", "w-1", "gate"], &[]);
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
            !self.patched(),
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
    let re = verb("retire-a-user-volume")["effect"]
        .as_str()
        .expect("retire-a-user-volume declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------ verbs

/// A read-only plan and a passkey write that takes exactly the plan's
/// params plus the hash, both on the forge (design 17835005's shape).
#[test]
fn the_verbs_are_a_read_only_plan_and_a_passkey_write() {
    let plan = verb("plan-a-user-volume-retirement");
    let write = verb("retire-a-user-volume");
    let about = |v: &Value| v["about"].as_str().unwrap_or_default().to_string();
    assert!(about(&plan).starts_with("READ-ONLY"), "{}", about(&plan));
    assert!(about(&write).starts_with("MUTATING"), "{}", about(&write));
    assert_ne!(plan["requires_approval"], json!(true));
    assert_eq!(write["requires_approval"], json!(true));
    assert_eq!(write["plan_verb"], json!("plan-a-user-volume-retirement"));
    assert_eq!(write["approvers"], json!(["emp-david"]));
    assert_eq!(plan["hosts"], json!(["forge"]));
    assert_eq!(write["hosts"], json!(["forge"]));
    assert_eq!(
        plan["argv"],
        json!([SCRIPT, "--plan", "{1}", "{2}"]),
        "the plan is the script's --plan"
    );
    assert_eq!(write["argv"], json!([SCRIPT, "{1}", "{2}", "{3}"]));
    let names = |v: &Value| -> Vec<String> {
        v["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(|p| p["name"].as_str().expect("name").to_string())
            .collect()
    };
    assert_eq!(names(&plan), ["node", "volume"]);
    assert_eq!(names(&write), ["node", "volume", "plan_sha256"]);
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_volume_both_selectors_and_the_delete_document() {
    let n = Node::new("plan");
    let (plan, hash) = n.plan();
    assert_eq!(hash.len(), 64, "{hash}");
    for want in [
        "plan: retire-a-user-volume",
        "node: w-1",
        "address: 10.20.0.14",
        "kubernetes node uid: uid-w-1",
        "UserVolumeConfig gate",
        "phase ready",
        "/dev/nvme1n1p1",
        "/dev/nvme1n1",
        "1.0 TB",
        "partition label u-gate",
        "PersistentVolume gate-w-1 (claim boss-dev/gate)",
        "--mode=no-reboot",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        plan.contains(DELETE_DOC),
        "the plan carries the exact delete document:\n{plan}"
    );
    let json_str = |s: &str| serde_json::to_string(s).expect("json");
    assert!(
        plan.contains(&format!("selector declared: {}", json_str(NEW_SELECTOR))),
        "the declared selector, as the tree states it:\n{plan}"
    );
    assert!(
        plan.contains(&format!("selector live: {}", json_str(LIVE_SELECTOR))),
        "the live selector, as Talos holds it:\n{plan}"
    );
    assert!(
        !plan.contains("SECRETTOKEN"),
        "the live machine config never reaches the plan:\n{plan}"
    );
    assert!(!n.patched(), "a plan patches nothing: {}", n.argv());
    // Two renders of one true state are byte-identical.
    let (again, hash2) = n.plan();
    assert_eq!((&plan, &hash), (&again, &hash2));
}

/// The retire is for a volume the tree no longer wants where it is: a live
/// document that MATCHes its declaration — the selector car not landed —
/// is refused, and so is a declaration that names no such volume.
#[test]
fn a_volume_the_tree_still_wants_where_it_is_is_refused() {
    let n = Node::new("match");
    n.declare_gate_selector(LIVE_SELECTOR);
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("MATCH"), "{out}");
    assert!(out.contains("selector"), "{out}");

    // A drift in something other than the selector is not a move.
    let n = Node::new("same-selector");
    let body = std::fs::read_to_string(n.patches().join("w-1.yaml")).expect("w-1.yaml");
    n.declare_gate_selector(LIVE_SELECTOR);
    let body2 = std::fs::read_to_string(n.patches().join("w-1.yaml")).expect("w-1.yaml");
    assert_ne!(body, body2);
    write_file(
        &n.patches().join("w-1.yaml"),
        &body2.replace("minSize: 500GB", "minSize: 600GB"),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("same diskSelector"), "{out}");

    let n = Node::new("undeclared");
    let out = n.refused(&["--plan", "w-1", "data"]);
    assert!(out.contains("UserVolumeConfig"), "{out}");
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

/// The claim: a gate run in flight mounts boss-dev/gate, bound to the PV
/// at /var/mnt/gate — refused, naming the pod. A hostPath under the mount
/// (the daily trim pod) is refused the same way.
#[test]
fn a_pod_that_mounts_the_volume_refuses_the_plan() {
    let n = Node::new("in-use");
    n.state(
        "pods-w-1.json",
        &json!([{
            "metadata": {"namespace": "boss-dev", "name": "gate-run-7f3e2a"},
            "spec": {"nodeName": "w-1",
                     "volumes": [{"name": "gate", "persistentVolumeClaim": {"claimName": "gate"}},
                                 {"name": "tmp", "emptyDir": {}}]},
            "status": {"phase": "Running"}
        }, {
            "metadata": {"namespace": "boss-dev", "name": "gate-run-done"},
            "spec": {"nodeName": "w-1",
                     "volumes": [{"name": "gate", "persistentVolumeClaim": {"claimName": "gate"}}]},
            "status": {"phase": "Succeeded"}
        }])
        .to_string(),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(
        out.contains("boss-dev/gate-run-7f3e2a"),
        "names the pod: {out}"
    );
    assert!(out.contains("boss-dev/gate"), "names the claim: {out}");
    assert!(
        !out.contains("gate-run-done"),
        "a finished pod holds no mount: {out}"
    );

    n.state(
        "pods-w-1.json",
        &json!([{
            "metadata": {"namespace": "boss-node-maintenance", "name": "boss-node-trim-29341-abcde"},
            "spec": {"nodeName": "w-1",
                     "volumes": [{"name": "gate", "hostPath": {"path": "/var/mnt/gate/seed", "type": "Directory"}}]},
            "status": {"phase": "Running"}
        }])
        .to_string(),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(
        out.contains("boss-node-maintenance/boss-node-trim-29341-abcde"),
        "{out}"
    );
}

/// A Running pod that mounts a PARENT of the volume with a subPath into it
/// — hostPath /var/mnt, volumeMount subPath gate — mounts /var/mnt/gate as
/// surely as a hostPath of /var/mnt/gate does (review f1d1c254, R1 P1: the
/// guard read only spec.volumes[].hostPath.path and passed this pod).
fn parent_subpath_pod(subpath_key: &str, subpath: &str) -> Value {
    json!([{
        "metadata": {"namespace": "kube-system", "name": "parent-mount-5c9d"},
        "spec": {"nodeName": "w-1",
                 "volumes": [{"name": "parent", "hostPath": {"path": "/var/mnt", "type": "Directory"}}],
                 "containers": [{"name": "c", "image": "x",
                                 "volumeMounts": [{"name": "parent", "mountPath": "/gate-runs", subpath_key: subpath}]}]},
        "status": {"phase": "Running"}
    }])
}

#[test]
fn a_parent_mount_with_a_subpath_into_the_volume_refuses_the_plan() {
    let n = Node::new("parent-subpath");
    n.state(
        "pods-w-1.json",
        &parent_subpath_pod("subPath", "gate").to_string(),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("kube-system/parent-mount-5c9d"), "{out}");
    assert!(
        out.contains("/var/mnt/gate"),
        "names the effective path: {out}"
    );

    // Spellings Kubernetes admits and the kubelet's filepath.Join cleans to
    // the volume, under it, or the whole parent (review e25dd362: `./gate`
    // passed a lexical comparison).
    for sub in ["./gate", "gate/.", "./gate/./seed", ".", "./"] {
        let n = Node::new(&format!("parent-dot-{}", sub.replace(['.', '/'], "d")));
        n.state(
            "pods-w-1.json",
            &parent_subpath_pod("subPath", sub).to_string(),
        );
        let out = n.refused(&["--plan", "w-1", "gate"]);
        assert!(
            out.contains("kube-system/parent-mount-5c9d"),
            "{sub}: {out}"
        );
    }
    // A dotted SOURCE is cleaned the same way.
    let n = Node::new("parent-dot-source");
    let mut pod = parent_subpath_pod("subPath", "gate");
    pod[0]["spec"]["volumes"][0]["hostPath"]["path"] = json!("/var/./mnt/.");
    n.state("pods-w-1.json", &pod.to_string());
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("kube-system/parent-mount-5c9d"), "{out}");

    // The same parent through a claim bound to a local PV at /var/mnt.
    let n = Node::new("parent-claim");
    n.state(
        "pvs.json",
        &json!([{
            "apiVersion": "v1", "kind": "PersistentVolume",
            "metadata": {"name": "mnt-w-1"},
            "spec": {"local": {"path": "/var/mnt/"},
                     "claimRef": {"namespace": "boss-dev", "name": "mnt"}}
        }])
        .to_string(),
    );
    n.state(
        "pods-w-1.json",
        &json!([{
            "metadata": {"namespace": "boss-dev", "name": "claim-parent-1"},
            "spec": {"nodeName": "w-1",
                     "volumes": [{"name": "m", "persistentVolumeClaim": {"claimName": "mnt"}}],
                     "initContainers": [{"name": "i", "image": "x",
                                         "volumeMounts": [{"name": "m", "mountPath": "/m", "subPath": "gate/seed"}]}],
                     "containers": [{"name": "c", "image": "x"}]},
            "status": {"phase": "Pending"}
        }])
        .to_string(),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("boss-dev/claim-parent-1"), "{out}");

    // A subPathExpr cannot be resolved here, so a related source refuses;
    // a parent mounted whole holds the volume too.
    for (key, val) in [("subPathExpr", "$(POD_NAME)"), ("subPath", "")] {
        let n = Node::new(&format!("parent-{key}"));
        n.state("pods-w-1.json", &parent_subpath_pod(key, val).to_string());
        let out = n.refused(&["--plan", "w-1", "gate"]);
        assert!(
            out.contains("kube-system/parent-mount-5c9d"),
            "{key}: {out}"
        );
    }

    // A parent with a subPath elsewhere does not mount the volume: the plan
    // renders (the effective path is judged, not the source alone).
    for sub in ["other", "./other", "gatex"] {
        let n = Node::new(&format!(
            "parent-elsewhere-{}",
            sub.replace(['.', '/'], "d")
        ));
        n.state(
            "pods-w-1.json",
            &parent_subpath_pod("subPath", sub).to_string(),
        );
        let (plan, _) = n.plan();
        assert!(plan.contains("in use: no pod"), "{sub}: {plan}");
    }
}

/// Signed while nothing used the volume, then a parent+subPath pod starts:
/// the write re-renders, sees it, and applies nothing (review f1d1c254:
/// the signed hash cannot protect a use the renderer omits).
#[test]
fn a_parent_subpath_pod_started_after_signing_refuses_the_write() {
    // `./gate` is the alias review e25dd362 drove through a signed write.
    for sub in ["gate", "./gate"] {
        let n = Node::new(&format!(
            "parent-interleave-{}",
            sub.replace(['.', '/'], "d")
        ));
        let (_, hash) = n.plan();
        n.state(
            "pods-w-1.json",
            &parent_subpath_pod("subPath", sub).to_string(),
        );
        let out = n.refused(&["w-1", "gate", &hash]);
        assert!(
            out.contains("kube-system/parent-mount-5c9d"),
            "{sub}: {out}"
        );
    }
}

/// Longhorn's instance-manager as it runs on every node: an upstream
/// DaemonSet-style pod in longhorn-system that mounts host `/` at /host
/// (the live refusal, ops-request cc8d602e, 2026-11-07 15:11Z, backlog
/// 76c8b647), beside /dev, /proc and its default disk. `extra` is
/// appended to its volumes, each mounted at /extra-<n>.
fn instance_manager_pod(name: &str, labels: Value, extra: &[&str]) -> Value {
    let mut volumes = vec![
        json!({"name": "host", "hostPath": {"path": "/"}}),
        json!({"name": "dev", "hostPath": {"path": "/dev"}}),
        json!({"name": "proc", "hostPath": {"path": "/proc"}}),
        json!({"name": "longhorn", "hostPath": {"path": "/var/lib/longhorn/"}}),
    ];
    let mut mounts = vec![
        json!({"name": "host", "mountPath": "/host", "mountPropagation": "HostToContainer"}),
        json!({"name": "dev", "mountPath": "/dev"}),
        json!({"name": "proc", "mountPath": "/host/proc"}),
        json!({"name": "longhorn", "mountPath": "/var/lib/longhorn/", "mountPropagation": "Bidirectional"}),
    ];
    for (i, p) in extra.iter().enumerate() {
        volumes.push(json!({"name": format!("extra-{i}"), "hostPath": {"path": p}}));
        mounts.push(json!({"name": format!("extra-{i}"), "mountPath": format!("/extra-{i}")}));
    }
    json!({
        "metadata": {"namespace": "longhorn-system", "name": name, "labels": labels,
                     "ownerReferences": [{"apiVersion": "longhorn.io/v1beta2", "kind": "InstanceManager",
                                          "name": name, "controller": true}]},
        "spec": {"nodeName": "w-1", "volumes": volumes,
                 "containers": [{"name": "instance-manager", "image": "longhornio/longhorn-instance-manager",
                                 "volumeMounts": mounts}]},
        "status": {"phase": "Running"}
    })
}

const IM: &str = "instance-manager-3acffeab4339444483956b9fef567da5";

fn im_labels() -> Value {
    json!({"longhorn.io/component": "instance-manager", "longhorn.io/node": "w-1",
           "longhorn.io/instance-manager-type": "aio", "longhorn.io/managed-by": "longhorn-manager"})
}

/// A pod mounting host `/` whole, outside Longhorn.
fn root_pod(ns: &str, name: &str) -> Value {
    json!({
        "metadata": {"namespace": ns, "name": name},
        "spec": {"nodeName": "w-1",
                 "volumes": [{"name": "root", "hostPath": {"path": "/"}}],
                 "containers": [{"name": "c", "image": "x",
                                 "volumeMounts": [{"name": "root", "mountPath": "/host"}]}]},
        "status": {"phase": "Running"}
    })
}

/// (1) Backlog 76c8b647: Longhorn's instance-manager mounting host `/` is
/// not a user of /var/mnt/gate when /var/mnt/gate is no Longhorn disk on
/// the node — Longhorn writes only its registered disks. The plan renders
/// and names the exemption.
#[test]
fn the_instance_managers_root_mount_is_exempt_when_the_volume_is_no_longhorn_disk() {
    let n = Node::new("im-exempt");
    n.state(
        "pods-w-1.json",
        &json!([instance_manager_pod(IM, im_labels(), &[])]).to_string(),
    );
    let (plan, _) = n.plan();
    assert!(
        plan.contains(&format!(
            "exempt: longhorn-system/{IM} mounts hostPath / at /host (Longhorn writes only its registered disks; /var/mnt/gate is not one)"
        )),
        "{plan}"
    );
    assert!(
        plan.contains("longhorn disks on w-1: default-disk-9a8b7c6d5e4f at /var/lib/longhorn/"),
        "the disks the exemption rests on are in the signed bytes: {plan}"
    );
}

/// (2) The same pod, with /var/mnt/gate (or a path holding it, or under
/// it) registered as a Longhorn disk: refused on the Longhorn-disk check,
/// before any pod is judged.
#[test]
fn a_volume_registered_as_a_longhorn_disk_is_refused_first() {
    for path in [
        "/var/mnt/gate",
        "/var/mnt/gate/",
        "/var/mnt",
        "/var/mnt/gate/replicas",
    ] {
        let n = Node::new("im-lh-disk");
        n.state(
            "pods-w-1.json",
            &json!([instance_manager_pod(IM, im_labels(), &[])]).to_string(),
        );
        n.longhorn_disks(
            json!({"default-disk-9a8b7c6d5e4f": "/var/lib/longhorn/", "u-gate": path}),
        );
        let out = n.refused(&["--plan", "w-1", "gate"]);
        assert!(out.contains("Longhorn disk"), "{path}: {out}");
        assert!(out.contains("u-gate"), "{path}: {out}");
        assert!(
            !out.contains("is in use"),
            "the disk check comes first: {path}: {out}"
        );
    }
    // A Longhorn node nobody could read is no evidence the volume is not
    // one of its disks.
    let n = Node::new("im-lh-dark");
    let o = n.run(
        &["--plan", "w-1", "gate"],
        &[("STUB_LONGHORN_FORBIDDEN", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
}

/// (3) and (4): the exemption is that one pod's root mount and nothing
/// else. A root mount outside Longhorn, an instance-manager look-alike
/// without the label (or outside longhorn-system), a longhorn-system pod
/// mounting /var/mnt, and the instance-manager itself mounting /var/mnt or
/// /var/mnt/gate, or reaching it by subPath from `/`, all still refuse.
#[test]
fn every_other_mount_of_a_path_that_holds_the_volume_still_refuses() {
    let mut no_label = im_labels();
    no_label["longhorn.io/component"] = json!("engine-image");
    let mut sub = instance_manager_pod(IM, im_labels(), &[]);
    sub["spec"]["containers"][0]["volumeMounts"][0]["subPath"] = json!("var/mnt/gate");
    let mut elsewhere = instance_manager_pod(IM, im_labels(), &[]);
    elsewhere["metadata"]["namespace"] = json!("kube-system");
    for (case, pod, who) in [
        (
            "root",
            root_pod("monitoring", "node-exporter-x7k2p"),
            "monitoring/node-exporter-x7k2p",
        ),
        (
            "lh-root",
            root_pod("longhorn-system", "longhorn-manager-abcde"),
            "longhorn-system/longhorn-manager-abcde",
        ),
        ("no-label", instance_manager_pod(IM, no_label, &[]), IM),
        ("other-ns", elsewhere, IM),
        (
            "lh-mnt",
            {
                let mut p = root_pod("longhorn-system", "longhorn-csi-plugin-q2w3e");
                p["spec"]["volumes"][0]["hostPath"]["path"] = json!("/var/mnt");
                p
            },
            "longhorn-system/longhorn-csi-plugin-q2w3e",
        ),
        (
            "im-mnt",
            instance_manager_pod(IM, im_labels(), &["/var/mnt"]),
            IM,
        ),
        (
            "im-gate",
            instance_manager_pod(IM, im_labels(), &["/var/mnt/gate"]),
            IM,
        ),
        ("im-subpath", sub, IM),
    ] {
        let n = Node::new(&format!("im-refuse-{case}"));
        n.state("pods-w-1.json", &json!([pod]).to_string());
        let out = n.refused(&["--plan", "w-1", "gate"]);
        assert!(out.contains("is in use"), "{case}: {out}");
        assert!(out.contains(who), "{case} names the pod: {out}");
    }
}

/// (5) Signed with the instance-manager present: the write re-renders the
/// same exemption and retires the volume. Signed the same way, a root
/// mount outside Longhorn that starts after signing refuses the write.
#[test]
fn the_exemption_survives_the_write_and_a_later_root_mount_does_not() {
    let n = Node::new("im-write");
    let im = json!([instance_manager_pod(IM, im_labels(), &[])]);
    n.state("pods-w-1.json", &im.to_string());
    let (plan, hash) = n.plan();
    let o = n.run(&["w-1", "gate", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the write re-rendered the same plan, exemption and all: {out}"
    );
    assert_eq!(effect_lines(&out).len(), 1, "{out}");

    let n = Node::new("im-write-late");
    n.state("pods-w-1.json", &im.to_string());
    let (_, hash) = n.plan();
    n.state(
        "pods-w-1.json",
        &json!([
            instance_manager_pod(IM, im_labels(), &[]),
            root_pod("monitoring", "node-exporter-x7k2p")
        ])
        .to_string(),
    );
    let out = n.refused(&["w-1", "gate", &hash]);
    assert!(out.contains("monitoring/node-exporter-x7k2p"), "{out}");
}

#[test]
fn a_volume_talos_does_not_read_ready_is_refused() {
    let n = Node::new("not-ready");
    n.volume(gate_volume("waiting", "1.0 TB"));
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("waiting"), "{out}");

    std::fs::remove_file(n.dir.join("state/vs-10.20.0.14-u-gate.json")).expect("rm");
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("u-gate"), "{out}");

    // A partition whose label is not the volume's is not one this door can
    // name again after the delete.
    let n = Node::new("label");
    n.state(
        "dv-10.20.0.14-nvme1n1p1.json",
        &resource(
            "10.20.0.14",
            "DiscoveredVolumes.block.talos.dev",
            "nvme1n1p1",
            gate_partition("data"),
        ),
    );
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("'data'"), "{out}");
}

#[test]
fn what_the_bound_does_not_admit_is_refused_before_any_door() {
    let n = Node::new("bounds");
    for args in [
        vec!["--plan", "w-1"],
        vec!["--plan", "w-1", "gate", "extra"],
        vec!["--plan", "W-1", "gate"],
        vec!["--plan", "-w", "gate"],
        vec!["--plan", "w-1", "Gate"],
        vec!["--plan", "w-1", "../gate"],
        vec!["w-1", "gate"],
        vec!["w-1", "gate", "not-a-hash"],
    ] {
        n.refused(&args);
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());

    // A control plane, even one declaring the volume, is refused by the
    // registry before any door opens.
    let w1 = std::fs::read_to_string(n.patches().join("w-1.yaml")).expect("w-1.yaml");
    write_file(&n.patches().join("cp-1.yaml"), &w1);
    let o = n.run(&["--plan", "cp-1", "gate"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("talos-worker"), "{out}");
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_refuses_a_plan_that_drifted() {
    let n = Node::new("drifted");
    let (_, hash) = n.plan();
    // The volume Talos reads moved since the passkey signed.
    n.volume(gate_volume("ready", "2.0 TB"));
    let out = n.refused(&["w-1", "gate", &hash]);
    assert!(out.contains(&hash), "names the approved hash: {out}");
    assert!(out.contains("not the approved plan"), "{out}");
}

#[test]
fn the_write_applies_the_delete_document_and_reads_the_retirement_back() {
    let n = Node::new("write");
    let (plan, hash) = n.plan();
    let o = n.run(&["w-1", "gate", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let state = |f: &str| std::fs::read_to_string(n.dir.join("state").join(f)).expect(f);
    assert_eq!(
        state("patch-arg"),
        DELETE_DOC.trim_end(),
        "the one document handed to talosctl"
    );
    assert_eq!(state("patch-mode"), "no-reboot");
    let effect = effect_lines(&out);
    assert_eq!(effect.len(), 1, "one effect line: {out}");
    for want in [
        "u-gate",
        "w-1",
        "/dev/nvme1n1p1",
        "/dev/nvme1n1",
        "label u-gate",
    ] {
        assert!(effect[0].contains(want), "{want}: {}", effect[0]);
    }
    assert!(!out.contains("SECRETTOKEN"), "{out}");

    // At most once: the volume is gone, so the plan now refuses.
    let o = n.run(&["--plan", "w-1", "gate"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
}

#[test]
fn a_retirement_talos_does_not_take_is_not_proven() {
    for (case, env) in [
        ("stays", ("STUB_VOLUME_STAYS", "1")),
        ("wiped", ("STUB_LABEL_GONE", "1")),
    ] {
        let n = Node::new(case);
        let (_, hash) = n.plan();
        let o = n.run(&["w-1", "gate", &hash], &[env]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{case}: {out}");
        assert!(out.contains("NOT proven"), "{case}: {out}");
        assert!(effect_lines(&out).is_empty(), "{case}: no effect: {out}");
    }

    let n = Node::new("patch-fails");
    let (_, hash) = n.plan();
    let o = n.run(&["w-1", "gate", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}
