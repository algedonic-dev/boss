//! `infra/gate.sh`'s disk floor is RUN, not read — against a synthetic
//! tree and a `df` that answers per path, so every property below is one
//! the floor actually has.
//!
//! THE DEFECT (backlog 878e5a0f, measured 2026-10-17 ~23:00Z by builder
//! run e4eee763). The floor read `df -Pk .` — the CHECKOUT's volume — in
//! every mode. On the dev pod a linked worktree's checkout is on /work
//! (then 84% full, 6 GB free) while wt-cargo puts its cargo target on
//! /scratch (248 GB free), which is where `--lint`'s clippy writes. So
//! `infra/gate.sh --lint` refused with "6GB free, need 12GB. Refusing to
//! start." before any check ran, the builder shipped the car without the
//! pre-flight, and a pre-flight run the same hour from a worktree under
//! /tmp was NOT refused: the answer depended on where the worktree lived
//! rather than where the build writes. Builder run e205c820 then measured
//! the same line refusing `--quick`, which compiles nothing at all.
//!
//! THE RULE PINNED HERE. The floor judges each volume the MODE writes to,
//! and names every one it measured when it refuses:
//!
//!   - the lint roster's scratch (`TMPDIR`) — every mode runs the roster;
//!   - the cargo target (`CARGO_TARGET_DIR`, or `./target`) — the modes
//!     that compile: `--lint` and the gate;
//!   - the checkout — the gate alone, whose web install and builds write
//!     into it.
//!
//! And a refusal carries the repair of the volume that refused, never a
//! fixed paragraph (backlog 011be442).
//!
//! The volumes are fixed by the fake `df` below from the PATH it is
//! handed, so a floor that measures the wrong path gets the wrong answer
//! here exactly as it did on the pod. The fixture's shape is the pod's:
//! a checkout on a nearly full volume, a cargo target on a roomy one,
//! and the target dir not yet created (a cold worktree's).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The floor every case applies; the fake answers 1 GB below it on the
/// full volume and 900 GB on the roomy one.
const FLOOR_GB: &str = "12";

