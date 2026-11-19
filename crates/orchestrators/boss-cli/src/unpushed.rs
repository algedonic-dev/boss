//! UNPUSHED WORK — the `boss orient` section that names commits sitting
//! in an agent worktree with nothing in the record pointing at them
//! (backlog aa4452ff).
//!
//! WHY. On 2026-09-24 commit f2dc6683 sat on a local branch in a locked
//! agent worktree for five hours after its builder died in a pod roll:
//! no forge branch, no gate-run, no car. It was found only because the
//! next builder on the same packet happened to look. Orient already
//! reads the record's strands (STRANDED, ABANDONED, ORPHANS, RESIDUE),
//! and every one of them starts from a packet or a forge head — work
//! that never reached either is invisible to all four.
//!
//! WHAT COUNTS. A linked worktree whose directory is named `agent-*`
//! (every launcher's spelling: /work/wt, .claude/worktrees, /scratch,
//! /tmp/run-*), on a branch that has commits not on origin/main, that no
//! forge head carries, and that no packet orient read claims (cars, the
//! week's gate-runs, held and open ones — the ORPHANS set). The naive
//! "ahead of origin/main" test is noise: trains squash, so every landed
//! branch is still "ahead" — measured 2026-11-15, 377 of 880 worktrees
//! were, and 140 once every car's branch was subtracted; the week held
//! 15. So the section shows the week, counts the older ones, and
//! `--all` lists them.
//!
//! WHAT IT NEVER DOES: fail the verb. Every read is git, and a failed
//! one is a line saying so — a workstation outside the repo must still
//! orient.

use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

/// One linked worktree, as `git worktree list --porcelain` names it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tree {
    pub path: String,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
}

/// A branch judged unpushed: its tree, and its newest commit's time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Unpushed {
    pub branch: String,
    pub path: String,
    /// The branch tip's committer time, epoch seconds.
    pub committed_at: i64,
}

/// What git said of one listed branch, read only for the lines shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shown {
    pub work: Unpushed,
    /// Commits not on origin/main; `None` when the count could not be read.
    pub ahead: Option<u64>,
    /// The first backlog id the branch's own commit messages mention.
    pub item: Option<String>,
}

/// The `for-each-ref` format [`unmerged`] reads: full refname, a tab,
/// the tip's committer time. The full name, not `:short`, because a tag
/// or remote of the same name makes the short one ambiguous.
pub(crate) const UNMERGED_FORMAT: &str = "--format=%(refname)%09%(committerdate:unix)";

/// The window the section shows by default: the week the gate-runs in
/// the claimed set span, so a branch older than it is not judged against
/// gate-runs orient never read — it is counted instead.
const WINDOW_DAYS: i64 = crate::orient::GATE_RUN_WINDOW_DAYS;

/// The worktrees in `git worktree list --porcelain` output.
pub(crate) fn worktrees(porcelain: &str) -> Vec<Tree> {
    porcelain
        .split("\n\n")
        .filter_map(|record| {
            let path = record
                .lines()
                .find_map(|l| l.strip_prefix("worktree "))?
                .to_string();
            let branch = record
                .lines()
                .find_map(|l| l.strip_prefix("branch refs/heads/"))
                .map(str::to_string);
            Some(Tree { path, branch })
        })
        .collect()
}

/// Branch -> tip committer time, from `git for-each-ref --no-merged`
/// output in [`UNMERGED_FORMAT`].
pub(crate) fn unmerged(refs: &str) -> BTreeMap<String, i64> {
    refs.lines()
        .filter_map(|l| {
            let (name, at) = l.split_once('\t')?;
            let branch = name.strip_prefix("refs/heads/").unwrap_or(name);
            Some((branch.to_string(), at.trim().parse().ok()?))
        })
        .collect()
}

