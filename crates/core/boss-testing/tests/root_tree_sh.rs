//! `infra/forge/root-tree.sh` and `infra/forge/forge-converge-launch.sh`:
//! the tree root executes from on the forge host, and which generation
//! of it a converge tick runs (backlog a604a35b; decided by David on
//! design-doc c98c79aa, question root-tree, and as position B on the
//! packet, 2026-10-07).
//!
//! THE DEFECT. Root ran its converge, backup, reaper, observers and the
//! cluster watchdog out of /home/david/boss, a checkout its owner can
//! write: any shell as that owner edited what root runs within ten
//! minutes (review 9a1e289b).
//!
//! WHAT IS HELD HERE, by running the two scripts against scratch
//! repositories — never read as text, except the one pin that says so:
//!   * a refresh makes a whole generation and only then moves `current`;
//!   * a fetch that fails, a commit that is not a fast-forward, a commit
//!     that lacks the files the units name, a tree directory that is not
//!     this account's own: each leaves `current` naming what it named,
//!     whole, and says so;
//!   * generations no link names are removed, the three that are named
//!     never are;
//!   * the launcher prefers the generation that has shown it can bring
//!     in the next one, gives a newer one two tries, and offers the
//!     checkout's copy only until some generation has ever been good;
//!   * the installer started from a checkout brings the tree and hands
//!     over to it, installs no unit whose command is not there, and
//!     carries the watchdog's count as data.
//!
//! NOT HELD HERE, because no suite is root with a second account: the
//! serving side run as ANOTHER uid (`runuser -u <owner> --` or
//! `setpriv --reuid`). The seam `ROOT_TREE_SOURCE_RUNAS` stands in for
//! those words; the cross-account fetch itself has run on the forge only
//! in its first spelling, the probe view's, which the last test here
//! holds this one equal to.

// not a tree-wide pin: its read_dir lists only this test's own scratch
// trees.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    repo_root().join("infra/forge/root-tree.sh")
}

fn launcher() -> PathBuf {
    repo_root().join("infra/forge/forge-converge-launch.sh")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// One fixture host: the forge's repository (`forge.git`, a plain
/// repository on branch main carrying the three files a generation
/// needs) and a tree directory root-tree.sh is pointed at.
struct Host {
    dir: PathBuf,
    forge: PathBuf,
    tree: PathBuf,
}

impl Host {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(&format!("root-tree-{name}"));
        let forge = dir.join("forge.git");
        create_dir(&forge.join("infra/forge"));
        git(&forge, &["init", "-q", "-b", "main"]);
        for f in ["forge-converge.sh", "install.sh", "root-tree.sh"] {
            write_exec(
                &forge.join("infra/forge").join(f),
                &format!("#!/bin/sh\n# fixture {f}\n"),
            );
        }
        git(&forge, &["add", "-A"]);
        git(&forge, &["commit", "-q", "-m", "one"]);
        let tree = dir.join("tree");
        Self { dir, forge, tree }
    }

    fn head(&self) -> String {
        git(&self.forge, &["rev-parse", "HEAD"])
    }

    /// One more commit on the forge's main; returns it.
    fn land(&self, file: &str) -> String {
        write_file(&self.forge.join(file), file);
        git(&self.forge, &["add", "-A"]);
        git(&self.forge, &["commit", "-q", "-m", file]);
        self.head()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("bash");
        c.arg(script())
            .args(args)
            .env("BOSS_ROOT_TREE", &self.tree)
            .env("BOSS_ROOT_TREE_SOURCE", &self.forge)
            .env("ROOT_TREE_SOURCE_RUNAS", "")
            .env_remove("BOSS_ROOT_TREE_REF");
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let o = self.cmd(args).output().unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    }

    fn refresh(&self) -> (i32, String) {
        self.run(&["refresh"])
    }

    fn link(&self, name: &str) -> String {
        std::fs::read_link(self.tree.join(name))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "none".into())
    }

    fn generations(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.tree.join("gen"))
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}

#[test]
fn a_refresh_makes_a_whole_generation_and_then_names_it() {
    let h = Host::new("first");
    let one = h.head();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(h.link("current"), format!("gen/{one}"));
    let cur = h.tree.join("current");
    for f in ["forge-converge.sh", "install.sh", "root-tree.sh"] {
        assert!(cur.join("infra/forge").join(f).is_file(), "{f}: {out}");
    }
    assert_eq!(read(&cur.join(".boss-tree-sha")).trim(), one);
    assert_eq!(h.link("previous"), "none", "nothing came before the first");
    assert_eq!(h.generations(), [one.as_str()]);
    assert_eq!(h.run(&["head"]), (0, format!("{one}\n")));
    let (rc, path) = h.run(&["path"]);
    assert_eq!(rc, 0);
    assert!(path.trim().ends_with(&format!("gen/{one}")), "{path}");

    // A second call is a no-op, and says so.
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("already"), "{out}");
    assert_eq!(h.generations(), [one]);
}