struct Fixture {
    dir: PathBuf,
    /// A volume with 1 GB free: `/full` in the fake's answer.
    full: PathBuf,
    /// A volume with 900 GB free: `/roomy` in the fake's answer.
    roomy: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = boss_testing::scratch_dir(&format!("disk-floor-where-{tag}"));
        let dir = dir.canonicalize().expect("a scratch dir canonicalizes");
        let full = dir.join("full");
        let roomy = dir.join("roomy");
        boss_testing::create_dir(&full.join("tmp"));
        boss_testing::create_dir(&roomy.join("tmp"));
        // One row per path argument, in order, the way df answers; the
        // volume is decided by the path, so only a floor that hands df
        // the right path reads the right number. Handed no path it
        // answers for the cwd: the floor before this packet spelled its
        // default `df -Pk .` and called the seam bare, so the cwd is the
        // path that floor measured.
        boss_testing::write_exec(
            &dir.join("df"),
            &format!(
                "#!/usr/bin/env bash\n\
                 paths=()\n\
                 for a in \"$@\"; do case \"$a\" in -*) ;; *) paths+=(\"$a\") ;; esac; done\n\
                 [ \"${{#paths[@]}}\" -gt 0 ] || paths=(.)\n\
                 echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
                 for a in \"${{paths[@]}}\"; do\n\
                 p=$(realpath -m -- \"$a\")\n\
                 case \"$p/\" in\n\
                 '{full}'/*) echo '/dev/full 1 1 1048576 99% /full' ;;\n\
                 *) echo '/dev/roomy 1 1 943718400 1% /roomy' ;;\n\
                 esac\n\
                 done\n",
                full = full.display()
            ),
        );
        Fixture { dir, full, roomy }
    }

    /// A one-commit repository holding this tree's gate.sh and the one
    /// lint the roster refuses to run without, under `parent`.
    fn tree_in(&self, parent: &Path) -> PathBuf {
        let tree = parent.join("checkout");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(
            &tree,
            &[
                "-c",
                "user.email=disk-floor@test",
                "-c",
                "user.name=disk-floor",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        tree
    }

    /// `gate.sh <mode>` in `tree`, its cargo target at `target`, its
    /// TMPDIR on the roomy volume.
    fn gate(&self, tree: &Path, mode: Option<&str>, target: &Path) -> Output {
        self.gate_with_tmp(tree, mode, target, &self.roomy.join("tmp"))
    }

    /// The same run with TMPDIR at `tmp`.
    fn gate_with_tmp(&self, tree: &Path, mode: Option<&str>, target: &Path, tmp: &Path) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(tree.join("infra/gate.sh"));
        if let Some(mode) = mode {
            cmd.arg(mode);
        }
        cmd.current_dir(tree)
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", FLOOR_GB)
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            .env("TMPDIR", tmp)
            .env("CARGO_TARGET_DIR", target)
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("WT_JOBS")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {mode:?} in {}: {e}", tree.display()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Past the startup floor, positively: the pre-flight's own line, which
/// only a run the floor let start can print. Absence of the word
/// "Refusing" alone would pass a floor that died some other way.
fn assert_started(t: &str) {
    assert!(
        t.contains("pre-flight: not run here, by their own headers"),
        "the run must get past the startup floor and into the pre-flight:\n{t}"
    );
}

/// The packet's own case: a checkout on the full volume, its cargo
/// target (not yet created) on the roomy one. `--lint` writes the target
/// and the lint scratch, never the checkout's volume, so it starts.
#[test]
fn lint_is_judged_on_its_target_not_on_the_checkout() {
    let f = Fixture::new("lint-roomy-target");
    let tree = f.tree_in(&f.full);
    let out = f.gate(&tree, Some("--lint"), &f.roomy.join("target-cold"));
    let t = text(&out);
    assert_started(&t);
    assert!(
        !t.contains("Refusing"),
        "--lint's clippy writes its cargo target (900 GB free here), not the checkout \
         (1 GB free): the floor must not refuse it on the checkout's volume \
         (backlog 878e5a0f):\n{t}"
    );
}

/// The addendum (builder run e205c820): `--quick` compiles nothing, so
/// the checkout's volume is not its to judge either.
#[test]
fn quick_is_not_refused_for_a_full_checkout() {
    let f = Fixture::new("quick-full-checkout");
    let tree = f.tree_in(&f.full);
    let out = f.gate(&tree, Some("--quick"), &f.full.join("target"));
    let t = text(&out);
    assert_started(&t);
    assert!(
        !t.contains("Refusing"),
        "--quick compiles nothing and writes only its lints' scratch (900 GB free here); \
         neither the checkout nor a cargo target is its to judge:\n{t}"
    );
}

/// The other direction, which today's floor could not see: a roomy
/// checkout whose cargo target sits on the full volume. Clippy would
/// fill that volume, so `--lint` refuses — and names the target.
#[test]
fn lint_refuses_a_full_target_volume_and_names_it() {
    let f = Fixture::new("lint-full-target");
    let tree = f.tree_in(&f.roomy);
    let target = f.full.join("target");
    let out = f.gate(&tree, Some("--lint"), &target);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2, not a failed check:\n{t}"
    );
    assert!(
        t.contains("1GB free, need 12GB. Refusing to start"),
        "the refusal keeps its first line, which readers already parse:\n{t}"
    );
    assert!(
        t.contains("/full")
            && t.contains("cargo target")
            && t.contains(&target.display().to_string()),
        "the refusal must name the volume it refused on and what writes there:\n{t}"
    );
}

/// The gate proper writes all three, so a full checkout still refuses
/// it — and the refusal names BOTH volumes it measured, so the reader
/// can see the target was not the problem.
#[test]
fn the_gate_judges_the_checkout_and_names_every_volume() {
    let f = Fixture::new("gate-full-checkout");
    let tree = f.tree_in(&f.full);
    let target = f.roomy.join("target-cold");
    let out = f.gate(&tree, Some("--auto"), &target);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2, not a failed check:\n{t}"
    );
    assert!(
        t.contains("1GB free, need 12GB. Refusing to start"),
        "the gate writes into its checkout (web install and builds), so a full one refuses:\n{t}"
    );
    assert!(
        t.contains("/full") && t.contains("the checkout"),
        "the refusal names the checkout's volume as the one that refused:\n{t}"
    );
    assert!(
        t.contains("/roomy") && t.contains("cargo target"),
        "the refusal names the target's volume too, with its free space:\n{t}"
    );
    assert_repairs_only(&t, CHECKOUT_REPAIR);
}

// THE ADVICE FOLLOWS THE VOLUME (backlog 011be442). Once the refusal named
// its volume, the fixed paragraph under it visibly mismatched: a /scratch
// refusal was told `du -sh */target` and `rm -rf <worktree>/target`
// (worktree targets live at /scratch/target-<name>), a TMPDIR refusal got
// target advice, and every refusal ended "build in the dev pod, which has
// 188GB of scratch" — circular on the dev pod, where every pre-flight runs.

