//! `talos-read-diskstats` (`infra/forge/talos-read-diskstats.sh`) is RUN,
//! not read — against fixture `/proc/diskstats` reads and a fixture Talos
//! `disks` resource, behind a stand-in for the forge's talosctl door, so
//! every figure below is one the script actually computed. Nothing here
//! reaches a cluster or needs a credential.
//!
//! WHY THE VERB EXISTS (backlog ba0b7308, design f29798c0 §7, car 1). The
//! PostgreSQL stall of 2026-10-29 was blamed on the control-plane system
//! disks — SAMSUNG MZALQ256 OEM client NVMe on cp-1 and cp-2 — from the
//! drive CLASS, because no door recorded per-device latency (d9b39685
//! `disks_20261029T1957Z`: "INFERRED from the drive class, not measured
//! per device"). The kernel already keeps the numbers: since 5.5 each
//! `/proc/diskstats` line carries flushes completed and the milliseconds
//! spent flushing, beside the write counters. Two reads a fixed interval
//! apart turn them into a mean write latency and a mean flush (fsync)
//! latency per device, and the Talos `disks` resource names the drive.
//!
//! THE FIXTURE is cp-2's shape as `talos-get cp-2 disks` returned it
//! (ops-request 13dceb8b, 2026-10-29): one NVMe system disk, its
//! partitions, Longhorn's iSCSI `VIRTUAL-DISK`s, and loop devices. Its
//! serial and WWIDs are INVENTED (`TESTSERIAL0001`, `eui.000…01`): the
//! first draft carried cp-2's real ones, and this tree publishes to the
//! public mirror, where a drive's identifiers cannot be taken back (review
//! c129255c, B1). The shape is real; the identities are not. The
//! deltas are chosen so each mean is exact: 2500 ms over 1000 writes is
//! 2.50 ms, 400000 ms over 50 flushes is 8000.00 ms — the 8 s WAL-write
//! stall d9b39685 measured, as a device would report it.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/talos-read-diskstats.sh";

/// The stand-in behind the talosctl door (`ops_talosctl`'s `sudo -n
/// docker run … /talosctl --talosconfig=/tc`). It answers `get disks -o
/// json` from `$STUB_STATE/disks.json` and the Nth `read /proc/diskstats`
/// from `$STUB_STATE/diskstats-N`; every door call lands in $STUB_ARGV.
/// STUB_TALOS_FAIL=1 is a node whose Talos API cannot be reached.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in
    --talosconfig=/tc) shift; break ;;
    esac
    shift
done
[ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
ip="$2"; shift 2
if [ -n "${STUB_TALOS_FAIL:-}" ]; then
    echo "error: rpc error: code = Unavailable desc = connection error: dial tcp $ip:50000: connect: no route to host" >&2
    exit 1
fi
case "$*" in
"get disks -o json") cat "$S/disks.json"; exit 0 ;;
"read /proc/diskstats")
    n=$(cat "$S/reads" 2>/dev/null || echo 0)
    n=$((n + 1))
    echo "$n" > "$S/reads"
    cat "$S/diskstats-$n"
    exit 0 ;;
esac
echo "stub: unexpected talosctl call: $*" >&2
exit 2
"#;

/// The Talos `disks` resource as `talosctl get disks -o json` prints it:
/// one pretty-printed object per resource, concatenated.
const DISKS: &str = r#"{
    "node": "10.20.0.12",
    "metadata": {"namespace": "runtime", "type": "Disks.block.talos.dev", "id": "loop0", "phase": "running"},
    "spec": {"dev_path": "/dev/loop0", "size": 4096, "pretty_size": "4.1 kB", "readonly": true, "bus_path": "/virtual"}
}
{
    "node": "10.20.0.12",
    "metadata": {"namespace": "runtime", "type": "Disks.block.talos.dev", "id": "nvme0n1", "phase": "running"},
    "spec": {"dev_path": "/dev/nvme0n1", "size": 256060514304, "pretty_size": "256 GB", "model": "SAMSUNG MZALQ256HBJD-00BL2", "serial": "TESTSERIAL0001", "wwid": "eui.0000000000000001", "transport": "nvme"}
}
{
    "node": "10.20.0.12",
    "metadata": {"namespace": "runtime", "type": "Disks.block.talos.dev", "id": "sda", "phase": "running"},
    "spec": {"dev_path": "/dev/sda", "size": 73014444032, "pretty_size": "73 GB", "model": "VIRTUAL-DISK", "wwid": "naa.00000000000000000000000000000001", "transport": "iscsi"}
}
{
    "node": "10.20.0.12",
    "metadata": {"namespace": "runtime", "type": "Disks.block.talos.dev", "id": "sdb", "phase": "running"},
    "spec": {"dev_path": "/dev/sdb", "size": 10737418240, "pretty_size": "11 GB", "model": "VIRTUAL-DISK", "transport": "iscsi"}
}
"#;

