//! `infra/dev/wt-target-dir.sh` — the one name for a worktree's own
//! cargo target dir, and the one decision about when a build must
//! leave a shared one.
//!
//! WHY (backlog 955c99b6, 2026-09-22). `infra/gate.sh` exports
//! `CARGO_INCREMENTAL=0` and never set `CARGO_TARGET_DIR`, so its
//! cargo inherited the pod-wide `/scratch/target`
//! (infra/cluster/manifests/boss-dev.yaml) — while every builder's
//! real build goes through `infra/dev/wt-cargo`, which gives each
//! worktree its own. Six builders ran concurrently on 2026-09-22, so
//! six source trees checked into ONE target directory, and
//! `infra/gate.sh --lint` — the door CLAUDE.md names before pushing —
//! false-redded a clean tree three times for one builder.
//!
//! The mechanism, reproduced deterministically in a two-package
//! fixture before this car was written: cargo's artifact identity does
//! NOT include the workspace path (two packages of the same name and
//! version at different paths were given the SAME `-C metadata` hash
//! and one rmeta), and its freshness check resolves the dep-info's
//! cwd-RELATIVE source paths against the current directory. So a
//! neighbour worktree's rlib, built minutes ago, reads as fresh here,
//! and this tree compiles against the neighbour's API: the reported
//! symptom was `no variant named List found for enum boss_expr::Expr`
//! against source that compiles cleanly under wt-cargo, cleared by
//! `touch`-ing the source file.
//!
//! What the helper must and must not do, each pinned below. It must
//! isolate a LINKED worktree — the only checkout that shares a target
//! dir with a checkout it does not own — and it must leave a MAIN
//! worktree alone, because that is the cluster gate runner (which
//! `git clone`s into a per-run emptyDir and exports its own
//! `/gate-target/target`) and forge CI. Redirecting there would trade
//! a false red for a cold ~74 GB build on every gate.

