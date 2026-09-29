//! `infra/dev/wt-cargo` — the door every cargo build on the dev pod goes
//! through: a per-worktree `CARGO_TARGET_DIR` reflink-seeded from the
//! warm shared one, and a jobs bound that keeps a builder inside its
//! share of the pod's cgroup — the limits the dev container declares in
//! `infra/cluster/manifests/boss-dev.yaml`, not a figure typed here
//! (backlog 34b29b52; the typed one said 16 GiB against a declared 32Gi,
//! 28fc3a39).
//!
//! Until 2026-09-14 this script was pod-local text under
//! /work/tools/bin, with its OWN copy of the cargo bound — and it
//! drifted (8, against pod-build.env's 6). Now it lives in the tree and
//! sources `infra/dev/pod-build.env`, so there is one number; these
//! tests read that file rather than retyping the number, for the same
//! reason. Pinned here, against a scratch git repository and stub
//! `cargo` / `nice` / `cp` binaries, so nothing touches /scratch:
//!
//!   * an operator worktree builds at pod-build.env's bound, un-niced;
//!   * a builder worktree (basename `agent-*`) builds 4-wide under
//!     `nice -n 10`, so the interactive shell wins the scheduler;
//!   * `WT_JOBS` wins over either;
//!   * the reflink seed is attempted only when the seed exists —
//!     otherwise the per-worktree dir starts cold, and says so;
//!   * the seed lands in a sibling `.seeding` dir and is renamed into
//!     place only once whole (backlog 08782b71): a copy that dies
//!     part-way leaves no half target for the next run to believe warm;
//!   * every seeded file is backdated to the epoch before that rename
//!     (backlog 1150c518), so a seed built after this tree's checkout,
//!     from other sources, is rebuilt rather than trusted — pinned with
//!     real cargo on a two-crate workspace, not the stub.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/wt-cargo";

