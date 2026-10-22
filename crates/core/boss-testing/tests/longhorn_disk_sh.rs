//! The two ops verbs that make a node's declared Talos user volume a
//! Longhorn disk (backlog 493a02b6): the read-only `plan-a-longhorn-disk`
//! (`infra/forge/longhorn-disk.sh --plan`) and the passkey-approved
//! `add-longhorn-disk`.
//!
//! WHY. Measured 2026-10-19T15:18Z while planning the third 1 TB disk for
//! w-2 (David, ~15:05Z: one more disk for a one-disk machine): every door
//! stopped one step short. node-converge admits a UserVolumeConfig, which
//! Talos mounts at /var/mnt/<name>; nothing then declared that mount as a
//! Longhorn disk (nodes.longhorn.io spec.disks), and set-volume-replicas /
//! move-volume-replica act only on disks that already exist. So the window
//! David was about to run ended at a mounted, unused volume.
//!
//! THE BOUND each test below pins: the volume is one the node's
//! declaration names as a UserVolumeConfig (and the declaration passes
//! check-declared.sh --only-read-classes, whose allowlist demands
//! `!system_disk`); Talos reads it ready, mounted at /var/mnt/<name>, on a
//! disk that is NOT the one EPHEMERAL lives on; no PersistentVolume and no
//! pod on the node uses that path (the gate volume on w-1 is the one this
//! exists to refuse); and no Longhorn disk on the node is that path, under
//! it or over it. The write is one compare-and-set patch, and its effect
//! is read back: the new disk Ready and Schedulable by Longhorn's own
//! conditions, with a diskUUID of its own, and every other disk unchanged.
//!
//! HOW THIS IS MEASURED. The script runs for real against a FIXTURE
//! patches directory handed to it through BOSS_TALOS_PATCHES (the seam it
//! shares with check-declared.sh): a copy of the tree's own declarations
//! (infra/cluster/talos/patches), so w-1's gate volume is the tree's real
//! one, with ONE fixture volume appended to w-2's — `longhorn`, a second
//! disk chosen by what it is. The tree declares no such volume and must
//! not: w-2 has one drive bay, and on 2026-10-19 its 1 TB disk replaced
//! the system disk, which Longhorn adopts as the default disk (join-a-node
//! 829f75a8; review 77ee3857 B1, which found the car declaring a disk the
//! record says cannot exist). `sudo` is stubbed on PATH as a small node
//! behind both doors — the Kubernetes Node, the Longhorn node, the PV and
//! pod lists, and Talos's volumestatus — and the estate registry is served
//! from a file. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-disk.sh";
const GIB: u64 = 1024 * 1024 * 1024;

