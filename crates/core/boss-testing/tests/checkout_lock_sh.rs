//! `infra/forge/checkout-lock.sh` is RUN, not read — against scratch
//! repositories, so each property below is one the helper actually
//! has.
//!
//! THE DEFECT (backlog d66f92b2, measured 2026-09-07 22:01). Three git
//! users share the forge checkout at `/home/david/boss`:
//! `cluster-deploy-runner.sh` (its :01/:11/:21 timer AND the
//! merge-triggered `converge` ops-request), `forge-converge.sh` (the
//! same tick), and whatever a human runs by hand. Train #257 merged at
//! 22:01:22, the converge fired within a second and died on
//! `cannot lock ref 'refs/remotes/forgejo/main'` because forge-converge's
//! fetch held the ref; the 22:11 retry died on a stale `index.lock`.
//! Two lost cycles, a twenty-minute converge lag, and a board held on
//! an occupied track.
//!
//! The fix is ONE lock — `.git/boss-converge.lock`, taken with
//! `flock` by every git user of the checkout — plus a short-backoff
//! retry on the two lock errors, and a stale `index.lock` sweep that
//! removes the file only when no live git process is working in the
//! checkout. The fourth piece (a converge that still dies annotates
//! the ops-request that started it) moved with the cluster converge,
//! which runs from root's tree and touches no checkout since backlog
//! 683d7fe9: the_converge_runs_as_root_from_roots_tree.rs.
//!
//! `flock` is util-linux, essential on the Debian image the gate runs
//! in; `/proc` is how the sweep sees live git processes. Both are what
//! the forge host has.

use boss_testing::{feed_stdin, repo_root};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn helper() -> PathBuf {
    repo_root().join("infra/forge/checkout-lock.sh")
}

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(case: &str) -> (Scratch, PathBuf) {
    let dir = boss_testing::scratch_dir(&format!("checkout-lock-{case}"));
    (Scratch(dir.clone()), dir)
}

/// A real repository with one commit — `.git` must exist for the lock
/// file to live in, and the stale-lock sweep looks for `.git/index.lock`.
fn repo(root: &Path) -> PathBuf {
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&work)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(work.join("README"), "hello\n").unwrap();
    git(&["add", "README"]);
    git(&["commit", "-q", "-m", "first"]);
    work
}

/// `bash -c '. helper; <snippet>' _ args...` with the helper's knobs
/// turned down so a test does not wait on production backoffs.
fn sh(snippet: &str, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(". '{}'; {snippet}", helper().display()))
        .arg("_")
        .args(args)
        .env("CHECKOUT_BACKOFF", "0")
        .env("CHECKOUT_STALE_AFTER", "0");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("bash runs")
}

fn text(out: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Two lock-holding instances against one checkout never overlap: the
/// second's `start` is written only after the first's `end`.
#[test]
fn two_lock_holders_serialize() {
    let (_keep, root) = scratch("serialize");
    let work = repo(&root);
    let log = root.join("log");
    let snippet = r#"with_checkout_lock "$1" bash -c "echo start-$3 >> '$2'; sleep 0.4; echo end-$3 >> '$2'""#;
    let spawn = |tag: &str| {
        Command::new("bash")
            .arg("-c")
            .arg(format!(". '{}'; {snippet}", helper().display()))
            .arg("_")
            .arg(&work)
            .arg(&log)
            .arg(tag)
            .spawn()
            .expect("spawns")
    };
    let mut a = spawn("a");
    let mut b = spawn("b");
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 4, "both instances ran: {lines:?}");
    // Whichever went first, its end precedes the other's start.
    let first = lines[0].trim_start_matches("start-").to_string();
    assert_eq!(
        lines[1],
        format!("end-{first}"),
        "the critical sections interleaved: {lines:?}"
    );
    assert!(lines[2].starts_with("start-"), "{lines:?}");
    assert!(
        work.join(".git/boss-converge.lock").exists(),
        "the lock is the ONE file every git user of the checkout takes"
    );
}

