//! The passkey door that drops ONE Talos partition on ONE worker:
//! `plan-a-partition-wipe` (`infra/forge/wipe-a-partition.sh --plan`) and
//! `wipe-a-partition` (the write) — backlog c98e3425, door 4 of the
//! gate-volume move David ordered on 88b35891 (2026-11-07).
//!
//! WHY. Moving w-1's gate volume off the Kingston without a reboot is
//! retire the UserVolumeConfig, drop its partition, converge the then-
//! ABSENT entry, prove. Talos provisions a user volume only where no
//! partition already carries its label, so the drop is what frees it — and
//! it is the most destructive verb after commission-a-disk. On origin/main
//! 226e15c7 no verb could do it, so it would have been `talosctl wipe`
//! typed by hand with the admin talosconfig.
//!
//! WHAT IS PINNED. Every refusal the packet's finish line names, each
//! shown to have written nothing (no `wipe` reached the door): a malformed
//! name, a partition of the system disk, a partition a live volume still
//! owns, one that is mounted, one whose disk carries another partition,
//! one whose mount path is a Longhorn disk — plus a partition Talos does
//! not discover, one that is not a user volume's, a control plane, and a
//! read that cannot look. The write runs only the signed plan, refuses a
//! drifted one, sends exactly `wipe disk <partition> --drop-partition`,
//! and prints its declared effect only after reading the disk back.
//!
//! HOW. As node_maintenance_verbs_sh.rs: the script runs for real, with
//! `sudo` stubbed on PATH as a small stateful node behind the two doors
//! (`ops_kubectl` and `ops_talosctl`), and the estate registry served from
//! a file. Talos's resources are JSON arrays under $STUB_STATE, printed
//! the way `talosctl get -o json` prints them (one object per resource,
//! each carrying `node`); a wipe the stub accepts removes the partition
//! from `discoveredvolumes`, as Talos's discovery would. Nothing here
//! reaches a node.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const WIPE: &str = "infra/forge/wipe-a-partition.sh";
const W1: &str = "10.20.0.14";

/// The stand-in behind both doors. State under $STUB_STATE: `ip-<node>`
/// its InternalIP, `cp-<node>` a control-plane label, `lh-<node>.json`
/// Longhorn's node object, `talos-<resource>.json` a Talos resource list
/// (an array), `mounts.txt` what `talosctl mounts` prints. Switches:
/// STUB_TALOS_FAIL=<word> (that talosctl call cannot reach the node),
/// STUB_WIPE_NOOP=1 (a wipe accepted that changes nothing).
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
        [ -f "$S/lh-$3.json" ] || { echo "Error from server (NotFound): nodes.longhorn.io \"$3\" not found" >&2; exit 1; }
        cat "$S/lh-$3.json"
        exit 0 ;;
    esac
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2
fi
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    shift 2
    word="$1"
    [ "$1" = get ] && word="$2"
    if [ -n "${STUB_TALOS_FAIL:-}" ] && [ "$STUB_TALOS_FAIL" = "$word" ]; then
        echo "error: rpc error: code = Unavailable desc = connection error" >&2
        exit 1
    fi
    case "$1" in
    get)
        [ "$3 $4" = "-o json" ] || { echo "stub: talosctl get without -o json: $*" >&2; exit 2; }
        f="$S/talos-$2.json"
        [ -f "$f" ] || echo '[]' > "$f"
        jq -c '.[]' "$f"
        exit 0 ;;
    mounts)
        cat "$S/mounts.txt"
        exit 0 ;;
    wipe)
        [ "$2" = disk ] || { echo "stub: unexpected wipe: $*" >&2; exit 2; }
        if [ -z "${STUB_WIPE_NOOP:-}" ]; then
            jq --arg p "$3" '[.[] | select(.metadata.id != $p)]' "$S/talos-discoveredvolumes.json" > "$S/dv.next" \
                && mv "$S/dv.next" "$S/talos-discoveredvolumes.json"
        fi
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
            "wipe_a_partition_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn node(id: &str, address: &str, role: &str) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": false
    })
}

