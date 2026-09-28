//! `commission-a-disk --plan` renders what would happen and writes
//! nothing — the "plan" of plan-then-approve.
//!
//! WHY (design 17835005, answered by David 2026-09-21). The question was
//! whether a passkey could approve a destructive change that the machine
//! then executes. Q1 settled what the signature binds: **a rendered plan
//! hash, not a verb call.** A verb call authorises an intent whose target
//! can still resolve differently at execution time — which is precisely
//! how 2026-09-21 would have gone wrong. A second NVMe went into the
//! forge and the kernel RENUMBERED the devices: the new blank drive came
//! up as `nvme0n1` and the live root filesystem moved to `nvme1n1p2`.
//! Approving "format nvme0n1" would have approved destroying the system
//! disk. Approving a plan that already names the resolved target by-id,
//! with its observed facts, cannot be replayed onto a different disk.
//!
//! THE PLAN IS HASHED OVER ITS OWN BYTES, which is why these tests care
//! so much about determinism. There is no canonicalisation step to drift
//! (§9a): whoever verifies re-hashes the bytes it was handed. So the
//! document must carry nothing that varies between two renders of the
//! same true state — no timestamp, no run id — or an approval that is
//! still valid breaks on its own. And conversely, if any OBSERVED fact
//! moves, the bytes move with it, which is exactly the drift q4 says
//! must void an approval.
//!
//! WHAT IS AND IS NOT TESTED HERE, stated rather than left to be
//! discovered. The precondition EVALUATION needs a real block device
//! under `/dev/disk/by-id/`, and the gate runs as uid 65534 on a pod
//! that has no such directory — so it is not exercised here, and these
//! tests say so instead of pretending. What IS exercised: the two
//! refusals that precede any device access, the rendering itself
//! (against the template file the script runs, not a copy of it), and
//! the structural property that matters most — that `--plan` cannot
//! reach a mutating command.

use std::path::PathBuf;
use std::process::Command;

use boss_testing::repo_root;

fn script() -> PathBuf {
    repo_root().join("infra/forge/commission-a-disk.sh")
}

/// The commands that change the host, as the script RUNS them — named
/// rather than matched on a pattern, because a pattern wide enough to
/// catch them catches prose, and the arrays' definitions and the render
/// that serialises them (both precede the plan) are not runs — a run is
/// the array expanded as a command, `"${ARR[@]}" ||`. Since the
/// review of car 0556935a the act runs from those arrays, one definition
/// for the plan and the write.
const MUTATORS: [&str; 6] = [
    "\"${PARTED[@]}\" ||",
    "\"${MKFS[@]}\" ||",
    "mkdir -p \"$MOUNT\"",
    "mktemp /etc/",
    "mv -f \"$FSTAB_TMP\"",
    "\"${MOUNT_CMD[@]}\" ||",
];