/// The stand-in behind both doors. State under $STUB_STATE: `ip-<node>`
/// its InternalIP, `cp-<node>` a control-plane label, `lh-<node>.json` the
/// Longhorn node, `pvs.json` the PV items, `pods-<node>.json` the pod items
/// on that node (default none), `vs-<ip>-<id>.json` what Talos answers for
/// `get volumestatus <id> -o json` (absent: NotFound).
/// A patch of nodes.longhorn.io applies the JSON patch's `test` of
/// /spec/disks and its `add`, then reports the new disk's status as
/// Longhorn would: Ready and Schedulable with a diskUUID of its own and a
/// storageMaximum. STUB_DISK bends ONE fact of what Longhorn then holds,
/// one per read-back conjunct of the script (review 77ee3857 N1):
/// `notready` (Ready and Schedulable False), `wrongpath` (the new disk's
/// spec path is not the mount), `otherchanged` (another disk's spec moved
/// with it), `nostatus` (no status for the new disk), `nouuid` (Ready with
/// an empty diskUUID), `dupuuid` (the default disk's diskUUID) and `nomax`
/// (storageMaximum 0).
/// STUB_FORBIDDEN=1 answers every longhorn.io read as RBAC does;
/// STUB_PATCH_FAIL=1 refuses the patch.
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
    "get nodes.longhorn.io")
        if [ -n "${STUB_FORBIDDEN:-}" ]; then
            echo "Error from server (Forbidden): nodes.longhorn.io \"$3\" is forbidden" >&2
            exit 1
        fi
        f="$S/lh-$3.json"
        [ -f "$f" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
        cat "$f"; exit 0 ;;
    "get pv")
        printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/pvs.json" 2>/dev/null || echo '[]')"; exit 0 ;;
    "get pods")
        n=""
        for a in "$@"; do case "$a" in spec.nodeName=*) n="${a#spec.nodeName=}" ;; esac; done
        printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/pods-$n.json" 2>/dev/null || echo '[]')"; exit 0 ;;
    "patch nodes.longhorn.io")
        n="$3"; f="$S/lh-$n.json"; p=""
        while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
        [ -z "${STUB_PATCH_FAIL:-}" ] || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
        want=$(printf '%s' "$p" | jq -c '.[] | select(.op == "test" and .path == "/spec/disks") | .value')
        [ -n "$want" ] && jq -e --argjson w "$want" '.spec.disks == $w' "$f" > /dev/null \
            || { echo "The request is invalid: the test operation failed" >&2; exit 1; }
        key=$(printf '%s' "$p" | jq -r '.[] | select(.op == "add") | .path | ltrimstr("/spec/disks/")')
        val=$(printf '%s' "$p" | jq -c '.[] | select(.op == "add") | .value')
        r=True; why=""; uuid=0b0b0b0b-1111-4222-8333-444455556666; max=999653638144
        case "${STUB_DISK:-}" in
        notready) r=False; why=DiskFilesystemChanged ;;
        nouuid) uuid="" ;;
        dupuuid) uuid=bf045701-eeec-47ec-899b-4c987ef122ce ;;
        nomax) max=0 ;;
        esac
        jq --arg k "$key" --argjson v "$val" --arg r "$r" --arg why "$why" \
            --arg u "$uuid" --argjson max "$max" --arg mode "${STUB_DISK:-}" \
            '.spec.disks[$k] = $v
             | if $mode == "wrongpath" then .spec.disks[$k].path = "/var/mnt/elsewhere" else . end
             | if $mode == "otherchanged"
               then .spec.disks |= with_entries(if .key == $k then . else .value.allowScheduling = false end)
               else . end
             | if $mode == "nostatus" then . else
               .status.diskStatus[$k] = {diskUUID: $u,
                 storageMaximum: $max, storageScheduled: 0, storageAvailable: 999000000000,
                 conditions: [{type: "Ready", status: $r, reason: $why, message: (if $why == "" then "" else "disk \($k) has a different file system than the one recorded" end)},
                              {type: "Schedulable", status: $r, reason: $why}]} end' \
            "$f" > "$f.new" && mv "$f.new" "$f"
        echo "node.longhorn.io/$n patched"; exit 0 ;;
    esac
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2
fi
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    ip="$2"; shift 2
    if [ "$1 $2" = "get volumestatus" ] && [ "$4 $5" = "-o json" ]; then
        f="$S/vs-$ip-$3.json"
        [ -f "$f" ] || { echo "error getting resource: rpc error: code = NotFound desc = resource VolumeStatuses.block.talos.dev(runtime/$3@undefined) not found" >&2; exit 1; }
        cat "$f"; exit 0
    fi
fi
echo "stub: unexpected call: $*" >&2
exit 2
"#;

/// The fixture's second disk, appended to the copy of w-2's declaration as
/// a document of its own, in the canonical form check-declared.sh reads:
/// the whole disk, chosen by what it is (not the system disk, larger than
/// 500 GB), one xfs filesystem Talos mounts at /var/mnt/longhorn.
const FIXTURE_VOLUME: &str = "---
apiVersion: v1alpha1
kind: UserVolumeConfig
name: longhorn
provisioning:
  diskSelector:
    match: '!system_disk && disk.size > 500u * GB'
  minSize: 500GB
  grow: true