#[test]
fn a_fast_forward_moves_current_and_only_named_generations_stay() {
    let h = Host::new("ff");
    let one = h.head();
    assert_eq!(h.refresh().0, 0);
    let two = h.land("two.txt");
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(h.link("current"), format!("gen/{two}"));
    assert_eq!(h.link("previous"), format!("gen/{one}"));
    assert!(h.tree.join("current/two.txt").is_file());

    // `one` is marked good; two more trains land. One generation no link
    // names (two) goes; the good one stays however old it is.
    let (rc, out) = h.run(&["mark-good", h.tree.join("gen").join(&one).to_str().unwrap()]);
    assert_eq!(rc, 0, "{out}");
    let three = h.land("three.txt");
    assert_eq!(h.refresh().0, 0);
    let four = h.land("four.txt");
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    let mut want = vec![one.clone(), three.clone(), four.clone()];
    want.sort();
    assert_eq!(h.generations(), want, "two is named by nothing: {out}");
    assert_eq!(h.link("good"), format!("gen/{one}"));
    assert_eq!(h.link("previous"), format!("gen/{three}"));
    assert!(!h.tree.join("gen").join(&two).exists());
}

#[test]
fn a_main_that_is_not_a_fast_forward_is_refused_and_current_stays() {
    let h = Host::new("rewind");
    assert_eq!(h.refresh().0, 0);
    let two = h.land("two.txt");
    assert_eq!(h.refresh().0, 0);
    // The forge's main is replaced by a commit that does not descend
    // from `two` — the 2026-09-25 shape.
    git(&h.forge, &["reset", "-q", "--hard", "HEAD~1"]);
    let other = h.land("other.txt");
    let (rc, out) = h.refresh();
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("REFUSED"), "{out}");
    assert!(out.contains("NOT a descendant"), "{out}");
    assert_eq!(h.link("current"), format!("gen/{two}"));
    assert!(
        h.tree.join("current/two.txt").is_file(),
        "the tree is whole"
    );
    assert!(
        !h.tree.join("gen").join(&other).exists(),
        "nothing was exported"
    );

    // Naming another commit accepts nothing; naming this one moves.
    let (rc, out) = h.run(&["refresh", "--accept-rewind", &two]);
    assert_eq!(rc, 3, "{out}");
    let (rc, out) = h.run(&["refresh", "--accept-rewind", &other]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("ACCEPTED A REWIND BY HAND"), "{out}");
    assert_eq!(h.link("current"), format!("gen/{other}"));
}

#[test]
fn a_fetch_that_fails_leaves_the_previous_tree_and_says_so() {
    let h = Host::new("fetch-fails");
    let one = h.head();
    assert_eq!(h.refresh().0, 0);
    h.land("two.txt");
    // The serving side refuses: the words that run it exit 1.
    let refuse = h.dir.join("refuse");
    write_exec(
        &refuse,
        "#!/bin/sh\necho 'upload-pack refused' >&2\nexit 1\n",
    );
    let o = h
        .cmd(&["refresh"])
        .env("ROOT_TREE_SOURCE_RUNAS", &refuse)
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("FAILED: fetching"), "{out}");
    assert!(
        out.contains(&format!("current stays at {}", &one[..12])),
        "{out}"
    );
    assert_eq!(h.link("current"), format!("gen/{one}"));
    assert!(h.tree.join("current/infra/forge/install.sh").is_file());
    assert_eq!(h.generations(), [one.as_str()], "no half tree: {out}");

    // A source that is not there at all: the same.
    let o = h
        .cmd(&["refresh"])
        .env("BOSS_ROOT_TREE_SOURCE", h.dir.join("nowhere"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(h.link("current"), format!("gen/{one}"));

    // And on a host with no tree yet, a failed first fetch leaves no
    // `current` at all — never an empty one.
    let fresh = Host::new("fetch-fails-fresh");
    let o = fresh
        .cmd(&["refresh"])
        .env("ROOT_TREE_SOURCE_RUNAS", &refuse)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(fresh.link("current"), "none");
    assert_eq!(fresh.run(&["path"]).0, 1);
    assert!(fresh.generations().is_empty());
}

#[test]
fn a_commit_without_the_files_the_units_name_is_not_a_tree() {
    let h = Host::new("lacks");
    assert_eq!(h.refresh().0, 0);
    // A REVERT OF THIS CAR IS STILL A TREE: it carries the converge and
    // the installer and no root-tree.sh, and it must be accepted so that
    // its own (old) converge runs and puts the old units back.
    git(&h.forge, &["rm", "-q", "infra/forge/root-tree.sh"]);
    git(&h.forge, &["commit", "-q", "-m", "reverts the car"]);
    let one = h.head();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "a revert must roll back by itself: {out}");
    assert_eq!(h.link("current"), format!("gen/{one}"));
    git(&h.forge, &["rm", "-q", "infra/forge/forge-converge.sh"]);
    git(&h.forge, &["commit", "-q", "-m", "drops the converge"]);
    let (rc, out) = h.refresh();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("carries no executable infra/forge/forge-converge.sh"),
        "{out}"
    );
    assert_eq!(h.link("current"), format!("gen/{one}"));
    assert!(
        !h.generations().iter().any(|g| g.starts_with('.')),
        "the staged tree is gone: {out}"
    );
}