fn template() -> PathBuf {
    repo_root().join("infra/forge/commission-a-disk.plan.jq")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn have(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `bash commission-a-disk.sh --plan <args>`, as whatever uid runs the
/// test. Returns (exit code, stdout, stderr).
fn plan(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(script()).arg("--plan");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("the script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// THE STRUCTURAL PROPERTY, and the one that would matter most if it
/// broke: `--plan` must exit before anything that writes.
///
/// Read BY LINE and only REAL invocations — this file is more comment
/// than code, and the header talks about `parted` and `mkfs` at length
/// while describing what the write path does. A needle that can match a
/// comment tests the comment (the lesson `quick_mode_exits_before_the_
/// first_compile` learned the same way, twice).
#[test]
fn the_plan_branch_exits_before_the_first_mutating_command() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();

    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };

    let plan_exit = lines
        .iter()
        .enumerate()
        .find(|(_, l)| code(l) && l.trim_start() == "exit 0")
        .map(|(i, _)| i)
        .expect("the --plan branch no longer exits");

    // The commands that change the disk, named rather than matched on a
    // pattern: a pattern wide enough to catch them catches prose too.
    let mutators = MUTATORS;
    let first_mutation = lines
        .iter()
        .enumerate()
        .find(|(_, l)| code(l) && mutators.iter().any(|m| l.contains(m)))
        .map(|(i, l)| (i, l.trim().to_string()))
        .expect("the script no longer mutates anything — this pin reads the wrong file");

    assert!(
        plan_exit < first_mutation.0,
        "`--plan` exits at line {} but the first mutating command is at line {} ({}) — \
         the plan branch must come FIRST, or a plan run partitions a disk, which is the \
         one thing it promises not to do",
        plan_exit + 1,
        first_mutation.0 + 1,
        first_mutation.1
    );
}

/// EVERY PRECONDITION THE WRITE PATH CHECKS IS CHECKED BEFORE THE PLAN
/// IS RENDERED, so a plan is only ever produced for something that
/// would actually run. A plan for a target that cannot be commissioned
/// is not a plan; it is a refusal wearing a plan's clothes, and it is
/// what an approver would be signing.
#[test]
fn the_plan_is_rendered_only_after_every_precondition_holds() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();
    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };
    // Anchored on the RENDER, not on the branch that prints it: since
    // backlog b2d5b546 the write path renders the same plan to compare
    // its hash, so the render is what must follow every precondition.
    let plan_at = lines
        .iter()
        .position(|l| code(l) && l.contains("commission-a-disk.plan.jq"))
        .expect("the plan is no longer rendered from its template");

    // Every precondition, by the refusal or the judgement that raises it.
    // The device and mount-path judgements live in
    // commission-a-disk.judge.sh (run for real below); here, the script
    // must CALL them before it renders.
    let needles = [
        "is not a stable identity",               // by-id, never a kernel name
        "names a partition",                      // a by-id name for a partition
        "mount path must be",                     // /srv/<name>, one component
        "disk_refusal \"$DEV\"", // whole raw disk: type, parts, signatures, mounts, root
        "mount_refusal \"$MOUNT\"", // an empty, unmounted path no fstab line names
        "findmnt --verify --tab-file /etc/fstab", // the fstab it will edit verifies now
    ];
    for n in needles {
        let at = lines
            .iter()
            .position(|l| code(l) && l.contains(n))
            .unwrap_or_else(|| panic!("no precondition raising {n:?} — the script changed shape"));
        assert!(
            at < plan_at,
            "the precondition raising {n:?} is at line {} but the plan renders at line {} — \
             a plan must not be produced for a target the write path would refuse",
            at + 1,
            plan_at + 1
        );
    }
}

/// THE REFUSALS THAT NEED NO DEVICE, run for real.
///
/// A kernel name is the exact mistake the renumbering set up, and it is
/// refused before anything is resolved — so this runs on any box, as any
/// uid, including the gate's 65534.
#[test]
fn a_kernel_name_is_refused_and_nothing_is_rendered() {
    let (code, out, err) = plan(&["/dev/nvme0n1", "/srv/boss-data"]);
    assert_eq!(
        code, 78,
        "a wrong request is exit 78, not a failed run: {err}"
    );
    assert!(
        err.contains("stable identity"),
        "the refusal must say why a kernel name is not a target: {err}"
    );
    assert!(
        out.trim().is_empty(),
        "a refused plan must render NOTHING — an approver must never be handed a document \
         for a target that was rejected: {out}"
    );
}

/// A by-id path that names nothing is refused too, and again renders
/// nothing. Same uid-independent reason: it fails at `[ -e ]`.
#[test]
fn a_by_id_path_that_names_nothing_is_refused() {
    let (code, out, err) = plan(&[
        "/dev/disk/by-id/nvme-THIS-DISK-DOES-NOT-EXIST-0000",
        "/srv/boss-data",
    ]);
    assert_eq!(code, 78, "{err}");
    assert!(out.trim().is_empty(), "nothing is rendered: {out}");
    assert!(
        err.contains("no such device") || err.contains("cannot resolve"),
        "the refusal names the missing device: {err}"
    );
}

