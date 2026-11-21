//! A run releases its worktree when it departs, and says so on the run
//! (backlog f8d2bffc; car 1 of design 777016f1, approved 2026-11-09).
//!
//! THE RULE (docs/architecture-decisions.md, "Every remnant leaves with
//! its owner"). A remnant is an allocation owned by the station that
//! made it, released when the job departs, and the release is recorded
//! on the job. A worktree is the build station's: the run recorded it
//! (`worker_session.local_root` from `boss dispatch --started`, then
//! `gate_run.worktree` from its gate), and nothing else ever let it go.
//!
//! WHY. Measured 2026-11-09 in /work/boss: 731 git worktrees, 617 of
//! them LOCKED — 317 by the launchers' own `git worktree add --lock`,
//! which writes git's sentence `added with --lock` and nothing else,
//! and the rest mostly by the Claude harness for a subagent, naming the
//! PARENT session's pid, which lives for days, so the lock never reads
//! stale. /work was at 80%. The one cleaner, the reclaim sidecar, had
//! not finished a pass since 2026-11-08 (e71557f4), and a hand sweep
//! could take only 10: the rest were locked, dirty, or held a
//! pre-rebase head the gate had replaced.
//!
//! WHEN. At `boss dispatch --report` once the run has DEPARTED — its
//! `building` (or, for a run handed to its gate, its `verdict`) closed
//! `gated`, `delivered`, `refused` or `died`. The report is the run's
//! own word that it is done with the tree, and a departed run builds
//! nothing more in it: a red restarts as a fresh run (87d1ae40). Not at
//! the hand-off itself, which the design's table also names: the
//! builder is still standing in the tree then — `boss gate --wait` was
//! exec'd there and the report follows it — and a run handed and
//! awaiting its verdict has not departed, so its report keeps the tree
//! and says why. A green nobody reported after is the orphan
//! reclaimer's (car 3 of the design).
//!
//! WHAT IT NEVER DOES: discard work. It keeps, and names why on the
//! run, a tree that: is not a linked worktree of the repo (the main
//! checkout is never released); has another branch checked out than
//! the one its gate judged (another car's now); holds any uncommitted
//! or untracked path (untracked enumeration is explicit, overriding
//! user configuration, also on `git worktree remove`); names no branch
//! or gated sha; has a head on no forge branch
//! that is not the gated sha and whose patches the gated sha does not
//! carry (`git cherry` only after excluding unmatched merges, for the
//! pre-rebase head `--rebase` leaves behind); has a build running in it now (wt-cargo's target lock, held
//! through the removal); or is locked by anyone but the tooling — git's
//! own `added with --lock`. Harness locks belong to a session across
//! runs and are always kept; one departed run cannot release them.
//!
//! WHAT IT RECORDS. `worktree_release` on the run: the path, the bytes
//! `du` counted in it, and why it was finished — or why it was kept, or
//! the refusal. A refusal is printed as a WARNING too, because a remnant
//! that would not go is a red step someone reads, not a restart count.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The key on the run that records what happened to its worktree.
pub(crate) const KEY: &str = "worktree_release";
/// git's own reason for `git worktree add --lock` given none — what
/// `boss dispatch --launch` and the review tooling write.
const GIT_ADD_LOCK: &str = "added with --lock";

/// The tree a run built in, as its record names it, and what its gate
/// judged there.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Owned {
    pub worktree: PathBuf,
    /// The branch its gate judged, when it had one.
    pub branch: Option<String>,
    /// The sha its gate judged, when it had one.
    pub gated: Option<String>,
}

fn step<'a>(run: &'a Value, slug: &str) -> Option<&'a Value> {
    crate::envelope::steps(run)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The run's tree: the worktree its gate stamped (`gate_run`, which the