#[test]
fn a_tree_directory_that_is_not_this_accounts_own_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let h = Host::new("loose");
    // A directory any account in its group can write.
    create_dir(&h.tree);
    // mode-bits-ok: a directory, made loose on purpose for the refusal.
    std::fs::set_permissions(&h.tree, std::fs::Permissions::from_mode(0o775)).unwrap();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("writable by its group or by anyone"), "{out}");
    assert_eq!(h.link("current"), "none");

    // A tree that is a symlink to somewhere else.
    let s = Host::new("symlinked");
    let elsewhere = s.dir.join("elsewhere");
    create_dir(&elsewhere);
    std::os::unix::fs::symlink(&elsewhere, &s.tree).unwrap();
    let (rc, out) = s.refresh();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is a symlink"), "{out}");
    assert!(
        std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
        "nothing was written through it"
    );
}

#[test]
fn only_a_generation_of_this_tree_can_be_marked_good() {
    let h = Host::new("good");
    let one = h.head();
    assert_eq!(h.refresh().0, 0);
    // The checkout's own directory, and a look-alike beside the tree.
    let (rc, out) = h.run(&["mark-good", h.forge.to_str().unwrap()]);
    assert_eq!(rc, 1, "{out}");
    let fake = h.dir.join("gen").join(&one);
    create_dir(&fake);
    write_file(&fake.join(".boss-tree-sha"), &format!("{one}\n"));
    let (rc, out) = h.run(&["mark-good", fake.to_str().unwrap()]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(h.link("good"), "none");
    // Through the `current` name, as the converge's own directory resolves.
    let (rc, out) = h.run(&["mark-good", h.tree.join("current").to_str().unwrap()]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(h.link("good"), format!("gen/{one}"));
}

/// THE REFRESH LOCK IS ROOT'S ALONE (review 5f3736a2, F1). flock(2) needs
/// only a descriptor, and the lock was made 0644: any account on the
/// host could open it, hold it, and root's refresh then waited two
/// minutes and failed — the tree never moved, and held across the first
/// ticks after landing no generation was ever marked good. Held by
/// effect in every harness: the mode. Where the harness is root and can
/// become a second uid (the dev pod; not the gate, which is one uid),
/// that uid is shown to read the tree and NOT to take the lock.
#[test]
fn the_refresh_lock_cannot_be_held_by_another_account() {
    use std::os::unix::fs::PermissionsExt;
    let h = Host::new("lock");
    // The scratch directory and its parent, open to a second uid: the
    // control below needs that uid to reach the tree at all.
    for d in [h.dir.parent().unwrap(), h.dir.as_path()] {
        // mode-bits-ok: a directory, opened so another uid can traverse it.
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(h.refresh().0, 0);
    let lock = h.tree.join(".lock");
    let mode = std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode,
        0o600,
        "{} is {mode:o}: an account that can open it can hold root's refresh",
        lock.display()
    );
    // The single-uid harness's pin on HOW: made under a closed umask and
    // re-closed on every refresh, so a lock an older copy left 0644 is too.
    let sh = read(&script());
    assert!(
        sh.contains("(umask 077 && timeout -k 2 10 sh -c ': >>\"$1\"' sh \"$TREE/.lock\")")
            && sh.contains("chmod 0600 -- \"$TREE/.lock\""),
        "root-tree.sh must make and keep the lock 0600"
    );

    let other = ["--reuid=12345", "--regid=12345", "--clear-groups"];
    let as_other = |args: &[&str]| {
        Command::new("setpriv")
            .args(other)
            .args(args)
            .output()
            .map(|o| (o.status.success(), text(&o)))
            .unwrap_or((false, "no setpriv".into()))
    };
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if !is_root || !as_other(&["true"]).0 {
        eprintln!("NOT RUN: the second-uid leg — this harness cannot become another uid");
        return;
    }
    // The method's own control: the other uid reaches the tree and reads
    // a world-readable file in it, so a refusal below is the lock's.
    let stamp = h.tree.join("current/.boss-tree-sha");
    let (ok, out) = as_other(&["cat", stamp.to_str().unwrap()]);
    assert!(
        ok,
        "the control: uid 12345 must be able to read {}: {out}",
        stamp.display()
    );
    let (held, out) = as_other(&["flock", "-n", "-x", lock.to_str().unwrap(), "-c", "true"]);
    assert!(
        !held,
        "uid 12345 took an exclusive flock on root's refresh lock: {out}"
    );
    // And with that uid still trying, root's next refresh is not delayed.
    let two = h.land("two.txt");
    let started = std::time::Instant::now();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(h.link("current"), format!("gen/{two}"));
    assert!(
        started.elapsed().as_secs() < 60,
        "the refresh waited: {out}"
    );
}

/// THE LOCK IS JUDGED BEFORE IT IS OPENED (review aa901496, N4). The
/// refresh opened and chmod'ed `.lock` and only then asked what it was:
/// a symlink there had its TARGET made 0600 before the refusal, and a
/// FIFO held the open — the refresh, and the converge behind it — with
/// no bound. Only root can put either there, so this is order, not
/// exposure; both are refused untouched, and promptly.
#[test]
fn a_lock_that_is_not_a_regular_file_is_refused_untouched_and_at_once() {
    use std::os::unix::fs::PermissionsExt;
    let h = Host::new("lock-shape");
    assert_eq!(h.refresh().0, 0, "a tree to refresh");
    let one = h.link("current");
    h.land("two.txt");
    let lock = h.tree.join(".lock");

    // A symlink: its target keeps the mode it had.
    let target = h.dir.join("someone-elses-file");
    write_file(&target, "not a lock\n");
    // mode-bits-ok: a regular file, given the mode the refresh must not change.
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(&target, &lock).unwrap();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("REFUSED:") && out.contains(".lock is not a regular file of its own"),
        "{out}"
    );
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644,
        "the symlink's target was chmod'ed before the refusal"
    );
    assert_eq!(read(&target), "not a lock\n");
    assert_eq!(h.link("current"), one, "current stays: {out}");

    // A dangling symlink is not followed into a new file either.
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(h.dir.join("not-there"), &lock).unwrap();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 1, "{out}");
    assert!(
        !h.dir.join("not-there").exists(),
        "the open went through the link"
    );

    // A FIFO: nothing ever writes to it, and the refresh still returns.
    std::fs::remove_file(&lock).unwrap();
    let o = Command::new("mkfifo").arg(&lock).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let started = std::time::Instant::now();
    let o = Command::new("timeout")
        .args(["-k", "2", "30", "bash"])
        .arg(script())
        .arg("refresh")
        .env("BOSS_ROOT_TREE", &h.tree)
        .env("BOSS_ROOT_TREE_SOURCE", &h.forge)
        .env("ROOT_TREE_SOURCE_RUNAS", "")
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "refused, not stopped at this test's own 30 s: {out}"
    );
    assert!(
        out.contains(".lock is not a regular file of its own"),
        "{out}"
    );
    assert!(
        started.elapsed().as_secs() < 10,
        "the refresh waited on the FIFO: {out}"
    );
    assert_eq!(h.link("current"), one);

    // The control: with a regular lock back, the same refresh moves.
    std::fs::remove_file(&lock).unwrap();
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    assert_ne!(h.link("current"), one, "{out}");
}