/// The bound pod-build.env declares, read from the file — the test
/// must not become the second literal the script just stopped being.
fn env_file_bound() -> String {
    let live = std::fs::read_to_string(repo_root().join("infra/dev/pod-build.env"))
        .expect("infra/dev/pod-build.env");
    live.lines()
        .find_map(|l| l.strip_prefix("CARGO_BUILD_JOBS="))
        .expect("pod-build.env sets CARGO_BUILD_JOBS")
        .trim()
        .to_string()
}

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    /// Written by the `nice` stub, once per call, with its argv.
    niced: PathBuf,
    /// Written by the `cp` stub, once per call, with its argv.
    copied: PathBuf,
    /// The stub scratch-floor pass wt-cargo runs before each build.
    reclaim: PathBuf,
    /// Written by that stub: its argv, the scratch mount and primary it
    /// was handed, and every dir under the mount with its mtime.
    reclaimed: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-cargo-{name}"));
        let bin = root.join("bin");
        boss_testing::create_dir(&bin);
        let niced = root.join("niced.txt");
        let copied = root.join("copied.txt");
        // cargo: print what the script decided. The real one is never
        // reached — this test is about the environment it is handed.
        // Under `--no-run` it also prints one `Executable` line per test
        // binary, the way the real cargo does — that is what the count
        // pass reads (backlog 3566f5b4). STUB_EXECUTABLES sets how many,
        // STUB_BUILD_FAIL makes the build fail instead.
        // With STUB_LOCK set it also says whether that lock is held
        // while cargo runs: an exclusive try from a fresh descriptor
        // fails exactly when a build holds it shared (94cd0c23).
        write_exec(
            &bin.join("cargo"),
            "#!/usr/bin/env bash\n\
             echo \"target=${CARGO_TARGET_DIR:-unset}\"\n\
             echo \"jobs=${CARGO_BUILD_JOBS:-unset}\"\n\
             echo \"argv=$*\"\n\
             if [ -n \"${STUB_LOCK:-}\" ]; then\n\
               if flock -n -x \"$STUB_LOCK\" true; then echo lock=free; else echo lock=held; fi\n\
             fi\n\
             case \"$*\" in\n\
               *--no-run*)\n\
                 if [ -n \"${STUB_BUILD_FAIL:-}\" ]; then\n\
                   echo 'error: could not compile'\n\
                   exit \"$STUB_BUILD_FAIL\"\n\
                 fi\n\
                 i=1\n\
                 while [ \"$i\" -le \"${STUB_EXECUTABLES:-3}\" ]; do\n\
                   echo \"  Executable tests/t$i.rs (/dev/null)\"\n\
                   i=$((i+1))\n\
                 done\n\
                 ;;\n\
             esac\n",
        );
        // nice: record that it was called and with what, then run the
        // rest exactly as the real one would.
        write_exec(
            &bin.join("nice"),
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> \"$STUB_NICED\"\n\
             shift 2\n\
             exec \"$@\"\n",
        );
        // cp: a reflink copy needs XFS and the test runs on whatever
        // scratch is (tmpfs, overlay). Record the call and make the
        // destination exist, which is all the script reads back. With
        // STUB_CP_FAIL set it dies PART-WAY the way a real cp does —
        // destination created, one file inside, nonzero exit — so a
        // test can see what the script leaves behind.
        write_exec(
            &bin.join("cp"),
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> \"$STUB_COPIED\"\n\
             mkdir -p \"${!#}\"\n\
             if [ -n \"${STUB_CP_FAIL:-}\" ]; then\n\
                 touch \"${!#}/half-copied\"\n\
                 exit \"$STUB_CP_FAIL\"\n\
             fi\n",
        );
        // The scratch-floor pass: never the real one here, which would
        // take a df of whatever filesystem the test runs on. Records
        // what it was handed and exits STUB_RECLAIM_RC.
        let reclaim = root.join("reclaim-stub.sh");
        let reclaimed = root.join("reclaimed.txt");
        write_exec(
            &reclaim,
            "#!/usr/bin/env bash\n\
             {\n\
               echo \"argv=$* mount=${SCRATCH_MOUNT:-unset} primary=${CARGO_TARGET_DIR:-unset}\"\n\
               for d in \"$SCRATCH_MOUNT\"/*/; do\n\
                 [ -d \"$d\" ] && echo \"dir=$(basename \"$d\") mtime=$(stat -c %Y \"$d\")\"\n\
               done\n\
             } >> \"$STUB_RECLAIMED\"\n\
             exit \"${STUB_RECLAIM_RC:-0}\"\n",
        );
        Self {
            root,
            bin,
            niced,
            copied,
            reclaim,
            reclaimed,
        }
    }

    fn reclaim_calls(&self) -> String {
        std::fs::read_to_string(&self.reclaimed).unwrap_or_default()
    }

    /// A real git repository whose basename is `name` — the script
    /// reads both through `git rev-parse --show-toplevel`.
    fn worktree(&self, name: &str) -> PathBuf {
        let wt = self.root.join("trees").join(name);
        boss_testing::create_dir(&wt);
        let out = Command::new("git")
            .args(["init", "-q"])
            .arg(&wt)
            .output()
            .expect("git init");
        assert!(
            out.status.success(),
            "git init {}: {}",
            wt.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        wt
    }

    fn targets(&self) -> PathBuf {
        self.root.join("targets")
    }

    fn run(&self, worktree: &Path, seed: &Path, env: &[(&str, &str)]) -> (i32, String) {
        self.run_args(worktree, seed, env, &["test", "-p", "boss-cli"])
    }

    fn run_args(
        &self,
        worktree: &Path,
        seed: &Path,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> (i32, String) {
        let out = self
            .command(worktree, seed, env, args)
            .output()
            .expect("run wt-cargo");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    fn command(
        &self,
        worktree: &Path,
        seed: &Path,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Command {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .current_dir(worktree)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_NICED", &self.niced)
            .env("STUB_COPIED", &self.copied)
            .env("WT_SEED", seed)
            .env("WT_TARGET_ROOT", self.targets())
            .env("WT_RECLAIM", &self.reclaim)
            .env("STUB_RECLAIMED", &self.reclaimed)
            .env_remove("STUB_RECLAIM_RC")
            .env_remove("WT_JOBS")
            .env_remove("STUB_CP_FAIL")
            .env_remove("STUB_EXECUTABLES")
            .env_remove("STUB_BUILD_FAIL")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("STUB_LOCK");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    fn nice_calls(&self) -> String {
        std::fs::read_to_string(&self.niced).unwrap_or_default()
    }

    fn cp_calls(&self) -> String {
        std::fs::read_to_string(&self.copied).unwrap_or_default()
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/wt-cargo is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read wt-cargo");
    assert!(
        text.contains("pod-build.env"),
        "the bound must be sourced from infra/dev/pod-build.env, not retyped"
    );
    assert!(
        !text.contains("WT_JOBS:-6") && !text.contains("WT_JOBS:-8"),
        "no second literal for the operator bound — that is the drift this car closes"
    );
}

/// The operator's worktree: pod-build.env's bound, no nice, and — with
/// no seed to copy — a cold target dir that says it is cold.
#[test]
fn an_operator_worktree_builds_at_the_pod_bound_unniced() {
    let f = Fixture::new("operator");
    let wt = f.worktree("boss");
    let missing_seed = f.root.join("no-such-seed");

    let (rc, out) = f.run(&wt, &missing_seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let expected_target = f.targets().join("target-boss");
    assert!(
        out.contains(&format!("target={}", expected_target.display())),
        "CARGO_TARGET_DIR must be the per-worktree dir: {out}"
    );
    assert!(
        expected_target.is_dir(),
        "the cold dir must be created: {out}"
    );
    assert!(
        out.contains(&format!("jobs={}", env_file_bound())),
        "the bound must be pod-build.env's: {out}"
    );
    assert!(
        out.contains("argv=test -p boss-cli"),
        "cargo's argv must pass through: {out}"
    );
    assert!(
        out.contains("starts cold"),
        "a missing seed must be said, not silent: {out}"
    );
    assert_eq!(f.nice_calls(), "", "the operator's build is not niced");
    assert_eq!(f.cp_calls(), "", "no seed, no copy attempted");
}

/// A builder's worktree: 4-wide under `nice -n 10`, and — with a seed
/// present — the per-worktree dir is reflink-seeded from it.
#[test]
fn a_builder_worktree_builds_four_wide_and_niced_from_the_seed() {
    let f = Fixture::new("builder");
    let wt = f.worktree("agent-0123abcd");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let expected_target = f.targets().join("target-agent-0123abcd");
    assert!(
        out.contains(&format!("target={}", expected_target.display())),
        "CARGO_TARGET_DIR must be the per-worktree dir: {out}"
    );
    assert!(out.contains("jobs=4"), "a builder builds 4-wide: {out}");
    assert!(
        f.nice_calls().starts_with("-n 10 cargo"),
        "a builder's cargo runs under nice -n 10: {:?}",
        f.nice_calls()
    );
    let cp = f.cp_calls();
    assert!(
        cp.contains("--reflink=always")
            && cp.contains(seed.to_str().expect("utf8"))
            && cp.contains(expected_target.to_str().expect("utf8")),
        "the seed must be reflink-copied to the per-worktree dir: {cp:?}"
    );
    assert!(
        out.contains("seeded") && out.contains("by reflink"),
        "the seed step must be reported: {out}"
    );

    // Second run: the dir exists, so no second copy.
    let (rc, _) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0);
    assert_eq!(
        f.cp_calls().lines().count(),
        1,
        "an existing per-worktree dir is never re-seeded"
    );
}

/// `WT_JOBS` is the explicit override, and it wins in both branches.
#[test]
fn wt_jobs_wins_in_both_branches() {
    let f = Fixture::new("override");
    let missing_seed = f.root.join("no-such-seed");

    let operator = f.worktree("boss");
    let (rc, out) = f.run(&operator, &missing_seed, &[("WT_JOBS", "2")]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    assert!(
        out.contains("jobs=2"),
        "WT_JOBS wins for the operator: {out}"
    );
    assert_eq!(f.nice_calls(), "", "WT_JOBS does not change the nice rule");

    let builder = f.worktree("agent-ffff");
    let (rc, out) = f.run(&builder, &missing_seed, &[("WT_JOBS", "2")]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    assert!(out.contains("jobs=2"), "WT_JOBS wins for a builder: {out}");
    assert!(
        f.nice_calls().starts_with("-n 10 cargo"),
        "a builder is still niced under WT_JOBS: {:?}",
        f.nice_calls()
    );
}

/// A seed copy that dies part-way (backlog 08782b71): the half copy
/// must not become `$dir`, or the next run sees the directory, skips
/// the seed, and builds against it believing it warm. The script must
/// clear the temp dir, start `$dir` cold, say WHY (cp's exit code),
/// and still run cargo.
#[test]
fn a_failed_seed_leaves_no_half_target_and_starts_cold_saying_why() {
    let f = Fixture::new("failed-seed");
    let wt = f.worktree("agent-halfcopy");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[("STUB_CP_FAIL", "1")]);
    assert_eq!(rc, 0, "a failed seed is not a failed build: {out}");

    let target = f.targets().join("target-agent-halfcopy");
    let seeding = f.targets().join("target-agent-halfcopy.seeding");
    assert!(
        !seeding.exists(),
        "the temp dir must be removed after a failed copy: {out}"
    );
    assert!(target.is_dir(), "the dir must still exist, cold: {out}");
    assert!(
        !target.join("half-copied").exists(),
        "the half copy must not be renamed into place: {out}"
    );
    assert!(
        out.contains("starts cold"),
        "the existing cold message must be kept: {out}"
    );
    assert!(
        out.contains("cp exit 1"),
        "the message must name why the seed failed: {out}"
    );
    assert!(
        out.contains(&format!("target={}", target.display()))
            && out.contains("argv=test -p boss-cli"),
        "cargo must still run against the cold dir: {out}"
    );
}

/// A seed copy that succeeds lands in `$dir.seeding` and is renamed to
/// `$dir`; a stale `.seeding` left by a killed run is cleared before
/// the attempt, so nothing from it rides into the new target.
#[test]
fn a_successful_seed_is_renamed_into_place_over_a_stale_temp_dir() {
    let f = Fixture::new("renamed-seed");
    let wt = f.worktree("agent-rename");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);
    let target = f.targets().join("target-agent-rename");
    let seeding = f.targets().join("target-agent-rename.seeding");
    boss_testing::create_dir(&seeding);
    std::fs::write(seeding.join("stale-from-killed-run"), "").expect("stale marker");

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    let cp = f.cp_calls();
    assert!(
        cp.trim_end().ends_with(seeding.to_str().expect("utf8")),
        "cp's destination must be the sibling temp dir, not $dir: {cp:?}"
    );
    assert!(
        target.is_dir(),
        "the seeded dir must be renamed into place: {out}"
    );
    assert!(
        !seeding.exists(),
        "no temp dir remains after a successful seed: {out}"
    );
    assert!(
        !target.join("stale-from-killed-run").exists(),
        "a stale .seeding is cleared before the attempt, not renamed in: {out}"
    );
    assert!(
        out.contains("seeded") && out.contains("by reflink"),
        "the seed step must be reported: {out}"
    );
}

/// Outside a git worktree there is no name to key the target dir on;
/// the script refuses rather than building into `target-`.
#[test]
fn outside_a_worktree_it_refuses() {
    let f = Fixture::new("nogit");
    let nowhere = f.root.join("plain-dir");
    boss_testing::create_dir(&nowhere);
    let (rc, out) = f.run(&nowhere, &f.root.join("no-such-seed"), &[]);
    assert_eq!(rc, 2, "must refuse with exit 2: {out}");
    assert!(out.contains("not in a git worktree"), "must say why: {out}");
    assert!(!out.contains("argv="), "cargo must not run: {out}");
}

/// MEASURED 2026-09-22 (backlog 3566f5b4). `cargo test -p boss-jobs
/// --all-features` reaches 148 test binaries in 11m20s on this pod —
/// longer than the 10-minute window a builder's tool call gets. A run
/// cut off at that mark has printed 125 `Running` lines, no failure and
/// no final `test result:` summary, so it reads exactly like a green
/// suite while the last eleven binaries (everything sorting at or after
/// `waiting_on_pg.rs`) never ran. That car went to the gate twice, red
/// on `yard_borders_http` and then on `yard_regions_http` — two
/// one-line stale expectations a completed local run would have named
/// in seconds.
///
/// Counting `Running` lines was already possible; what was missing was
/// the total to count them against, and it has to be printed BEFORE the
/// run or the truncation eats it too. `cargo test --no-run` prints one
/// `Executable` line per test binary and the real run then prints one
/// `Running` line per binary — measured equal at 148 — so the count
/// pass is the same build the run needs anyway.
#[test]
fn a_test_run_says_how_many_binaries_it_must_reach_before_it_starts() {
    let f = Fixture::new("count");
    let wt = f.worktree("agent-count");
    let (rc, out) = f.run(
        &wt,
        &f.root.join("no-such-seed"),
        &[("STUB_EXECUTABLES", "7")],
    );
    assert_eq!(rc, 0, "wt-cargo failed: {out}");

    assert!(
        out.contains("7 test binaries"),
        "the count must be stated: {out}"
    );
    assert!(
        out.contains("Running"),
        "the line must name what to count against it: {out}"
    );
    assert!(
        out.contains("argv=test -p boss-cli --no-run"),
        "the count pass is a --no-run build of the same arguments: {out}"
    );
    assert!(
        out.lines()
            .filter(|l| *l == "argv=test -p boss-cli")
            .count()
            == 1,
        "the real run must still happen, once, with the argv it was given: {out}"
    );
}

/// The count pass IS the build, so a build that fails must fail there
/// and stop — running cargo a second time would print every compiler
/// error twice, which is the opposite of the legibility this line is
/// for.
#[test]
fn a_build_that_fails_in_the_count_pass_stops_there() {
    let f = Fixture::new("count-fail");
    let wt = f.worktree("agent-countfail");
    let (rc, out) = f.run(
        &wt,
        &f.root.join("no-such-seed"),
        &[("STUB_BUILD_FAIL", "101")],
    );

    assert_eq!(rc, 101, "the build's own exit code must survive: {out}");
    assert!(
        out.contains("could not compile"),
        "the compiler's output must be shown, not swallowed: {out}"
    );
    assert_eq!(
        out.matches("argv=").count(),
        1,
        "cargo must not be run a second time after a failed build: {out}"
    );
    assert!(
        !out.contains("test binaries"),
        "nothing was built, so there is no count to state: {out}"
    );
}

/// The count pass is for a whole-crate suite run and nothing else: a
/// non-`test` subcommand, a run that already says `--no-run`, and an
/// invocation with its own `--` harness arguments (where `--no-run`
/// would land on libtest, which does not know it) are all left alone.
#[test]
fn the_count_pass_is_skipped_where_it_would_not_apply() {
    let f = Fixture::new("count-skip");
    let seed = f.root.join("no-such-seed");
    let cases: [&[&str]; 4] = [
        &["clippy", "-p", "boss-cli"],
        &["build", "-p", "boss-cli"],
        &["test", "-p", "boss-cli", "--no-run"],
        &["test", "-p", "boss-cli", "--", "--nocapture"],
    ];
    for args in cases {
        let wt = f.worktree(&format!("agent-skip-{}", args.join("-").replace("--", "x")));
        let (rc, out) = f.run_args(&wt, &seed, &[("STUB_EXECUTABLES", "7")], args);
        assert_eq!(rc, 0, "wt-cargo failed for {args:?}: {out}");
        assert_eq!(
            out.matches("argv=").count(),
            1,
            "{args:?} must reach cargo exactly once: {out}"
        );
        assert!(
            !out.contains("test binaries"),
            "{args:?} is not a whole-crate suite run: {out}"
        );
    }
}

/// THE FLOOR FOLLOWS THE BUILD (backlog 3f2a08ab, 2026-09-24): the dev
/// pod was evicted with the 25% scratch floor in force, because the
/// sidecar checks it hourly and w-1 fell from above the floor to the
/// kubelet's 15% line in under 27 minutes. The builds are what fill
/// /scratch, so each one runs the floor's sibling pass first — on this
/// door's own scratch root, with the SEED named as the primary (the pod
/// sets CARGO_TARGET_DIR to it, and this script re-points that variable
/// at the worktree's dir — handed through, the floor pass would read
/// the caller's dir as the primary and the warm seed as a sibling it
/// may take).
#[test]
fn a_build_first_runs_the_scratch_floor_pass_on_its_own_scratch_root() {
    let f = Fixture::new("floor");
    let wt = f.worktree("agent-floor0001");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    let calls = f.reclaim_calls();
    let first = calls
        .lines()
        .next()
        .unwrap_or_else(|| panic!("wt-cargo never ran the scratch-floor pass: {out}"));
    assert_eq!(
        first,
        format!(
            "argv=--scratch-floor mount={} primary={}",
            f.targets().display(),
            seed.display()
        ),
        "the pass runs in its floor mode, on this door's root, with the seed as primary"
    );
}

/// The floor pass takes idle siblings, least recently used first — and
/// a builder that has been reading for half an hour has an idle target.
/// Its own must not be the one taken the moment before it builds, so
/// the door marks it live first.
#[test]
fn the_callers_own_target_is_marked_live_before_the_floor_pass_runs() {
    let f = Fixture::new("floor-own");
    let wt = f.worktree("agent-floor0002");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);
    let own = f.targets().join("target-agent-floor0002");
    boss_testing::create_dir(&own);
    let ok = Command::new("touch")
        .args(["-d", "-5 hours"])
        .arg(&own)
        .status()
        .expect("touch")
        .success();
    assert!(ok, "touch -d");

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    let calls = f.reclaim_calls();
    let mtime: u64 = calls
        .lines()
        .find_map(|l| l.strip_prefix("dir=target-agent-floor0002 mtime="))
        .unwrap_or_else(|| panic!("the stub never saw the caller's target: {calls}"))
        .parse()
        .expect("mtime is a number");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    assert!(
        now.saturating_sub(mtime) < 600,
        "the caller's target must read as live (mtime {mtime}, now {now}) when the \
         floor pass looks at it: {calls}"
    );
}

/// A reclaim that fails is the hourly pass's to report; it never stops
/// a build, and it is never silent either.
#[test]
fn a_floor_pass_that_fails_never_stops_the_build() {
    let f = Fixture::new("floor-fails");
    let wt = f.worktree("agent-floor0003");
    let seed = f.root.join("no-such-seed");
    let (rc, out) = f.run(&wt, &seed, &[("STUB_RECLAIM_RC", "3")]);
    assert_eq!(rc, 0, "a failed floor pass must not fail the build: {out}");
    assert!(out.contains("argv=test"), "cargo still ran: {out}");
    assert!(
        out.contains("scratch-floor pass exited 3"),
        "the failure is named: {out}"
    );
}

/// Unstubbed, the door runs the sidecar's own script out of the tree
/// beside it — one reclaim, two triggers — and a floor of 0% keeps the
/// real pass a no-op on whatever filesystem this test runs on.
#[test]
fn the_default_floor_pass_is_the_sidecars_own_script() {
    let f = Fixture::new("floor-default");
    let wt = f.worktree("agent-floor0004");
    let seed = f.root.join("no-such-seed");
    let mut cmd = Command::new(repo_root().join(SCRIPT));
    cmd.args(["build", "-p", "boss-cli"])
        .current_dir(&wt)
        .env(
            "PATH",
            format!(
                "{}:{}",
                f.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("STUB_NICED", &f.niced)
        .env("STUB_COPIED", &f.copied)
        .env("WT_SEED", &seed)
        .env("WT_TARGET_ROOT", f.targets())
        .env("BOSS_SCRATCH_FLOOR_PCT", "0")
        .env_remove("WT_RECLAIM")
        .env_remove("CARGO_TARGET_DIR");
    let out = cmd.output().expect("run wt-cargo");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        !text.contains("scratch-floor"),
        "the tree's own pass is found, and above its floor it says nothing: {text}"
    );
    assert!(
        repo_root()
            .join("infra/cluster/dev-scratch-reclaim.sh")
            .is_file(),
        "the pass wt-cargo runs by default is the sidecar's"
    );
}

/// THE BUILD HOLDS ITS TARGET (backlog 94cd0c23, 0c18deed). A green's
/// free (boss-cli `scratch_target::free`) takes `<target>.lock`
/// exclusive without waiting and keeps the target when it will not
/// come — so wt-cargo must hold it shared for cargo's WHOLE run, and
/// let it go when cargo is done, or an idle tree is never freed.
#[test]
fn the_build_holds_its_targets_lock_for_its_whole_run_and_no_longer() {
    let f = Fixture::new("lock-held");
    let wt = f.worktree("agent-lock0001");
    let seed = f.root.join("no-such-seed");
    let lock = f.targets().join("target-agent-lock0001.lock");
    let lock_s = lock.to_str().expect("utf8").to_string();

    let (rc, out) = f.run(&wt, &seed, &[("STUB_LOCK", &lock_s)]);
    assert_eq!(rc, 0, "wt-cargo failed: {out}");
    assert!(
        out.contains("lock=held") && !out.contains("lock=free"),
        "cargo must run with the target's lock held — the count pass and the run both: {out}"
    );
    let after = Command::new("flock")
        .args(["-n", "-x"])
        .arg(&lock)
        .arg("true")
        .status()
        .expect("flock runs");
    assert!(
        after.success(),
        "once cargo exits the lock is free, or no green could ever free the target"
    );
}

/// A free that is removing the target holds the lock exclusive; a
/// build that starts meanwhile WAITS for it, says so, and then builds
/// — never into a dir being deleted under it.
#[test]
fn a_build_waits_out_a_free_in_progress_and_says_so() {
    use std::io::{BufRead, Read};
    let f = Fixture::new("lock-wait");
    let wt = f.worktree("agent-lock0002");
    let seed = f.root.join("no-such-seed");
    boss_testing::create_dir(&f.targets());
    let lock = f.targets().join("target-agent-lock0002.lock");

    let mut free = Command::new("bash")
        .arg("-c")
        .arg(r#"exec 8>>"$1" && flock -x 8 && echo held && exec sleep 300"#)
        .arg("free")
        .arg(&lock)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("bash runs");
    let mut line = String::new();
    std::io::BufReader::new(free.stdout.take().expect("piped"))
        .read_line(&mut line)
        .expect("the free says it holds the lock");
    assert_eq!(line.trim(), "held");

    let mut build = f
        .command(&wt, &seed, &[], &["build", "-p", "boss-cli"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("wt-cargo runs");
    let mut err = std::io::BufReader::new(build.stderr.take().expect("piped"));
    let mut said = String::new();
    loop {
        let mut l = String::new();
        let n = err.read_line(&mut l).expect("read stderr");
        said.push_str(&l);
        if n == 0 || l.contains("waiting") {
            break;
        }
    }
    let _ = free.kill();
    let _ = free.wait();
    err.read_to_string(&mut said).expect("rest of stderr");
    let mut out = String::new();
    build
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut out)
        .expect("stdout");
    let status = build.wait().expect("wt-cargo exits");
    assert!(
        said.contains("waiting") && said.contains(lock.to_str().expect("utf8")),
        "the wait names the lock it waits on: {said}"
    );
    assert!(status.success(), "{said}{out}");
    assert!(
        out.contains("argv=build -p boss-cli"),
        "then it builds: {out}"
    );
}

/// A seed whose files could not all be backdated is a seed cargo may
/// trust over this tree's sources (1150c518), so it is cleared and the
/// target starts cold, naming why — never renamed into place.
#[test]
fn a_seed_that_cannot_be_backdated_starts_cold_saying_why() {
    let f = Fixture::new("backdate-fails");
    write_exec(
        &f.bin.join("touch"),
        "#!/usr/bin/env bash\n\
         case \" $* \" in *' @1 '*) exit 4 ;; esac\n\
         for c in /usr/bin/touch /bin/touch; do [ -x \"$c\" ] && exec \"$c\" \"$@\"; done\n\
         exit 127\n",
    );
    let wt = f.worktree("agent-nobackdate");
    let seed = f.root.join("seed");
    boss_testing::create_dir(&seed);

    let (rc, out) = f.run(&wt, &seed, &[]);
    assert_eq!(rc, 0, "a seed that fails is not a failed build: {out}");
    assert!(
        out.contains("backdating exit") && out.contains("starts cold"),
        "the message must say the backdate failed and the target is cold: {out}"
    );
    assert!(
        !f.targets().join("target-agent-nobackdate.seeding").exists(),
        "the un-backdated copy must be removed: {out}"
    );
    assert!(
        !out.contains("seeded"),
        "an un-backdated copy must never be reported as a seed: {out}"
    );
}

/// Run `git` in `dir` with an identity of its own, and require success.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// THE SEED IS OLDER THAN ANY CHECKOUT (backlog 1150c518, 2026-09-28).
/// `cp -a` keeps every mtime, and cargo trusts a path crate's artifact
/// whose dep-info is NEWER than its sources — while the artifact's
/// identity (the `-C metadata` hash) leaves the workspace path out. So
/// a seed built AFTER a worktree was checked out, from a revision where
/// a path crate reads differently, is trusted as this tree's own build:
/// measured by analyst run d666c988, the first build printed `value=1`
/// from a tree whose source says 2, with no Compiling line, and the
/// second did too. A false green that nothing reports.
///
/// Real cargo, a real two-crate workspace, the hazardous order: the
/// worktree at B (value 2) exists first, the seed is built from A
/// (value 1) after it. The worktree's sources are also dated an hour
/// back, so the order holds on a filesystem with coarse mtimes. `cp`
/// is a shim that drops `--reflink=always` and runs the real `cp -a`:
/// reflink is where the bytes live, `-a` is what keeps the mtimes, and
/// the test must not depend on the filesystem it runs on being XFS.
#[test]
fn a_seed_built_after_the_checkout_from_other_sources_is_rebuilt_not_trusted() {
    let root = scratch_dir("wt-cargo-late-seed");
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    write_exec(
        &bin.join("cp"),
        "#!/usr/bin/env bash\n\
         args=()\n\
         for a in \"$@\"; do [ \"$a\" = --reflink=always ] || args+=(\"$a\"); done\n\
         for c in /usr/bin/cp /bin/cp; do [ -x \"$c\" ] && exec \"$c\" \"${args[@]}\"; done\n\
         echo 'cp shim: no real cp' >&2; exit 127\n",
    );

    // The main checkout: commit A says 1, commit B says 2, and the
    // checkout is left at A — the tree the seed is built from.
    let repo = root.join("repo");
    boss_testing::create_dir(&repo.join("dep/src"));
    boss_testing::create_dir(&repo.join("app/src"));
    boss_testing::write_file(
        &repo.join("Cargo.toml"),
        "[workspace]\nmembers = [\"dep\", \"app\"]\nresolver = \"2\"\n",
    );
    boss_testing::write_file(
        &repo.join("dep/Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    boss_testing::write_file(
        &repo.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    );
    boss_testing::write_file(
        &repo.join("app/src/main.rs"),
        "fn main() { println!(\"value={}\", dep::value()); }\n",
    );
    boss_testing::write_file(
        &repo.join("dep/src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    );
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "A"]);
    boss_testing::write_file(
        &repo.join("dep/src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    );
    git(&repo, &["commit", "-qam", "B"]);
    git(&repo, &["tag", "B"]);
    git(&repo, &["checkout", "-q", "HEAD~1"]);

    // The worktree at B, checked out FIRST, its sources an hour old.
    let wt = root.join("trees").join("late-seed");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().expect("utf8"),
            "B",
        ],
    );
    let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for f in [
        "Cargo.toml",
        "dep/Cargo.toml",
        "dep/src/lib.rs",
        "app/Cargo.toml",
        "app/src/main.rs",
    ] {
        std::fs::File::options()
            .write(true)
            .open(wt.join(f))
            .and_then(|h| h.set_modified(an_hour_ago))
            .unwrap_or_else(|e| panic!("date {f} back: {e}"));
    }
    assert!(
        std::fs::read_to_string(wt.join("dep/src/lib.rs"))
            .expect("worktree dep")
            .contains("{ 2 }"),
        "the worktree must be at B"
    );

    // The seed, built from A AFTER the worktree exists — the order a
    // seed re-built by a sidecar pass lands in (da1d903f).
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let seed = root.join("seed");
    let built = Command::new(&cargo)
        .args(["build", "-q", "--offline", "-p", "app"])
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", &seed)
        .output()
        .expect("cargo builds the seed");
    assert!(
        built.status.success(),
        "the seed build: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let out = Command::new(repo_root().join(SCRIPT))
        .args(["run", "-q", "--offline", "-p", "app"])
        .current_dir(&wt)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("WT_SEED", &seed)
        .env("WT_TARGET_ROOT", root.join("targets"))
        .env("WT_RECLAIM", root.join("no-floor-pass"))
        .env_remove("WT_JOBS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_JOBS")
        .output()
        .expect("run wt-cargo");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "wt-cargo run: {stdout}{stderr}");
    assert!(
        stderr.contains("seeded") && stderr.contains("by reflink"),
        "the seed must have been used, or a cold target passes this for nothing: {stderr}"
    );
    assert_eq!(
        stdout.trim(),
        "value=2",
        "the first build from the seed must run THIS tree's source, not the seed's: {stderr}"
    );
}

// ---------------------------------------------------------------------
// `wt-cargo suite <crate>` (backlog ede6245a, 2026-09-28).
// ---------------------------------------------------------------------
// MEASURED by triage run ed6ad65a over twenty builder logs that day:
// `wt-cargo test -p boss-testing --all-features` spent 410-782 s in test
// execution alone and 411-1128 s with its compile — median 11.2 min, and
// 12 of 20 past the ten-minute window a builder's tool call gets, after
// which the harness moves the call to the background by itself. Rule 4
// asks for the whole suite before the push and rule 9 forbids background
// tasks, so every boss-testing car broke one of them; and the suite grew
// from 169 to 206 binaries in two days, so no split typed into the rules
// stays under the window for long.
//
// So no single call can run the whole suite, however it is chunked. The
// verb runs the crate's test targets — listed from cargo at run time,
// never typed — in chunks sized by each target's MEASURED duration, and
// stops before its call's budget; the next call resumes on the same
// tree, and only a call that finds every target passed on that tree
// exits 0. These tests drive it with REAL cargo on a small fixture
// crate: the target list, the `--test` selectors and libtest's output
// are cargo's own, not a stub's idea of them.

/// A standalone crate with a library (one unit test, one doctest) and
/// three integration tests. `beta` fails while `FX_FAIL` is set, so a
/// test can make one binary red without editing the tree.
struct Suite {
    root: PathBuf,
    wt: PathBuf,
}

impl Suite {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-cargo-suite-{name}"));
        let wt = root.join("trees").join(format!("agent-suite-{name}"));
        boss_testing::create_dir(&wt.join("src"));
        boss_testing::create_dir(&wt.join("tests"));
        boss_testing::write_file(
            &wt.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        );
        boss_testing::write_file(
            &wt.join("src/lib.rs"),
            "/// ```\n/// assert_eq!(fx::one(), 1);\n/// ```\npub fn one() -> u32 { 1 }\n\n\
             #[cfg(test)]\nmod tests {\n    #[test]\n    fn unit() { assert_eq!(super::one(), 1); }\n}\n",
        );
        boss_testing::write_file(
            &wt.join("tests/alpha.rs"),
            "#[test]\nfn alpha() { assert_eq!(fx::one(), 1); }\n",
        );
        boss_testing::write_file(
            &wt.join("tests/beta.rs"),
            "#[test]\nfn beta() { assert!(std::env::var(\"FX_FAIL\").is_err(), \"FX_FAIL is set\"); }\n",
        );
        boss_testing::write_file(
            &wt.join("tests/gamma.rs"),
            "#[test]\nfn gamma() { assert_eq!(fx::one(), 1); }\n",
        );
        git(&wt, &["init", "-q"]);
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "fixture"]);
        Self { root, wt }
    }

    fn targets(&self) -> PathBuf {
        self.root.join("targets")
    }

    /// The measured-duration history the verb keeps beside the targets.
    fn times(&self) -> PathBuf {
        self.targets().join("wt-cargo-suite-times").join("fx.tsv")
    }

    /// `wt-cargo suite fx` with a chunk cap of 20 s and an unmeasured
    /// target estimated at 10 s, so two unmeasured targets fill a chunk.
    fn run(&self, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(["suite", "fx"])
            .current_dir(&self.wt)
            .env("WT_SEED", self.root.join("no-such-seed"))
            .env("WT_TARGET_ROOT", self.targets())
            .env("WT_RECLAIM", self.root.join("no-floor-pass"))
            .env("WT_SUITE_CHUNK_SECS", "20")
            .env("WT_SUITE_DEFAULT_SECS", "10")
            .env("WT_SUITE_BUDGET_SECS", "3600")
            .env_remove("FX_FAIL")
            .env_remove("WT_JOBS")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_JOBS");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run wt-cargo suite");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

/// The one-line result each chunk prints, in order.
fn chunk_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.starts_with("wt-cargo suite: chunk "))
        .collect()
}

/// Every target cargo lists is run exactly once, in chunks no larger
/// than the cap (two unmeasured targets at 10 s under a 20 s cap), the
/// doctests in a chunk of their own because cargo refuses to mix
/// `--doc` with other targets, and the call ends on a tally.
#[test]
fn the_suite_runs_every_target_in_bounded_chunks_and_ends_on_a_tally() {
    let s = Suite::new("chunks");
    let (rc, out) = s.run(&[]);
    assert_eq!(rc, 0, "a green suite exits 0: {out}");
    let chunks = chunk_lines(&out);
    assert_eq!(chunks.len(), 3, "three chunks: {out}");
    assert!(
        chunks[0].contains("ok") && chunks[0].ends_with("lib test:alpha"),
        "chunk 1 is the library and alpha: {out}"
    );
    assert!(
        chunks[1].contains("ok") && chunks[1].ends_with("test:beta test:gamma"),
        "chunk 2 is beta and gamma: {out}"
    );
    assert!(
        chunks[2].contains("ok") && chunks[2].ends_with("doc"),
        "the doctests run alone: {out}"
    );
    assert!(
        out.contains("suite result: ok. 5 of 5 test targets passed"),
        "the tally names every target: {out}"
    );
}

/// A target an earlier run MEASURED as heavy gets a chunk to itself,
/// whatever its neighbours weigh — the chunks follow the durations, not
/// the count — and each run records what it measured for the next.
#[test]
fn a_target_measured_heavy_on_an_earlier_run_gets_a_chunk_of_its_own() {
    let s = Suite::new("heavy");
    boss_testing::create_dir(s.times().parent().expect("times dir"));
    boss_testing::write_file(&s.times(), "test:beta\t100\n");
    let (rc, out) = s.run(&[]);
    assert_eq!(rc, 0, "{out}");
    let chunks = chunk_lines(&out);
    let names: Vec<&str> = chunks
        .iter()
        .map(|l| l.rsplit(": ").next().unwrap_or(""))
        .collect();
    assert_eq!(
        names,
        ["lib test:alpha", "test:beta", "test:gamma", "doc"],
        "beta, measured at 100 s against a 20 s cap, runs alone: {out}"
    );
    let times = std::fs::read_to_string(s.times()).expect("the history is written");
    for target in ["lib", "test:alpha", "test:beta", "test:gamma", "doc"] {
        assert!(
            times.lines().any(|l| l.starts_with(&format!("{target}\t"))),
            "{target}'s measured duration is recorded for the next run: {times}"
        );
    }
    assert!(
        !times.contains("test:beta\t100"),
        "beta's new measurement replaces the old one: {times}"
    );
}

/// A call stops at its budget — after at least one chunk, so every call
/// makes progress — and says the suite is NOT green; the next call on
/// the same tree resumes where it stopped, and only the call that runs
/// the last target exits 0 with the tally over all of them.
#[test]
fn a_call_that_reaches_its_budget_stops_and_the_next_call_resumes() {
    let s = Suite::new("resume");
    let budget = [("WT_SUITE_BUDGET_SECS", "1")];

    let (rc, out) = s.run(&budget);
    assert_eq!(rc, 75, "a call that stops early is not a pass: {out}");
    assert_eq!(chunk_lines(&out).len(), 1, "one chunk per call here: {out}");
    assert!(
        out.contains("3 of 5 test targets remain") && out.contains("NOT GREEN"),
        "it says what remains and that the suite is not green: {out}"
    );
    assert!(
        out.contains("wt-cargo suite fx"),
        "it names the command that continues: {out}"
    );

    let (rc, out) = s.run(&budget);
    assert_eq!(rc, 75, "{out}");
    let chunks = chunk_lines(&out);
    assert_eq!(chunks.len(), 1, "{out}");
    assert!(
        chunks[0].ends_with("test:beta test:gamma"),
        "the second call resumes after what passed: {out}"
    );

    let (rc, out) = s.run(&budget);
    assert_eq!(rc, 0, "the call that finishes the suite exits 0: {out}");
    assert!(chunk_lines(&out)[0].ends_with("doc"), "{out}");
    assert!(
        out.contains("suite result: ok. 5 of 5 test targets passed"),
        "the tally counts the targets every call ran: {out}"
    );

    let (rc, out) = s.run(&budget);
    assert_eq!(rc, 0, "{out}");
    assert!(
        chunk_lines(&out).is_empty(),
        "an unchanged tree whose suite passed runs nothing again: {out}"
    );
    assert!(out.contains("suite result: ok. 5 of 5"), "{out}");
}

/// A pass belongs to the tree it ran on. Any change — here an untracked
/// file — and the next call starts the suite over.
#[test]
fn a_changed_tree_starts_the_suite_over() {
    let s = Suite::new("changed");
    let (rc, out) = s.run(&[]);
    assert_eq!(rc, 0, "{out}");
    boss_testing::write_file(&s.wt.join("NOTES"), "a change\n");
    let (rc, out) = s.run(&[]);
    assert_eq!(rc, 0, "{out}");
    // The chunking differs now (the first run measured every target),
    // so read which targets ran, not how they were grouped.
    let mut ran: Vec<&str> = chunk_lines(&out)
        .iter()
        .flat_map(|l| l.rsplit(": ").next().unwrap_or("").split(' '))
        .collect();
    ran.sort_unstable();
    assert_eq!(
        ran,
        ["doc", "lib", "test:alpha", "test:beta", "test:gamma"],
        "every target runs again on the changed tree: {out}"
    );
}

/// A failing binary is named on its chunk's line and in the tally, the
/// chunk's own log is printed rather than lost, the other chunks still
/// run, and the call exits nonzero. A later call on the same tree runs
/// only what has not passed.
#[test]
fn a_failing_target_is_named_its_log_is_shown_and_only_it_runs_again() {
    let s = Suite::new("failing");
    let (rc, out) = s.run(&[("FX_FAIL", "1")]);
    assert_eq!(rc, 101, "a red suite exits 101: {out}");
    let chunks = chunk_lines(&out);
    assert_eq!(
        chunks.len(),
        3,
        "a red chunk does not stop the others: {out}"
    );
    assert!(
        chunks[1].contains("FAILED") && chunks[1].contains("test:beta"),
        "the red chunk says so: {out}"
    );
    assert!(
        out.contains("FX_FAIL is set"),
        "the failing chunk's log is printed: {out}"
    );
    assert!(
        out.contains("suite result: FAILED. 4 of 5 test targets passed; failed: test:beta"),
        "the tally names the failure: {out}"
    );

    let (rc, out) = s.run(&[]);
    assert_eq!(rc, 0, "{out}");
    let chunks = chunk_lines(&out);
    assert_eq!(chunks.len(), 1, "{out}");
    assert!(
        chunks[0].ends_with(": test:beta"),
        "only the target that had not passed runs again: {out}"
    );
    assert!(out.contains("suite result: ok. 5 of 5"), "{out}");
}

/// Rule 4 of the builder rules is where a builder learns to run a whole
/// suite; it must name this verb, and the verb it names must be the one
/// the door answers to — a rule pointing at a verb that does not exist
/// is the drift CLAUDE.md 9a pins.
#[test]
fn the_builder_rules_send_a_whole_suite_to_this_verb() {
    let rules =
        std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
            .expect("the builder rules");
    assert!(
        rules.contains("`wt-cargo suite <crate>`"),
        "rule 4 names `wt-cargo suite <crate>` as the way to run a whole suite"
    );
    let door = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("wt-cargo");
    assert!(
        door.contains("\"${1:-}\" = suite") && door.contains("wt-cargo-suite.sh"),
        "wt-cargo answers to `suite` through wt-cargo-suite.sh"
    );
}

/// `suite` takes exactly one crate; anything else is a usage error that
/// runs nothing.
#[test]
fn the_suite_verb_takes_exactly_one_crate() {
    let s = Suite::new("usage");
    for args in [&["suite"][..], &["suite", "fx", "--lib"][..]] {
        let out = Command::new(repo_root().join(SCRIPT))
            .args(args)
            .current_dir(&s.wt)
            .env("WT_SEED", s.root.join("no-such-seed"))
            .env("WT_TARGET_ROOT", s.targets())
            .env("WT_RECLAIM", s.root.join("no-floor-pass"))
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .expect("run wt-cargo");
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {text}");
        assert!(text.contains("usage: wt-cargo suite <crate>"), "{text}");
    }
}