/// Each writer's repair opens with its own line; a refusal prints the one
/// for the volume that refused and no other.
const TMP_REPAIR: &str = "remediation for the lint roster's scratch";
const TARGET_REPAIR: &str = "remediation for the cargo target";
const CHECKOUT_REPAIR: &str = "remediation for the checkout";

fn assert_repairs_only(t: &str, which: &str) {
    assert_repairs_exactly(t, &[which]);
}

/// Every writer in `which` gets its repair, and no other writer does.
/// Several, when writers share the volume that refused (review 2452d6ff,
/// F1): on the dev pod TMPDIR (/tmp, the overlay root) and /scratch are
/// one partition, so every `--lint` refusal there binds both rows at the
/// same free GB.
fn assert_repairs_exactly(t: &str, which: &[&str]) {
    for w in which {
        assert!(
            t.contains(w),
            "the refusal must carry the repair for every writer on the volume that refused ({w}):\n{t}"
        );
    }
    for other in [TMP_REPAIR, TARGET_REPAIR, CHECKOUT_REPAIR] {
        if !which.contains(&other) {
            assert!(
                !t.contains(other),
                "a volume that did not refuse gets no repair ({other}):\n{t}"
            );
        }
    }
    for stale in ["build in the dev pod", "*/target"] {
        assert!(
            !t.contains(stale),
            "the old fixed advice ({stale}) must be gone (backlog 011be442):\n{t}"
        );
    }
}

/// A full cargo target is repaired as a cargo target, at its own path.
#[test]
fn a_target_refusal_repairs_the_target() {
    let f = Fixture::new("repair-target");
    let tree = f.tree_in(&f.roomy);
    let target = f.full.join("target");
    let out = f.gate(&tree, Some("--lint"), &target);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2:\n{t}"
    );
    assert_repairs_only(&t, TARGET_REPAIR);
    // The door before the hand (review 2452d6ff, F2): the target's repair
    // names the reclaim that takes idle targets oldest first, spares the
    // seed and records what it took, as a COMMAND, ahead of any rm -rf.
    let door = t
        .lines()
        .position(|l| l.trim_start().starts_with(RECLAIM_DOOR))
        .unwrap_or_else(|| {
            panic!("the target's repair must name `{RECLAIM_DOOR}` as a command to run:\n{t}")
        });
    if let Some(hand) = t.lines().position(|l| l.contains("rm -rf")) {
        assert!(
            door < hand,
            "the reclaim door comes before deleting a target by hand:\n{t}"
        );
    }
}

/// The door a target refusal is sent to on the dev pod.
const RECLAIM_DOOR: &str = "(";

