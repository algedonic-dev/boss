//! `infra/gate.sh --lints <paths>` — the dock's LINT DELTA (backlog
//! ce60b5c6, design 7c4024d8 Q2) — RUN against synthetic trees, so the
//! property is one the gate has, not one its comments claim.
//!
//! THE MEASUREMENT. On 2026-11-19 twelve dock re-gates took about a
//! quarter of the day's gate capacity, a median 41 minutes each, and every one
//! was green. Four of them answered #1850, whose only gate input was one
//! lint: `infra/lint/the-controls-are-bounded-verbs.sh`. The dock already
//! classed that change lint-only; what it lacked was a runner that runs
//! the changed lints and nothing else. This is that runner's gate half.
//!
//! WHAT IT MUST DO, case by case below:
//!   - run MAIN's copy of each named lint, never the car's older one, over
//!     the car's head MERGED WITH MAIN (main first, so a lint's
//!     `merge-base(origin/main, HEAD)` reads the car's own diff);
//!   - run nothing else: no fmt, no other lint, no build;
//!   - write a receipt of mode `lints` whose head is the CAR's head (the
//!     head the dock's `lint_delta` vouches for), naming the main it
//!     merged and each lint's sha256 — the dock copies those;
//!   - fail closed: a merge that conflicts is a red, a lint the shape
//!     refuses or main does not hold is a refusal, and `--lints` beside
//!     another mode is refused before anything runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// #1850's lint, by its real path.
const LINT: &str = "infra/lint/the-controls-are-bounded-verbs.sh";
const CHECK: &str = "the-controls-are-bounded-verbs";
/// A lint that fails whenever it runs: the delta must never run it.
const OTHER: &str = "infra/lint/an-unnamed-lint-that-always-fails.sh";

/// The car's base (main before #1850) carries the lint at its OLD body,
/// which passes everything. Main then changes it to refuse the word
/// FORBIDDEN under docs/ — the car's own copy would pass a car carrying it.
const OLD_LINT: &str = "#!/usr/bin/env bash\nexit 0\n";
const NEW_LINT: &str = "#!/usr/bin/env bash\n\
    # main's new rule: no FORBIDDEN under docs/, and main's own file present\n\
    test -f docs/main.md || { echo 'main was not merged' >&2; exit 1; }\n\
    if grep -rl FORBIDDEN docs/ 2>/dev/null; then echo 'FORBIDDEN found' >&2; exit 1; fi\n\
    exit 0\n";

struct Tree {
    dir: PathBuf,
    tree: PathBuf,
    car_head: String,
    main: String,
}