filesystem:
  type: xfs
";

/// The same document with a selector the allowlist refuses: no
/// `!system_disk`, so it could match the install disk.
const FIXTURE_VOLUME_ANY_DISK: &str = "---
apiVersion: v1alpha1
kind: UserVolumeConfig
name: longhorn
provisioning:
  diskSelector:
    match: 'disk.size > 500u * GB'
  minSize: 500GB
  grow: true
filesystem:
  type: xfs
";

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
            "longhorn_disk_sh: no {tool} on this box — the gate image has it, and a \
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

/// A Longhorn node with its default disk on the install disk's
/// filesystem, as every node in the estate has one today.
fn lh_node(name: &str, default_disk: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false,
                 "disks": {default_disk: {"allowScheduling": true, "evictionRequested": false,
                                          "path": "/var/lib/longhorn/", "storageReserved": 35 * GIB,
                                          "tags": [], "diskType": "filesystem"}}},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": {default_disk: {"diskUUID": "bf045701-eeec-47ec-899b-4c987ef122ce",
                                                 "storageMaximum": 117 * GIB,
                                                 "storageScheduled": 40 * GIB,
                                                 "storageAvailable": 60 * GIB,
                                                 "conditions": [{"type": "Ready", "status": "True"},
                                                                {"type": "Schedulable", "status": "True"}]}}}
    })
}

/// Talos's answer for one volume, printed the way `talosctl -o json`
/// prints it: indented, the node's address beside the resource.
fn volume_status(ip: &str, id: &str, spec: Value) -> String {
    serde_json::to_string_pretty(&json!({
        "node": ip,
        "metadata": {"namespace": "runtime", "type": "VolumeStatuses.block.talos.dev",
                     "id": id, "version": "3", "phase": "running"},
        "spec": spec
    }))
    .expect("json")
}

fn user_volume(name: &str, partition: &str, disk: &str) -> Value {
    json!({"phase": "ready", "type": "partition", "location": partition,
           "parentLocation": disk, "partitionIndex": 1, "filesystem": "xfs",
           "prettySize": "1.0 TB", "partitionUUID": "6f1d7c3e-8a4b-4d2e-9f10-2b3c4d5e6f70",
           "mountSpec": {"targetPath": name, "parentId": "/var/mnt"}})
}

fn ephemeral(disk: &str) -> Value {
    json!({"phase": "ready", "type": "partition", "location": format!("{disk}6"),
           "parentLocation": disk, "filesystem": "xfs", "prettySize": "110 GB",
           "mountSpec": {"targetPath": "/var"}})
}