use boss_testing::{repo_root, scratch_dir};
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER: &str = "infra/dev/wt-target-dir.sh";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    root: PathBuf,
    /// The main checkout — what the gate runner's clone and CI look like.
    main: PathBuf,
    /// The dir the pod's manifest would hand every build.
    shared: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-target-dir-{name}"));
        let main = root.join("boss");
        boss_testing::create_dir(&main);
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@example.invalid"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join("f"), "x").expect("seed file");
        git(&main, &["add", "f"]);
        git(&main, &["commit", "-qm", "seed"]);
        let shared = root.join("scratch").join("target");
        boss_testing::create_dir(&shared);
        Self { root, main, shared }
    }

    /// A linked worktree, exactly as a builder gets one.
    fn worktree(&self, name: &str) -> PathBuf {
        let wt = self.root.join("trees").join(name);
        boss_testing::create_dir(&self.root.join("trees"));
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                wt.to_str().expect("utf8"),
            ],
        );
        wt
    }

    /// Source the helper in `cwd`, call it, and report what
    /// `CARGO_TARGET_DIR` became. `target` is the ambient value, or
    /// `None` for unset — the case where there is nothing to leave.
    fn isolate(&self, cwd: &Path, target: Option<&Path>) -> (i32, String, String) {
        let script = format!(
            "set -u\n. '{}' || exit 2\nwt_isolate_target_dir\necho \"target=${{CARGO_TARGET_DIR:-unset}}\"\n",
            repo_root().join(HELPER).display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(script).current_dir(cwd);
        match target {
            Some(t) => cmd.env("CARGO_TARGET_DIR", t),
            None => cmd.env_remove("CARGO_TARGET_DIR"),
        };
        let out = cmd.output().expect("run the helper");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Source the helper in `cwd`, call `wt_bound_build_jobs`, and
    /// report what `CARGO_BUILD_JOBS` became. `jobs` is the ambient
    /// value, or `None` for unset — the pod's own case, since nothing
    /// in its manifest sets one.
    fn bound(&self, cwd: &Path, jobs: Option<&str>) -> (i32, String, String) {
        self.bound_with("bash", &repo_root().join(HELPER), cwd, jobs)
    }

    /// [`Fixture::bound`] under `shell`, sourcing the helper at `helper`
    /// — a copy beside a pod-build.env of the test's choosing, or none.
    fn bound_with(
        &self,
        shell: &str,
        helper: &Path,
        cwd: &Path,
        jobs: Option<&str>,
    ) -> (i32, String, String) {
        let script = format!(
            "set -u\n. '{}' || exit 2\nwt_bound_build_jobs || exit $?\necho \"jobs=${{CARGO_BUILD_JOBS:-unset}}\"\n",
            helper.display()
        );
        let mut cmd = Command::new(shell);
        cmd.arg("-c")
            .arg(script)
            .current_dir(cwd)
            .env_remove("WT_JOBS");
        match jobs {
            Some(j) => cmd.env("CARGO_BUILD_JOBS", j),
            None => cmd.env_remove("CARGO_BUILD_JOBS"),
        };
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run the helper under {shell}: {e}"));
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The helper copied into a dir of its own, beside a pod-build.env
    /// holding `env` — or beside none at all, for `None` — so the
    /// bound's two refusals can be driven without touching the tree's
    /// own file. The helper finds the file beside ITSELF, which is why
    /// the copy, not the env file alone, is what moves.
    fn helper_beside(&self, env: Option<&str>) -> PathBuf {
        let dir = self.root.join("helper");
        boss_testing::create_dir(&dir);
        let helper = dir.join("wt-target-dir.sh");
        std::fs::copy(repo_root().join(HELPER), &helper).expect("copy the helper");
        if let Some(body) = env {
            boss_testing::write_file(&dir.join("pod-build.env"), body);
        }
        helper
    }
}

/// The bound pod-build.env declares, read from the file rather than
/// retyped (the same reading wt_cargo_sh.rs makes).
fn pod_bound() -> String {
    std::fs::read_to_string(repo_root().join("infra/dev/pod-build.env"))
        .expect("infra/dev/pod-build.env")
        .lines()
        .find_map(|l| l.strip_prefix("CARGO_BUILD_JOBS="))
        .expect("pod-build.env sets CARGO_BUILD_JOBS")
        .trim()
        .to_string()
}

/// THE DEFECT OF BACKLOG 9ecd12ea, ASK 3. `infra/gate.sh --lint` runs
/// `cargo clippy --workspace` in a builder's worktree, and nothing bounded
/// its width: cargo takes one job per CPU, and on the dev pod `nproc`
/// reads the NODE (32), not the container's cgroup (16 CPU / 32Gi).
/// Measured 2026-10-10 from the memory watch's ring: one such check held
/// ten clippy-drivers at once (3.6 GB summed, the two largest 1.10 and
/// 1.05 GB), more than the four any wt-cargo builder build is allowed.
/// A linked builder worktree must check at the width wt-cargo builds at.
#[test]
fn a_builder_worktrees_check_runs_at_the_builder_width_and_says_so() {
    let f = Fixture::new("jobs-builder");
    let wt = f.worktree("agent-0123abcd");

    let (rc, stdout, stderr) = f.bound(&wt, None);
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains("jobs=4"),
        "a builder worktree's check runs 4 wide, as wt-cargo builds it: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("9ecd12ea") && stderr.contains('4'),
        "the bound is said, with the packet that measured it: {stderr:?}"
    );
}

/// The operator's own linked worktree takes the pod's bound, the one
/// wt-cargo gives it — read from pod-build.env, never retyped.
#[test]
fn an_operator_linked_worktrees_check_runs_at_the_pod_bound() {
    let f = Fixture::new("jobs-operator");
    let wt = f.worktree("feature-x");

    let (rc, stdout, stderr) = f.bound(&wt, None);
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("jobs={}", pod_bound())),
        "{stdout}{stderr}"
    );
}

