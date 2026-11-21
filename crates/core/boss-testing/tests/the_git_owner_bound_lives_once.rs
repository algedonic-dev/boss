//! The git-owner bound lives once, in `infra/lib/git-owner.sh` (backlog
//! 5ff118da, left by car f36eb7ce's run e3d10551 on 2026-10-18).
//!
//! THE BOUND. An ops verb runs as root, and git refuses to read a clone
//! root does not own ("dubious ownership", ops-request c9877f75), while
//! silencing that with `safe.directory` would let root write objects
//! into the owner's clone. So every verb that reads a checkout resolves
//! the directory's owner and runs each git call AS that owner. That is a
//! way to READ a checkout, never proof of what it holds: the two retire
//! verbs once took the owner's `git status` as their deletion authority,
//! and review a04e64e9 P1 (2026-11-15) hid an edit from it with
//! skip-worktree. They prove a generation of root's tree now
//! (infra/lib/root-generation.sh, pinned in
//! infra/lint/the-controls-are-bounded-verbs.sh), and the lib's
//! cleanliness check went with its last caller.
//!
//! THE DEFECT. That bound was a function body copied into every verb that
//! needed it: three when the packet was filed, nine when this car was
//! built (measured on origin/main 2026-11-14), seven of them byte-equal.
//! CLAUDE.md §9a: collapse it, then pin it. Five of the seven equal
//! copies now source the lib, and the two retire verbs need no owner at
//! all; a copy that differs ON PURPOSE stays, and says why on
//! a line of its own — `# git-owner.sh not used: <reason>` — so the
//! exemption lives in the file that takes it and cannot outlive it (the
//! pin refuses a marker on a script that no longer defines its own).
//!
//! The lib is also RUN here, against a real git checkout, so what the
//! pin holds every verb to is a property the lib actually has.
//!
//! tree-wide pin — it walks infra/forge and infra/gcp, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::{create_dir, git_config_isolated, repo_root, scratch_dir, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The one definition.
const LIB: &str = "infra/lib/git-owner.sh";

/// Where the mutating forge and gcp verbs live.
const VERB_DIRS: [&str; 2] = ["infra/forge", "infra/gcp"];

/// A script that keeps its own `as_owner` says why on this line.
const OWN_COPY: &str = "# git-owner.sh not used:";

fn shell_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            shell_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "sh") {
            out.push(path);
        }
    }
}

/// The verb scripts, repo-relative, with their text.
fn verb_scripts() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in VERB_DIRS {
        shell_files(&root.join(dir), &mut files);
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let src = std::fs::read_to_string(&p).ok()?;
            let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
            Some((rel, src))
        })
        .collect()
}

/// The 1-based line on which a script defines a function named
/// `as_owner`, in any of bash's three spellings.
fn defines_as_owner(src: &str) -> Option<usize> {
    src.lines()
        .position(|l| {
            let t = l.trim_start();
            t.starts_with("as_owner()")
                || t.starts_with("as_owner ()")
                || t.starts_with("function as_owner")
        })
        .map(|i| i + 1)
}

/// The reason after the marker, when the script carries one.
fn own_copy_reason(src: &str) -> Option<&str> {
    src.lines()
        .find_map(|l| l.trim_start().strip_prefix(OWN_COPY))
        .map(str::trim)
}

/// A line that sources the lib, as `.` or `source`.
fn sources_the_lib(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim_start();
        (t.starts_with(". ") || t.starts_with("source ")) && t.contains("lib/git-owner.sh")
    })
}

/// A line that CALLS as_owner (a definition is not a call).
fn calls_as_owner(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim_start();
        !t.starts_with('#') && !t.starts_with("as_owner()") && l.contains("as_owner \"")
    })
}