/// The status verb's own description is held to what the launcher says
/// (review aa901496, N6): the tree closes the CONVERGE's road through the
/// checkout and no other, and the verb that reports on the tree must not
/// say more than that.
#[test]
fn the_status_verb_does_not_claim_more_than_the_tree_closes() {
    let about = read(&repo_root().join("infra/ops/verbs/root-tree-status.json"));
    assert!(
        !about.contains("nothing unattended runs the checkout's copy again"),
        "root-tree-status.json still says nothing unattended runs the checkout's copy"
    );
    assert!(
        !about.contains("which every unit's ExecStart goes through"),
        "two units still start from the checkout"
    );
    assert!(about.contains("the forge CONVERGE never runs the checkout's copy again"));
    // And the two it names as still on the checkout are the two the unit
    // files say.
    for (unit, named) in [
        ("cluster-deploy-runner", "the cluster converge"),
        ("disk-floor-sweep", "the disk sweep"),
    ] {
        let body = read(&repo_root().join(format!("infra/forge/{unit}.service")));
        assert!(
            body.contains("\nExecStart=/home/david/boss/"),
            "{unit}.service no longer starts from the checkout — root-tree-status.json says it does"
        );
        assert!(about.contains(named), "{named}");
    }
}

/// ROOT'S STORE REMEMBERS WHAT IT ACCEPTED (review 5f3736a2, F4). It
/// kept no ref, so a fetch had nothing to offer the serving side and
/// every move of main downloaded the whole history again: four moves,
/// four packs of 31 MB at the real repository's size. A history of more
/// than a hundred objects arrives as a pack; a second move of three new
/// objects must not bring a second one.
#[test]
fn a_second_move_fetches_only_what_is_new() {
    let h = Host::new("incremental");
    for n in 0..150 {
        write_file(
            &h.forge.join(format!("bulk-{n}.txt")),
            &format!("file {n}\n"),
        );
    }
    git(&h.forge, &["add", "-A"]);
    git(&h.forge, &["commit", "-q", "-m", "bulk"]);
    assert_eq!(h.refresh().0, 0);
    let store = h.tree.join("repo.git");
    // Objects root's store holds, packed and loose, and objects the
    // forge's history has: equal means nothing was transferred twice.
    // (Counted, not "one pack": with every object checked, git keeps even
    // a four-object fetch as a pack of its own.)
    let held = || -> usize {
        git(&store, &["count-objects", "-v"])
            .lines()
            .filter_map(|l| {
                l.strip_prefix("in-pack: ")
                    .or_else(|| l.strip_prefix("count: "))
            })
            .map(|n| n.trim().parse::<usize>().unwrap())
            .sum()
    };
    let in_history = || {
        git(&h.forge, &["rev-list", "--objects", "--all"])
            .lines()
            .count()
    };
    assert!(
        in_history() > 100,
        "the control: a history a fetch brings as a pack"
    );
    assert_eq!(
        held(),
        in_history(),
        "the control: the first fetch brought the history once"
    );
    let two = h.land("two.txt");
    let (rc, out) = h.refresh();
    assert_eq!(rc, 0, "{out}");
    // A handful over is a thin pack's bases, written out again when the
    // pack is completed; the whole history over is the defect (+158 here).
    assert!(
        held() < in_history() + 20,
        "the second move downloaded objects root already held ({} held, {} in the history): its store has no ref to negotiate with",
        held(),
        in_history()
    );
    assert_eq!(
        git(&store, &["rev-parse", "refs/boss/accepted"]),
        two,
        "the ref names what `current` names"
    );
    // A refused move leaves the ref where it was.
    git(&h.forge, &["reset", "-q", "--hard", "HEAD~1"]);
    h.land("other.txt");
    assert_eq!(h.refresh().0, 3);
    assert_eq!(git(&store, &["rev-parse", "refs/boss/accepted"]), two);
}