/// THE CASE THAT MUST NOT NARROW. The cluster gate runner and forge CI
/// are main checkouts on machines sized for one build; bounding them to
/// the dev pod's share would slow the judge to suit a shared box (the
/// argument pod-build.env makes against `.cargo/config.toml`).
#[test]
fn a_main_checkouts_check_keeps_cargos_own_width() {
    let f = Fixture::new("jobs-main");

    let (rc, stdout, stderr) = f.bound(&f.main, None);
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(stdout.contains("jobs=unset"), "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing was changed, so nothing is said");
}

/// A width the caller already chose — wt-cargo's own, or a human's —
/// is kept: the helper bounds an UNBOUNDED cargo, nothing else.
#[test]
fn a_width_already_chosen_is_kept() {
    let f = Fixture::new("jobs-explicit");
    let wt = f.worktree("agent-explicit");

    let (rc, stdout, stderr) = f.bound(&wt, Some("2"));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(stdout.contains("jobs=2"), "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing was changed, so nothing is said");
}

/// A MISSING pod-build.env is called missing (backlog 9f7ee428, from
/// review 9612e118 of car d7b90451, ask 1). The refusal said the file
/// "sets no CARGO_BUILD_JOBS" whether it was absent or present and
/// empty, while wt-cargo, which reads the same file, has always told
/// the two apart — and the two have different repairs: a missing file
/// is a checkout or a fixture that did not carry it, an empty one is an
/// edit to the file itself.
#[test]
fn a_missing_pod_build_env_is_called_missing() {
    let f = Fixture::new("jobs-missing-env");
    let wt = f.worktree("agent-missing");
    let helper = f.helper_beside(None);

    let (rc, stdout, stderr) = f.bound_with("bash", &helper, &wt, None);
    assert_eq!(
        rc, 2,
        "a linked worktree with no bound refuses: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("is missing"),
        "the refusal names the file as missing: {stderr:?}"
    );
    assert!(
        !stderr.contains("sets no CARGO_BUILD_JOBS"),
        "a file that is not there sets nothing, and saying so sends the reader to edit it: {stderr:?}"
    );
    assert!(!stdout.contains("jobs="), "{stdout}");
}

/// The other refusal keeps its own words: the file is there and
/// declares no bound.
#[test]
fn a_pod_build_env_that_declares_no_bound_says_so() {
    let f = Fixture::new("jobs-empty-env");
    let wt = f.worktree("agent-empty");
    let helper = f.helper_beside(Some("# a bound used to live here\n"));

    let (rc, stdout, stderr) = f.bound_with("bash", &helper, &wt, None);
    assert_eq!(rc, 2, "{stdout}{stderr}");
    assert!(stderr.contains("sets no CARGO_BUILD_JOBS"), "{stderr:?}");
    assert!(!stderr.contains("is missing"), "{stderr:?}");
}

/// UNDER dash THE HELPER SAYS IT NEEDS bash (ask 4). It finds
/// pod-build.env through `BASH_SOURCE`, which dash does not have: dash
/// printed `Bad substitution`, read the file as `/pod-build.env`, and
/// refused with the missing-bound text — a refusal naming a file that
/// was never the one in question. Both of its callers are bash, so the
/// helper refuses by name rather than guessing a path; a main checkout
/// is still left alone, as it is under bash.
#[test]
fn under_dash_the_bound_says_it_needs_bash() {
    let f = Fixture::new("jobs-dash");
    let wt = f.worktree("agent-dash");

    let (rc, stdout, stderr) = f.bound_with("dash", &repo_root().join(HELPER), &wt, None);
    assert_eq!(rc, 2, "{stdout}{stderr}");
    assert!(
        stderr.contains("needs bash"),
        "the refusal names the shell it needs: {stderr:?}"
    );
    assert!(
        !stderr.contains("Bad substitution") && !stderr.contains("CARGO_BUILD_JOBS —"),
        "dash must not reach the BASH_SOURCE read, nor refuse in the missing-bound words: {stderr:?}"
    );

    let (rc, stdout, stderr) = f.bound_with("dash", &repo_root().join(HELPER), &f.main, None);
    assert_eq!(
        rc, 0,
        "a main checkout is inert under any shell: {stdout}{stderr}"
    );
    assert!(stdout.contains("jobs=unset"), "{stdout}{stderr}");
}