/// A stale `index.lock` — nobody holds it — is removed and the removal
/// is logged; the same file with a live git process in the checkout is
/// left alone.
#[test]
fn a_stale_index_lock_is_removed_only_when_no_git_process_holds_the_checkout() {
    let (_keep, root) = scratch("stale");
    let work = repo(&root);
    let index_lock = work.join(".git/index.lock");

    // A live git in the checkout: `hash-object --stdin` blocks on stdin
    // until we close it, so its comm is `git` and its cwd is the repo.
    std::fs::write(&index_lock, "").unwrap();
    let mut live = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("git spawns");
    let out = sh(
        r#"clear_stale_index_lock "$1""#,
        &[work.to_str().unwrap()],
        &[],
    );
    let kept = index_lock.exists();
    // Only a release; `kept` above is the verdict (backlog fec29a02).
    feed_stdin(&mut live, b"\n");
    let _ = live.wait();
    assert!(
        kept,
        "index.lock was removed under a live git process: {}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("live"),
        "leaving the lock must say why: {}",
        text(&out)
    );

    // Nobody holds it now: removed, and said so.
    let out = sh(
        r#"clear_stale_index_lock "$1""#,
        &[work.to_str().unwrap()],
        &[],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !index_lock.exists(),
        "a stale index.lock must be removed: {}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("removed stale"),
        "the removal must be logged: {}",
        text(&out)
    );

    // Too young to judge (the default 30s guard): left alone.
    std::fs::write(&index_lock, "").unwrap();
    let out = sh(
        r#"clear_stale_index_lock "$1""#,
        &[work.to_str().unwrap()],
        &[("CHECKOUT_STALE_AFTER", "3600")],
    );
    assert!(
        index_lock.exists(),
        "a fresh index.lock is not stale: {}",
        text(&out)
    );
}

/// A fake `git` that fails with a lock error a set number of times,
/// counting its calls, then succeeds — so the retry is measured, not
/// assumed.
fn fake_git(bin: &Path, failures: u32, message: &str) {
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/usr/bin/env bash\n\
         n=$(cat \"$FAKE_GIT_COUNT\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$FAKE_GIT_COUNT\"\n\
         if [ \"$n\" -le {failures} ]; then echo \"{message}\" >&2; exit 128; fi\n\
         echo ok\n"
    );
    let path = bin.join("git");
    std::fs::write(&path, script).unwrap();
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&path, perm).unwrap();
}