// ── the launcher ─────────────────────────────────────────────────────

/// A tree whose generations each carry a converge script that prints
/// its own name; `legacy` prints `legacy`.
struct Launch {
    dir: PathBuf,
    tree: PathBuf,
    legacy: PathBuf,
}

impl Launch {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(&format!("converge-launch-{name}"));
        let tree = dir.join("tree");
        create_dir(&tree.join("gen"));
        let legacy = dir.join("checkout/infra/forge/forge-converge.sh");
        create_dir(legacy.parent().unwrap());
        write_exec(&legacy, "#!/bin/sh\necho ran=legacy\n");
        Self { dir, tree, legacy }
    }

    fn generation(&self, name: &str) {
        let at = self.tree.join("gen").join(name).join("infra/forge");
        create_dir(&at);
        write_exec(
            &at.join("forge-converge.sh"),
            &format!("#!/bin/sh\necho ran={name}\n"),
        );
    }

    fn point(&self, link: &str, name: &str) {
        let l = self.tree.join(link);
        let _ = std::fs::remove_file(&l);
        std::os::unix::fs::symlink(format!("gen/{name}"), &l).unwrap();
    }

    /// One tick: which script ran, and what the launcher said.
    fn tick(&self) -> (String, String) {
        let o = Command::new("bash")
            .arg(launcher())
            .env("BOSS_ROOT_TREE", &self.tree)
            .env("BOSS_FORGE_LEGACY_CONVERGE", &self.legacy)
            .output()
            .unwrap();
        let out = text(&o);
        let ran = out
            .lines()
            .find_map(|l| l.strip_prefix("ran="))
            .unwrap_or("nothing")
            .to_string();
        (ran, out)
    }
}

#[test]
fn the_launcher_runs_current_when_it_is_the_good_generation() {
    let l = Launch::new("steady");
    l.generation("aaa");
    l.point("current", "aaa");
    l.point("good", "aaa");
    for _ in 0..4 {
        let (ran, out) = l.tick();
        assert_eq!(ran, "aaa", "{out}");
    }
    assert!(
        !l.tree.join(".launch-tries").exists(),
        "a good current is not counted"
    );
}

#[test]
fn a_newer_generation_gets_two_tries_then_every_other_tick() {
    let l = Launch::new("newer");
    l.generation("aaa");
    l.generation("bbb");
    l.point("good", "aaa");
    l.point("current", "bbb");
    // bbb never marks itself good (its script here marks nothing).
    let ran: Vec<String> = (0..7).map(|_| l.tick().0).collect();
    assert_eq!(ran, ["bbb", "bbb", "aaa", "bbb", "aaa", "bbb", "aaa"]);
    let (_, out) = {
        l.point("current", "bbb");
        l.tick()
    };
    assert!(out.contains("bbb"), "{out}");

    // A third generation starts its own count.
    l.generation("ccc");
    l.point("current", "ccc");
    let ran: Vec<String> = (0..3).map(|_| l.tick().0).collect();
    assert_eq!(ran, ["ccc", "ccc", "aaa"]);
    // And once it is good, it simply runs.
    l.point("good", "ccc");
    assert_eq!(l.tick().0, "ccc");
}

#[test]
fn the_checkouts_copy_runs_only_until_a_generation_has_ever_been_good() {
    // No tree at all: the old way keeps working.
    let l = Launch::new("bootstrap");
    let (ran, out) = l.tick();
    assert_eq!(ran, "legacy", "{out}");
    assert!(out.contains("BOOTSTRAP"), "{out}");

    // A first generation that never becomes good: two tries, then the
    // checkout's copy every other tick.
    l.generation("aaa");
    l.point("current", "aaa");
    let ran: Vec<String> = (0..5).map(|_| l.tick().0).collect();
    assert_eq!(ran, ["aaa", "aaa", "legacy", "aaa", "legacy"]);

    // It becomes good: the checkout's copy is never offered again — not
    // for a newer generation that fails, and not when `current` is gone.
    l.point("good", "aaa");
    l.generation("bbb");
    l.point("current", "bbb");
    let ran: Vec<String> = (0..5).map(|_| l.tick().0).collect();
    assert_eq!(ran, ["bbb", "bbb", "aaa", "bbb", "aaa"]);
    std::fs::remove_file(l.tree.join("current")).unwrap();
    assert_eq!(l.tick().0, "aaa");

    // `good` standing but naming nothing runnable still closes the arm.
    let d = Launch::new("bootstrap-closed");
    d.point("good", "gone");
    let (ran, out) = d.tick();
    assert_eq!(ran, "nothing", "{out}");
    assert!(out.contains("NOTHING TO RUN"), "{out}");
    assert!(
        d.dir.join("checkout").exists(),
        "the control: the legacy copy was there to run"
    );
}

// ── the installer ────────────────────────────────────────────────────