/// A bare tree holding this tree's gate.sh (and everything
/// `copy_gate_sh` says it needs), committed, with a LINKED builder
/// worktree of it — the one shape in which the gate bounds cargo. The
/// fake `df` reports plenty at startup and trips at the next poll, the
/// one before the first check, so no mode compiles anything (the trick
/// a_quick_preflight_refuses_an_untracked_file.rs uses).
struct LinkedGate {
    dir: PathBuf,
    wt: PathBuf,
}

impl LinkedGate {
    fn new(label: &str) -> Self {
        let dir = scratch_dir(&format!("wt-bound-gate-{label}"));
        let main = dir.join("boss");
        boss_testing::create_dir(&main.join("infra/lint/lib"));
        boss_testing::copy_gate_sh(&main);
        boss_testing::write_file(
            &main.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@example.invalid"]);
        git(&main, &["config", "user.name", "t"]);
        git(&main, &["add", "."]);
        git(&main, &["commit", "-qm", "the gate under test"]);
        let wt = dir.join("trees").join("agent-gate");
        boss_testing::create_dir(&dir.join("trees"));
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent-gate",
                wt.to_str().expect("utf8"),
            ],
        );
        let counter = dir.join("df-calls");
        boss_testing::write_exec(
            &dir.join("df"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=$(cat {c} 2>/dev/null || echo 0)\n\
                 echo $((n+1)) > {c}\n\
                 echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
                 for _ in \"$@\"; do if [ \"$n\" -lt 1 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
                 else echo '/dev/fake 1 1 1048576 99% /'; fi; done\n",
                c = counter.display()
            ),
        );
        Self { dir, wt }
    }

    fn run(&self, mode: &str) -> (i32, String, String) {
        let _ = std::fs::remove_file(self.dir.join("df-calls"));
        let out = Command::new("bash")
            .arg(self.wt.join("infra/gate.sh"))
            .arg(mode)
            .current_dir(&self.wt)
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            // The pod's own case: nothing has chosen a width. Under
            // wt-cargo or a gate runner these are set, and a set width
            // is kept, which would hide everything asserted here.
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("WT_JOBS")
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {mode} in {}: {e}", self.wt.display()));
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for LinkedGate {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The bound's own announcement, as the helper prints it.
const BOUND_LINE: &str = "jobs wide, the width wt-cargo builds it at";

/// `copy_gate_sh` CARRIES THE FILE THE BOUND READS (ask 3). The helper
/// sources pod-build.env through a variable, which the source-line walk
/// cannot resolve, so a copied gate in a linked worktree had no bound to
/// read and refused before its first check — a refusal about the
/// fixture that a test would read as a verdict about the gate.
#[test]
fn a_copied_gate_carries_the_bound_its_helper_reads() {
    let dir = scratch_dir("wt-bound-copy-gate");
    boss_testing::copy_gate_sh(&dir);
    let carried = std::fs::read_to_string(dir.join("infra/dev/pod-build.env"))
        .unwrap_or_else(|e| panic!("copy_gate_sh did not carry infra/dev/pod-build.env: {e}"));
    let ours = std::fs::read_to_string(repo_root().join("infra/dev/pod-build.env"))
        .expect("infra/dev/pod-build.env");
    assert_eq!(
        carried, ours,
        "the fixture's bound is this tree's, byte for byte"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LISTINGS ANSWER BEFORE THE BOUND (ask 2). `--roster`,
/// `--exclusions` and `--serial-lane` read no tree and run no cargo,
/// and gate.sh's rule since backlog 13700f6f is that they answer before
/// EVERY refusal. The bound ran at the top of the script, above that
/// dispatch: in a linked worktree a listing printed a cargo width it
/// would never use, and with the bound missing it refused the question.
#[test]
fn a_linked_worktrees_listings_answer_without_a_bound_and_state_none() {
    let g = LinkedGate::new("listings");
    std::fs::remove_file(g.wt.join("infra/dev/pod-build.env")).ok();
    for mode in ["--roster", "--exclusions", "--serial-lane"] {
        let (rc, stdout, stderr) = g.run(mode);
        assert_eq!(
            rc, 0,
            "`gate.sh {mode}` is a listing and answers whatever the bound's state.\n{stdout}{stderr}"
        );
        assert!(
            !stderr.contains(BOUND_LINE) && !stderr.contains("CARGO_BUILD_JOBS"),
            "a listing runs no cargo, so it states no cargo width and refuses on none: {stderr:?}"
        );
    }
}

/// THE BOUND SPEAKS WHERE CARGO COMPILES, AND ONLY THERE. `--quick`
/// compiles nothing — it says so in its own closing line — so it states
/// no width; `--lint` runs clippy and the tree-wide pins, so it is
/// bounded before them, as before.
#[test]
fn the_bound_is_stated_by_lint_and_not_by_quick() {
    let g = LinkedGate::new("modes");

    let (_, stdout, stderr) = g.run("--quick");
    assert!(
        stderr.contains("Refusing to continue before"),
        "--quick must reach its first check for this to say anything:\n{stdout}{stderr}"
    );
    assert!(
        !stderr.contains(BOUND_LINE),
        "--quick runs no compiling cargo, so it states no cargo width: {stderr:?}"
    );

    let (_, stdout, stderr) = g.run("--lint");
    assert!(
        stderr.contains(BOUND_LINE) && stderr.contains("runs 4 jobs wide"),
        "--lint's cargo is still bounded in a linked builder worktree:\n{stdout}{stderr}"
    );
    let bound_at = stderr.find(BOUND_LINE).expect("found above");
    let trip_at = stderr
        .find("Refusing to continue before")
        .unwrap_or_else(|| panic!("--lint reached its first check:\n{stdout}{stderr}"));
    assert!(
        bound_at < trip_at,
        "the bound is set before --lint's first check, not after it: {stderr:?}"
    );
}

/// §9a — the builder width lives ONCE, in the helper: wt-cargo builds
/// at it and gate.sh checks at it, and neither spells the number.
#[test]
fn wt_cargo_and_the_gate_both_take_the_width_from_here() {
    let helper = std::fs::read_to_string(repo_root().join(HELPER)).expect(HELPER);
    assert!(
        helper.contains("wt_build_jobs()") && helper.contains("wt_bound_build_jobs()"),
        "{HELPER} defines both the width and the decision"
    );
    for (path, call) in [
        ("infra/dev/wt-cargo", "wt_build_jobs"),
        ("infra/gate.sh", "wt_bound_build_jobs"),
    ] {
        let text = std::fs::read_to_string(repo_root().join(path))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(
            text.contains(call),
            "{path} must call {call}() rather than choose a width itself"
        );
        assert!(
            !text.contains("WT_JOBS:-4"),
            "{path} must not retype the builder width — that is the drift §9a names"
        );
    }
}

/// THE DEFECT. A builder's worktree must not check into the dir the
/// main checkout owns — it must check into the SAME per-worktree dir
/// `wt-cargo` already seeded for it, so `--lint` predicts the build
/// the builder ran rather than a neighbour's.
#[test]
fn a_linked_worktree_leaves_the_shared_target_dir() {
    let f = Fixture::new("linked");
    let wt = f.worktree("agent-0123abcd");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&f.shared));
    assert_eq!(rc, 0, "{stdout}{stderr}");

    let mine = f.root.join("scratch").join("target-agent-0123abcd");
    assert!(
        stdout.contains(&format!("target={}", mine.display())),
        "a linked worktree must build into its own dir: {stdout}{stderr}"
    );
    // The line is the whole point: a builder who sees `--lint` use a
    // different dir than the run before must be able to read why.
    assert!(
        stderr.contains(mine.to_str().expect("utf8"))
            && stderr.contains(f.shared.to_str().expect("utf8")),
        "the redirect must name both dirs: {stderr:?}"
    );
    assert!(
        stderr.contains("955c99b6"),
        "the message must carry the packet that measured this: {stderr:?}"
    );
}

/// THE CASE THAT MUST NOT MOVE. The cluster gate runner clones into a
/// per-run emptyDir and exports `/gate-target/target`; forge CI clones
/// too. Both are MAIN worktrees, both own their target dir, and both
/// would pay a cold build for a redirect they do not need.
#[test]
fn a_main_checkout_keeps_the_target_dir_it_was_given() {
    let f = Fixture::new("main");
    let given = f.root.join("gate-target").join("target");

    let (rc, stdout, stderr) = f.isolate(&f.main, Some(&given));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", given.display())),
        "a main checkout owns its target dir: {stdout}{stderr}"
    );
    assert_eq!(
        stderr, "",
        "nothing was changed, so nothing is said: {stderr:?}"
    );
}

/// Unset is not shared: cargo then uses `<workspace>/target`, which is
/// per-worktree by construction. Nothing to leave, nothing to say —
/// and the helper must not invent a `/scratch` that this machine may
/// not have.
#[test]
fn an_unset_target_dir_is_left_unset() {
    let f = Fixture::new("unset");
    let wt = f.worktree("agent-unset");

    let (rc, stdout, stderr) = f.isolate(&wt, None);
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(stdout.contains("target=unset"), "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing to say: {stderr:?}");
}

/// A target dir INSIDE the worktree is already this tree's own — the
/// operator who exported one by hand keeps it.
#[test]
fn a_target_dir_inside_the_worktree_is_already_its_own() {
    let f = Fixture::new("inside");
    let wt = f.worktree("agent-inside");
    let inside = wt.join("target");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&inside));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", inside.display())),
        "{stdout}{stderr}"
    );
    assert_eq!(stderr, "", "nothing to say: {stderr:?}");
}