impl Node {
    /// A fixture worker w-2 WITH a second disk (the real w-2 has one bay;
    /// see the module comment): its declaration is the tree's plus
    /// FIXTURE_VOLUME, the registry and the cluster agree on it, Talos
    /// reads u-longhorn ready on /dev/sdb and EPHEMERAL on /dev/sda, and
    /// Longhorn has its one default disk. w-1 as it stands: the tree's own
    /// declaration, u-gate ready on its second NVMe, held by the gate's
    /// local PV.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("longhorn-disk-{name}"));
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
        n.declare_w2(FIXTURE_VOLUME);
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
        n.lh("w-2", lh_node("w-2", "default-disk-1f2e3d4c5b6a"));
        n.lh("w-1", lh_node("w-1", "default-disk-9a8b7c6d5e4f"));
        n.state(
            "vs-10.20.0.16-u-longhorn.json",
            &volume_status(
                "10.20.0.16",
                "u-longhorn",
                user_volume("longhorn", "/dev/sdb1", "/dev/sdb"),
            ),
        );
        n.state(
            "vs-10.20.0.16-EPHEMERAL.json",
            &volume_status("10.20.0.16", "EPHEMERAL", ephemeral("/dev/sda")),
        );
        n.state(
            "vs-10.20.0.14-u-gate.json",
            &volume_status(
                "10.20.0.14",
                "u-gate",
                user_volume("gate", "/dev/nvme1n1p1", "/dev/nvme1n1"),
            ),
        );
        n.state(
            "vs-10.20.0.14-EPHEMERAL.json",
            &volume_status("10.20.0.14", "EPHEMERAL", ephemeral("/dev/nvme0n1")),
        );
        n.pvs(json!([{
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
        }]));
        n
    }

    /// The fixture's w-2.yaml: the tree's declaration, then `doc`.
    fn declare_w2(&self, doc: &str) {
        let tree =
            std::fs::read_to_string(repo_root().join("infra/cluster/talos/patches/w-2.yaml"))
                .expect("w-2.yaml");
        assert!(tree.ends_with('\n'), "w-2.yaml ends its last line");
        write_file(&self.patches().join("w-2.yaml"), &format!("{tree}{doc}"));
    }

    fn patches(&self) -> PathBuf {
        self.dir.join("patches")
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

    fn pvs(&self, v: Value) {
        self.state("pvs.json", &v.to_string());
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
            .env("BOSS_LONGHORN_DISK_WAIT_S", "2")
            .env("BOSS_LONGHORN_DISK_POLL_S", "1")
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
        self.argv()
            .lines()
            .any(|l| l.contains("kubectl --kubeconfig=/kc patch "))
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

    /// A refusal: exit 78, no patch, no plan on stdout (a write refused
    /// for a moved plan shows today's on stderr, never as its output), and
    /// the text it said.
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
    let re = verb("add-longhorn-disk")["effect"]
        .as_str()
        .expect("add-longhorn-disk declares an `effect`")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------ declaration

/// The fixture's second disk is a declaration node-converge would admit —
/// so every plan below exercises the door, not a malformed fixture — and
/// w-1's gate volume, which the refusal below reads, is the tree's own.
#[test]
fn the_fixture_volume_is_one_node_converge_would_admit() {
    let n = Node::new("fixture");
    let body = std::fs::read_to_string(n.patches().join("w-2.yaml")).expect("w-2.yaml");
    assert_eq!(
        body.matches("\nkind: UserVolumeConfig\n").count(),
        1,
        "one user volume:\n{body}"
    );
    let o = Command::new("bash")
        .arg(repo_root().join("infra/cluster/talos/check-declared.sh"))
        .args(["--only-read-classes", "w-2"])
        .env("BOSS_TALOS_PATCHES", n.patches())
        .output()
        .expect("the check runs");
    assert!(
        o.status.success(),
        "node-converge would admit it: {}",
        text(&o)
    );
    let w1 = std::fs::read_to_string(repo_root().join("infra/cluster/talos/patches/w-1.yaml"))
        .expect("w-1.yaml");
    assert!(
        w1.contains("\nkind: UserVolumeConfig\nname: gate\n"),
        "the tree declares w-1's gate volume:\n{w1}"
    );
}

/// A declaration check-declared.sh --only-read-classes refuses is refused
/// here before any door opens, by that check and not by the system-disk
/// comparison that would otherwise stand behind it (review 77ee3857 N2).
#[test]
fn a_declaration_node_converge_would_refuse_is_refused_before_any_door() {
    let n = Node::new("any-disk");
    n.declare_w2(FIXTURE_VOLUME_ANY_DISK);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(
        out.contains("not a declaration node-converge would apply"),
        "{out}"
    );
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_renders_w2s_new_volume_as_a_longhorn_disk_and_patches_nothing() {
    let n = Node::new("plan");
    let (plan, hash) = n.plan("w-2", "longhorn");
    for want in [
        "plan: add-longhorn-disk",
        "node: w-2",
        "address: 10.20.0.16",
        "user volume: longhorn",
        "kind: UserVolumeConfig",
        "name: longhorn",
        "talos: u-longhorn ready, xfs, mounted at /var/mnt/longhorn, partition /dev/sdb1 on disk /dev/sdb",
        "system disk: /dev/sda (the disk EPHEMERAL lives on)",
        "disk default-disk-1f2e3d4c5b6a: path /var/lib/longhorn/",
        "disk to add: u-longhorn at /var/mnt/longhorn",
        "kubectl patch nodes.longhorn.io w-2 -n longhorn-system --type=json",
        "untagged",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(!n.patched(), "the plan patches nothing: {}", n.argv());
    assert!(effect_lines(&plan).is_empty(), "{plan}");

    // Deterministic, and blind to the live free space.
    let (again, hash2) = n.plan("w-2", "longhorn");
    assert_eq!((&plan, &hash), (&again, &hash2));
    let mut moved = lh_node("w-2", "default-disk-1f2e3d4c5b6a");
    moved["status"]["diskStatus"]["default-disk-1f2e3d4c5b6a"]["storageAvailable"] =
        json!(12 * GIB);
    n.lh("w-2", moved);
    assert_eq!(
        n.plan("w-2", "longhorn").1,
        hash,
        "storageAvailable is not in the signed bytes"
    );
}

#[test]
fn the_gate_volume_on_w1_is_refused_by_what_uses_it() {
    let n = Node::new("gate");
    let out = n.refused(&["--plan", "w-1", "gate"]);
    assert!(out.contains("gate-w-1"), "names the PV: {out}");
    assert!(out.contains("/var/mnt/gate"), "{out}");

    // Without the PV, a pod's hostPath under the mount refuses it the same.
    n.pvs(json!([]));
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

#[test]
fn a_volume_on_the_system_disk_is_refused() {
    let n = Node::new("system-disk");
    n.state(
        "vs-10.20.0.16-u-longhorn.json",
        &volume_status(
            "10.20.0.16",
            "u-longhorn",
            user_volume("longhorn", "/dev/sda7", "/dev/sda"),
        ),
    );
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("system disk"), "{out}");
    assert!(out.contains("/dev/sda"), "{out}");
}

#[test]
fn a_path_that_is_already_a_longhorn_disk_is_refused() {
    let n = Node::new("existing");
    for path in [
        "/var/mnt/longhorn",
        "/var/mnt/longhorn/",
        "/var/mnt/longhorn/replicas",
        "/var/mnt",
    ] {
        let mut v = lh_node("w-2", "default-disk-1f2e3d4c5b6a");
        v["spec"]["disks"]["extra"] = json!({"path": path, "allowScheduling": true,
            "evictionRequested": false, "storageReserved": 0, "tags": [], "diskType": "filesystem"});
        n.lh("w-2", v);
        let out = n.refused(&["--plan", "w-2", "longhorn"]);
        assert!(out.contains("already a Longhorn disk"), "{path}: {out}");
    }
    // The disk name the verb would use, at another path.
    let mut v = lh_node("w-2", "default-disk-1f2e3d4c5b6a");
    v["spec"]["disks"]["u-longhorn"] = json!({"path": "/srv/x", "allowScheduling": true,
        "evictionRequested": false, "storageReserved": 0, "tags": [], "diskType": "filesystem"});
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("u-longhorn"), "{out}");
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
        vec!["--plan", "w-2", "u-longhorn/x"],
        vec!["w-2", "longhorn"],
        vec!["w-2", "longhorn", "not-a-hash"],
    ] {
        n.refused(&args);
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());

    // A volume the node's declaration does not name: the door reads the
    // mount from the declaration, never from the packet.
    let out = n.refused(&["--plan", "w-2", "data"]);
    assert!(out.contains("UserVolumeConfig"), "{out}");
    assert!(out.contains("patches/w-2.yaml"), "{out}");
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());

    // A control plane, even one declaring the volume, is refused by the
    // registry before any door opens: a mutating node verb acts on
    // workers (the-controls-are-bounded-verbs).
    let w2 = std::fs::read_to_string(n.patches().join("w-2.yaml")).expect("w-2.yaml");
    write_file(&n.patches().join("cp-1.yaml"), &w2);
    let o = n.run(&["--plan", "cp-1", "longhorn"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("talos-worker"), "{out}");
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

#[test]
fn a_declared_volume_talos_has_not_made_ready_is_refused() {
    let n = Node::new("not-ready");
    // Declared, never applied (or no disk matched): Talos holds no volume.
    std::fs::remove_file(n.dir.join("state/vs-10.20.0.16-u-longhorn.json")).unwrap();
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("u-longhorn"), "{out}");
    assert!(
        out.contains("node-converge"),
        "names the door before it: {out}"
    );

    let mut spec = user_volume("longhorn", "", "");
    spec["phase"] = json!("waiting");
    n.state(
        "vs-10.20.0.16-u-longhorn.json",
        &volume_status("10.20.0.16", "u-longhorn", spec),
    );
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("waiting"), "{out}");

    // Mounted anywhere but /var/mnt/<name> is not the declared mount.
    let mut spec = user_volume("longhorn", "/dev/sdb1", "/dev/sdb");
    spec["mountSpec"]["parentId"] = json!("/var/lib");
    n.state(
        "vs-10.20.0.16-u-longhorn.json",
        &volume_status("10.20.0.16", "u-longhorn", spec),
    );
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("/var/mnt/longhorn"), "{out}");
}