impl Tree {
    /// A repository whose `main` is the base plus #1850's shape (the lint
    /// changed, and one more file), and whose checked-out branch is a car
    /// cut from the base carrying `car_doc` at docs/car.md. `main_doc`, when
    /// given, is main's own docs/car.md — the conflict case.
    fn new(tag: &str, car_doc: &str, main_doc: Option<&str>) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("lint-delta-{tag}"));
        let tree = dir.join("tree");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        boss_testing::write_exec(&tree.join(LINT), OLD_LINT);
        boss_testing::write_exec(
            &tree.join(OTHER),
            "#!/usr/bin/env bash\necho 'the delta ran a lint it was not given' >&2\nexit 1\n",
        );
        git(&tree, &["init", "-q", "-b", "main"]);
        commit(&tree, "the base");
        git(&tree, &["checkout", "-q", "-b", "feat/car"]);
        boss_testing::create_dir(&tree.join("docs"));
        boss_testing::write_file(&tree.join("docs/car.md"), car_doc);
        commit(&tree, "the car");
        let car_head = rev(&tree, "HEAD");
        git(&tree, &["checkout", "-q", "main"]);
        boss_testing::write_exec(&tree.join(LINT), NEW_LINT);
        boss_testing::create_dir(&tree.join("docs"));
        boss_testing::write_file(&tree.join("docs/main.md"), "landed on main\n");
        if let Some(doc) = main_doc {
            boss_testing::write_file(&tree.join("docs/car.md"), doc);
        }
        commit(&tree, "train #1850: the lint changes");
        let main = rev(&tree, "HEAD");
        // The runner fetches main as origin/main beside the gated sha.
        git(&tree, &["update-ref", "refs/remotes/origin/main", &main]);
        git(&tree, &["checkout", "-q", "feat/car"]);
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(
                &bin.join(tool),
                "#!/usr/bin/env bash\necho \"a lint delta ran $0\" >&2\nexit 1\n",
            );
        }
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             for _ in \"$@\"; do echo '/dev/fake 1 1 943718400 1% /'; done\n",
        );
        Tree {
            dir,
            tree,
            car_head,
            main,
        }
    }

    fn receipt_path(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .args(args)
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.receipt_path())
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {args:?} in {}: {e}", self.tree.display()))
    }

    fn receipt(&self, out: &Output) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.receipt_path())
            .unwrap_or_else(|e| panic!("a receipt is written: {e}\n{}", text(out)));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("the receipt is JSON: {e}\n{raw}"))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=lint-delta@test",
            "-c",
            "user.name=lint-delta",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn rev(dir: &Path, r: &str) -> String {
    git(dir, &["rev-parse", r])
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn check_names(receipt: &serde_json::Value) -> Vec<String> {
    receipt["checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// THE SCOPED CASE, green. A car whose docs are clean, under main's new
/// lint: exit 0, a `lints` receipt for the CAR's head that names the main
/// it merged and the lint's hash, and exactly one check — the named lint.
/// The failing lint beside it, fmt and every compiled phase never ran.
#[test]
fn a_clean_car_is_green_on_mains_lint_and_nothing_else_runs() {
    let tree = Tree::new("green", "a clean doc\n", None);
    let out = tree.run(&["--lints", LINT]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "{t}");
    let r = tree.receipt(&out);
    assert_eq!(r["verdict"], "green", "{r}");
    assert_eq!(r["mode"], "lints", "{r}");
    assert_eq!(
        r["head"], tree.car_head,
        "the receipt vouches for the car's head, not the merge: {r}"
    );
    assert_eq!(
        r["dirty"], false,
        "the merge leaves the car's tree as it was: {r}"
    );
    assert_eq!(r["lints"]["main"], tree.main, "{r}");
    assert_eq!(check_names(&r), vec![CHECK.to_string()], "{r}");
    let sha = r["lints"]["ran"][0]["sha256"].as_str().unwrap_or_default();
    let want = {
        let copy = tree.dir.join("main-lint.sh");
        boss_testing::write_file(&copy, NEW_LINT);
        let out = Command::new("sha256sum")
            .arg(&copy)
            .output()
            .expect("sha256sum runs");
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(sha, want, "the hash is main's copy of the lint: {r}");
    assert_eq!(r["lints"]["ran"][0]["path"], LINT, "{r}");
    assert!(!t.contains("a lint delta ran"), "nothing compiled: {t}");
}

/// THE SCOPED CASE, red — and the proof it ran MAIN's lint over the
/// MERGED tree: the car's own copy of the lint passes everything, so only
/// main's copy can find the car's FORBIDDEN, and that copy also requires
/// main's own file.
#[test]
fn a_car_that_breaks_mains_lint_is_red_naming_it() {
    let tree = Tree::new("red", "FORBIDDEN\n", None);
    let out = tree.run(&["--lints", LINT]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    let r = tree.receipt(&out);
    assert_eq!(r["verdict"], "failed", "{r}");
    assert_eq!(r["mode"], "lints", "{r}");
    assert_eq!(r["head"], tree.car_head, "{r}");
    assert_eq!(r["checks"][0]["name"], CHECK, "{r}");
    assert_eq!(r["checks"][0]["result"], "fail", "{r}");
    assert!(
        t.contains("FORBIDDEN found"),
        "the lint's own words reach the log: {t}"
    );
}

/// FAIL CLOSED. A car whose files conflict with main cannot be merged, so
/// no lint can judge the boarding tree: a red, named, and no lint ran.
#[test]
fn a_car_that_conflicts_with_main_is_red_and_no_lint_runs() {
    let tree = Tree::new("conflict", "the car's words\n", Some("main's words\n"));
    let out = tree.run(&["--lints", LINT]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    let r = tree.receipt(&out);
    assert_eq!(r["verdict"], "failed", "{r}");
    assert_eq!(check_names(&r), vec!["lints-merge".to_string()], "{r}");
    assert_eq!(r["dirty"], false, "{r}");
}

/// A REFUSAL, not a red: what the launcher asked for is not a lint main
/// holds, so nothing about the car was judged. And `--lints` is an
/// alternative to every other mode, refused beside one before anything runs.
#[test]
fn a_lint_main_does_not_hold_or_a_second_mode_is_refused() {
    let tree = Tree::new("refused", "a clean doc\n", None);
    for (args, says) in [
        (vec!["--lints", "infra/lint/not-on-main.sh"], "not-on-main"),
        (
            vec!["--lints", "infra/lint/lib/git-answer.sh"],
            "lib/git-answer.sh",
        ),
        (vec!["--lints", "infra/gate.sh"], "infra/gate.sh"),
    ] {
        let _ = std::fs::remove_file(tree.receipt_path());
        let out = tree.run(&args);
        let t = text(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {t}");
        let r = tree.receipt(&out);
        assert_eq!(r["verdict"], "refused", "{args:?}: {r}");
        let why = r["refused_because"].as_str().unwrap_or_default();
        assert!(why.contains(says), "{args:?}: {why}");
        assert!(check_names(&r).is_empty(), "{args:?}: nothing ran: {r}");
    }
    let out = tree.run(&["--auto", "--lints", LINT]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("alternatives"), "{}", text(&out));
}