/// Idempotence, the correctness protocol's own property: a second call
/// on the result of the first must be a no-op, so a gate re-entered
/// through a wrapper cannot walk `target-agent-x` into
/// `target-target-agent-x`.
#[test]
fn isolating_an_already_isolated_dir_changes_nothing() {
    let f = Fixture::new("idempotent");
    let wt = f.worktree("agent-twice");
    let mine = f.root.join("scratch").join("target-agent-twice");

    let (rc, stdout, stderr) = f.isolate(&wt, Some(&mine));
    assert_eq!(rc, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("target={}", mine.display())),
        "{stdout}{stderr}"
    );
    assert_eq!(stderr, "", "a no-op says nothing: {stderr:?}");
}

/// §9a — the name lives ONCE. `wt-cargo` seeds the dir and `gate.sh`
/// checks into it; if either spelled `target-<basename>` itself the
/// two would drift, and the drift's symptom is the silent one this
/// car exists to close: a `--lint` that reads a dir no build wrote.
#[test]
fn wt_cargo_and_the_gate_both_read_the_name_from_here() {
    let helper = std::fs::read_to_string(repo_root().join(HELPER)).expect(HELPER);
    assert!(
        helper.contains("wt_target_dir()") && helper.contains("wt_isolate_target_dir()"),
        "{HELPER} defines both the name and the decision"
    );

    for (path, call) in [
        ("infra/dev/wt-cargo", "wt_target_dir"),
        ("infra/gate.sh", "wt_isolate_target_dir"),
    ] {
        let text = std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| {
            panic!("{path}: {e}");
        });
        assert!(
            text.contains("wt-target-dir.sh"),
            "{path} must source {HELPER}, not carry its own copy of the rule"
        );
        assert!(
            text.contains(call),
            "{path} must call {call}() rather than spell the dir itself"
        );
        assert!(
            !text.contains("/target-$"),
            "{path} must not retype the per-worktree name — that is the drift §9a names"
        );
    }
}
