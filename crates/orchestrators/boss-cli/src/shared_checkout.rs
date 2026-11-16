//! A run does not gate from the shared checkout (backlog 916fe290).
//!
//! THE SHARED CHECKOUT is the main checkout every linked worktree hangs
//! off — on the dev pod `/work/boss`, the tree every door under
//! `/work/tools/bin` resolves into. A dispatched builder works in a
//! worktree of its own and never here.
//!
//! MEASURED 2026-10-09, four times across four builder runs (f6a9b90f,
//! 503689a2, 3c758be6, b9bf68e4), and once more on purpose by run
//! 171055b8 while building this: in an isolated-worktree agent each Bash
//! call starts with its cwd in the DISPATCHING session's checkout, not
//! the agent's worktree, and the harness DROPS a leading `cd <worktree>
//! &&` from the command text before bash sees it — the `bash -c` line it
//! ran held the rest of the command and no cd, and `pwd` printed
//! `/work/boss`. `cd <worktree>;`, `cd <worktree>/sub &&`, and a script
//! that cds first all took effect. So a builder that wrote the cd the
//! natural way ran cargo, git and once a relative-path `sed` in the
//! shared checkout: the third switched it off main for four minutes, the
//! fourth edited two source files there.
//!
//! The mistake was SILENT — every command answered, from the wrong tree.
//! This makes it loud at the doors a builder cannot avoid: `boss gate`
//! here and `infra/dev/wt-cargo` in the shell, each refusing when
//! `BOSS_AGENT_RUN` names a run and the checkout is the shared one.
//! Shared is judged by SHAPE, not by a path typed here: git's `--git-dir`
//! equals its `--git-common-dir` (a main checkout, not a linked worktree)
//! AND that checkout has linked worktrees. A gate runner's fresh clone
//! has none, a dispatched adapter's `git worktree add` workspace is
//! linked, and an operator gating `/work/boss` by hand names no run —
//! none of the three is refused.

use std::path::{Path, PathBuf};

/// What git says about the checkout a verb runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkout {
    pub(crate) toplevel: PathBuf,
    /// `--git-dir` and `--git-common-dir`, absolute: equal in a main
    /// checkout, different in a linked worktree.
    pub(crate) git_dir: PathBuf,
    pub(crate) common_dir: PathBuf,
    /// The linked worktrees git has registered under the common dir.
    pub(crate) linked: usize,
}

impl Checkout {
    /// A main checkout that worktrees hang off.
    pub(crate) fn is_shared(&self) -> bool {
        self.git_dir == self.common_dir && self.linked > 0
    }

    /// The top of the main checkout this one belongs to — itself when it
    /// IS the main checkout, else the tree whose `.git` is the common
    /// dir (on the dev pod `/work/boss`, from any builder's worktree).
    /// A common dir not named `.git` has no work tree of its own to
    /// name, so the answer stays this checkout's own top (backlog
    /// 6d2e2653: the held brief names main's door by this path).
    pub(crate) fn main_toplevel(&self) -> PathBuf {
        match self.common_dir.parent() {
            Some(main)
                if self.git_dir != self.common_dir
                    && self.common_dir.file_name() == Some(".git".as_ref()) =>
            {
                main.to_path_buf()
            }
            _ => self.toplevel.clone(),
        }
    }
}

/// The checkout `dir` is in, as git reports it — `None` when git cannot
/// say (no git, not a repository), which refuses nothing: a door that
/// refuses because it cannot SEE is worse than the miss it prevents.
pub(crate) fn read(dir: &Path) -> Option<Checkout> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from);
    let (toplevel, git_dir, common_dir) = (lines.next()?, lines.next()?, lines.next()?);
    let linked = std::fs::read_dir(common_dir.join("worktrees"))
        .map(|d| d.count())
        .unwrap_or(0);
    Some(Checkout {
        toplevel,
        git_dir,
        common_dir,
        linked,
    })
}