#[test]
fn printed_reclaim_binds_the_seed_and_declared_token_transport() {
    let f = Fixture::new("repair-target-authenticated");
    let tree = f.tree_in(&f.roomy);
    let target = f.full.join("target");
    let out = f.gate(&tree, Some("--lint"), &target);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(2), "{t}");
    let start = t
        .lines()
        .position(|l| l.trim() == "(")
        .expect("printed reclaim subshell");
    let lines: Vec<_> = t
        .lines()
        .skip(start)
        .take_while(|l| l.trim() != ")")
        .collect();
    let command = format!("{}\n)\n", lines.join("\n"));
    boss_testing::write_file(
        &tree.join("infra/dev/machine-token-dir"),
        "declared-token-dir\n",
    );
    boss_testing::write_file(
        &tree.join("infra/dev/sor-url"),
        "http://jobs.example.invalid:7900\n",
    );
    boss_testing::create_dir(&tree.join("infra/cluster"));
    boss_testing::write_exec(
        &tree.join("infra/cluster/dev-scratch-reclaim.sh"),
        "#!/usr/bin/env bash\n[ \"$1\" = --scratch-floor ] || exit 17\nprintf '%s\\n' \"$SCRATCH_MOUNT\" \"$CARGO_TARGET_DIR\" \"$BOSS_MACHINE_TOKEN_DIR\" \"$BOSS_MACHINE_TOKEN_HOSTS\"\n",
    );
    let replay = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&tree)
        .env("WT_TARGET_ROOT", &f.full)
        .env_remove("WT_SEED")
        .env("CARGO_TARGET_DIR", "hostile-worktree-target")
        .env("BOSS_MACHINE_TOKEN_DIR", "hostile-token-dir")
        .env("BOSS_MACHINE_TOKEN_HOSTS", "hostile.example.invalid")
        .output()
        .unwrap();
    assert!(replay.status.success(), "{}", text(&replay));
    let expected_seed = f.full.join("target-checkout");
    assert_eq!(
        String::from_utf8_lossy(&replay.stdout),
        format!(
            "{}\n{}\ndeclared-token-dir\njobs.example.invalid\n",
            f.full.display(),
            expected_seed.display()
        )
    );
    std::fs::remove_file(tree.join("infra/dev/machine-token-dir")).unwrap();
    let unavailable = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&tree)
        .env("WT_TARGET_ROOT", &f.full)
        .env_remove("WT_SEED")
        .env("BOSS_MACHINE_TOKEN_DIR", "hostile-token-dir")
        .output()
        .unwrap();
    assert!(
        !unavailable.status.success(),
        "a missing declaration must refuse before reclaim: {}",
        text(&unavailable)
    );
    assert!(
        unavailable.stdout.is_empty(),
        "the stand-in reclaim must not execute without declared transport: {}",
        text(&unavailable)
    );
    boss_testing::write_file(&tree.join("infra/dev/machine-token-dir"), "");
    let empty = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&tree)
        .env("WT_TARGET_ROOT", &f.full)
        .env_remove("WT_SEED")
        .env("BOSS_MACHINE_TOKEN_DIR", "hostile-token-dir")
        .output()
        .unwrap();
    assert!(
        !empty.status.success(),
        "an empty declaration must refuse before reclaim: {}",
        text(&empty)
    );
    assert!(
        empty.stdout.is_empty(),
        "the stand-in reclaim must not execute with an empty declared token directory: {}",
        text(&empty)
    );
}

/// The dev pod's shape (review 2452d6ff, F1): TMPDIR and the cargo target
/// on ONE full volume, so both rows bind at the same free GB. The refusal
/// repairs both writers — before this, only the first row at the minimum
/// was repaired, which is always TMPDIR, so the target's advice never
/// printed on the pod the car was built for.
#[test]
fn a_refusal_shared_by_two_writers_repairs_both() {
    let f = Fixture::new("repair-shared-tmp-target");
    let tree = f.tree_in(&f.roomy);
    let out = f.gate_with_tmp(
        &tree,
        Some("--lint"),
        &f.full.join("target"),
        &f.full.join("tmp"),
    );
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2:\n{t}"
    );
    assert_repairs_exactly(&t, &[TMP_REPAIR, TARGET_REPAIR]);
}

/// The gate pod's shape: the checkout and the target share /gate-target,
/// TMPDIR is elsewhere. Both writers on the full volume are repaired; the
/// roomy TMPDIR is not.
#[test]
fn a_gate_refusal_shared_by_checkout_and_target_repairs_both() {
    let f = Fixture::new("repair-shared-checkout-target");
    let tree = f.tree_in(&f.full);
    let out = f.gate(&tree, Some("--auto"), &f.full.join("target"));
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2:\n{t}"
    );
    assert_repairs_exactly(&t, &[TARGET_REPAIR, CHECKOUT_REPAIR]);
}

/// A full TMPDIR under `--lint` (target roomy) is repaired as scratch, not
/// as a target — the case the fixed paragraph got wrong.
#[test]
fn a_tmpdir_refusal_repairs_the_scratch_not_a_target() {
    let f = Fixture::new("repair-tmp");
    let tree = f.tree_in(&f.roomy);
    let out = f.gate_with_tmp(
        &tree,
        Some("--lint"),
        &f.roomy.join("target-cold"),
        &f.full.join("tmp"),
    );
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2:\n{t}"
    );
    assert!(
        t.contains("(TMPDIR)") && t.contains("/full"),
        "the refusal names TMPDIR's volume:\n{t}"
    );
    assert_repairs_only(&t, TMP_REPAIR);
}

/// `--quick` judges TMPDIR alone, so its refusal is always a scratch
/// refusal — and must never send its reader to a cargo target it does not
/// build.
#[test]
fn a_quick_refusal_repairs_the_scratch() {
    let f = Fixture::new("quick-full-tmp");
    let tree = f.tree_in(&f.roomy);
    let out = f.gate_with_tmp(
        &tree,
        Some("--quick"),
        &f.roomy.join("target"),
        &f.full.join("tmp"),
    );
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2:\n{t}"
    );
    assert_repairs_only(&t, TMP_REPAIR);
}
