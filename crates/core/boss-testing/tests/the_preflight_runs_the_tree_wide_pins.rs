//! `infra/gate.sh --lint` runs the declared tree-wide pins (backlog
//! 2f0c722a, 2026-10-16).
//!
//! WHAT WAS MEASURED. Protocol-retro adc3812b read every failed W41 car
//! gate one by one: 19 of 61 (31%) failed on a STANDING source-scan pin
//! in boss-testing or boss-policy-client that the builder's change broke
//! — `an_executable_is_written_by_write_exec` 5, the machine-token
//! sender pins 6, `every_door_is_a_declared_control` 4,
//! `a_jq_guard_asks_whether_there_is_a_document` 4, and others. Each
//! answers in seconds and needs no database, and each cost a full car
//! gate (median 27.7-57.3 min by concurrency) plus a re-gate, because
//! `--lint` ran fmt, the build-free lints and clippy and nothing else.
//!
//! THE SET IS THE ONE THE GATE ALREADY OWES. Those pins are tree-wide:
//! each declares itself with a `//! tree-wide pin` header (or a
//! `// tree-wide pin` line above one test under `src/`), and
//! `tree_wide_pins` in infra/gate.sh derives every scoped gate's set
//! from those declarations. `--lint` runs the same set through the same
//! `run_tree_wide_pins`, so the pre-flight cannot disagree with the
//! check it predicts and no list of pins lives in gate.sh (CLAUDE.md
//! §9a). A synthetic tree drives the real script here with a `cargo`
//! that records what it was asked.
//!
//! DATABASE-FREE, AND HELD SO. `--lint` runs where there is no test
//! database — on the dev pod 127.0.0.1 may be a port-forward to
//! production — so a tree-wide `tests/` file that opens a test database would
//! turn the pre-flight into a database client. The last test here
//! refuses one, naming the file.
//!
//! tree-wide pin — the last test reads every crate's tests/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The one lint the roster refuses to run without.
const LINT: &str = "infra/lint/workspace-declares-what-it-runs.sh";

/// The declared pin the synthetic tree carries, and one undeclared test
/// beside it that `--lint` must leave alone.
const PIN: &str = "crates/core/boss-testing/tests/a_declared_pin.rs";
const PLAIN: &str = "crates/core/boss-testing/tests/a_plain_test.rs";

/// A synthetic repository with this tree's gate.sh, one lint, one
/// declared tree-wide pin, and a `cargo` that appends each invocation to
/// `cargo.log` — exiting 1 for `cargo test` when `fail_tests` is set.
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str, fail_tests: bool) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("preflight-tree-wide-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(&tree.join(LINT), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::create_dir(&tree.join("crates/core/boss-testing/tests"));
        boss_testing::write_file(
            &tree.join(PIN),
            "//! tree-wide pin — it scans a tree\n#[test]\nfn t() {}\n",
        );
        boss_testing::write_file(&tree.join(PLAIN), "#[test]\nfn t() {}\n");
        boss_testing::write_file(&tree.join(".gitignore"), "scratch/\n");
        run_git(&tree, &["init", "-q", "-b", "main"]);
        run_git(&tree, &["add", "."]);
        run_git(
            &tree,
            &[
                "-c",
                "user.email=preflight-pins@test",
                "-c",
                "user.name=preflight-pins",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        let fail = if fail_tests {
            "[ \"$1\" = test ] && exit 1\n"
        } else {
            ""
        };
        boss_testing::write_exec(
            &bin.join("cargo"),
            &format!(
                "#!/usr/bin/env bash\necho \"$*\" >> '{}'\n{fail}exit 0\n",
                dir.join("cargo.log").display()
            ),
        );
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        Tree { dir, tree }
    }

    fn run(&self, mode: &str) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg(mode)
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "main")
            .env("BOSS_TRUNK_REF", "main")
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {mode} in {}: {e}", self.tree.display()))
    }

    fn cargo_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("cargo.log")).unwrap_or_default()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
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

/// THE DEFECT. `--lint` runs the declared pin, by the gate's own
/// invocation, and only the declared one.
#[test]
fn lint_runs_every_declared_tree_wide_pin() {
    let tree = Tree::new("runs", false);
    let out = tree.run("--lint");
    let log = tree.cargo_log();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "a passing pin is clean:\n{t}");
    assert!(
        log.lines().any(|l| l.starts_with("test -p boss-testing")
            && l.contains("--test a_declared_pin")
            && l.contains("--all-features")),
        "--lint must run the declared tree-wide pin the way the gate does \
         (cargo test -p <crate> --test <file> --all-features); cargo was asked:\n{log}\n{t}"
    );
    assert!(
        !log.contains("a_plain_test"),
        "--lint runs the DECLARED set, not every test in the crate; cargo was asked:\n{log}"
    );
}

/// THE CONTROL, AND THE VERDICT. A failing pin fails `--lint`, named.
#[test]
fn a_failing_tree_wide_pin_fails_the_preflight() {
    let tree = Tree::new("fails", true);
    let out = tree.run("--lint");
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a red pin must red the pre-flight, or a gate learns it instead:\n{t}"
    );
    assert!(
        t.contains("tree-wide pins"),
        "the failed check must be named:\n{t}"
    );
}

/// `--quick` stays build-free: it compiles nothing, so it runs no pin.
#[test]
fn quick_runs_no_pin() {
    let tree = Tree::new("quick", false);
    let out = tree.run("--quick");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        !tree.cargo_log().contains("a_declared_pin"),
        "--quick compiles nothing; cargo was asked:\n{}",
        tree.cargo_log()
    );
}

/// How a test opens its database, spelled in two halves so this file's
/// own source does not match it.
const OPENER: &str = concat!("Test", "Db::");

/// No declared `tests/` pin opens a test database, so `--lint` stays a
/// source scan wherever it runs.
#[test]
fn no_tree_wide_pin_file_opens_a_test_database() {
    let crates = repo_root().join("crates");
    let mut offenders = Vec::new();
    let mut declared_files = 0;
    for tier in read_dirs(&crates) {
        for krate in read_dirs(&tier) {
            let tests = krate.join("tests");
            if !tests.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&tests)
                .unwrap_or_else(|e| panic!("read {}: {e}", tests.display()))
                .map(|e| e.expect("dir entry").path())
                .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            {
                let body = std::fs::read_to_string(&f)
                    .unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
                let declared = body
                    .lines()
                    .any(|l| l == "//! tree-wide pin" || l.starts_with("//! tree-wide pin — "));
                let opens = body
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| l.contains(OPENER));
                declared_files += usize::from(declared);
                if declared && opens {
                    offenders.push(
                        f.strip_prefix(repo_root())
                            .unwrap_or(&f)
                            .display()
                            .to_string(),
                    );
                }
            }
        }
    }
    assert!(
        declared_files > 0,
        "read no declared tree-wide pin under {} — a scan that scanned nothing is not a pass",
        crates.display()
    );
    assert!(
        offenders.is_empty(),
        "a declared tree-wide pin opens a test database, and `infra/gate.sh --lint` runs every \
         declared pin where there is no test database (backlog 2f0c722a). Move the \
         database half to a file of its own: {offenders:?}"
    );
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .collect()
}