/// The refusal `verb` gives when `run` (the `BOSS_AGENT_RUN` export)
/// names a run and the checkout is the shared one; `None` otherwise.
pub(crate) fn refusal(
    verb: &str,
    run: Option<&str>,
    checkout: Option<&Checkout>,
) -> Option<String> {
    let run = run.map(str::trim).filter(|r| !r.is_empty())?;
    let c = checkout.filter(|c| c.is_shared())?;
    let top = c.toplevel.display();
    Some(format!(
        "{verb}: REFUSED — BOSS_AGENT_RUN={run} names an agent run, and this shell is in \
         {top}, the SHARED checkout: the main checkout {linked} worktree(s) hang off and \
         every door on this host runs from, not a worktree of the run's own (backlog \
         916fe290). An isolated-worktree agent's Bash call starts here, and its harness \
         drops a leading `cd <worktree> &&` from the command text, so a cd written that \
         way never ran. Run this from a script you wrote whose first lines are `cd <your \
         worktree> || exit 1` and `pwd`, by absolute path, and read the pwd in its log; \
         `git -C {top} worktree list` names every worktree. An operator working in this \
         checkout on purpose unsets BOSS_AGENT_RUN.",
        linked = c.linked,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const RUN: &str = "171055b8-1a59-4399-9ba1-9fc460cb1c66";

    fn shape(main: bool, linked: usize) -> Checkout {
        let common = PathBuf::from("/work/boss/.git");
        Checkout {
            toplevel: PathBuf::from(if main {
                "/work/boss"
            } else {
                "/work/boss/.claude/worktrees/agent-a9"
            }),
            git_dir: if main {
                common.clone()
            } else {
                common.join("worktrees/agent-a9")
            },
            common_dir: common,
            linked,
        }
    }

    /// The case the four builders of 2026-10-09 were in: a run's
    /// command, in the main checkout worktrees hang off.
    #[test]
    fn a_run_in_the_shared_checkout_is_refused_naming_the_fix() {
        let why = refusal("boss gate", Some(RUN), Some(&shape(true, 3)))
            .expect("a run in the shared checkout is refused");
        for needle in [
            "boss gate: REFUSED",
            RUN,
            "/work/boss",
            "cd <your worktree> || exit 1",
            "pwd",
            "git -C /work/boss worktree list",
            "unsets BOSS_AGENT_RUN",
            "916fe290",
        ] {
            assert!(why.contains(needle), "the refusal names {needle:?}: {why}");
        }
    }

    /// THE CONTROLS, each a legitimate caller: the run's own worktree,
    /// the operator's hand gate, a clone nothing hangs off (the gate
    /// runner's), a blank export, and a checkout git could not read.
    #[test]
    fn everything_but_a_run_in_the_shared_checkout_passes() {
        assert_eq!(
            refusal("boss gate", Some(RUN), Some(&shape(false, 3))),
            None
        );
        assert_eq!(refusal("boss gate", None, Some(&shape(true, 3))), None);
        assert_eq!(
            refusal("boss gate", Some("  "), Some(&shape(true, 3))),
            None
        );
        assert_eq!(refusal("boss gate", Some(RUN), Some(&shape(true, 0))), None);
        assert_eq!(refusal("boss gate", Some(RUN), None), None);
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The shape is READ from real git, not assumed: a lone clone is not
    /// shared, the same clone once a worktree hangs off it is, and the
    /// worktree itself never is — from its top or from a subdirectory.
    #[test]
    fn the_shape_is_read_from_git_itself() {
        let root = boss_testing::scratch_dir("cli-shared-checkout");
        let main = root.join("main");
        boss_testing::create_dir(&main);
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let alone = read(&main).expect("git reads a lone clone");
        assert!(!alone.is_shared(), "a clone nothing hangs off: {alone:?}");

        let wt = root.join("agent-a9");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().expect("utf-8"),
            ],
        );
        let shared = read(&main).expect("git reads the main checkout");
        assert!(shared.is_shared(), "worktrees hang off it now: {shared:?}");
        assert_eq!(shared.linked, 1);

        let sub = main.join("sub");
        boss_testing::create_dir(&sub);
        assert!(
            read(&sub).expect("git reads a subdirectory").is_shared(),
            "a subdirectory of the shared checkout is still in it"
        );
        let own = read(&wt).expect("git reads the worktree");
        assert!(!own.is_shared(), "the run's own worktree: {own:?}");
        assert!(refusal("boss gate", Some(RUN), Some(&own)).is_none());
        assert!(refusal("boss gate", Some(RUN), Some(&shared)).is_some());
        assert!(read(&root).is_none(), "not a repository refuses nothing");

        // THE MAIN CHECKOUT, from anywhere in the family (backlog
        // 6d2e2653): the held brief names main's door by this path, so a
        // worktree must answer the checkout it hangs off, not itself.
        let canon = |p: &Path| p.canonicalize().expect("the path exists");
        for from in [&main, &sub, &wt] {
            assert_eq!(
                canon(&read(from).expect("git reads it").main_toplevel()),
                canon(&main),
                "from {}",
                from.display()
            );
        }
    }
}