/// A checkout that is this tree's `infra/` committed as a repository,
/// which is also the fixture's "forge repository"; a scratch /etc, a
/// stub systemctl, and every seam pointed inside the scratch directory.
struct Install {
    dir: PathBuf,
    checkout: PathBuf,
    tree: PathBuf,
    etc: PathBuf,
}

impl Install {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(&format!("install-root-tree-{name}"));
        let checkout = dir.join("checkout");
        create_dir(&checkout);
        let o = Command::new("cp")
            .arg("-R")
            .arg("--preserve=mode")
            .arg(repo_root().join("infra"))
            .arg(&checkout)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o));
        git(&checkout, &["init", "-q", "-b", "main"]);
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "one"]);
        let etc = dir.join("etc");
        create_dir(&etc);
        create_dir(&dir.join("bin"));
        write_exec(
            &dir.join("bin/systemctl"),
            "#!/bin/sh\necho \"$*\" >> \"$STUB_LOG\"\ncase \"$1\" in is-active) echo active ;; esac\nexit 0\n",
        );
        write_exec(&dir.join("bin/apt-get"), "#!/bin/sh\nexit 0\n");
        create_dir(&dir.join("unit-lib"));
        let tree = dir.join("tree");
        Self {
            dir,
            checkout,
            tree,
            etc,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new("bash");
        c.arg(self.checkout.join("infra/forge/install.sh"))
            .env("INSTALL_ETC", &self.etc)
            .env("INSTALL_SYSTEMCTL", self.dir.join("bin/systemctl"))
            .env("STUB_LOG", self.dir.join("systemctl.log"))
            .env("INSTALL_SOR_ENV", self.dir.join("sor.env"))
            .env("BOSS_SOR_ENV", self.dir.join("sor.env"))
            .env("INSTALL_KUBECTL", "0")
            // Every seam the installer judges is named, or it refuses on a
            // machine that is not the forge (backlog 62b09c57, N7): until
            // that check this fixture left the package manager and the ops
            // runner's retirement marker at the machine's own.
            .env("INSTALL_APT_GET", self.dir.join("bin/apt-get"))
            .env("INSTALL_UNIT_LIB", self.dir.join("unit-lib"))
            .env(
                "BOSS_OPS_RUNNER_RETIRED",
                self.dir.join("ops-runner.retired"),
            )
            .env("INSTALL_ROOT_TREE", "1")
            .env("BOSS_ROOT_TREE", &self.tree)
            .env("BOSS_ROOT_TREE_SOURCE", &self.checkout)
            .env("ROOT_TREE_SOURCE_RUNAS", "")
            .env("INSTALL_TREE_LIBEXEC", self.dir.join("libexec"))
            .env("INSTALL_WATCHDOG_STATE", self.dir.join("watchdog"))
            .env("INSTALL_WATCHDOG_OLD_HOME", self.dir.join("oldhome"))
            .env("BOSS_FORGE_REPO_DIR", &self.checkout)
            .env("BOSS_RUN_SUMMARY_FILE", self.dir.join("summary.json"))
            .env_remove("BOSS_INSTALL_FROM_TREE")
            .env_remove("BOSS_NODE_ROLES");
        c
    }

    fn run(&self) -> (i32, String) {
        let o = self.cmd().output().unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    }
}

#[test]
fn started_from_the_checkout_the_installer_brings_the_tree_and_hands_over() {
    let i = Install::new("bootstrap");
    let old = i.dir.join("oldhome");
    create_dir(&old);
    write_file(&old.join(".boss-watchdog-dark"), "2\n");
    // A blind marker that is not a timestamp: a name planted to be read
    // through. Its bytes must reach neither the new state nor the output.
    write_file(
        &old.join(".boss-watchdog-blind"),
        "SECRET-LOOKING-BYTES-0123456789\n",
    );
    // THE THREAT ITSELF: the checkout's owner edits a unit in his working
    // tree — an edit the forge's main does not carry.
    let edited = i.checkout.join("infra/forge/cluster-watchdog.service");
    write_file(
        &edited,
        &format!(
            "{}ExecStartPre=/bin/sh -c EDITED-IN-THE-CHECKOUT\n",
            read(&edited)
        ),
    );
    let (rc, out) = i.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        !read(&i.etc.join("cluster-watchdog.service")).contains("EDITED-IN-THE-CHECKOUT"),
        "what is installed is what root fetched from the forge's main, never the checkout's \
         working tree: {out}"
    );
    assert!(
        i.tree.join("current/infra/forge/install.sh").is_file(),
        "{out}"
    );
    assert!(out.contains("which is not root's tree — running "), "{out}");

    // The units are the tree's, and each names the tree, not a home.
    let wd = read(&i.etc.join("cluster-watchdog.service"));
    assert!(wd.contains("\nUser=root\n"), "{wd}");
    assert!(
        wd.contains("\nExecStart=/var/lib/boss/tree/current/infra/forge/cluster-watchdog.sh\n")
    );
    let fc = read(&i.etc.join("forge-converge.service"));
    assert!(
        fc.contains("\nExecStart=/usr/local/libexec/boss/forge-converge-launch\n"),
        "{fc}"
    );
    assert_eq!(
        read(&i.dir.join("libexec/forge-converge-launch")),
        read(&launcher()),
        "root's copy of the launcher is this file"
    );

    // The ops runner's unit pair comes from the tree; its ExecStart
    // still names the checkout, by name (stage 2 is not this car).
    let dropin = read(&i.etc.join("boss-ops-runner.service.d/forge.conf"));
    assert!(
        dropin.contains(&format!(
            "ExecStart={}/infra/ops/ops-runner.sh",
            i.checkout.display()
        )),
        "{dropin}"
    );

    // The count is carried; the thing that was not a timestamp is not,
    // and is not printed; the carry runs once.
    assert_eq!(read(&i.dir.join("watchdog/dark")), "2\n");
    assert!(!i.dir.join("watchdog/blind").exists());
    assert!(!out.contains("SECRET-LOOKING"), "{out}");
    assert!(out.contains("not carried (and not printed)"), "{out}");
    write_file(&old.join(".boss-watchdog-dark"), "5\n");
    std::fs::remove_file(i.dir.join("watchdog/dark")).unwrap();
    let (rc, out) = i.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        !i.dir.join("watchdog/dark").exists(),
        "the old home is read once: {out}"
    );
}