/// A Longhorn node that does not read Ready=True is refused: a disk added
/// there could not be read back (review 77ee3857 N3).
#[test]
fn a_longhorn_node_that_is_not_ready_is_refused() {
    let n = Node::new("lh-not-ready");
    let mut v = lh_node("w-2", "default-disk-1f2e3d4c5b6a");
    v["status"]["conditions"][0] =
        json!({"type": "Ready", "status": "False", "reason": "ManagerPodDown"});
    n.lh("w-2", v);
    let out = n.refused(&["--plan", "w-2", "longhorn"]);
    assert!(out.contains("does not read Ready=True"), "{out}");
    assert!(
        out.contains("ManagerPodDown"),
        "names Longhorn's reason: {out}"
    );
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    let n = Node::new("dark");
    let o = n.run(&["--plan", "w-2", "longhorn"], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");

    std::fs::remove_file(n.dir.join("state/vs-10.20.0.16-EPHEMERAL.json")).unwrap();
    let o = n.run(&["--plan", "w-2", "longhorn"], &[]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "no system disk read is no plan: {out}"
    );
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    assert!(!n.patched(), "{}", n.argv());
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_the_new_disk_back() {
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
    let patches: Vec<String> = n
        .argv()
        .lines()
        .filter(|l| l.contains("kubectl --kubeconfig=/kc patch "))
        .map(str::to_string)
        .collect();
    assert_eq!(patches.len(), 1, "one patch: {patches:?}");
    let p = &patches[0];
    assert!(
        p.contains("patch nodes.longhorn.io w-2 -n longhorn-system --type=json"),
        "{p}"
    );
    assert!(
        p.contains(r#"{"op":"test","path":"/spec/disks","value":"#),
        "a compare-and-set on the disks the plan read: {p}"
    );
    assert!(
        p.contains(r#""op":"add","path":"/spec/disks/u-longhorn""#),
        "{p}"
    );
    let now = n.lh_now("w-2");
    assert_eq!(
        now["spec"]["disks"]["u-longhorn"]["path"],
        "/var/mnt/longhorn"
    );
    assert_eq!(now["spec"]["disks"]["u-longhorn"]["allowScheduling"], true);
    let mut others = now["spec"]["disks"].clone();
    others.as_object_mut().unwrap().remove("u-longhorn");
    assert_eq!(others, before, "every other disk as it was");
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(hits[0].contains("u-longhorn"), "{hits:?}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let n = Node::new("drift");
    let (_, hash) = n.plan("w-2", "longhorn");
    // Another disk appeared after the plan was signed.
    let mut v = lh_node("w-2", "default-disk-1f2e3d4c5b6a");
    v["spec"]["disks"]["other"] = json!({"path": "/srv/other", "allowScheduling": false,
        "evictionRequested": false, "storageReserved": 0, "tags": [], "diskType": "filesystem"});
    n.lh("w-2", v);
    let out = n.refused(&["w-2", "longhorn", &hash]);
    assert!(out.contains(&hash), "names the approved hash: {out}");
    n.refused(&["w-2", "longhorn", &"0".repeat(64)]);

    // Applied once, the same approval is spent.
    n.lh("w-2", lh_node("w-2", "default-disk-1f2e3d4c5b6a"));
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let o = n.run(&["w-2", "longhorn", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    let patches = n
        .argv()
        .lines()
        .filter(|l| l.contains("kubectl --kubeconfig=/kc patch "))
        .count();
    assert_eq!(patches, 1, "no second patch: {}", n.argv());
}

#[test]
fn a_disk_longhorn_never_readies_is_not_an_effect() {
    let n = Node::new("notready");
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[("STUB_DISK", "notready")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("NOT proven"), "{out}");
    assert!(
        out.contains("DiskFilesystemChanged"),
        "names Longhorn's own reason: {out}"
    );

    let n = Node::new("patch-refused");
    let (_, hash) = n.plan("w-2", "longhorn");
    let o = n.run(&["w-2", "longhorn", &hash], &[("STUB_PATCH_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("validator.longhorn.io"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(n.lh_now("w-2")["spec"]["disks"].get("u-longhorn").is_none());
}

/// Every conjunct of the read-back is a claim the effect line makes, so
/// each is shown refusing: Longhorn accepted the patch, ONE fact of what it
/// then holds is wrong (STUB_DISK=`mode`), and the run is NOT proven with
/// that fact named and no effect line printed (review 77ee3857 N1: each of
/// these conjuncts could be deleted with the suite still green). One test
/// per conjunct, so they run side by side and a failure names its fact.
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
fn a_new_disk_whose_spec_path_is_not_the_mount_is_not_an_effect() {
    read_back_refuses(
        "wrongpath",
        "spec.disks/u-longhorn reads path /var/mnt/elsewhere",
    );
}

#[test]
fn another_disk_that_changed_with_the_patch_is_not_an_effect() {
    read_back_refuses("otherchanged", "another disk on the node changed");
}

#[test]
fn a_new_disk_longhorn_reports_no_status_for_is_not_an_effect() {
    read_back_refuses("nostatus", "Longhorn reports no status for u-longhorn");
}

#[test]
fn a_ready_disk_with_no_disk_uuid_is_not_an_effect() {
    read_back_refuses("nouuid", "Longhorn reads u-longhorn Ready with no diskUUID");
}

#[test]
fn a_disk_uuid_another_disk_has_is_not_an_effect() {
    read_back_refuses("dupuuid", "which another disk on the node already has");
}

#[test]
fn a_disk_with_no_storage_maximum_is_not_an_effect() {
    read_back_refuses("nomax", "Longhorn reads u-longhorn with no storageMaximum");
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("add-longhorn-disk");
    let p = verb("plan-a-longhorn-disk");
    for (name, v) in [("add-longhorn-disk", &w), ("plan-a-longhorn-disk", &p)] {
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
    assert_eq!(w["plan_verb"], "plan-a-longhorn-disk");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["params"][2]["name"], "plan_sha256");
    assert!(
        w["timeout"].as_u64().unwrap_or(0) > 300,
        "outlasts the script's read-back"
    );
    let vol = w["params"][1]["pattern"].as_str().expect("a pattern");
    for ok in ["longhorn", "gate", "data-1"] {
        assert!(jq_test(vol, ok), "{ok}");
    }
    for bad in ["", "-x", "Longhorn", "../gate", "a/b", "a b", "u-", "x-"] {
        assert!(!jq_test(vol, bad), "the volume pattern admits {bad:?}");
    }
}