/// THE RENDERER ITSELF, run against the TEMPLATE THE SCRIPT RUNS.
///
/// The template lives in its own file precisely so this is not a copy:
/// `commission-a-disk.sh` invokes it with `jq -f`, and so does this.
/// That is what lets the rendering be tested on a box with no block
/// devices at all.
#[test]
fn the_plan_document_renders_the_resolved_target_and_the_observed_facts() {
    if !have("jq") {
        eprintln!("a_destructive_verb_renders_a_plan_first: SKIPPED — no jq");
        return;
    }
    let text = render_plan(&[]);
    let plan: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("the plan is not JSON: {e}\n{text}"));

    // THE RESOLVED TARGET, both spellings. The by-id name is what was
    // asked for and what cannot move; the kernel name is what it
    // resolved to at plan time and is exactly the thing that renumbers.
    // An approver needs both: one to know which disk, one to see what it
    // was called when the plan was made.
    assert_eq!(
        plan["target_by_id"],
        "/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234"
    );
    assert_eq!(plan["resolves_to"], "/dev/nvme0n1");
    assert_eq!(plan["mount_path"], "/srv/boss-data");
    assert_eq!(plan["size_bytes"], 1024209543168_u64);

    // THE OBSERVED FACTS ARE NUMBERS AND BOOLEANS, not strings: "0" != 0
    // would make a drift check that never matches. Since the review of
    // car 0556935a (2026-09-27) they include what the whole-disk and
    // mount-path judgements read, so those facts are hashed too.
    let o = &plan["observed"];
    assert_eq!(o["device_type"], "disk");
    assert_eq!(o["partition_count"], 0);
    assert_eq!(o["signatures"], serde_json::json!([]));
    assert_eq!(o["mounted_filesystems"], 0);
    assert_eq!(o["backs_root"], false);
    assert_eq!(
        o["mount_path"],
        serde_json::json!({"state": "absent", "entries": 0, "is_mountpoint": false,
                           "in_fstab": false})
    );
    assert_eq!(o["fstab_verifies"], true);
    assert!(
        o["partition_count"].is_number(),
        "observed facts must be numbers, or a drift comparison silently never fires"
    );

    // THE ACT IS IN THE SIGNED BYTES: what will be run, rendered from
    // the arrays and the line the script itself runs (the review's
    // item 4), so an approver signs the geometry, the mkfs options and
    // the fstab line, not only the target.
    let a = &plan["act"];
    assert_eq!(a["partition"][0], "parted");
    assert_eq!(a["mkfs"][0], "mkfs.ext4");
    assert_eq!(a["mount"], serde_json::json!(["mount", "/srv/boss-data"]));
    assert!(
        a["fstab_line"]
            .as_str()
            .unwrap_or_default()
            .contains("/srv/boss-data ext4"),
        "{a}"
    );

    // The argv names the by-id target, never the kernel name — the
    // whole point of the verb.
    let argv: Vec<&str> = plan["argv"]
        .as_array()
        .expect("argv is a list")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        argv.contains(&"/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234"),
        "argv must carry the by-id target: {argv:?}"
    );
    assert!(
        !argv.contains(&"/dev/nvme0n1"),
        "argv must NOT carry the kernel name — it moves, and that is the defect this \
         verb exists to make unrepresentable: {argv:?}"
    );
}

/// THE DETERMINISM THE HASH DEPENDS ON: two renders of the same state
/// are byte-identical, and a changed observation changes the bytes.
///
/// Both halves matter. Without the first, an approval breaks on its own
/// between signing and applying. Without the second, a plan could drift
/// under an approval that still verified — which is the failure q4's
/// "voided by drift" exists to prevent.
///
/// WHAT THIS TEST CANNOT CATCH, measured rather than assumed: a COARSE
/// clock. Adding `rendered_at: (now | todate)` to the template was tried
/// here, and this test still PASSED — two renders inside the same second
/// produce the same second-resolution string. It is
/// `the_plan_template_carries_no_clock_and_no_run_identity` that caught
/// it, by reading the template rather than running it. So the pair is
/// load-bearing and neither half is redundant: one proves the render is
/// stable, the other forbids the ingredients whose instability this one
/// would miss.
#[test]
fn the_same_state_renders_the_same_bytes_and_a_moved_fact_does_not() {
    if !have("jq") {
        eprintln!("a_destructive_verb_renders_a_plan_first: SKIPPED — no jq");
        return;
    }
    assert_eq!(
        render_plan(&[]),
        render_plan(&[]),
        "two renders of the same state must be byte-identical — the plan is hashed over \
         its own bytes, so anything that varies on its own breaks a valid approval"
    );
    // Every observed fact, and the act, moves the bytes when it moves.
    for (k, v) in [
        ("parts", "1"),
        ("dtype", "part"),
        ("sigs", "ext4"),
        ("mounted", "1"),
        ("backs_root", "yes"),
        ("m_state", "dir"),
        ("m_entries", "2"),
        ("m_mp", "yes"),
        ("m_fstab", "yes"),
        ("size", "1024209543169"),
        ("fstab_line", "UUID=x /srv/boss-data ext4 defaults 0 2"),
    ] {
        assert_ne!(
            render_plan(&[]),
            render_plan(&[(k, v)]),
            "a plan whose {k} moved must render different bytes, or an approval survives \
             the drift it exists to be voided by"
        );
    }
}