#[test]
fn with_no_tree_no_unit_file_is_replaced_and_the_run_is_red() {
    let i = Install::new("held");
    // What is installed today: a unit that works, from the checkout.
    let installed =
        "[Service]\nUser=david\nExecStart=/home/david/boss/infra/forge/cluster-watchdog.sh\n";
    write_file(&i.etc.join("cluster-watchdog.service"), installed);
    // The forge's repository cannot be fetched.
    let o = i
        .cmd()
        .env("BOSS_ROOT_TREE_SOURCE", i.dir.join("nowhere"))
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("UNITS HELD"), "{out}");
    assert!(
        out.contains(
            "cluster-watchdog.service:/var/lib/boss/tree/current/infra/forge/cluster-watchdog.sh"
        ),
        "{out}"
    );
    assert_eq!(
        read(&i.etc.join("cluster-watchdog.service")),
        installed,
        "the watchdog keeps the unit it had"
    );
    assert!(
        !i.etc.join("forge-converge.service").exists(),
        "no unit file was written"
    );
    assert!(
        !i.dir.join("watchdog").exists(),
        "no state is moved for a unit that did not change"
    );
    // Everything else converged: the address file, the ops runner, the timers.
    assert!(i.dir.join("sor.env").is_file(), "{out}");
    assert!(i.etc.join("boss-ops-runner.service").is_file(), "{out}");
    assert!(read(&i.dir.join("systemctl.log")).contains("enable --now cluster-watchdog.timer"));
    assert!(read(&i.dir.join("summary.json")).contains("units_held"));
    // A HELD RUN PRINTS NO SUCCESS LINE (review 5f3736a2, F7): it used to
    // end "9 unit pair(s) installed and enabled" with nothing installed.
    assert!(
        !out.contains("installed and enabled"),
        "a held run must not say it installed anything: {out}"
    );
    let summary: serde_json::Value =
        serde_json::from_str(&read(&i.dir.join("summary.json"))).expect("the summary is JSON");
    assert_eq!(summary["units_installed"], "0", "{summary}");
    assert!(
        summary["summary"]
            .as_str()
            .unwrap_or("")
            .starts_with("HELD"),
        "{summary}"
    );

    // The next tick, with the repository back, installs them.
    let (rc, out) = i.run();
    assert_eq!(rc, 0, "{out}");
    assert!(read(&i.etc.join("cluster-watchdog.service")).contains("\nUser=root\n"));
}

// ── the kit reader's grantee ─────────────────────────────────────────

const KIT_RULE: &str = "ALL=(root) NOPASSWD: /usr/local/libexec/boss/recovery-kit-read";

impl Install {
    /// The same run with the recovery kit's leg switched on, every path
    /// it writes inside the scratch directory: the reader's copy, the
    /// sudoers directory, a `visudo` that accepts, and a `getent` whose
    /// passwd table is the fixture's — `kitforge` (uid 1000) and `toor`
    /// (a second name for uid 0), nobody else.
    fn kit_cmd(&self, owner: Option<&str>) -> Command {
        create_dir(&self.dir.join("sudoers.d"));
        write_exec(&self.dir.join("bin/visudo"), "#!/bin/sh\nexit 0\n");
        write_exec(
            &self.dir.join("bin/getent"),
            "#!/bin/sh\n[ \"$1\" = passwd ] || exit 2\ncase \"$2\" in\n  kitforge) echo 'kitforge:x:1000:1000::/home/kitforge:/bin/sh' ;;\n  toor) echo 'toor:x:0:0::/root:/bin/sh' ;;\n  *) exit 2 ;;\nesac\n",
        );
        let mut c = self.cmd();
        c.env("INSTALL_KIT_LIBEXEC", self.dir.join("libexec"))
            .env("INSTALL_KIT_SUDOERS_DIR", self.dir.join("sudoers.d"))
            .env("INSTALL_VISUDO", self.dir.join("bin/visudo"))
            .env("INSTALL_KIT_GETENT", self.dir.join("bin/getent"))
            .env("INSTALL_KIT_OWNER", "")
            .env_remove("INSTALL_KIT_USER")
            .env_remove("BOSS_FORGE_REPO_OWNER");
        if let Some(o) = owner {
            c.env("BOSS_FORGE_REPO_OWNER", o);
        }
        c
    }

