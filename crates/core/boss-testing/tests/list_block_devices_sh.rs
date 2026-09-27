//! `list-block-devices` (`infra/forge/list-block-devices.sh`) — the
//! read verb that tells an operator which `/dev/disk/by-id` name the
//! forge's blank drive has, and marks the disks `commission-a-disk`
//! would accept.
//!
//! WHY (backlog 88eda2eb, 2026-09-27). Filing `plan-a-disk-commission`
//! needs the new 1 TB drive's by-id name, and no read the system had
//! could produce it — `disk-report` lists mounted filesystems only, the
//! estate registry holds no block-device facts, and the journal door
//! showed no kernel NVMe lines — so the only way was a person running
//! `ls` on the host. The candidate mark must be commission-a-disk's own
//! judgement, or the listing and the verb can disagree about the one
//! disk that matters: so the script SOURCES
//! `commission-a-disk.judge.sh`, and this file pins its fact reads to
//! the destructive script's, command for command (CLAUDE.md §9a).
//!
//! The script is RUN here. No test box has a block device (the gate
//! pod and the dev pod have none — `commission-a-disk.judge.sh`'s
//! header), so `lsblk`, `wipefs` and `findmnt` are stubs on PATH that
//! answer for a fixture host shaped like the forge as measured on
//! 2026-09-26 — root on the second NVMe's p2, EFI on its p1, and the
//! new drive blank — and `/dev/disk/by-id` is a scratch directory of
//! links the script is pointed at through `BOSS_BY_ID_DIR`. `readlink`
//! and `jq` are the real ones, because the script resolves a link and
//! tests the verb's pattern exactly the way the runner does.