/// One Talos resource as `talosctl get -o json` prints it.
fn res(kind: &str, id: &str, spec: Value) -> Value {
    json!({
        "node": W1,
        "metadata": {"namespace": "runtime", "type": kind, "id": id, "version": "3",
                     "owner": "block.DiscoveryController", "phase": "running",
                     "created": "2026-11-07T01:00:00Z", "updated": "2026-11-07T01:00:00Z"},
        "spec": spec
    })
}

fn disk(id: &str, model: &str, serial: &str, size: u64, pretty: &str) -> Value {
    res(
        "Disks.block.talos.dev",
        id,
        json!({"dev_path": format!("/dev/{id}"), "size": size, "pretty_size": pretty,
               "model": model, "serial": serial, "wwid": format!("eui.{serial}"),
               "transport": "nvme", "readonly": false}),
    )
}

fn whole(id: &str, table: Option<&str>) -> Value {
    let mut s = json!({"dev_path": format!("/dev/{id}"), "type": "disk", "size": 1});
    if let Some(t) = table {
        s["name"] = json!(t);
    }
    res("DiscoveredVolumes.block.talos.dev", id, s)
}

fn part(id: &str, parent: &str, index: u32, label: &str, fs: &str, size: u64) -> Value {
    res(
        "DiscoveredVolumes.block.talos.dev",
        id,
        json!({"dev_path": format!("/dev/{id}"), "type": "partition",
               "parent": parent, "parent_dev_path": format!("/dev/{parent}"),
               "size": size, "pretty_size": "1.0 TB", "partition_index": index,
               "partition_label": label, "partition_uuid": format!("puuid-{id}"),
               "name": fs, "uuid": format!("fsuuid-{id}"), "label": ""}),
    )
}

fn volume(id: &str, location: &str, parent: &str) -> Value {
    res(
        "VolumeStatuses.block.talos.dev",
        id,
        json!({"phase": "ready", "type": "partition", "location": location,
               "parentLocation": parent, "filesystem": "xfs"}),
    )
}

impl Estate {
    /// w-1 as door 4 of the move expects to find it: the gate volume
    /// retired (no volumestatus u-gate, nothing mounted from the
    /// Kingston), its one partition `u-gate` still on the disk, the two
    /// Micron drives blank, Longhorn's one disk on the system disk.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("wipe-a-partition-{name}"));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let e = Estate { dir };
        write_file(
            &e.dir.join("nodes.json"),
            &json!({"data": [
                node("forge", "10.20.0.15", "forge"),
                node("cp-1", "10.20.0.11", "talos-control-plane"),
                node("w-1", W1, "talos-worker"),
            ]})
            .to_string(),
        );
        e.state("ip-w-1", W1);
        e.state("ip-cp-1", "10.20.0.11");
        e.state("cp-cp-1", "");
        e.talos(
            "systemdisk",
            json!([res(
                "SystemDisks.block.talos.dev",
                "system-disk",
                json!({"diskID": "nvme0n1", "devPath": "/dev/nvme0n1"})
            )]),
        );
        e.talos(
            "disks",
            json!([
                disk(
                    "nvme0n1",
                    "SAMSUNG MZVL2512HCJQ",
                    "TESTSYS0001",
                    512_110_190_592,
                    "512 GB"
                ),
                disk(
                    "nvme1n1",
                    "KINGSTON SNV3S1000G",
                    "TESTKINGSTON01",
                    1_000_204_886_016,
                    "1.0 TB"
                ),
                disk(
                    "nvme2n1",
                    "Micron_7400_MTFDKBG1T9TDZ",
                    "TESTMICRON0001",
                    1_920_383_410_176,
                    "1.9 TB"
                ),
                disk(
                    "nvme3n1",
                    "Micron_7400_MTFDKBG1T9TDZ",
                    "TESTMICRON0002",
                    1_920_383_410_176,
                    "1.9 TB"
                ),
            ]),
        );
        e.discovered(json!([]));
        e.talos(
            "volumestatus",
            json!([
                volume("EPHEMERAL", "/dev/nvme0n1p6", "/dev/nvme0n1"),
                volume("STATE", "/dev/nvme0n1p5", "/dev/nvme0n1"),
            ]),
        );
        e.state(
            "mounts.txt",
            "NODE         FILESYSTEM       SIZE(GB)   USED(GB)   AVAILABLE(GB)   PERCENT USED   MOUNTED ON\n\
             10.20.0.14   /dev/nvme0n1p6   480.00     120.00     360.00          25.00%         /var\n\
             10.20.0.14   /dev/nvme0n1p5   0.10       0.01       0.09            10.00%         /system/state\n\
             10.20.0.14   overlay          480.00     120.00     360.00          25.00%         /var/lib/containerd/x\n",
        );
        e.longhorn(
            json!({"default-disk-ab12": {"path": "/var/lib/longhorn", "allowScheduling": true}}),
        );
        e
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn talos(&self, resource: &str, list: Value) {
        self.state(&format!("talos-{resource}.json"), &list.to_string());
    }