/// `/proc/diskstats` on a 6.18 kernel: major, minor, name, then 17
/// counters — reads, merged, sectors, ms; writes, merged, sectors, ms; in
/// flight, io ms, weighted io ms; discards, merged, sectors, ms; flushes,
/// flush ms.
const READ_1: &str = "   7       0 loop0 96 0 2200 10 0 0 0 0 0 20 10 0 0 0 0 0 0
 259       0 nvme0n1 50000 100 4000000 20000 900000 5000 80000000 2000000 2 3000000 4500000 0 0 0 0 100000 3000000
 259       1 nvme0n1p1 100 0 2000 50 0 0 0 0 0 40 50 0 0 0 0 0 0
 259       6 nvme0n1p6 40000 100 3500000 15000 899000 5000 79990000 1999000 2 2900000 4400000 0 0 0 0 0 0
   8       0 sda 3000 0 240000 900 400000 0 6400000 800000 1 900000 1200000 0 0 0 0 50000 600000
   8      16 sdb 10 0 80 1 0 0 0 0 0 1 1 0 0 0 0 0 0
";

/// The same devices one interval later. nvme0n1: +10 reads in +3 ms,
/// +1000 writes in +2500 ms, +40000 sectors, +200 flushes in +8000 ms.
/// nvme0n1p6: +990 writes in +2400 ms, no flushes (a partition counts
/// none). sda: +400 writes in +32000 ms, +50 flushes in +400000 ms.
/// nvme0n1p1 and sdb: idle. loop0: moved, and never reported.
const READ_2: &str = "   7       0 loop0 196 0 4200 30 0 0 0 0 0 40 30 0 0 0 0 0 0
 259       0 nvme0n1 50010 100 4000800 20003 901000 5000 80040000 2002500 1 3000900 4510000 0 0 0 0 100200 3008000
 259       1 nvme0n1p1 100 0 2000 50 0 0 0 0 0 40 50 0 0 0 0 0 0
 259       6 nvme0n1p6 40000 100 3500000 15000 899990 5000 80029000 2001400 1 2900800 4402400 0 0 0 0 0 0
   8       0 sda 3000 0 240000 900 400400 0 6403200 832000 0 900950 1632000 0 0 0 0 50050 1000000
   8      16 sdb 10 0 80 1 0 0 0 0 0 1 1 0 0 0 0 0 0
";

struct Estate {
    dir: PathBuf,
}

fn needs_tools() {
    // `awk --version` is gawk's; mawk and busybox refuse the flag, so awk
    // is asked to run an empty program instead (measured on the worker,
    // 2026-10-29: the flag read as "no awk" on a box that has one).
    for (tool, args) in [
        ("jq", &["--version"][..]),
        ("curl", &["--version"][..]),
        ("awk", &["BEGIN { exit 0 }"][..]),
    ] {
        let ok = Command::new(tool)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "talos_read_diskstats_sh: no {tool} on this box — the gate image has it, and a \
             test that cannot run must fail, never pass by returning early"
        );
    }
}

fn node(id: &str, address: &str, role: &str, retired: bool) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": retired
    })
}