/// The unpushed agent work among `trees`, newest first.
pub(crate) fn select(
    trees: &[Tree],
    unmerged: &BTreeMap<String, i64>,
    forge_heads: &BTreeSet<String>,
    claimed: &BTreeSet<String>,
) -> Vec<Unpushed> {
    let mut out: Vec<Unpushed> = trees
        .iter()
        .filter(|t| {
            std::path::Path::new(&t.path)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-"))
        })
        .filter_map(|t| {
            let branch = t.branch.as_ref()?;
            let committed_at = *unmerged.get(branch)?;
            (!forge_heads.contains(branch) && !claimed.contains(branch)).then(|| Unpushed {
                branch: branch.clone(),
                path: t.path.clone(),
                committed_at,
            })
        })
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.committed_at));
    out
}

/// The first `backlog <id>` / `backlog-item <id>` / `backlog item <id>`
/// the messages mention, as its first eight hex digits. A word after
/// `backlog` that is not a whole hex run of at least eight is not an id.
pub(crate) fn item_named(messages: &str) -> Option<String> {
    let text = messages.to_lowercase();
    text.match_indices("backlog").find_map(|(at, word)| {
        let rest = &text[at + word.len()..];
        let rest = rest
            .strip_prefix("-item")
            .or_else(|| rest.strip_prefix(" item"))
            .unwrap_or(rest);
        let rest = rest.trim_start_matches([' ', ':']);
        let hex: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
        let whole = !rest[hex.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        (hex.len() >= 8 && whole).then(|| hex[..8].to_string())
    })
}

/// The section's lines: every shown branch, and a count of the older
/// ones the window left out.
pub(crate) fn lines(shown: &[Shown], older: usize, all: bool) -> Vec<String> {
    let held = "every agent worktree's commits are on a forge head, a car, a gate-run or main";
    let mut out = if shown.is_empty() && older == 0 {
        vec![format!("\n  UNPUSHED WORK — none: {held}")]
    } else if shown.is_empty() {
        vec![format!(
            "\n  UNPUSHED WORK — none in the last {WINDOW_DAYS} days: {held}"
        )]
    } else {
        let scope = if all {
            String::new()
        } else {
            format!(" in the last {WINDOW_DAYS} days")
        };
        let mut head = vec![format!(
            "\n  UNPUSHED WORK — {} agent worktree branch(es) with commits no forge head, car or \
             gate-run holds{scope} (a dead builder's work: push and gate it, or read it before \
             rebuilding — never rebuild blind):",
            shown.len()
        )];
        head.extend(shown.iter().map(|s| {
            let ahead = s.ahead.map_or("?".to_string(), |n| n.to_string());
            let item = s
                .item
                .as_ref()
                .map_or("names no backlog item".to_string(), |i| {
                    format!("backlog {i}")
                });
            format!(
                "    {}  {ahead} ahead  committed {}  {item}  {}",
                s.work.branch,
                stamp(s.work.committed_at),
                s.work.path
            )
        }));
        head
    };
    if older > 0 {
        out.push(format!(
            "    …and {older} older than {WINDOW_DAYS} days — `boss orient --all` lists every one"
        ));
    }
    out
}

fn stamp(at: i64) -> String {
    DateTime::<Utc>::from_timestamp(at, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%MZ").to_string())
        .unwrap_or_else(|| "?".into())
}

/// One git read in the current checkout, as text, or why it failed.
fn git(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("could not run git ({e})"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed ({})",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("no stderr")
        ))
    }
}

/// What git says of one listed branch: its ahead count and the item its
/// own commits mention, oldest commit first (the RED usually names it).
fn observe(work: Unpushed) -> Shown {
    let range = format!("origin/main..refs/heads/{}", work.branch);
    let ahead = git(&["rev-list", "--count", &range])
        .ok()
        .and_then(|n| n.trim().parse().ok());
    let item = git(&["log", "--reverse", "--format=%B", &range])
        .ok()
        .and_then(|m| item_named(&m));
    Shown { work, ahead, item }
}