/// The two lock errors are retried with backoff and the command then
/// succeeds; any other error is NOT retried; retries are bounded.
#[test]
fn lock_errors_are_retried_other_errors_are_not() {
    let (_keep, root) = scratch("retry");
    let work = repo(&root);
    let bin = root.join("bin");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let count = root.join("count");
    let run = |snippet: &str| {
        let _ = std::fs::remove_file(&count);
        sh(
            snippet,
            &[work.to_str().unwrap()],
            &[
                ("PATH", path.as_str()),
                ("FAKE_GIT_COUNT", count.to_str().unwrap()),
            ],
        )
    };
    let calls = || {
        std::fs::read_to_string(&count)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    // cannot lock ref: two failures, third try succeeds.
    fake_git(
        &bin,
        2,
        "error: cannot lock ref 'refs/remotes/forgejo/main': is at 1994077 but expected 77152b8",
    );
    let out = run(r#"checkout_git "$1" fetch -q forgejo main"#);
    assert!(
        out.status.success(),
        "the third try succeeds: {}",
        text(&out)
    );
    assert_eq!(calls(), "3", "two retries then success: {}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("retrying"),
        "each retry is logged: {}",
        text(&out)
    );

    // index.lock: the other lock error, same treatment.
    fake_git(
        &bin,
        1,
        "fatal: Unable to create '/x/.git/index.lock': File exists.",
    );
    let out = run(r#"checkout_git "$1" checkout -qf abc1234"#);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(calls(), "2", "{}", text(&out));

    // A non-lock error is a real verdict: one call, non-zero.
    fake_git(&bin, 9, "fatal: couldn't find remote ref main");
    let out = run(r#"checkout_git "$1" fetch -q forgejo main"#);
    assert!(
        !out.status.success(),
        "a real error must not be retried away: {}",
        text(&out)
    );
    assert_eq!(calls(), "1", "not retried: {}", text(&out));

    // Bounded: CHECKOUT_RETRIES tries, then give up non-zero.
    fake_git(
        &bin,
        9,
        "error: cannot lock ref 'refs/remotes/forgejo/main'",
    );
    let _ = std::fs::remove_file(&count);
    let out = sh(
        r#"checkout_git "$1" fetch -q forgejo main"#,
        &[work.to_str().unwrap()],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GIT_COUNT", count.to_str().unwrap()),
            ("CHECKOUT_RETRIES", "3"),
        ],
    );
    assert!(
        !out.status.success(),
        "exhausted retries fail: {}",
        text(&out)
    );
    assert_eq!(
        calls(),
        "3",
        "exactly CHECKOUT_RETRIES tries: {}",
        text(&out)
    );
}

/// The rule that settles the mid-scan race, tested where it can be.
///
/// `git_holds_checkout` reads `comm` first, then `cwd` and `cmdline`.
/// A process can vanish between those reads, and the old code counted
/// "both unreadable" as a live holder on the reasoning that the sweep
/// must never guess in favour of deleting. That is right for a LIVE
/// git it cannot inspect and wrong for one that is simply gone, and
/// the two are told apart by whether the /proc entry still exists.
/// It fired on 2026-09-09 at 05:20: a busy CI box, some git exiting
/// mid-scan, and a lock 0 seconds old reported as held by a live
/// process. On the forge the same race leaves a genuinely stale
/// index.lock in place, which is the condition the sweep exists for.
///
/// The race itself cannot be driven from fixtures — it needs a pid to
/// disappear between two reads inside one loop iteration — so the
/// DECISION is extracted as `pid_is_present` and pinned here. An
/// earlier version of this test removed the whole fixture directory
/// and asserted the outcome, which proved nothing: the glob then
/// matches no entry and the old code answered the same way.
#[test]
fn a_vanished_proc_entry_is_not_a_live_process() {
    let (_guard, dir) = scratch("pidpresent");
    let live = dir.join("4242");
    std::fs::create_dir_all(&live).unwrap();

    let present = |path: &std::path::Path| -> bool {
        Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {lib}\npid_is_present {p}\n",
                lib = repo_root().join("infra/forge/checkout-lock.sh").display(),
                p = path.display()
            ))
            .output()
            .expect("bash runs")
            .status
            .success()
    };

    assert!(present(&live), "an entry that exists is a live process");
    std::fs::remove_dir_all(&live).unwrap();
    assert!(
        !present(&live),
        "an entry that is gone is a process that exited, and it holds nothing"
    );
}

/// A live git the sweep CANNOT inspect still holds the checkout —
/// unknown must keep the lock, which is the half that must not change.
#[test]
fn a_live_but_unreadable_git_still_holds_the_checkout() {
    let (_guard, dir) = scratch("unreadable");
    let proc = dir.join("proc");
    let repo = dir.join("work");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let pid = proc.join("4242");
    std::fs::create_dir_all(&pid).unwrap();
    std::fs::write(pid.join("comm"), "git\n").unwrap();

    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". {lib}\nPROC_ROOT={proc} git_holds_checkout {repo}\n",
            lib = repo_root().join("infra/forge/checkout-lock.sh").display(),
            proc = proc.display(),
            repo = repo.display()
        ))
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "a git whose cwd and cmdline cannot be read, but which is THERE, keeps the lock"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "and the scan is silent: a vanished pid used to print No such file to the real stderr, because 2>/dev/null came after the input redirect: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