    /// The discovered volumes: the system disk's six partitions and the
    /// three data disks, plus `extra`.
    fn discovered(&self, extra: Value) {
        let mut all = vec![
            whole("nvme0n1", Some("gpt")),
            part("nvme0n1p1", "nvme0n1", 1, "EFI", "vfat", 1),
            part("nvme0n1p2", "nvme0n1", 2, "BIOS", "", 1),
            part("nvme0n1p3", "nvme0n1", 3, "BOOT", "xfs", 1),
            part("nvme0n1p4", "nvme0n1", 4, "META", "talosmeta", 1),
            part("nvme0n1p5", "nvme0n1", 5, "STATE", "xfs", 1),
            part("nvme0n1p6", "nvme0n1", 6, "EPHEMERAL", "xfs", 1),
            whole("nvme1n1", Some("gpt")),
            part(
                "nvme1n1p1",
                "nvme1n1",
                1,
                "u-gate",
                "xfs",
                1_000_202_043_392,
            ),
            whole("nvme2n1", None),
            whole("nvme3n1", None),
        ];
        all.extend(extra.as_array().cloned().unwrap_or_default());
        self.talos("discoveredvolumes", Value::Array(all));
    }

    fn longhorn(&self, disks: Value) {
        self.state(
            "lh-w-1.json",
            &json!({"kind": "Node", "metadata": {"name": "w-1", "namespace": "longhorn-system"},
                    "spec": {"disks": disks}})
            .to_string(),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(WIPE))
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
            .env("BOSS_WIPE_WAIT_S", "3")
            .env("BOSS_WIPE_POLL_S", "1")
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
                    .or_else(|| l.split_once("--talosconfig=/tc "))
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .collect()
    }

    fn wiped(&self) -> bool {
        self.calls().iter().any(|c| c.contains(" wipe "))
    }