/// `jq -n <args> -f commission-a-disk.plan.jq` with the script's full
/// argument set — a clean whole disk and an absent /srv/boss-data — and
/// any of them overridden. The same template file the script runs.
fn render_plan(overrides: &[(&str, &str)]) -> String {
    assert!(
        have("jq"),
        "jq is in the gate image; a render that cannot run is not a pass"
    );
    let defaults: [(&str, &str); 14] = [
        ("by_id", "/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234"),
        ("dev", "/dev/nvme0n1"),
        ("mount", "/srv/boss-data"),
        ("size", "1024209543168"),
        ("dtype", "disk"),
        ("parts", "0"),
        ("sigs", ""),
        ("mounted", "0"),
        ("backs_root", "no"),
        ("m_state", "absent"),
        ("m_entries", "0"),
        ("m_mp", "no"),
        ("m_fstab", "no"),
        (
            "fstab_line",
            "UUID=<uuid of the new filesystem> /srv/boss-data ext4 defaults,noatime,nofail,x-systemd.device-timeout=10s 0 2",
        ),
    ];
    let json_args: [(&str, &str); 3] = [
        (
            "parted",
            r#"["parted","-s","/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234","mklabel","gpt","mkpart","boss-data","ext4","0%","100%"]"#,
        ),
        (
            "mkfs",
            r#"["mkfs.ext4","-F","-q","-L","boss-data","/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234-part1"]"#,
        ),
        ("mount_cmd", r#"["mount","/srv/boss-data"]"#),
    ];
    let mut cmd = Command::new("jq");
    cmd.arg("-n");
    for (k, v) in defaults {
        let v = overrides
            .iter()
            .find(|(ok, _)| *ok == k)
            .map_or(v, |(_, ov)| *ov);
        cmd.args(["--arg", k, v]);
    }
    for (k, v) in json_args {
        cmd.args(["--argjson", k, v]);
    }
    let out = cmd.arg("-f").arg(template()).output().expect("jq runs");
    assert!(
        out.status.success(),
        "the plan template is not valid jq over the script's arguments: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `bash -c '. commission-a-disk.judge.sh; <fn> <args>'` — the refusal
/// functions the script sources, run for real on facts a test chooses.
/// Returns (exit code, what it printed).
fn judge(func: &str, args: &[&str]) -> (i32, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#"set -eu; . "$1"; shift; "$@""#)
        .arg("judge")
        .arg(repo_root().join("infra/forge/commission-a-disk.judge.sh"))
        .arg(func)
        .args(args)
        .output()
        .expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// ONLY A WHOLE, RAW, UNUSED DISK PASSES (the review of car 0556935a,
/// 2026-09-27, finding 1). A by-id link to a partition, or to an
/// unmounted device-mapper or md node, had a child count of 1 and passed
/// the prefix-matched root check, so it reached `parted`. Each case below
/// is a disk this verb must refuse, with the words it refuses in; the
/// facts are what lsblk and wipefs would have said. Run through the same
/// file the script sources, since no test box has a block device.
#[test]
fn the_disk_judgement_passes_only_a_whole_raw_unused_disk() {
    let d = "/dev/nvme0n1";
    let cases: [(&[&str], &str); 17] = [
        (&[d, "part", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "crypt", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "lvm", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "raid1", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "loop", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "", "0", "", "0", "no"], "not a whole disk"),
        (&[d, "disk", "1", "", "0", "no"], "partition(s)"),
        (&[d, "disk", "0", "ext4", "0", "no"], "carries a signature"),
        (
            &[d, "disk", "0", "crypto_LUKS", "0", "no"],
            "carries a signature",
        ),
        (
            &[d, "disk", "0", "LVM2_member", "0", "no"],
            "carries a signature",
        ),
        (
            &[d, "disk", "0", "zfs_member", "0", "no"],
            "carries a signature",
        ),
        (
            &[d, "disk", "0", "gpt\nPMBR", "0", "no"],
            "carries a signature",
        ),
        (&[d, "disk", "0", "", "1", "no"], "mounted filesystem(s)"),
        (
            &[d, "disk", "0", "", "0", "yes"],
            "backs the root filesystem",
        ),
        // Fail closed: a fact that is not a clean reading refuses.
        (
            &[d, "disk", "", "", "0", "no"],
            "cannot count the partitions",
        ),
        (&[d, "disk", "0", "", "x", "no"], "cannot count the mounts"),
        (&[d, "disk", "0", "", "0", ""], "backs the root filesystem"),
    ];
    for (args, expected) in cases {
        let (code, out) = judge("disk_refusal", args);
        assert_eq!(code, 1, "{args:?} must be refused: {out}");
        assert!(out.contains(expected), "{args:?} -> {out}");
    }
    let (code, out) = judge("disk_refusal", &[d, "disk", "0", "", "0", "no"]);
    assert_eq!(code, 0, "a whole raw unused disk passes: {out}");
    assert!(out.is_empty(), "a pass prints nothing: {out}");
}

/// THE ACT IN THE PLAN IS THE ACT THAT RUNS, flags included. The arrays
/// the write runs reach the plan through `as_json`, and `jq --args`
/// goes on parsing ITS OWN options among the positional words: measured
/// 2026-09-27 on jq 1.6, `--args parted -s …` rendered
/// `["parted", …]` with the `-s` gone (jq took it as --slurp), and the
/// mkfs array would have lost `-F -q -L`. A plan that omits a flag the
/// write passes is a plan the approver did not see. Run through the
/// same sourced file the script uses.
#[test]
fn the_act_reaches_the_plan_with_every_flag() {
    for argv in [
        &[
            "parted",
            "-s",
            "/dev/disk/by-id/nvme-X",
            "mklabel",
            "gpt",
            "mkpart",
            "boss-data",
            "ext4",
            "0%",
            "100%",
        ][..],
        &[
            "mkfs.ext4",
            "-F",
            "-q",
            "-L",
            "boss-data",
            "/dev/disk/by-id/nvme-X-part1",
        ][..],
        &["mount", "/srv/boss-data"][..],
        &["a", "--", "-n", "--args"][..],
    ] {
        let (code, out) = judge("as_json", argv);
        assert_eq!(code, 0, "{argv:?}: {out}");
        let got: Vec<String> = serde_json::from_str(out.trim())
            .unwrap_or_else(|e| panic!("{argv:?} did not render a JSON array ({e}): {out}"));
        assert_eq!(got, argv, "every word, flags included, reaches the plan");
    }
}

/// THE MOUNT PATH MUST BE UNUSED (finding 3): not a symlink or a file,
/// empty, not mounted on, and not already an fstab target — each judged
/// on the facts the script reads, before anything is written.
#[test]
fn the_mount_judgement_refuses_a_path_in_use() {
    let m = "/srv/boss-data";
    let cases: [(&[&str], &str); 7] = [
        (&[m, "symlink", "0", "no", "no"], "is a symlink"),
        (&[m, "other", "0", "no", "no"], "not a directory"),
        (&[m, "dir", "3", "no", "no"], "is not empty"),
        (&[m, "dir", "0", "yes", "no"], "already a mountpoint"),
        (&[m, "dir", "0", "no", "yes"], "already an fstab target"),
        (&[m, "dir", "", "no", "no"], "cannot count"),
        (&[m, "dir", "0", "", "no"], "already a mountpoint"),
    ];
    for (args, expected) in cases {
        let (code, out) = judge("mount_refusal", args);
        assert_eq!(code, 1, "{args:?} must be refused: {out}");
        assert!(out.contains(expected), "{args:?} -> {out}");
    }
    for state in ["absent", "dir"] {
        let (code, out) = judge("mount_refusal", &[m, state, "0", "no", "no"]);
        assert_eq!(code, 0, "an unused {state} path passes: {out}");
        assert!(out.is_empty(), "{out}");
    }
}

/// THE WRITE CAN FAIL, BUT IT CANNOT LEAVE THE HOST UNBOOTABLE OR LIE
/// ABOUT WHAT RAN (findings 2, 5 and 6). Read by line, since the write
/// needs a disk:
///   - the fstab entry carries nofail and a device timeout, so a missing
///     disk cannot stop a boot;
///   - fstab is edited as a copy — its last line terminated first —
///     verified with findmnt, then renamed over the original, never
///     appended to in place;
///   - only the one new mount is mounted, never `mount -a`;
///   - mkfs is forced (-F), since the plan already proved the disk raw;
///   - after the first write, a failure is exit 1 (`fail`), never exit
///     78 (`die`), which would tell the runner nothing ran.
#[test]
fn the_write_edits_fstab_fail_safe_and_fails_loudly_after_it_starts() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();
    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };
    let at = |needle: &str| -> usize {
        lines
            .iter()
            .position(|l| code(l) && l.contains(needle))
            .unwrap_or_else(|| panic!("no code line carries {needle:?} — the script changed shape"))
    };

    let opts = lines[at("FSTAB_OPTS=")];
    assert!(
        opts.contains("nofail") && opts.contains("x-systemd.device-timeout=10s"),
        "a data disk that fails to appear must not stop the boot: {opts}"
    );
    assert!(lines[at("MKFS=(")].contains(" -F "), "mkfs is forced");
    assert!(
        !lines.iter().any(|l| code(l) && l.contains("mount -a")),
        "mount -a mounts every fstab line, not the one this verb added"
    );
    assert!(
        !lines.iter().any(|l| code(l) && l.contains(">> /etc/fstab")),
        "fstab is never appended to in place"
    );
    let newline = at("tail -c 1 \"$FSTAB_TMP\"");
    let append = at(">> \"$FSTAB_TMP\"");
    let verify = at("findmnt --verify --tab-file \"$FSTAB_TMP\"");
    let rename = at("mv -f \"$FSTAB_TMP\" /etc/fstab");
    let mount = at("\"${MOUNT_CMD[@]}\" ||");
    assert!(
        newline < append && append < verify && verify < rename && rename < mount,
        "terminate the last line ({}), append ({}), verify the copy ({}), rename ({}), then \
         mount ({})",
        newline + 1,
        append + 1,
        verify + 1,
        rename + 1,
        mount + 1
    );

    let first_write = at("\"${PARTED[@]}\" ||");
    let late_die: Vec<String> = lines
        .iter()
        .enumerate()
        .skip(first_write)
        .filter(|(_, l)| code(l) && l.contains("die "))
        .map(|(i, l)| format!("{}: {}", i + 1, l.trim()))
        .collect();
    assert!(
        late_die.is_empty(),
        "after parted has run, a failure is exit 1, not a 78 that says nothing ran: \
         {late_die:?}"
    );
}

/// NOTHING IN THE TEMPLATE MAY VARY ON ITS OWN. A timestamp is the
/// obvious way this gets broken later — it reads like provenance and it
/// silently makes every approval single-render.
#[test]
fn the_plan_template_carries_no_clock_and_no_run_identity() {
    let t = read("infra/forge/commission-a-disk.plan.jq");
    let code: String = t
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for banned in ["now", "localtime", "todate", "strftime", "$ENV", "env."] {
        assert!(
            !code.contains(banned),
            "the plan template uses {banned:?} — the plan is signed over its own bytes, so \
             anything that varies between two renders of the same state breaks an approval \
             that is still valid. The render time belongs on the packet, outside what is signed."
        );
    }
}

/// THE WRITE RE-RENDERS AND COMPARES BEFORE IT WRITES (backlog b2d5b546,
/// 2026-09-26). The verb was inert until this: the runner hands the
/// write sha256 of the SIGNED plan, and refuses an approval verb whose
/// script cannot re-render the plan and refuse bytes that moved since
/// the signature (design 17835005 q4: drift voids an approval). So the
/// write path renders the plan ONCE, from the one template, exactly as
/// `--plan` does, and compares before the first mutating command.
///
/// Read by line, as the pins above are, and for the same reason the
/// success path is not run: it needs a raw block device under
/// /dev/disk/by-id/, and the gate has none. What is run for real is
/// the refusal of a missing or malformed hash (ops_runner_sh.rs,
/// `the_disk_verb_requires_approval_and_refuses_an_unstable_target`)
/// and the runner accepting the verb's contract
/// (ops_runner_approval_sh.rs).
#[test]
fn the_write_compares_the_rerendered_plan_before_the_first_mutation() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();
    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };
    let at = |needle: &str| -> usize {
        lines
            .iter()
            .position(|l| code(l) && l.contains(needle))
            .unwrap_or_else(|| panic!("no code line carries {needle:?} — the script changed shape"))
    };

    // ONE render: the plan a passkey signs and the plan the write
    // compares are the same bytes because they are the same command.
    let renders = lines
        .iter()
        .filter(|l| code(l) && l.contains("commission-a-disk.plan.jq"))
        .count();
    assert_eq!(
        renders, 1,
        "the plan must be rendered by ONE command for both --plan and the write — two \
         renders are two definitions of what was approved (§9a)"
    );

    let render = at("commission-a-disk.plan.jq");
    let compare = at("[ \"$HASH\" = \"$APPROVED\" ]");
    let mutators = MUTATORS;
    let first_mutation = lines
        .iter()
        .position(|l| code(l) && mutators.iter().any(|m| l.contains(m)))
        .expect("the script no longer mutates anything — this pin reads the wrong file");
    assert!(
        render < compare && compare < first_mutation,
        "render at line {}, compare at line {}, first mutation at line {}: the write must \
         re-render, then compare against the approved hash, then write",
        render + 1,
        compare + 1,
        first_mutation + 1
    );

    // The plan names its own hash on stderr, the runner's contract with
    // every plan verb: a plan whose stdout does not hash to the
    // `plan-sha256:` it prints is refused before it reaches a passkey.
    let names_hash = at("plan-sha256: $HASH");
    assert!(
        lines[names_hash].contains(">&2") && names_hash < first_mutation,
        "the plan's hash rides stderr, since a hash cannot be inside the bytes it hashes: {}",
        lines[names_hash].trim()
    );
}