/// landing rule writes on `verdict` for a run handed to its gate and on
/// `building` otherwise), else the root its worker was bound from
/// (`worker_session.local_root` on `building`). `None` when the record
/// names neither — nothing of its own to release.
pub(crate) fn owned(run: &Value) -> Option<Owned> {
    let gate = [
        boss_jobs::gate_handoff::VERDICT_SLUG,
        crate::dispatch::BUILDING_SLUG,
    ]
    .into_iter()
    .filter_map(|slug| step(run, slug)?.pointer("/metadata/gate_run"))
    .find(|g| text(g.get("worktree")).is_some());
    let bound = step(run, crate::dispatch::BUILDING_SLUG)
        .and_then(|b| text(b.pointer("/metadata/worker_session/local_root")));
    let worktree = gate.and_then(|g| text(g.get("worktree"))).or(bound)?;
    Some(Owned {
        worktree: PathBuf::from(worktree),
        branch: gate
            .and_then(|g| text(g.get("branch")))
            .or_else(|| crate::dispatch::run_branch(run)),
        gated: gate.and_then(|g| text(g.get("sha"))),
    })
}

/// How the run departed — the result its `building` (or `verdict`)
/// closed with — or `None` while it is building or awaiting a verdict.
pub(crate) fn departed(run: &Value) -> Option<&str> {
    crate::dispatch::building_result(run)
        .filter(|r| matches!(*r, "gated" | "delivered" | "refused" | "died"))
}

/// What releasing did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Released {
    /// Unlocked if the tooling had locked it, then removed by git.
    Removed { bytes: u64, accounted: String },
    /// Nothing at that path.
    Absent,
    /// A guard kept it; the reason names the work it protects.
    Kept(String),
    /// git would not do it, or the tree could not be read.
    Refused(String),
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git could not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// One block of `git worktree list --porcelain`.
#[derive(Debug, Default)]
struct Listed {
    path: PathBuf,
    branch: Option<String>,
    /// `Some("")` for a lock with no reason.
    locked: Option<String>,
}

fn listed(porcelain: &str) -> Vec<Listed> {
    porcelain
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let mut l = Listed::default();
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    l.path = PathBuf::from(p);
                } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                    l.branch = Some(b.to_string());
                } else if line == "locked" {
                    l.locked = Some(String::new());
                } else if let Some(r) = line.strip_prefix("locked ") {
                    l.locked = Some(r.to_string());
                }
            }
            l
        })
        .collect()
}

/// Only git's `add --lock` sentence is ours to release. A harness lock
/// names a parent session that may still execute other runs in its tree.
fn tooling_lock(reason: &str) -> bool {
    reason == GIT_ADD_LOCK
}

/// Is the tree's head accounted for — the gated sha, on a forge branch,
/// or carried by the gated sha as a replay? The reason, or `None`.
fn accounted(tree: &Path, head: &str, gated: Option<&str>) -> Option<String> {
    let short = |s: &str| s.chars().take(8).collect::<String>();
    if gated == Some(head) {
        return Some(format!(
            "its head {} is the sha its gate judged",
            short(head)
        ));
    }
    let on_forge = git(
        tree,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname:short)",
            "--contains",
            head,
            "refs/remotes/origin",
        ],
    )
    .ok()
    .filter(|r| !r.is_empty());
    if let Some(r) = on_forge {
        return Some(format!("its head {} is on the forge in {r}", short(head)));
    }
    let gated = gated?;
    // Patch ids omit merge commits and therefore their unique conflict
    // resolutions. An unmatched merge is not evidence of preservation.
    if !git(tree, &["rev-list", "--merges", &format!("{gated}..{head}")])
        .ok()?
        .is_empty()
    {
        return None;
    }
    let cherry = git(tree, &["cherry", gated, head]).ok()?;
    cherry.lines().all(|l| !l.starts_with('+')).then(|| {
        format!(
            "its head {} was replayed by its gate as {}, which carries every one of its \
                 patches",
            short(head),
            short(gated)
        )
    })
}