use boss_testing::repo_root;
use boss_testing::scratch::{scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/forge/list-block-devices.sh";
const COMMISSION: &str = "infra/forge/commission-a-disk.sh";

const BLANK_NAME: &str = "nvme-FIXTURE_NVMe_1TB_BLANK0001";
const BLANK_EUI: &str = "nvme-eui.00000000000000aa";
const ROOT_NAME: &str = "nvme-FIXTURE_NVMe_512GB_ROOT0001";

struct Dev {
    kname: &'static str,
    dtype: &'static str,
    size: &'static str,
    model: &'static str,
    serial: &'static str,
    /// Its own mountpoint, if any.
    mount: &'static str,
    /// Everything lsblk lists below it, in order.
    below: Vec<&'static str>,
    /// What `wipefs -n -i -O TYPE` finds, one per line.
    sigs: &'static str,
}

struct Host {
    dir: PathBuf,
    devs: Vec<Dev>,
    /// `findmnt -nvro SOURCE /`; `None`: findmnt fails.
    root_src: Option<&'static str>,
    /// The chain `lsblk -nrso KNAME <root_src>` answers.
    root_chain: &'static str,
    /// by-id link name -> kernel name it resolves to.
    by_id: Vec<(&'static str, &'static str)>,
    /// Stub invocations (the whole argv, space-joined) that fail.
    fail: Vec<String>,
}

impl Host {
    /// The forge as measured: nvme1n1 carries / (p2) and /boot/efi
    /// (p1); nvme0n1 is the new drive, blank.
    fn forge(case: &str) -> Self {
        Host {
            dir: scratch_dir(&format!("list-block-devices-{case}")),
            devs: vec![
                Dev {
                    kname: "nvme0n1",
                    dtype: "disk",
                    size: "931.5G",
                    model: "FIXTURE NVMe 1TB",
                    serial: "BLANK0001",
                    mount: "",
                    below: vec![],
                    sigs: "",
                },
                Dev {
                    kname: "nvme1n1",
                    dtype: "disk",
                    size: "476.9G",
                    model: "FIXTURE NVMe 512GB",
                    serial: "ROOT0001",
                    mount: "",
                    below: vec!["nvme1n1p1", "nvme1n1p2"],
                    sigs: "gpt\nPMBR\n",
                },
                Dev {
                    kname: "nvme1n1p1",
                    dtype: "part",
                    size: "1G",
                    model: "",
                    serial: "",
                    mount: "/boot/efi",
                    below: vec![],
                    sigs: "vfat\n",
                },
                Dev {
                    kname: "nvme1n1p2",
                    dtype: "part",
                    size: "475.9G",
                    model: "",
                    serial: "",
                    mount: "/",
                    below: vec![],
                    sigs: "ext4\n",
                },
            ],
            root_src: Some("/dev/nvme1n1p2"),
            root_chain: "nvme1n1p2\nnvme1n1\n",
            by_id: vec![
                (BLANK_NAME, "nvme0n1"),
                (BLANK_EUI, "nvme0n1"),
                (ROOT_NAME, "nvme1n1"),
                ("nvme-FIXTURE_NVMe_512GB_ROOT0001-part1", "nvme1n1p1"),
                ("nvme-FIXTURE_NVMe_512GB_ROOT0001-part2", "nvme1n1p2"),
            ],
            fail: vec![],
        }
    }

    fn dev_mut(&mut self, kname: &str) -> &mut Dev {
        self.devs
            .iter_mut()
            .find(|d| d.kname == kname)
            .unwrap_or_else(|| panic!("no fixture device {kname}"))
    }

    /// Writes the stubs and the by-id links; returns (PATH, by-id dir).
    fn install(&self) -> (String, PathBuf) {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        let by_id = self.dir.join("by-id");
        std::fs::create_dir_all(&by_id).expect("mkdir by-id");
        for (name, kname) in &self.by_id {
            std::os::unix::fs::symlink(format!("/dev/{kname}"), by_id.join(name))
                .expect("symlink a by-id link");
        }

        // Every answer the stubs give, keyed by the whole argv.
        let mut arms: Vec<(String, String)> = vec![];
        let all: String = self.devs.iter().map(|d| format!("{}\n", d.kname)).collect();
        arms.push(("lsblk -nro KNAME".into(), all));
        for d in &self.devs {
            let dev = format!("/dev/{}", d.kname);
            arms.push((format!("lsblk -dno TYPE {dev}"), format!("{}\n", d.dtype)));
            arms.push((format!("lsblk -dno SIZE {dev}"), format!("{}\n", d.size)));
            arms.push((format!("lsblk -dno MODEL {dev}"), format!("{}\n", d.model)));
            arms.push((
                format!("lsblk -dno SERIAL {dev}"),
                format!("{}\n", d.serial),
            ));
            let mut kn = format!("{}\n", d.kname);
            let mut mp = format!("{}\n", d.mount);
            for b in &d.below {
                kn.push_str(&format!("{b}\n"));
                let m = self
                    .devs
                    .iter()
                    .find(|x| x.kname == *b)
                    .map(|x| x.mount)
                    .unwrap_or("");
                mp.push_str(&format!("{m}\n"));
            }
            arms.push((format!("lsblk -nro KNAME {dev}"), kn));
            arms.push((format!("lsblk -nro MOUNTPOINTS {dev}"), mp));
            arms.push((format!("wipefs -n -i -O TYPE {dev}"), d.sigs.to_string()));
        }
        if let Some(src) = self.root_src {
            arms.push(("findmnt -nvro SOURCE /".into(), format!("{src}\n")));
            arms.push((
                format!("lsblk -nrso KNAME {src}"),
                self.root_chain.to_string(),
            ));
        }

        for prog in ["lsblk", "wipefs", "findmnt"] {
            let mut body = String::from("#!/usr/bin/env bash\ncase \"$0 $*\" in\n");
            for f in &self.fail {
                if f.starts_with(&format!("{prog} ")) {
                    body.push_str(&format!(
                        "  */'{f}') echo \"{prog}: stub refused: $*\" >&2; exit 32 ;;\n"
                    ));
                }
            }
            for (args, out) in &arms {
                if args.starts_with(&format!("{prog} ")) {
                    body.push_str(&format!("  */'{args}') printf '%s' '{out}' ;;\n"));
                }
            }
            body.push_str(&format!(
                "  *) echo \"{prog}: stub has no answer for: $*\" >&2; exit 32 ;;\nesac\n"
            ));
            write_exec(&bin.join(prog), &body);
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        (path, by_id)
    }

    /// Runs `bash <script> <args>` against the fixture; (exit, stdout, stderr).
    fn run_script(&self, script: &str, args: &[&str]) -> (i32, String, String) {
        let (path, by_id) = self.install();
        let out = Command::new("bash")
            .arg(repo_root().join(script))
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("BOSS_BY_ID_DIR", by_id)
            .output()
            .expect("the script runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run(&self) -> (i32, String, String) {
        self.run_script(SCRIPT, &[])
    }
}

/// The block the listing prints for one kernel name.
fn block<'a>(out: &'a str, kname: &str) -> &'a str {
    let head = format!("== {kname} ==");
    let start = out
        .find(&head)
        .unwrap_or_else(|| panic!("no block for {kname}:\n{out}"));
    let rest = &out[start + head.len()..];
    let end = rest.find("\n== ").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// The lines under the closing candidates header.
fn candidates_section(out: &str) -> &str {
    let head = "== candidates for commission-a-disk";
    let start = out
        .find(head)
        .unwrap_or_else(|| panic!("no candidates section:\n{out}"));
    &out[start..]
}

/// The name the listing prints: always the /dev/disk/by-id path, which
/// is what commission-a-disk takes, whichever directory the links were
/// read from.
fn by_id(name: &str) -> String {
    format!("/dev/disk/by-id/{name}")
}

/// THE MEASURED CASE: the forge with its new drive in. The blank disk is
/// the one candidate, named by every by-id link that resolves to it;
/// the root disk and its partitions are refused, each with the reason
/// commission-a-disk's own judge gives; and every fact the packet asks
/// for is on the page.
#[test]
fn the_blank_drive_is_the_one_candidate_named_by_its_by_id_links() {
    let host = Host::forge("measured");
    let (rc, out, err) = host.run();
    assert_eq!(rc, 0, "every fact read, so exit 0:\n{out}\n{err}");

    let blank = block(&out, "nvme0n1");
    for want in [
        "type:        disk",
        "931.5G",
        "FIXTURE NVMe 1TB",
        "BLANK0001",
        "signatures:  none",
        "backs /:     no",
        "candidate:   YES",
    ] {
        assert!(blank.contains(want), "nvme0n1 must show {want:?}:\n{blank}");
    }
    assert!(
        blank.contains(&by_id(BLANK_NAME)) && blank.contains(&by_id(BLANK_EUI)),
        "every by-id link that resolves to the drive is listed:\n{blank}"
    );

    let root = block(&out, "nvme1n1");
    for want in [
        "below it:    nvme1n1p1 nvme1n1p2",
        "signatures:  gpt PMBR",
        "/boot/efi /",
        "backs /:     yes",
        "candidate:   no — /dev/nvme1n1 already carries 2 partition(s)",
    ] {
        assert!(root.contains(want), "nvme1n1 must show {want:?}:\n{root}");
    }
    let p2 = block(&out, "nvme1n1p2");
    assert!(
        p2.contains("is a 'part', not a whole disk") && p2.contains("backs /:     yes"),
        "a partition is refused by the judge's own words:\n{p2}"
    );
    assert!(
        p2.contains(&by_id("nvme-FIXTURE_NVMe_512GB_ROOT0001-part2")),
        "a -partN link is listed under the partition it names:\n{p2}"
    );
    assert!(
        out.contains("/ is mounted from /dev/nvme1n1p2"),
        "the listing says what backs /:\n{out}"
    );

    let cands = candidates_section(&out);
    assert!(
        cands.contains(&by_id(BLANK_NAME)) && cands.contains(&by_id(BLANK_EUI)),
        "the summary names the blank drive by every by-id link:\n{cands}"
    );
    assert!(
        !cands.contains("ROOT0001") && !cands.contains("nvme1n1"),
        "the root disk is never a candidate:\n{cands}"
    );
}

/// `--candidates` prints the candidate lines alone — what
/// commission-a-disk's refusal of a wrong name carries.
#[test]
fn candidates_mode_prints_only_the_candidate_lines() {
    let host = Host::forge("candidates-mode");
    let (rc, out, err) = host.run_script(SCRIPT, &["--candidates"]);
    assert_eq!(rc, 0, "{out}\n{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "one line per usable by-id name:\n{out}");
    assert!(
        lines
            .iter()
            .all(|l| l.contains("(nvme0n1, 931.5G, FIXTURE NVMe 1TB, serial BLANK0001)")),
        "each line names the kernel name, size, model and serial:\n{out}"
    );
}

/// A whole disk with nothing mounted can still hold data — a filesystem
/// written straight onto it. The judge refuses it, so the listing does.
#[test]
fn a_disk_carrying_a_signature_is_not_a_candidate() {
    let mut host = Host::forge("signature");
    host.dev_mut("nvme0n1").sigs = "ext4\n";
    let (rc, out, err) = host.run();
    assert_eq!(rc, 0, "a refusal is an answer:\n{out}\n{err}");
    assert!(
        block(&out, "nvme0n1").contains("candidate:   no — /dev/nvme0n1 carries a signature (ext4"),
        "{out}"
    );
    assert!(
        candidates_section(&out).contains("none"),
        "no disk is a candidate:\n{out}"
    );
}

/// EVERY FACT FAILS CLOSED: wipefs cannot read the drive, so its
/// signatures are unknown — never "none" — the drive is not a
/// candidate, and the run exits 1 naming the read.
#[test]
fn an_unreadable_fact_is_never_a_candidate_and_the_run_says_so() {
    let mut host = Host::forge("wipefs-fails");
    host.fail.push("wipefs -n -i -O TYPE /dev/nvme0n1".into());
    let (rc, out, err) = host.run();
    assert_eq!(rc, 1, "an incomplete listing is not a pass:\n{out}\n{err}");
    assert!(
        block(&out, "nvme0n1").contains("candidate:   no — cannot read: signatures(wipefs)"),
        "{out}"
    );
    assert!(
        !candidates_section(&out).contains(BLANK_NAME),
        "an unjudged disk is not listed as a candidate:\n{out}"
    );
    assert!(
        err.contains("INCOMPLETE") && err.contains("nvme0n1"),
        "stderr names what could not be read:\n{err}"
    );
}

/// Without the chain backing /, no disk can be ruled out as the root
/// disk, so none is a candidate — the renumbering of 2026-09-21 is
/// exactly the case where the blank-looking disk might be it.
#[test]
fn an_unreadable_root_makes_every_disk_unjudgeable() {
    let mut host = Host::forge("root-dark");
    host.root_src = None;
    host.fail.push("findmnt -nvro SOURCE /".into());
    let (rc, out, err) = host.run();
    assert_eq!(rc, 1, "{out}\n{err}");
    assert!(out.contains("/ is backed by: UNREADABLE"), "{out}");
    assert!(
        block(&out, "nvme0n1").contains("candidate:   no — findmnt could not read the source of /"),
        "{out}"
    );
    assert!(candidates_section(&out).contains("none"), "{out}");
}

/// A disk the judge would accept but that has no by-id name the verb's
/// device_by_id pattern admits cannot be commissioned through the verb:
/// a USB bridge's `...-0:0` carries a colon the pattern refuses.
#[test]
fn a_disk_with_no_name_the_verb_admits_is_not_a_candidate() {
    let mut host = Host::forge("no-usable-name");
    host.by_id.retain(|(_, k)| *k != "nvme0n1");
    host.by_id.push(("usb-FIXTURE_Bridge_0000-0:0", "nvme0n1"));
    let (rc, out, err) = host.run();
    assert_eq!(rc, 0, "{out}\n{err}");
    let blank = block(&out, "nvme0n1");
    assert!(
        blank.contains(&by_id("usb-FIXTURE_Bridge_0000-0:0")),
        "the link is still listed:\n{blank}"
    );
    assert!(
        blank.contains("candidate:   no — no /dev/disk/by-id name for it passes commission-a-disk's device_by_id"),
        "{blank}"
    );
    assert!(candidates_section(&out).contains("none"), "{out}");
}

/// THE §9a PIN. The judgement is one file, sourced by both scripts; the
/// FACTS it judges are read in both, so each read commission-a-disk.sh
/// makes is found here command for command. Derived from the
/// destructive script, not listed: if it changes how it reads a fact,
/// this names the read that moved.
#[test]
fn the_listing_reads_every_fact_the_way_commission_a_disk_reads_it() {
    let read = |rel: &str| {
        let p = repo_root().join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    };
    let commission = read(COMMISSION);
    let listing = read(SCRIPT);
    for var in ["dtype", "knames", "sigs", "mps", "root_src", "root_chain"] {
        let needle = format!("{var}=$(");
        let line = commission
            .lines()
            .find(|l| l.trim_start().starts_with(&needle))
            .unwrap_or_else(|| panic!("{COMMISSION} no longer reads `{var}` — re-derive this pin"));
        let start = line.find(&needle).expect("needle") + needle.len();
        let end = line[start..]
            .find(')')
            .map(|i| start + i)
            .unwrap_or_else(|| panic!("no closing paren in: {line}"));
        let cmd = &line[start..end];
        assert!(
            listing.contains(&format!("$({cmd})")),
            "{COMMISSION} reads `{var}` with `{cmd}`, and {SCRIPT} does not — the two would \
             judge different facts, and the listing could mark a disk the verb refuses"
        );
    }
    assert!(
        listing.contains(". \"$HERE/commission-a-disk.judge.sh\""),
        "the listing must SOURCE the judge commission-a-disk sources"
    );
    assert!(
        !listing.contains("disk_refusal()"),
        "the listing must not define its own copy of the judgement"
    );
}

/// "Better still" (the packet): plan-a-disk-commission's refusal of a
/// wrong device name lists the candidates, so the next request can name
/// one without anybody logging in. Each wrong shape an operator reaches
/// for — the kernel name the renumbering made dangerous, a by-id name
/// that names nothing, a `-partN` of the root disk — is still exit 78
/// with nothing rendered, and now carries the blank drive's by-id name.
#[test]
fn a_wrong_device_name_is_refused_with_the_candidates_listed() {
    for (case, bad, why) in [
        ("kernel-name", "/dev/nvme0n1", "stable identity"),
        (
            "names-nothing",
            "/dev/disk/by-id/nvme-THIS-DISK-DOES-NOT-EXIST-0000",
            "no such device",
        ),
        (
            "a-partition",
            "/dev/disk/by-id/nvme-FIXTURE_NVMe_512GB_ROOT0001-part1",
            "names a partition",
        ),
    ] {
        let host = Host::forge(&format!("refusal-{case}"));
        let (rc, out, err) = host.run_script(COMMISSION, &["--plan", bad, "/srv/boss-data"]);
        assert_eq!(rc, 78, "{case}: a wrong name is still exit 78:\n{err}");
        assert!(out.trim().is_empty(), "{case}: nothing is rendered: {out}");
        assert!(
            err.contains(why),
            "{case}: the refusal keeps its reason:\n{err}"
        );
        assert!(
            err.contains(&by_id(BLANK_NAME)) && err.contains("nvme0n1, 931.5G"),
            "{case}: the refusal lists the disk this verb would accept:\n{err}"
        );
        let offered = err
            .split_once("would accept")
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("{case}: no candidates in the refusal:\n{err}"));
        assert!(
            !offered.contains("ROOT0001") && !offered.contains("nvme1n1"),
            "{case}: the root disk is never offered:\n{err}"
        );
    }
}

/// The verb runs this script, on the forge, with no parameters — the
/// shape every read-only report verb has (infra/ops/verbs/README.md).
#[test]
fn the_list_block_devices_verb_runs_the_script_read_only_on_the_forge() {
    let path = repo_root().join("infra/ops/verbs/list-block-devices.json");
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the verb file {} is readable: {e}", path.display())),
    )
    .expect("the verb file is JSON");
    assert_eq!(verb["argv"], serde_json::json!([SCRIPT]), "{verb}");
    assert_eq!(verb["params"], serde_json::json!([]), "{verb}");
    assert_eq!(verb["hosts"], serde_json::json!(["forge"]), "{verb}");
    let about = verb["about"].as_str().unwrap_or_default();
    assert!(
        about.starts_with("READ-ONLY") && !about.contains("MUTATING"),
        "a report verb declares itself read-only: {about}"
    );
    assert!(verb.get("requires_approval").is_none(), "{verb}");
}