    fn plan(&self) -> (String, String) {
        let o = self.run(&["--plan", "w-1", "nvme1n1p1"], &[]);
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

    /// The plan is REFUSED (exit 78), saying `why`, and nothing was wiped.
    fn refused(&self, partition: &str, why: &str) {
        let o = self.run(&["--plan", "w-1", partition], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "refused: {out}");
        assert!(out.contains(why), "the refusal says {why:?}: {out}");
        assert!(o.stdout.is_empty(), "a refusal renders no plan: {out}");
        let o = self.run(&["w-1", partition, &"a".repeat(64)], &[]);
        assert_eq!(
            o.status.code(),
            Some(78),
            "the write refuses too: {}",
            text(&o)
        );
        assert!(!self.wiped(), "nothing was wiped: {:?}", self.calls());
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
    let re = verb("wipe-a-partition")["effect"]
        .as_str()
        .expect("wipe-a-partition declares an `effect`")
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

// ------------------------------------------------------------------ the plan

#[test]
fn the_plan_names_the_partition_its_disk_and_the_exact_act() {
    let e = Estate::new("plan");
    let (plan, hash) = e.plan();
    for want in [
        "node: w-1",
        "address: 10.20.0.14",
        "estate role: talos-worker",
        "partition: nvme1n1p1 (/dev/nvme1n1p1)",
        "partition label: u-gate",
        "partition index: 1",
        "partition uuid: puuid-nvme1n1p1",
        "filesystem: xfs (uuid fsuuid-nvme1n1p1",
        "1000202043392 bytes",
        "parent disk: nvme1n1 (/dev/nvme1n1) — KINGSTON SNV3S1000G, serial TESTKINGSTON01",
        "system disk: nvme0n1",
        "other partitions on nvme1n1: none",
        "volumes on nvme1n1 (volumestatus): none",
        "mounted from nvme1n1 (talosctl mounts): nothing",
        "default-disk-ab12 at /var/lib/longhorn",
        "command: talosctl -n 10.20.0.14 wipe disk nvme1n1p1 --drop-partition",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(!e.wiped(), "the plan wipes nothing: {:?}", e.calls());
    assert!(
        effect_lines(&plan).is_empty(),
        "no plan line reads as the effect: {plan}"
    );
    // Deterministic, and blind to what moves without the disk moving: a
    // resource version is not part of what David signs.
    let list =
        std::fs::read_to_string(e.dir.join("state/talos-discoveredvolumes.json")).expect("dv");
    e.state(
        "talos-discoveredvolumes.json",
        &list.replace("\"version\":\"3\"", "\"version\":\"9\""),
    );
    assert_eq!(e.plan(), (plan, hash));
}

#[test]
fn a_name_that_is_not_one_nvme_partition_is_refused_before_any_door() {
    let e = Estate::new("bad-name");
    for bad in [
        "nvme1n1",
        "sda1",
        "nvme1n1p1x",
        "/dev/nvme1n1p1",
        "nvme1n1p",
        "-nvme1n1p1",
        "nvme1n1p1 nvme0n1p6",
        "",
    ] {
        let o = e.run(&["--plan", "w-1", bad], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{bad:?}: {out}");
        assert!(
            out.contains("^nvme[0-9]+n[0-9]+p[0-9]+$"),
            "{bad:?} names the shape: {out}"
        );
    }
    assert!(
        e.calls().is_empty(),
        "a malformed name opens no door: {:?}",
        e.calls()
    );
}

#[test]
fn a_partition_of_the_system_disk_is_refused() {
    let e = Estate::new("system-disk");
    e.refused("nvme0n1p6", "system disk");
}

#[test]
fn a_partition_on_the_disk_ephemeral_lives_on_is_refused() {
    // Talos's systemdisk and the disk EPHEMERAL lives on are both read;
    // either one names a disk this verb never touches.
    let e = Estate::new("ephemeral-disk");
    e.talos(
        "volumestatus",
        json!([
            volume("EPHEMERAL", "/dev/nvme1n1p2", "/dev/nvme1n1"),
            volume("STATE", "/dev/nvme0n1p5", "/dev/nvme0n1"),
        ]),
    );
    e.refused("nvme1n1p1", "EPHEMERAL");
}

#[test]
fn a_partition_a_live_volume_still_owns_is_refused() {
    let e = Estate::new("live-volume");
    e.talos(
        "volumestatus",
        json!([
            volume("EPHEMERAL", "/dev/nvme0n1p6", "/dev/nvme0n1"),
            volume("u-gate", "/dev/nvme1n1p1", "/dev/nvme1n1"),
        ]),
    );
    e.refused("nvme1n1p1", "u-gate (phase ready");

    // Owned by its label alone, whatever location the status reports.
    let e = Estate::new("live-volume-label");
    e.talos(
        "volumestatus",
        json!([
            volume("EPHEMERAL", "/dev/nvme0n1p6", "/dev/nvme0n1"),
            volume("u-gate", "", ""),
        ]),
    );
    e.refused("nvme1n1p1", "retire-a-user-volume");
}

#[test]
fn a_mounted_partition_is_refused() {
    let e = Estate::new("mounted");
    e.state(
        "mounts.txt",
        "NODE         FILESYSTEM       SIZE(GB)   USED(GB)   AVAILABLE(GB)   PERCENT USED   MOUNTED ON\n\
         10.20.0.14   /dev/nvme0n1p6   480.00     120.00     360.00          25.00%         /var\n\
         10.20.0.14   /dev/nvme1n1p1   1000.00    74.00      926.00          7.40%          /var/mnt/gate\n",
    );
    e.refused("nvme1n1p1", "/dev/nvme1n1p1");
}

#[test]
fn a_mount_read_that_lists_no_device_is_not_a_clean_read() {
    let e = Estate::new("mounts-empty");
    e.state("mounts.txt", "NODE   FILESYSTEM   MOUNTED ON\n");
    let o = e.run(&["--plan", "w-1", "nvme1n1p1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(o.stdout.is_empty(), "{out}");
    assert!(!e.wiped(), "{:?}", e.calls());
}

#[test]
fn a_disk_that_carries_another_partition_is_refused() {
    let e = Estate::new("sibling");
    e.discovered(json!([part(
        "nvme1n1p2",
        "nvme1n1",
        2,
        "u-other",
        "xfs",
        1_000
    )]));
    e.refused("nvme1n1p1", "nvme1n1p2");
}

#[test]
fn a_partition_that_is_not_a_user_volumes_is_refused() {
    let e = Estate::new("not-user");
    e.discovered(json!([
        whole("nvme4n1", Some("gpt")),
        part("nvme4n1p1", "nvme4n1", 1, "STATE", "xfs", 1_000),
    ]));
    e.refused("nvme4n1p1", "u-<name>");
}

#[test]
fn a_partition_talos_does_not_discover_is_refused() {
    let e = Estate::new("absent");
    e.refused("nvme5n1p1", "discovers no partition nvme5n1p1");
}

#[test]
fn a_longhorn_disk_is_refused() {
    let e = Estate::new("longhorn");
    e.longhorn(json!({
        "default-disk-ab12": {"path": "/var/lib/longhorn", "allowScheduling": true},
        "u-gate": {"path": "/var/mnt/gate/", "allowScheduling": false}
    }));
    e.refused("nvme1n1p1", "Longhorn disk u-gate at /var/mnt/gate/");
}

#[test]
fn a_control_plane_is_refused_by_the_estate_registry() {
    let e = Estate::new("control-plane");
    let o = e.run(&["--plan", "cp-1", "nvme1n1p1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("talos-control-plane"), "{out}");
    assert!(
        !e.calls().iter().any(|c| c.contains("talos")),
        "refused before any talosctl call: {:?}",
        e.calls()
    );
}

#[test]
fn a_read_that_cannot_look_renders_no_plan() {
    for (what, env) in [
        ("discoveredvolumes", "discoveredvolumes"),
        ("volumestatus", "volumestatus"),
        ("systemdisk", "systemdisk"),
        ("disks", "disks"),
        ("mounts", "mounts"),
    ] {
        let e = Estate::new(&format!("unread-{what}"));
        let o = e.run(&["--plan", "w-1", "nvme1n1p1"], &[("STUB_TALOS_FAIL", env)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{what}: {out}");
        assert!(o.stdout.is_empty(), "{what}: {out}");
        assert!(out.contains(what), "{what} is named: {out}");
    }
    // Longhorn's node unread is not "no Longhorn disk".
    let e = Estate::new("unread-longhorn");
    std::fs::remove_file(e.dir.join("state/lh-w-1.json")).expect("rm");
    let o = e.run(&["--plan", "w-1", "nvme1n1p1"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(o.stdout.is_empty(), "{}", text(&o));
}

/// A Longhorn read that SUCCEEDS with nothing in it is not "no Longhorn
/// disk" either. On jq-1.6 `jq -e` over an input carrying no document
/// exits 0 (infra/lib/jq.sh, backlog d96e38ab), so a bare `jq -e
/// '.spec.disks | type == "object"'` passed silence, the disk list read
/// empty, and the Longhorn refusal was skipped — a clearance nobody read.
/// Caught by the pre-flight's a_jq_guard_asks_whether_there_is_a_document
/// pin on this car's first run (c440c2f1, 2026-11-07).
#[test]
fn a_longhorn_read_that_answers_silence_is_not_a_clean_read() {
    for (what, body) in [("empty", ""), ("whitespace", "  \n\n")] {
        let e = Estate::new(&format!("silent-longhorn-{what}"));
        e.state("lh-w-1.json", body);
        let o = e.run(&["--plan", "w-1", "nvme1n1p1"], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{what}: {out}");
        assert!(o.stdout.is_empty(), "{what} renders no plan: {out}");
        assert!(
            out.contains("nodes.longhorn.io"),
            "{what} names the read: {out}"
        );
    }
}

// ----------------------------------------------------------------- the write

#[test]
fn the_write_runs_only_the_signed_plan_and_reads_the_partition_gone() {
    let e = Estate::new("write");
    let (plan, hash) = e.plan();
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let wipes: Vec<String> = e
        .calls()
        .into_iter()
        .filter(|c| c.contains(" wipe "))
        .collect();
    assert_eq!(
        wipes,
        vec![format!("-n {W1} wipe disk nvme1n1p1 --drop-partition")],
        "exactly one wipe, of the named partition, dropping it"
    );
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("nvme1n1p1 is gone from w-1")
            && hits[0].contains("KINGSTON SNV3S1000G, serial TESTKINGSTON01")
            && hits[0].contains("carries no partition table entry for it"),
        "{hits:?}"
    );
}

#[test]
fn the_write_refuses_a_plan_that_drifted() {
    let e = Estate::new("drift");
    let (_, hash) = e.plan();
    // The filesystem on the partition changed after the plan was signed:
    // the passkey never saw this disk in this state.
    let list =
        std::fs::read_to_string(e.dir.join("state/talos-discoveredvolumes.json")).expect("dv");
    e.state(
        "talos-discoveredvolumes.json",
        &list.replace("fsuuid-nvme1n1p1", "fsuuid-reformatted"),
    );
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    assert!(
        out.contains("fsuuid-reformatted"),
        "shows today's plan: {out}"
    );
    assert!(!e.wiped(), "{:?}", e.calls());

    for bad in ["0".repeat(64), "not-a-hash".to_string(), "A".repeat(64)] {
        let o = e.run(&["w-1", "nvme1n1p1", &bad], &[]);
        assert_eq!(o.status.code(), Some(78), "{bad}: {}", text(&o));
    }
    assert!(!e.wiped(), "{:?}", e.calls());
}

#[test]
fn a_second_run_of_an_applied_plan_is_refused() {
    let e = Estate::new("twice");
    let (_, hash) = e.plan();
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let before = e.calls().len();
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        !e.calls()[before..].iter().any(|c| c.contains(" wipe ")),
        "no second wipe: {:?}",
        e.calls()
    );
}

#[test]
fn a_wipe_the_disk_never_took_is_not_an_effect() {
    let e = Estate::new("noop");
    let (_, hash) = e.plan();
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[("STUB_WIPE_NOOP", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("NOT proven gone"), "{out}");
}

#[test]
fn a_wipe_talosctl_refused_is_a_failure_that_says_so() {
    let e = Estate::new("wipe-fails");
    let (_, hash) = e.plan();
    let o = e.run(&["w-1", "nvme1n1p1", &hash], &[("STUB_TALOS_FAIL", "wipe")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("Unavailable"), "talosctl's own words: {out}");
}

// ------------------------------------------------------------ the verb files

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("wipe-a-partition");
    let p = verb("plan-a-partition-wipe");
    for (name, v) in [("wipe-a-partition", &w), ("plan-a-partition-wipe", &p)] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], WIPE, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        assert_eq!(v["params"][1]["name"], "partition", "{name}");
        assert_eq!(
            v["params"][1]["pattern"], "^nvme[0-9]+n[0-9]+p[0-9]+$",
            "{name}: one NVMe partition's kernel name, nothing else"
        );
        assert!(
            v["about"].as_str().unwrap_or_default().contains(
                "talosctl -n <the registry's address> wipe disk <partition> --drop-partition"
            ),
            "{name} records the exact argv"
        );
    }
    assert!(w["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-partition-wipe");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["argv"], json!([WIPE, "{1}", "{2}", "{3}"]));
    assert_eq!(w["params"][2]["name"], "plan_sha256");
    assert!(w["effect"].is_string(), "the write declares its effect");
    assert!(
        p["about"]
            .as_str()
            .unwrap_or_default()
            .contains("READ-ONLY")
    );
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_ne!(p["requires_approval"], true);
    assert_eq!(p["argv"], json!([WIPE, "--plan", "{1}", "{2}"]));
}