/// Release `owned`'s tree if every guard holds. `target` is the cargo
/// target wt-cargo builds this tree into; while a build holds its lock
/// the tree stays. Blocking: it runs git and walks the tree for its size.
pub(crate) fn release(owned: &Owned, target: Option<&Path>) -> Released {
    let tree = &owned.worktree;
    match std::fs::symlink_metadata(tree) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Released::Absent,
        Err(e) => return Released::Refused(format!("it could not be read: {e}")),
        Ok(m) if !m.is_dir() => {
            return Released::Refused("it is not a directory".into());
        }
        Ok(_) => {}
    }
    let real = match tree.canonicalize() {
        Ok(r) => r,
        Err(e) => return Released::Refused(format!("it does not resolve: {e}")),
    };
    let blocks = match git(&real, &["worktree", "list", "--porcelain"]) {
        Ok(p) => listed(&p),
        Err(why) => return Released::Refused(format!("it is not a git worktree: {why}")),
    };
    let same = |p: &Path| p.canonicalize().is_ok_and(|c| c == real);
    let Some(main) = blocks.first().map(|b| b.path.clone()) else {
        return Released::Refused("git listed no worktrees".into());
    };
    let Some(mine) = blocks.iter().skip(1).find(|b| same(&b.path)) else {
        return Released::Kept(format!(
            "it is not a linked worktree of {} — a main checkout is never released",
            main.display()
        ));
    };
    if owned.branch.is_none() && owned.gated.is_none() {
        return Released::Kept("its run names neither a branch nor a gated sha — a worker binding alone does not make a session tree disposable".into());
    }
    if let Some(want) = owned.branch.as_deref()
        && mine.branch.as_deref() != Some(want)
    {
        return Released::Kept(format!(
            "it has {} checked out, not {want}, the branch its run built — another car's now",
            mine.branch.as_deref().unwrap_or("a detached head")
        ));
    }
    match git(
        &real,
        &[
            "-c",
            "status.showUntrackedFiles=all",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    ) {
        Err(why) => return Released::Refused(why),
        Ok(s) if !s.is_empty() => {
            let paths: Vec<&str> = s.lines().map(|l| l.get(3..).unwrap_or(l)).collect();
            return Released::Kept(format!(
                "it holds {} uncommitted or untracked path(s): {}{}",
                paths.len(),
                paths.iter().take(5).copied().collect::<Vec<_>>().join(", "),
                if paths.len() > 5 { ", …" } else { "" }
            ));
        }
        Ok(_) => {}
    }
    let head = match git(&real, &["rev-parse", "HEAD"]) {
        Ok(h) => h,
        Err(why) => return Released::Refused(why),
    };
    let Some(accounted) = accounted(&real, &head, owned.gated.as_deref()) else {
        return Released::Kept(format!(
            "its head {head} is on no forge branch, is not the sha its gate judged and is not \
             carried by it — work that exists only here"
        ));
    };
    // A BUILD IN THIS TREE NOW — another car of the run, or another run
    // reusing the tree (backlog 94cd0c23, 0c18deed) — compiles clean and
    // pushed, so no guard above sees it; wt-cargo's lock does. Taken
    // EXCLUSIVE and held through the removal, as the target's own free
    // takes it; a lock file that was never made means no build ever ran.
    let _idle = match target.filter(|t| crate::scratch_target::lock_path(t).exists()) {
        None => None,
        Some(t) => match crate::scratch_target::hold_idle(t) {
            Ok(lock) => Some(lock),
            Err(
                crate::scratch_target::Freed::Busy(why)
                | crate::scratch_target::Freed::Refused(why),
            ) => return Released::Kept(why),
            Err(other) => return Released::Kept(format!("{other:?}")),
        },
    };
    let unlock = match &mine.locked {
        None => None,
        Some(reason) if tooling_lock(reason) => Some(reason.clone()),
        Some(reason) => {
            return Released::Kept(format!(
                "it is locked by someone other than its tooling ({:?}) — their lock, kept",
                reason
            ));
        }
    };
    let text = real.display().to_string();
    let bytes = crate::scratch_target::du_bytes(&real);
    if unlock.is_some()
        && let Err(why) = git(&main, &["worktree", "unlock", &text])
    {
        return Released::Refused(why);
    }
    if let Err(why) = git(
        &main,
        &[
            "-c",
            "status.showUntrackedFiles=all",
            "worktree",
            "remove",
            &text,
        ],
    ) {
        // Put back exactly the lock it had, so a refused release leaves
        // the tree as it found it.
        let relocked = match &unlock {
            Some(reason) => git(&main, &["worktree", "lock", "--reason", reason, &text])
                .err()
                .map_or(String::new(), |e| {
                    format!("; and it could NOT be relocked: {e}")
                }),
            None => String::new(),
        };
        return Released::Refused(format!("{why}{relocked}"));
    }
    Released::Removed { bytes, accounted }
}

