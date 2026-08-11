//! `conductor.py --preflight` — the locomotive check.
//!
//! The 2026-08-10 18:01 window crashed before boarding: root-owned
//! objects in the conductor's clone (left by a sudo probe) made its
//! fetch fail at the moment the window opened. The consist had been
//! rehearsed; the locomotive had not. Pre-flight closes that: every
//! conductor entry (including the 10-minute reconcile, which becomes
//! the early-warning cadence) first proves the clone is healthy —
//! owned by the running user, remotes reachable — and fails LOUDLY
//! with a distinct exit code before touching any train state.
//!
//! Same idiom as migrate_sh.rs: build a scratch fixture (bare local
//! "upstream" + "fork" repos, a clone wired to both), run the real
//! script against it, assert on exit codes and named problems.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A conductor HOME with a healthy clone: local bare upstream (with
/// one commit on main) + local bare fork, HOME/repo cloned from
/// upstream with a `fork` remote. Scratch dirs are pid+name-keyed
/// under the system temp dir (same tradeoff as migrate_sh.rs: a
/// panic can leak one, and the `boss-preflight-` prefix makes
/// orphans easy to find).
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    upstream: PathBuf,
    fork: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("boss-preflight-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let root = root.as_path();
        let upstream = root.join("upstream.git");
        let fork = root.join("fork.git");
        let seed = root.join("seed");
        let home = root.join("train-home");

        for bare in [&upstream, &fork] {
            std::fs::create_dir_all(bare).unwrap();
            git(bare, &["init", "--bare", "--initial-branch=main", "."]);
        }
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--initial-branch=main", "."]);
        git(&seed, &["config", "user.name", "fixture"]);
        git(&seed, &["config", "user.email", "fixture@test.invalid"]);
        std::fs::write(seed.join("README"), "fixture\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "seed"]);
        git(
            &seed,
            &["remote", "add", "origin", upstream.to_str().unwrap()],
        );
        git(&seed, &["push", "origin", "main"]);

        std::fs::create_dir_all(&home).unwrap();
        let clone = home.join("repo");
        let out = Command::new("git")
            .args(["clone", upstream.to_str().unwrap(), clone.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clone: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        git(&clone, &["remote", "add", "fork", fork.to_str().unwrap()]);

        Fixture {
            root: root.to_path_buf(),
            home,
            upstream,
            fork,
        }
    }

    fn preflight(&self) -> Output {
        Command::new("python3")
            .arg(repo_root().join("infra/train/conductor.py"))
            .arg("--preflight")
            .env("BOSS_TRAIN_HOME", &self.home)
            .env("BOSS_TRAIN_UPSTREAM_URL", &self.upstream)
            .env("BOSS_TRAIN_FORK_URL", &self.fork)
            // Never contacted by preflight; a wrong port makes any
            // accidental API call fail the test instead of touching
            // a real jobs-api.
            .env("BOSS_JOBS_URL", "http://127.0.0.1:1")
            .output()
            .expect("conductor.py runs")
    }
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn healthy_clone_passes() {
    let fx = Fixture::new("healthy");
    let out = fx.preflight();
    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "healthy locomotive must pre-flight clean: {text}"
    );
    assert!(text.contains("preflight ok"), "names the verdict: {text}");
}

#[test]
fn unreachable_fork_remote_fails_loudly_with_exit_3() {
    let fx = Fixture::new("sick-fork");
    let clone = fx.home.join("repo");
    git(
        &clone,
        &[
            "remote",
            "set-url",
            "fork",
            fx.root.join("gone.git").to_str().unwrap(),
        ],
    );
    let out = fx.preflight();
    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a sick locomotive exits 3, distinct from a clean run: {text}"
    );
    assert!(
        text.contains("fork"),
        "the failure names the remote that broke: {text}"
    );
}

#[test]
fn missing_clone_passes_as_first_boarding() {
    // No clone yet is not sickness — the first board() creates it.
    let fx = Fixture::new("no-clone");
    std::fs::remove_dir_all(fx.home.join("repo")).unwrap();
    let out = fx.preflight();
    let text = combined(&out);
    assert_eq!(out.status.code(), Some(0), "no clone yet passes: {text}");
    assert!(
        text.contains("no clone"),
        "says why there was nothing to check: {text}"
    );
}