/// The section, read from the checkout orient runs in. `forge_heads`
/// and `claimed` are the ORPHANS read's own sets, so this judges against
/// exactly what that section did. Never fails: a failed read is a line.
pub(crate) fn section(
    forge_heads: &BTreeSet<String>,
    claimed: &BTreeSet<String>,
    now: DateTime<Utc>,
    all: bool,
) -> Vec<String> {
    let read = git(&["worktree", "list", "--porcelain"]).and_then(|trees| {
        git(&[
            "for-each-ref",
            "--no-merged=origin/main",
            UNMERGED_FORMAT,
            "refs/heads",
        ])
        .map(|refs| (trees, refs))
    });
    let (trees, refs) = match read {
        Ok(read) => read,
        Err(e) => return vec![format!("\n  UNPUSHED WORK — skipped: {e}")],
    };
    let since = now.timestamp() - WINDOW_DAYS * 86_400;
    let (recent, older): (Vec<Unpushed>, Vec<Unpushed>) =
        select(&worktrees(&trees), &unmerged(&refs), forge_heads, claimed)
            .into_iter()
            .partition(|w| all || w.committed_at >= since);
    let shown: Vec<Shown> = recent.into_iter().map(observe).collect();
    lines(&shown, older.len(), all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The porcelain shape, measured on the pod: the main checkout
    /// first, a branch line or `detached`, a `locked` / `prunable` line
    /// with or without a reason, and a blank line between records.
    const PORCELAIN: &str = "worktree /work/boss\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
worktree /work/wt/agent-claude-b-8228e66b\nHEAD 2222222222222222222222222222222222222222\nbranch refs/heads/fix/a-dead-builders-work\nlocked\n\n\
worktree /work/boss/.claude/worktrees/agent-ac245f1e\nHEAD 3333333333333333333333333333333333333333\ndetached\nlocked claude agent agent-ac245f1e (pid 7)\n\n\
worktree /scratch/run-7e2ba71e/agent-7e2ba71e\nHEAD 4444444444444444444444444444444444444444\nbranch refs/heads/fix/gone-tree\nprunable gitdir file points to non-existent location\n";

    #[test]
    fn the_porcelain_reads_every_tree_with_its_branch_or_none() {
        assert_eq!(
            worktrees(PORCELAIN),
            vec![
                Tree {
                    path: "/work/boss".into(),
                    branch: Some("main".into())
                },
                Tree {
                    path: "/work/wt/agent-claude-b-8228e66b".into(),
                    branch: Some("fix/a-dead-builders-work".into())
                },
                Tree {
                    path: "/work/boss/.claude/worktrees/agent-ac245f1e".into(),
                    branch: None
                },
                Tree {
                    path: "/scratch/run-7e2ba71e/agent-7e2ba71e".into(),
                    branch: Some("fix/gone-tree".into())
                },
            ]
        );
    }

    #[test]
    fn the_unmerged_refs_read_branch_and_time_and_skip_a_bad_line() {
        let refs =
            "fix/a-dead-builders-work\t1763180000\nfeat/x\tnot-a-time\n\nfeat/y\t1763000000\n";
        let got = unmerged(refs);
        assert_eq!(got.get("fix/a-dead-builders-work"), Some(&1763180000));
        assert_eq!(got.get("feat/y"), Some(&1763000000));
        assert_eq!(got.len(), 2, "{got:?}");
    }

    /// The 2026-09-24 case is listed; each thing the record already
    /// points at is not, and neither is a tree that is not an agent's.
    #[test]
    fn a_dead_builders_branch_is_listed_and_everything_the_record_holds_is_not() {
        let trees = vec![
            Tree {
                path: "/work/boss".into(),
                branch: Some("operator/local".into()),
            },
            Tree {
                path: "/work/wt/agent-dead".into(),
                branch: Some("fix/a-dead-builders-work".into()),
            },
            Tree {
                path: "/work/wt/agent-newer".into(),
                branch: Some("feat/newer".into()),
            },
            Tree {
                path: "/work/wt/agent-pushed".into(),
                branch: Some("fix/pushed".into()),
            },
            Tree {
                path: "/work/wt/agent-carred".into(),
                branch: Some("fix/landed-by-a-car".into()),
            },
            Tree {
                path: "/work/wt/agent-merged".into(),
                branch: Some("fix/an-ancestor-of-main".into()),
            },
            Tree {
                path: "/work/wt/agent-detached".into(),
                branch: None,
            },
        ];
        let unmerged: BTreeMap<String, i64> = [
            ("operator/local", 50),
            ("fix/a-dead-builders-work", 100),
            ("feat/newer", 200),
            ("fix/pushed", 300),
            ("fix/landed-by-a-car", 400),
        ]
        .into_iter()
        .map(|(b, t)| (b.to_string(), t))
        .collect();
        let got = select(
            &trees,
            &unmerged,
            &set(&["fix/pushed", "main"]),
            &set(&["fix/landed-by-a-car"]),
        );
        assert_eq!(
            got,
            vec![
                Unpushed {
                    branch: "feat/newer".into(),
                    path: "/work/wt/agent-newer".into(),
                    committed_at: 200
                },
                Unpushed {
                    branch: "fix/a-dead-builders-work".into(),
                    path: "/work/wt/agent-dead".into(),
                    committed_at: 100
                },
            ]
        );
    }

    /// Builders write the item in prose, not a trailer — measured over
    /// 400 car commits on 2026-11-15, no packet trailer key exists — so
    /// the read is the first `backlog <id>` the messages carry.
    #[test]
    fn the_item_is_the_first_backlog_id_the_messages_mention() {
        assert_eq!(
            item_named("Add the thing\n\nBacklog f8d2bffc: a run releases.\nbacklog 11111111"),
            Some("f8d2bffc".into())
        );
        assert_eq!(
            item_named("Fix it (backlog-item aa4452ff-13df-4618-b5f0-ac2a7f3c3608)"),
            Some("aa4452ff".into())
        );
        assert_eq!(
            item_named("names the backlog item 203c2a72 here"),
            Some("203c2a72".into())
        );
        assert_eq!(item_named("the backlog is long; backlog abc"), None);
        assert_eq!(item_named("no id at all"), None);
    }

    fn shown(branch: &str, at: i64, ahead: Option<u64>, item: Option<&str>) -> Shown {
        Shown {
            work: Unpushed {
                branch: branch.into(),
                path: format!("/work/wt/agent-{at}"),
                committed_at: at,
            },
            ahead,
            item: item.map(str::to_string),
        }
    }

    #[test]
    fn a_listed_branch_names_its_ahead_count_time_item_and_tree() {
        // 2026-11-14T22:24:12Z
        let got = lines(
            &[shown("fix/a", 1794695052, Some(4), Some("f8d2bffc"))],
            0,
            false,
        );
        assert!(
            got[0].contains("UNPUSHED WORK — 1 agent worktree branch(es)")
                && got[0].contains("never rebuild blind"),
            "{got:?}"
        );
        assert_eq!(
            got[1],
            "    fix/a  4 ahead  committed 2026-11-14T22:24Z  backlog f8d2bffc  /work/wt/agent-1794695052"
        );
        assert_eq!(got.len(), 2, "{got:?}");
        let unread = lines(&[shown("fix/b", 1794695052, None, None)], 0, false);
        assert!(
            unread[1].contains("? ahead") && unread[1].contains("names no backlog item"),
            "{unread:?}"
        );
    }

    #[test]
    fn older_work_is_counted_and_all_is_named_not_dropped() {
        let got = lines(&[shown("fix/a", 1794695052, Some(1), None)], 121, false);
        assert_eq!(
            got.last().map(String::as_str),
            Some("    …and 121 older than 7 days — `boss orient --all` lists every one"),
            "{got:?}"
        );
        let quiet = lines(&[], 3, false);
        assert_eq!(
            quiet,
            vec![
                "\n  UNPUSHED WORK — none in the last 7 days: every agent worktree's commits are on a forge head, a car, a gate-run or main".to_string(),
                "    …and 3 older than 7 days — `boss orient --all` lists every one".to_string(),
            ]
        );
        assert_eq!(
            lines(&[], 0, false),
            vec!["\n  UNPUSHED WORK — none: every agent worktree's commits are on a forge head, a car, a gate-run or main".to_string()]
        );
        // --all shows everything and owes no tail.
        let all = lines(&[shown("fix/a", 1794695052, Some(1), None)], 0, true);
        assert_eq!(all.len(), 2, "{all:?}");
    }
}