/// THE PLAN VERB IS READ-ONLY, and the write verb runs only under an
/// approval of it. The two declarations are one word apart from each
/// other's meaning, so they are pinned rather than trusted to review.
#[test]
fn the_plan_verb_needs_no_approval_and_the_write_verb_does() {
    let plan_spec: serde_json::Value =
        serde_json::from_str(&read("infra/ops/verbs/plan-a-disk-commission.json"))
            .expect("the plan verb file is JSON");
    let write_spec: serde_json::Value =
        serde_json::from_str(&read("infra/ops/verbs/commission-a-disk.json"))
            .expect("the write verb file is JSON");

    assert!(
        plan_spec.get("requires_approval").is_none()
            || plan_spec["requires_approval"] == serde_json::Value::Bool(false),
        "the plan verb mutates nothing, so it must NOT require an approval — it is the half \
         that is usable before the approval channel exists"
    );
    assert_eq!(
        write_spec["requires_approval"], true,
        "the write verb runs only under a verified passkey approval of its plan"
    );
    assert_eq!(write_spec["plan_verb"], "plan-a-disk-commission");

    // The write takes exactly the plan's params plus the approved hash,
    // last — the shape the runner's contract refuses anything else in.
    let names = |s: &serde_json::Value| -> Vec<String> {
        s["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(|p| p["name"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    let mut write_names = names(&write_spec);
    assert_eq!(
        write_names.pop().as_deref(),
        Some("plan_sha256"),
        "the write's last param is the approved plan's hash"
    );
    assert_eq!(
        write_names,
        names(&plan_spec),
        "the plan is rendered from exactly the args the write acts on"
    );

    // And the plan verb must actually pass --plan, or it IS the write
    // verb under another name.
    let argv: Vec<&str> = plan_spec["argv"]
        .as_array()
        .expect("argv")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        argv.contains(&"--plan"),
        "the plan verb must invoke the script with --plan, or it partitions the disk: {argv:?}"
    );
    assert_eq!(
        argv.first().copied(),
        Some("infra/forge/commission-a-disk.sh"),
        "both verbs run the same script, so the preconditions cannot differ between plan \
         and apply: {argv:?}"
    );
}