impl Estate {
    /// The estate of 2026-10-29: control planes and workers the registry
    /// declares, the forge (not a Talos node), and a retired worker.
    fn new(name: &str, read_1: &str, read_2: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        write_file(
            &dir.join("nodes.json"),
            &json!({ "data": [
                node("forge", "10.20.0.15", "forge", false),
                node("cp-2", "10.20.0.12", "talos-control-plane", false),
                node("w-2", "10.20.0.16", "talos-worker", false),
                node("w-0", "10.20.0.9", "talos-worker", true),
            ]})
            .to_string(),
        );
        let e = Estate { dir };
        e.state("disks.json", DISKS);
        e.state("diskstats-1", read_1);
        e.state("diskstats-2", read_2);
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
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_TALOSCTL", self.dir.join("talosctl"))
            .env("BOSS_DISKSTATS_INTERVAL_S", "1")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// Every talosctl call, with the door's own prefix cut off.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                l.split_once("--talosconfig=/tc ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .collect()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The report block for one device: its heading line and the indented
/// lines under it, up to the next device.
fn block(out: &str, device: &str) -> String {
    let mut lines = out
        .lines()
        .skip_while(|l| !l.starts_with(&format!("{device} ")));
    let mut b = match lines.next() {
        Some(h) => format!("{h}\n"),
        None => return String::new(),
    };
    for l in lines.take_while(|l| l.starts_with("    ")) {
        b.push_str(l);
        b.push('\n');
    }
    b
}

#[test]
fn it_reports_each_devices_mean_write_and_flush_latency_and_names_the_drive() {
    let e = Estate::new("talos-read-diskstats", READ_1, READ_2);
    let o = e.run(&["cp-2"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();

    let nvme = block(&stdout, "nvme0n1");
    for want in [
        "SAMSUNG MZALQ256HBJD-00BL2",
        "serial TESTSERIAL0001",
        "256 GB",
        "nvme",
        "writes 1000",
        "2500 ms",
        "mean write 2.50 ms",
        "flushes 200",
        "8000 ms",
        "mean flush 40.00 ms",
        "reads 10",
        "mean read 0.30 ms",
    ] {
        assert!(
            nvme.contains(want),
            "nvme0n1 says {want:?}:\n{nvme}\n--\n{stdout}"
        );
    }

    // The Longhorn volume's block device, as the consumer's node sees it:
    // the 8 s flush is a figure, not an inference.
    let sda = block(&stdout, "sda");
    for want in [
        "VIRTUAL-DISK",
        "iscsi",
        "writes 400",
        "mean write 80.00 ms",
        "flushes 50",
        "mean flush 8000.00 ms",
    ] {
        assert!(sda.contains(want), "sda says {want:?}:\n{sda}");
    }

    // A partition is named as its disk's, and a device that flushed
    // nothing has no mean — never a 0.00 that reads as a fast disk.
    let p6 = block(&stdout, "nvme0n1p6");
    assert!(p6.contains("partition of nvme0n1"), "{p6}");
    assert!(p6.contains("mean write 2.42 ms"), "{p6}");
    assert!(p6.contains("flushes 0"), "{p6}");
    assert!(!p6.contains("mean flush"), "no flushes, no mean: {p6}");
    let idle = block(&stdout, "sdb");
    assert!(idle.contains("writes 0"), "{idle}");
    assert!(!idle.contains("mean write"), "no writes, no mean: {idle}");

    // Loop devices are the Talos image's squashfs, never a disk.
    assert!(block(&stdout, "loop0").is_empty(), "{stdout}");

    // The worst flush is said once, so a reader need not scan.
    assert!(
        stdout.contains("worst mean flush: sda 8000.00 ms over 50 flushes"),
        "{stdout}"
    );
    // The two reads ride beside the figures: a reduction is not the record.
    assert!(
        stdout.contains(
            "read 1: 259 0 nvme0n1 50000 100 4000000 20000 900000 5000 80000000 2000000 2 3000000 4500000 0 0 0 0 100000 3000000"
        ),
        "{stdout}"
    );

    // READ-ONLY, and exactly these three calls: the drive names, then
    // the counters twice, all at the registry's address.
    assert_eq!(
        e.calls(),
        vec![
            "-n 10.20.0.12 get disks -o json",
            "-n 10.20.0.12 read /proc/diskstats",
            "-n 10.20.0.12 read /proc/diskstats",
        ],
    );
}

#[test]
fn it_refuses_a_node_the_registry_does_not_hold_as_a_talos_node_before_any_door() {
    let e = Estate::new("talos-read-diskstats-refusals", READ_1, READ_2);
    for args in [
        vec!["forge"],
        vec!["w-9"],
        vec!["w-0"],
        vec!["W-2"],
        vec!["w-2,cp-2"],
        vec!["10.20.0.16"],
        vec!["w-2", "nvme0n1"],
        vec![],
    ] {
        let o = e.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(e.calls().is_empty(), "talosctl never ran: {:?}", e.calls());
    // The interval is a test seam with a bound, never an unbounded sleep.
    for bad in ["0", "61", "ten", ""] {
        let o = e.run(&["w-2"], &[("BOSS_DISKSTATS_INTERVAL_S", bad)]);
        assert_eq!(o.status.code(), Some(78), "{bad:?}: {}", text(&o));
    }
    assert!(e.calls().is_empty(), "talosctl never ran: {:?}", e.calls());
}

#[test]
fn a_read_that_cannot_look_is_a_failure_that_says_what_talosctl_said() {
    let e = Estate::new("talos-read-diskstats-dark", READ_1, READ_2);
    let o = e.run(&["w-2"], &[("STUB_TALOS_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("no route to host"), "{out}");
    assert!(
        !out.contains("mean write"),
        "no figures from no read: {out}"
    );

    // An answer that is not /proc/diskstats is not an idle node.
    let e = Estate::new("talos-read-diskstats-garbled", "not diskstats\n", "\n");
    let o = e.run(&["w-2"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("did not read as /proc/diskstats"), "{out}");
}

#[test]
fn an_unreadable_counter_is_named_never_read_as_zero() {
    // nvme0n1 on a kernel older than 5.5: 14 fields, no flush counters.
    // sda's counters went BACKWARDS between the reads. sdc appeared only
    // in the second read.
    let read_1 = " 259 0 nvme0n1 1 0 8 1 100 0 800 250 0 300 251
   8 0 sda 10 0 80 5 900 0 7200 1800 0 1000 1805 0 0 0 0 10 50
";
    let read_2 = " 259 0 nvme0n1 1 0 8 1 200 0 1600 500 0 600 501
   8 0 sda 10 0 80 5 100 0 800 200 0 100 205 0 0 0 0 1 5
   8 32 sdc 1 0 8 1 1 0 8 1 0 1 1 0 0 0 0 1 1
";
    let e = Estate::new("talos-read-diskstats-odd", read_1, read_2);
    let o = e.run(&["w-2"], &[]);
    let out = text(&o);
    assert!(
        o.status.success(),
        "the rest of the node is still read: {out}"
    );
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    let nvme = block(&stdout, "nvme0n1");
    assert!(nvme.contains("mean write 2.50 ms"), "{nvme}");
    assert!(
        nvme.contains("flushes unread: this kernel's /proc/diskstats has 14 fields"),
        "{nvme}"
    );
    let sda = block(&stdout, "sda");
    assert!(sda.contains("counters went backwards"), "{sda}");
    assert!(!sda.contains("mean"), "{sda}");
    let sdc = block(&stdout, "sdc");
    assert!(sdc.contains("in only one of the two reads"), "{sdc}");
    assert!(
        stdout.contains("worst mean flush: none"),
        "no device has a flush figure, and it says so: {stdout}"
    );
}

#[test]
fn a_32_bit_millisecond_wrap_is_named_a_wrap_and_a_reset_stays_a_reset() {
    // The kernel prints the time fields — 7, 11, 13, 14, 18 and 20 — as
    // `%u`, a 32-bit unsigned (block/genhd.c diskstats_show), while the
    // I/O counts are 64-bit. Summed per-request time on a busy device with
    // a deep queue passes 2^32 ms in days (review c129255c, N1), and an
    // interval spanning that reads as a NEGATIVE delta.
    //
    // nvme0n1 wraps write ms, weighted ms and flush ms while its writes
    // and flushes advance: modulo 2^32 the deltas are the very ones the
    // first test uses (+2500 ms over +1000 writes, +8000 ms over +200
    // flushes), so the means are 2.50 and 40.00 ms.
    // sda is a genuine reset: its counts fell as well as its times.
    // sdb's write ms fell while it completed nothing: a wrap needs time
    // to accrue, and time accrues only as requests finish, so that is no
    // wrap. sdc's write ms fell from a value no 32-bit field can hold.
    let read_1 = " 259 0 nvme0n1 100 0 800 50 900000 5000 80000000 4294967000 2 3000000 4294967000 0 0 0 0 100000 4294966296
   8 0 sda 10 0 80 5 900 0 7200 1800 0 1000 1805 0 0 0 0 10 50
   8 16 sdb 10 0 80 5 50 0 400 4294967000 0 100 4294967005 0 0 0 0 0 0
   8 32 sdc 10 0 80 5 50 0 400 5000000000 0 100 5000000005 0 0 0 0 0 0
";
    let read_2 =
        " 259 0 nvme0n1 110 0 880 53 901000 5000 80040000 2204 1 3000900 9704 0 0 0 0 100200 7000
   8 0 sda 10 0 80 5 100 0 800 200 0 100 205 0 0 0 0 1 5
   8 16 sdb 10 0 80 5 50 0 400 100 0 100 105 0 0 0 0 0 0
   8 32 sdc 10 0 80 5 60 0 480 100 0 110 105 0 0 0 0 0 0
";
    let e = Estate::new("talos-read-diskstats-wrap", read_1, read_2);
    let o = e.run(&["w-2"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();

    let nvme = block(&stdout, "nvme0n1");
    for want in [
        "wrapped",
        "32-bit",
        "writes 1000",
        "2500 ms",
        "mean write 2.50 ms",
        "flushes 200",
        "8000 ms",
        "mean flush 40.00 ms",
        "mean read 0.30 ms",
    ] {
        assert!(nvme.contains(want), "nvme0n1 says {want:?}:\n{nvme}");
    }
    assert!(
        !nvme.contains("went backwards"),
        "a wrap is not a reset: {nvme}"
    );
    assert!(
        stdout.contains("worst mean flush: nvme0n1 40.00 ms over 200 flushes"),
        "{stdout}"
    );

    for dev in ["sda", "sdb", "sdc"] {
        let b = block(&stdout, dev);
        assert!(
            b.contains(
                "counters went backwards between the two reads (the device was reset or re-created)"
            ),
            "{dev} is a reset, named so: {b}"
        );
        assert!(!b.contains("wrapped"), "{dev} is not a wrap: {b}");
        assert!(!b.contains("mean"), "{dev} gets no figure: {b}");
    }
}

#[test]
fn a_recreated_device_is_not_a_wrap_and_busy_time_is_bounded_by_the_interval() {
    let e = Estate::new(
        "talos-read-diskstats-recreated",
        "8 0 sda 10 0 80 5 50 0 400 900 0 100 905 0 0 0 0 5 60\n",
        "8 0 sda 20 0 160 2 60 0 480 100 0 50 102 0 0 0 0 6 10\n",
    );
    let o = e.run(&["w-2"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let b = block(&out, "sda");
    assert!(b.contains("reset or re-created"), "{out}");
    assert!(!b.contains("mean"), "{out}");
    assert!(!out.contains("worst mean write: sda"), "{out}");
    assert!(!out.contains("worst mean flush: sda"), "{out}");

    // Strict half-range: exactly 2^31 cannot distinguish a wrap from a
    // reset. Just below it remains a wrap; summed request time is NOT
    // bounded by wall time, but the device's busy milliseconds are.
    for (end, reset) in [("0", true), ("0", false)] {
        let start = if reset { "2147483648" } else { "2147483649" };
        let before = format!("8 0 sda 10 0 80 5 50 0 400 {start} 0 100 905 0 0 0 0 5 60\n");
        let after = format!("8 0 sda 20 0 160 6 60 0 480 {end} 0 2100 2905 0 0 0 0 6 70\n");
        let e = Estate::new("talos-read-diskstats-half-range", &before, &after);
        let o = e.run(&["w-2"], &[]);
        let out = text(&o);
        assert!(o.status.success(), "{out}");
        let b = block(&out, "sda");
        if reset {
            assert!(b.contains("reset or re-created"), "{out}");
        } else {
            assert!(b.contains("2147483647 ms spent"), "{out}");
            assert!(b.contains("busy 100.0%"), "{out}");
        }
    }
}

#[test]
fn the_verb_file_bounds_what_a_packet_can_ask() {
    let path = repo_root().join("infra/ops/verbs/talos-read-diskstats.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the verb file"))
        .expect("the verb file is JSON");
    assert_eq!(
        v["hosts"],
        json!(["forge"]),
        "the talosconfig lives on the forge"
    );
    assert_eq!(v["argv"], json!([SCRIPT, "{1}"]), "the node, nothing else");
    assert_eq!(v["params"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["params"][0]["name"], "node");
    assert!(
        v["params"][0].get("default").is_none(),
        "a node verb names its node"
    );
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
    assert!(v["effect"].is_null(), "a read declares no effect");
    assert!(
        v.get("requires_approval").is_none(),
        "a read needs no passkey"
    );
    // The bound the about states: registry 15 s, a first image pull 120 s,
    // three talosctl calls at 20 s, and the 30 s interval — 225 s, plus
    // the containers' own start time, which no inner timeout bounds.
    let t = v["timeout"].as_u64().expect("a timeout");
    assert!((225..=300).contains(&t), "timeout {t}");
}