#[test]
fn no_forge_or_gcp_verb_defines_its_own_as_owner_without_naming_why() {
    let scripts = verb_scripts();
    assert!(
        scripts.len() > 20,
        "read only {} scripts under {VERB_DIRS:?}: the walk found the wrong tree",
        scripts.len()
    );
    let mut offenders = Vec::new();
    for (rel, src) in &scripts {
        match (defines_as_owner(src), own_copy_reason(src)) {
            (Some(line), None) => offenders.push(format!(
                "{rel}:{line} defines its own as_owner — source {LIB} instead, or, if this copy \
                 differs on purpose, say why on a line `{OWN_COPY} <reason>`"
            )),
            (Some(line), Some(why)) if why.len() < 20 => offenders.push(format!(
                "{rel}:{line} keeps its own as_owner and its reason is {why:?} — a reason nobody \
                 can read is a skip"
            )),
            (None, Some(_)) => offenders.push(format!(
                "{rel} says `{OWN_COPY}` but defines no as_owner of its own — the exemption \
                 outlived the copy; delete the marker"
            )),
            _ => {}
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

#[test]
fn every_verb_that_calls_as_owner_sources_the_one_lib() {
    let mut users = 0;
    let mut offenders = Vec::new();
    for (rel, src) in verb_scripts() {
        if !calls_as_owner(&src) || defines_as_owner(&src).is_some() {
            continue;
        }
        users += 1;
        if !sources_the_lib(&src) {
            offenders.push(format!("{rel} calls as_owner and never sources {LIB}"));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
    // Seven verbs carried the byte-equal copy on 2026-11-14, and the two
    // retire verbs left it for a proved generation of root's tree (review
    // a04e64e9 P1); fewer users than five means the walk, not the tree,
    // changed.
    assert!(
        users >= 5,
        "only {users} verbs call as_owner through the lib — the walk found the wrong tree"
    );
}

// --- the lib, run --------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = git_config_isolated(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A committed checkout with a declaration directory and one outside it.
fn checkout(tag: &str) -> PathBuf {
    let tree = scratch_dir(&format!("git-owner-{tag}")).join("tree");
    create_dir(&tree.join("infra/cluster/manifests"));
    create_dir(&tree.join("docs"));
    write_file(&tree.join("infra/cluster/manifests/a.yaml"), "kind: A\n");
    write_file(&tree.join("docs/notes.md"), "notes\n");
    git(&tree, &["init", "-q", "-b", "main"]);
    git(&tree, &["add", "-A"]);
    git(&tree, &["commit", "-q", "-m", "seed"]);
    tree
}

/// Source the lib and run `body` with $TREE set; git's config channels
/// are closed so a host's `safe.directory` cannot answer for the lib.
fn run_lib(tree: &Path, body: &str, foreign_owner: bool) -> Output {
    let script = format!(". '{}'\n{body}\n", repo_root().join(LIB).display());
    let mut cmd = Command::new("bash");
    git_config_isolated(&mut cmd)
        .arg("-c")
        .arg(script)
        .env("TREE", tree)
        .env_remove("GIT_TEST_ASSUME_DIFFERENT_OWNER");
    if foreign_owner {
        cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
    }
    cmd.output().expect("bash runs")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn id(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("id runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn git_owner_of_reads_the_owner_off_the_directory() {
    let tree = checkout("owner");
    let out = run_lib(
        &tree,
        r#"git_owner_of "$TREE" && printf '%s|%s\n' "$OWNER" "$OWNER_UID""#,
        false,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out).trim(), format!("{}|{}", id("-un"), id("-u")));
}

#[test]
fn git_owner_of_a_missing_directory_resolves_nobody() {
    let tree = checkout("missing").join("not-there");
    let out = run_lib(
        &tree,
        r#"git_owner_of "$TREE"; echo "rc=$? owner=${OWNER:-}""#,
        false,
    );
    assert_eq!(stdout(&out).trim(), "rc=1 owner=", "{out:?}");
}

#[test]
fn as_owner_runs_the_command_string_and_keeps_its_exit() {
    let tree = checkout("run");
    let out = run_lib(
        &tree,
        r#"git_owner_of "$TREE" || exit 9
as_owner "git -C '$TREE' rev-parse --abbrev-ref HEAD; exit 3"; echo "rc=$?""#,
        false,
    );
    assert_eq!(stdout(&out), "main\nrc=3\n", "{out:?}");
}

/// The case the drop exists for: a checkout git says another account
/// owns. Read as the wrong account, git refuses, and as_owner carries
/// that refusal out as a nonzero status with git's words — never an
/// empty answer a caller could read as a clean one.
#[test]
fn a_checkout_git_will_not_read_fails_loud_through_as_owner() {
    let tree = checkout("foreign");
    let out = run_lib(
        &tree,
        r#"git_owner_of "$TREE" || exit 9
as_owner "git -C '$TREE' status --porcelain"; echo "rc=$?""#,
        true,
    );
    assert_ne!(stdout(&out).trim(), "rc=0", "{out:?}");
    assert!(stderr(&out).contains("dubious ownership"), "{out:?}");
}

/// The lib is a way to READ a checkout, never deletion authority (review
/// a04e64e9 P1). Its cleanliness check left with its last caller, and a
/// verb that derives what root may delete proves a generation of root's
/// tree instead; a check of that shape coming back here is the defect
/// returning under a new name.
#[test]
fn the_lib_offers_no_cleanliness_check_to_mistake_for_proof() {
    let lib = std::fs::read_to_string(repo_root().join(LIB)).expect("the lib reads");
    let code: Vec<&str> = lib
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let offenders: Vec<&&str> = code
        .iter()
        .filter(|l| l.contains("status --porcelain") || l.contains("git_owner_clean"))
        .collect();
    assert!(
        offenders.is_empty(),
        "{LIB} answers whether a checkout is clean again — that is the owner's testimony, \
         not proof; derive deletion authority through infra/lib/root-generation.sh:\n{offenders:?}"
    );
}