/// The report's settlement: the line to print after `boss dispatch: run
/// <short>`, and the record for the run — `None` when the run names no
/// tree or an earlier report already released it, whose record stands.
pub(crate) fn settle(
    run: &Value,
    scratch_root: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Option<Value>) {
    let Some(owned) = owned(run) else {
        return (
            "names no worktree on its record — no tree of its own to release".into(),
            None,
        );
    };
    let path = owned.worktree.display().to_string();
    if run
        .pointer(&format!("/metadata/{KEY}/released_bytes"))
        .is_some()
    {
        return (
            format!("already released its worktree {path} — the record on the run stands"),
            None,
        );
    }
    let mut rec = json!({
        "worktree": path,
        "branch": owned.branch,
        "at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "by": crate::scratch_target::REPORT_BY,
    });
    let Some(result) = departed(run) else {
        let why = "the run has not departed — it is still building, or handed to its gate and \
                   awaiting the verdict — so its tree stays; a later --report releases it";
        rec["kept"] = json!(why);
        return (format!("kept its worktree {path}: {why}"), Some(rec));
    };
    rec["departed"] = json!(result);
    let target = crate::scratch_target::target_dir(scratch_root, &owned.worktree);
    let line = match release(&owned, target.as_deref()) {
        Released::Removed { bytes, accounted } => {
            rec["released_bytes"] = json!(bytes);
            rec["accounted"] = json!(accounted);
            format!(
                "released its worktree {path} — {bytes} bytes as du counts them; the run ended \
                 {result} and {accounted}"
            )
        }
        Released::Absent => {
            rec["absent"] = json!(true);
            format!("had no worktree at {path} to release — nothing there")
        }
        Released::Kept(why) => {
            rec["kept"] = json!(why);
            format!("kept its worktree {path}: {why}")
        }
        Released::Refused(why) => {
            rec["refused"] = json!(why);
            format!("WARNING — did NOT release its worktree {path}: {why}")
        }
    };
    (line, Some(rec))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A forge (bare), a clone of it, and a builder worktree on `fix/x`
    /// whose one commit is pushed — the shape a gated builder leaves.
    pub(crate) struct Fixture {
        pub(crate) repo: PathBuf,
        pub(crate) tree: PathBuf,
        pub(crate) head: String,
    }

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=builder@example.invalid",
                "-c",
                "user.name=builder",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub(crate) fn fixture(name: &str, lock: Option<&str>) -> Fixture {
        let root = boss_testing::scratch_dir(&format!("worktree-release-{name}"));
        let forge = root.join("forge.git");
        let repo = root.join("repo");
        run_git(&root, &["init", "-q", "--bare", forge.to_str().unwrap()]);
        run_git(&root, &["init", "-q", repo.to_str().unwrap()]);
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        run_git(&repo, &["add", ".gitignore"]);
        run_git(&repo, &["commit", "-q", "-m", "base"]);
        run_git(&repo, &["remote", "add", "origin", forge.to_str().unwrap()]);
        run_git(&repo, &["push", "-q", "origin", "HEAD:main"]);
        run_git(&repo, &["fetch", "-q", "origin"]);
        // agent-* like a harness tree, so the harness lock names it.
        let tree = root.join("agent-0123abcd");
        let tree_text = tree.to_str().unwrap();
        // `Some("git")` is `add --lock` with no reason (git's sentence),
        // `Some("")` a bare `git worktree lock`, any other a named lock.
        let mut add = vec![
            "worktree",
            "add",
            "-q",
            "-b",
            "fix/x",
            tree_text,
            "origin/main",
        ];
        if lock == Some("git") {
            add.insert(3, "--lock");
        }
        run_git(&repo, &add);
        match lock {
            None | Some("git") => {}
            Some("") => {
                run_git(&repo, &["worktree", "lock", tree_text]);
            }
            Some(reason) => {
                run_git(&repo, &["worktree", "lock", "--reason", reason, tree_text]);
            }
        }
        std::fs::write(tree.join("fix.rs"), "fn fixed() {}\n").unwrap();
        run_git(&tree, &["add", "fix.rs"]);
        run_git(&tree, &["commit", "-q", "-m", "fix"]);
        run_git(&tree, &["push", "-q", "origin", "HEAD:fix/x"]);
        run_git(&tree, &["fetch", "-q", "origin"]);
        // Build output the release must not count as work.
        std::fs::create_dir_all(tree.join("target/debug")).unwrap();
        std::fs::write(tree.join("target/debug/out"), vec![0u8; 16 * 1024]).unwrap();
        let head = run_git(&tree, &["rev-parse", "HEAD"]);
        Fixture { repo, tree, head }
    }

    fn owned_by(f: &Fixture, gated: Option<&str>) -> Owned {
        Owned {
            worktree: f.tree.clone(),
            branch: Some("fix/x".into()),
            gated: gated.map(str::to_string),
        }
    }

    fn registered(f: &Fixture) -> bool {
        run_git(&f.repo, &["worktree", "list", "--porcelain"])
            .contains(f.tree.file_name().unwrap().to_str().unwrap())
    }

    /// The pile's own shape: a clean tree, its head on the forge, locked
    /// by the tooling with git's `added with --lock` — released, the
    /// lock and git's entry gone, and its bytes counted, build output
    /// included.
    #[test]
    fn a_departed_clean_pushed_tree_locked_by_its_tooling_is_released() {
        let f = fixture("git-lock", Some("git"));
        match release(&owned_by(&f, None), None) {
            Released::Removed { bytes, accounted } => {
                assert!(bytes >= 16 * 1024, "{bytes}");
                assert!(accounted.contains("on the forge"), "{accounted}");
            }
            other => panic!("released, not {other:?}"),
        }
        assert!(!f.tree.exists(), "the tree is gone");
        assert!(!registered(&f), "git no longer lists it");
    }

    /// A harness owns its tree across several runs: a run report must
    /// never unlock that session's still-live working directory.
    #[test]
    fn a_live_harness_lock_keeps_its_tree() {
        let reason = format!(
            "claude agent agent-0123abcd (pid {} start 1)",
            std::process::id()
        );
        let f = fixture("harness", Some(&reason));
        assert!(
            matches!(release(&owned_by(&f, None), None), Released::Kept(_)),
            "the harness lock is not this run's to release"
        );
        assert!(f.tree.exists() && registered(&f));
        run_git(&f.tree, &["checkout", "--detach", "-q"]);
        let unbound = Owned {
            worktree: f.tree.clone(),
            branch: None,
            gated: None,
        };
        assert!(matches!(release(&unbound, None), Released::Kept(_)));
        assert!(f.tree.exists() && registered(&f));
    }

    #[test]
    fn a_run_without_a_branch_or_gated_sha_keeps_its_bound_tree() {
        let f = fixture("unbound", Some("git"));
        let unbound = Owned {
            worktree: f.tree.clone(),
            branch: None,
            gated: None,
        };
        assert!(matches!(release(&unbound, None), Released::Kept(_)));
        assert!(f.tree.exists() && registered(&f));
    }

    #[test]
    fn hidden_untracked_source_is_kept_and_named() {
        let f = fixture("hidden-untracked", Some("git"));
        run_git(&f.tree, &["config", "status.showUntrackedFiles", "no"]);
        std::fs::write(f.tree.join("only_copy.rs"), "fn unfinished() {}\n").unwrap();
        assert!(run_git(&f.tree, &["status", "--porcelain"]).is_empty());
        match release(&owned_by(&f, None), None) {
            Released::Kept(why) => assert!(why.contains("only_copy.rs"), "{why}"),
            other => panic!("untracked source must survive: {other:?}"),
        }
        assert!(f.tree.join("only_copy.rs").exists());
    }

    #[test]
    fn an_unpublished_unique_merge_resolution_is_kept() {
        let f = fixture("unique-merge", None);
        run_git(&f.tree, &["checkout", "-q", "-b", "side", "origin/main"]);
        std::fs::write(f.tree.join("side.rs"), "fn side() {}\n").unwrap();
        run_git(&f.tree, &["add", "side.rs"]);
        run_git(&f.tree, &["commit", "-q", "-m", "side"]);
        let side = run_git(&f.tree, &["rev-parse", "HEAD"]);
        run_git(&f.tree, &["checkout", "-q", "fix/x"]);
        std::fs::write(f.tree.join("resolution.rs"), "gated resolution\n").unwrap();
        run_git(&f.tree, &["add", "resolution.rs"]);
        let gated_tree = run_git(&f.tree, &["write-tree"]);
        let gated = run_git(
            &f.tree,
            &[
                "commit-tree",
                &gated_tree,
                "-p",
                &f.head,
                "-p",
                &side,
                "-m",
                "gated merge",
            ],
        );
        std::fs::write(f.tree.join("resolution.rs"), "UNPUSHED UNIQUE RESOLUTION\n").unwrap();
        run_git(&f.tree, &["add", "resolution.rs"]);
        let unique_tree = run_git(&f.tree, &["write-tree"]);
        let unique = run_git(
            &f.tree,
            &[
                "commit-tree",
                &unique_tree,
                "-p",
                &f.head,
                "-p",
                &side,
                "-m",
                "unique merge",
            ],
        );
        run_git(&f.tree, &["update-ref", "refs/heads/fix/x", &unique]);
        assert!(
            run_git(&f.tree, &["cherry", &gated, &unique]).is_empty(),
            "patch ids omit merge resolutions"
        );
        run_git(&f.tree, &["checkout", "--detach", "-q"]);
        let owned = Owned {
            worktree: f.tree.clone(),
            branch: None,
            gated: Some(gated),
        };
        assert!(matches!(release(&owned, None), Released::Kept(_)));
        assert_eq!(
            std::fs::read_to_string(f.tree.join("resolution.rs")).unwrap(),
            "UNPUSHED UNIQUE RESOLUTION\n"
        );
    }

    /// Anyone else's lock — a reason naming someone, or a bare `git
    /// worktree lock` — is theirs: kept, and the reason is quoted.
    #[test]
    fn a_lock_the_tooling_did_not_write_keeps_the_tree() {
        for (name, reason) in [("named", "coordinator recovery 7b13c000"), ("bare", "")] {
            let f = fixture(name, Some(reason));
            match release(&owned_by(&f, None), None) {
                Released::Kept(why) => assert!(why.contains("someone other"), "{why}"),
                other => panic!("{name}: kept, not {other:?}"),
            }
            assert!(f.tree.exists() && registered(&f), "{name}: still there");
        }
    }

    /// The pin the item asks for: a tree with an uncommitted source file
    /// is kept, and the file is named.
    #[test]
    fn a_tree_with_an_uncommitted_source_file_is_kept_and_named() {
        let f = fixture("dirty", Some("git"));
        std::fs::write(f.tree.join("half_done.rs"), "fn wip() {}\n").unwrap();
        match release(&owned_by(&f, None), None) {
            Released::Kept(why) => assert!(why.contains("half_done.rs"), "{why}"),
            other => panic!("kept, not {other:?}"),
        }
        assert!(f.tree.join("half_done.rs").exists());
    }

    /// A build compiling in the tree right now holds wt-cargo's lock
    /// SHARED; the tree stays while it does, and goes once it is idle.
    #[test]
    fn a_tree_a_build_holds_is_kept_until_the_build_lets_go() {
        let f = fixture("building", Some("git"));
        let target = f.repo.parent().unwrap().join("target-agent-0123abcd");
        let build = std::fs::File::create(crate::scratch_target::lock_path(&target)).unwrap();
        build.lock_shared().unwrap();
        match release(&owned_by(&f, None), Some(&target)) {
            Released::Kept(why) => assert!(why.contains("a build holds"), "{why}"),
            other => panic!("kept, not {other:?}"),
        }
        assert!(f.tree.exists());
        build.unlock().unwrap();
        assert!(matches!(
            release(&owned_by(&f, None), Some(&target)),
            Released::Removed { .. }
        ));
    }

    /// A commit no forge branch holds is work that exists only here.
    #[test]
    fn an_unpushed_head_is_kept() {
        let f = fixture("unpushed", None);
        std::fs::write(f.tree.join("more.rs"), "fn more() {}\n").unwrap();
        run_git(&f.tree, &["add", "more.rs"]);
        run_git(&f.tree, &["commit", "-q", "-m", "more"]);
        match release(&owned_by(&f, Some(&f.head)), None) {
            Released::Kept(why) => assert!(why.contains("exists only here"), "{why}"),
            other => panic!("kept, not {other:?}"),
        }
        assert!(f.tree.exists());
    }

    /// `boss gate --rebase` replays the car onto main and pushes the new
    /// head with a lease, leaving the builder's tree on the OLD head,
    /// which no forge branch holds any longer. The gated sha carries
    /// its patch, so the work is accounted for and the tree goes.
    #[test]
    fn a_pre_rebase_head_the_gated_replay_carries_is_released() {
        let f = fixture("replayed", Some("git"));
        let side = f.repo.parent().unwrap().join("side");
        run_git(
            &f.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                side.to_str().unwrap(),
                "origin/main",
            ],
        );
        std::fs::write(side.join("main.rs"), "fn landed() {}\n").unwrap();
        run_git(&side, &["add", "main.rs"]);
        run_git(&side, &["commit", "-q", "-m", "main moved"]);
        run_git(&side, &["push", "-q", "origin", "HEAD:main"]);
        run_git(&side, &["cherry-pick", &f.head]);
        let replayed = run_git(&side, &["rev-parse", "HEAD"]);
        run_git(&side, &["push", "-q", "--force", "origin", "HEAD:fix/x"]);
        run_git(&f.repo, &["fetch", "-q", "origin"]);
        run_git(&f.repo, &["worktree", "remove", side.to_str().unwrap()]);
        match release(&owned_by(&f, Some(&replayed)), None) {
            Released::Removed { accounted, .. } => {
                assert!(accounted.contains("replayed"), "{accounted}")
            }
            other => panic!("released, not {other:?}"),
        }
        assert!(!f.tree.exists());
    }

    /// A green is one car's, not the tree's: a tree that has moved on to
    /// another branch is another car's work and stays.
    #[test]
    fn a_tree_now_on_another_branch_is_kept() {
        let f = fixture("moved-on", None);
        run_git(&f.tree, &["checkout", "-q", "-b", "fix/next"]);
        match release(&owned_by(&f, None), None) {
            Released::Kept(why) => assert!(why.contains("fix/next"), "{why}"),
            other => panic!("kept, not {other:?}"),
        }
    }

    /// The main checkout is never a run's to release, whatever the
    /// record says.
    #[test]
    fn the_main_checkout_is_never_released() {
        let f = fixture("main", None);
        let owned = Owned {
            worktree: f.repo.clone(),
            branch: None,
            gated: None,
        };
        assert!(matches!(release(&owned, None), Released::Kept(_)));
        assert!(f.repo.join(".git").exists());
    }

    #[test]
    fn a_tree_already_gone_is_absent() {
        let root = boss_testing::scratch_dir("worktree-release-absent");
        let owned = Owned {
            worktree: root.join("agent-gone"),
            branch: None,
            gated: None,
        };
        assert_eq!(release(&owned, None), Released::Absent);
    }

    fn run_with(building: Value, verdict: Option<Value>) -> Value {
        let mut steps = vec![
            json!({"spec_slug": "building", "status": building["status"],
                                    "metadata": building["metadata"]}),
        ];
        if let Some(v) = verdict {
            steps.push(json!({"spec_slug": "verdict", "status": v["status"],
                              "metadata": v["metadata"]}));
        }
        json!({ "id": "67404869-3819-44db-8d2a-4f684ea59451", "metadata": {}, "steps": steps })
    }

    /// The record names the tree: the gate's stamp on `verdict` for a
    /// run handed to its gate (where the landing rule writes it), else
    /// on `building`, else the root its worker was bound from.
    #[test]
    fn the_tree_is_read_off_the_gate_stamp_then_the_worker_root() {
        let bound = json!({"worker_session": {"local_root": "/work/wt/agent-bound"}});
        let run = run_with(json!({"status": "active", "metadata": bound}), None);
        assert_eq!(
            owned(&run).map(|o| o.worktree),
            Some(PathBuf::from("/work/wt/agent-bound"))
        );
        let mut handed = bound.clone();
        handed["result"] = json!("handed");
        let gated = json!({"result": "gated", "gate_run": {
            "worktree": "/work/wt/agent-gated", "branch": "fix/x", "sha": "abc"}});
        let run = run_with(
            json!({"status": "completed", "metadata": handed}),
            Some(json!({"status": "completed", "metadata": gated})),
        );
        assert_eq!(
            owned(&run),
            Some(Owned {
                worktree: PathBuf::from("/work/wt/agent-gated"),
                branch: Some("fix/x".into()),
                gated: Some("abc".into()),
            })
        );
        assert_eq!(departed(&run), Some("gated"));
        assert_eq!(
            owned(&run_with(json!({"status": "active", "metadata": {}}), None)),
            None
        );
    }

    /// A run handed to its gate and awaiting the verdict has not
    /// departed: its tree stays, and the record says why.
    #[test]
    fn a_run_awaiting_its_verdict_keeps_its_tree_and_says_so() {
        let f = fixture("awaiting", Some("git"));
        let run = run_with(
            json!({"status": "completed", "metadata": {"result": "handed",
                   "worker_session": {"local_root": f.tree.to_str().unwrap()}}}),
            Some(json!({"status": "ready", "metadata": {}})),
        );
        let (line, rec) = settle(&run, &f.repo, "2026-11-09T18:00:00Z".parse().unwrap());
        assert!(line.contains("kept"), "{line}");
        assert!(rec.expect("recorded")["kept"].is_string());
        assert!(f.tree.exists(), "an open run's tree is never touched");
    }

    /// The other pin the item asks for: a closed run's tree is gone after
    /// one report, and the bytes ride the run.
    #[test]
    fn a_closed_run_s_tree_is_gone_after_its_report_and_the_bytes_are_recorded() {
        for result in ["refused", "died"] {
            let f = fixture(&format!("closed-{result}"), Some("git"));
            let run = run_with(
                json!({"status": "completed", "metadata": {"result": result,
                       "gate_run": {"worktree": f.tree.to_str().unwrap(), "branch": "fix/x"},
                       "worker_session": {"local_root": f.tree.to_str().unwrap()}}}),
                None,
            );
            let (line, rec) = settle(&run, &f.repo, "2026-11-09T18:00:00Z".parse().unwrap());
            let rec = rec.expect("recorded");
            assert!(line.contains("released"), "{line}");
            assert!(
                rec["released_bytes"].as_u64().unwrap_or(0) >= 16 * 1024,
                "{rec}"
            );
            assert_eq!(rec["departed"], result);
            assert!(!f.tree.exists(), "{result}: gone after one pass");
        }
    }
}