    fn kit_rule(&self) -> String {
        read(&self.dir.join("sudoers.d/boss-recovery-kit"))
    }
}

/// Backlog ebfd2f46, measured on the forge 2026-10-08 00:42Z: from the
/// first tick under root's tree `forge-converge.service` exited 1 every
/// ten minutes on ONE step — "install-recovery-kit-reader: FAILED — the
/// checkout is root's". The kit installer read its grantee off the
/// owner of the tree it runs from, and that tree is now root's
/// generation, not the forge user's checkout.
///
/// In root's tree install.sh names the grantee: the account
/// forge-converge.sh already runs the checkout's git as
/// (`BOSS_FORGE_REPO_OWNER`), a name the tree carries and no account on
/// the host can write. The fixture's generation belongs to whoever runs
/// this suite — root on the dev pod, which is the live defect exactly —
/// and that account is never the one the rule names.
#[test]
fn in_roots_tree_the_kit_readers_rule_names_the_forge_user_not_the_trees_owner() {
    let i = Install::new("kit-user");
    let o = i.kit_cmd(Some("kitforge")).output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("which is not root's tree — running "), "{out}");
    let rule = i.kit_rule();
    assert_eq!(
        rule.lines().last(),
        Some(format!("kitforge {KIT_RULE}").as_str()),
        "the rule names someone other than the forge user:\n{rule}\n{out}"
    );
    assert_eq!(
        read(&i.dir.join("libexec/recovery-kit-read")),
        read(&i.tree.join("current/infra/forge/recovery-kit-read.sh")),
        "the reader root runs is the tree's"
    );
    // The packet says who holds the rule, read back off the rule.
    let summary: serde_json::Value =
        serde_json::from_str(&read(&i.dir.join("summary.json"))).expect("the summary is JSON");
    assert_eq!(
        summary["recovery_kit_reader"], "installed; the rule grants the reader to: kitforge",
        "{summary}"
    );

    // A REAL FAILURE IS STILL CARRIED, AND THE STANDING RULE STAYS. Each
    // of these is a name the installer must refuse: root, a second name
    // for uid 0, an account this host does not have, and a word that is
    // sudoers syntax. The run is red, says which step, and the rule the
    // good run placed is not replaced.
    for (bad, why) in [
        ("root", "the kit user is root"),
        ("toor", "is uid 0"),
        ("ghost", "there is no account 'ghost' on this host"),
        ("ALL", "is not an account name sudoers can carry safely"),
    ] {
        let o = i.kit_cmd(Some(bad)).output().unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{bad}: {out}");
        assert!(out.contains(why), "{bad}: {out}");
        assert!(
            out.contains("the recovery kit's reader did NOT install (exit 1)"),
            "{bad}: {out}"
        );
        assert_eq!(i.kit_rule(), rule, "{bad}: the standing rule was replaced");
        assert!(
            read(&i.dir.join("summary.json")).contains("install failed (exit 1)"),
            "{bad}: the packet's summary does not carry the failure"
        );
    }
}

/// The forge user's name is spelled in forge-converge.sh, which runs the
/// checkout's git as that account, and in install.sh, which a hand
/// `sudo infra/forge/install.sh` starts without the converge. Two
/// spellings of one fact, held equal (CLAUDE.md §9a) — and unnamed, the
/// installer hands the kit leg that default.
#[test]
fn the_forge_user_install_names_is_the_one_the_converge_runs_git_as() {
    fn default_of(file: &str) -> String {
        let body = read(&repo_root().join(file));
        let key = "${BOSS_FORGE_REPO_OWNER:-";
        let at: Vec<_> = body.match_indices(key).collect();
        assert_eq!(at.len(), 1, "{file} spells the default {} times", at.len());
        let rest = &body[at[0].0 + key.len()..];
        rest[..rest.find('}').expect("a closing brace")].to_string()
    }
    let converge = default_of("infra/forge/forge-converge.sh");
    assert_eq!(default_of("infra/forge/install.sh"), converge);
    assert_ne!(converge, "root");

    // Unnamed, the default is what reaches the installer: the fixture's
    // passwd table has no such account, and the refusal names it.
    let i = Install::new("kit-user-default");
    let o = i.kit_cmd(None).output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("there is no account '{converge}' on this host")),
        "{out}"
    );
    assert!(i.kit_rule().is_empty(), "a rule was placed: {out}");
}

// ── the one fact that lives twice ────────────────────────────────────

/// The probe view's fetch and this tree's are one mechanism spelled
/// twice — probe-account.sh's installed copy must stand alone and
/// source nothing — so the settings that make it safe are held equal
/// (CLAUDE.md §9a).
#[test]
fn the_trees_fetch_is_the_probe_views_fetch() {
    let view = read(&repo_root().join("infra/forge/probe-account.sh"));
    let tree = read(&script());
    for same in [
        "env GIT_CONFIG_GLOBAL=/dev/null \\\n",
        "git -c core.hooksPath=/dev/null -c fetch.fsckObjects=true ",
        "fetch -q --no-tags --upload-pack=\"${runas:+$runas }git upload-pack\" -- \"$",
    ] {
        assert!(view.contains(same), "probe-account.sh lost `{same}`");
        assert!(tree.contains(same), "root-tree.sh lost `{same}`");
    }
}
