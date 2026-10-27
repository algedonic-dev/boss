//! Main's twin of a car commit that stopped on a conflict — the ONE
//! lookup `boss rerail` and `boss gate --rebase` both ask.
//!
//! WHY ONE FUNCTION (backlog 6e38a79e, 2026-10-18). Car dd64f4ad carried a
//! stale copy of a change main had landed and later amended, so its replay
//! CONFLICTED rather than replaying empty, and it sat 87 windows on the
//! stop. `boss rerail` learned to name main's twin at that stop (ec05a62f);
//! `boss gate --rebase` — which the dock re-gate also runs — replays the
//! same commits the same way and did not, so the same car stalled there
//! with nothing said. Two copies of "is this main's twin" is the drift
//! CLAUDE.md 9a counts, so the lookup moved here out of rerail.rs and each
//! verb renders only its own words around it.
//!
//! Both callers NAME the twin and drop nothing: three adversarial reviews
//! of car fd13ddf2 (runs 9b231beb, 3f14cf26, 1c7dd9c7) each found a commit
//! main no longer held dropped by a twin. A twin proves main took the
//! change ONCE, never that it holds it now — that decision is the human's.

use anyhow::{Context, Result, anyhow, bail};

/// Shell out to git in a directory, capturing stderr for the error.
fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What main holds that matches a stopped commit's patch text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TwinFound {
    /// The same change: equal `git diff-tree --raw` — every path, mode,
    /// old blob and new blob — so main took THIS change once.
    Same,
    /// Only the same patch TEXT (`patch-id --verbatim` equal, raw
    /// diff-tree not): patch-id drops the @@ line numbers, so the hunk
    /// may sit elsewhere in the file — a repeated stanza in a TOML
    /// registry, a YAML manifest, a test (review 0a5496cb, finding 1) —
    /// OR it is this very change, landed on a file main had already
    /// changed outside the hunk, so its blobs differ (review 22073d6e).
    /// The match cannot tell the two apart, and the note says so.
    Elsewhere,
}

/// Main's twin of a stopped commit, as [`twin_on_main`] found it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Twin {
    pub found: TwinFound,
    /// Main's commit.
    pub sha: String,
    /// Its subject line.
    pub subject: String,
    /// The STOPPED commit's files, which the twin's patch text covers.
    pub files: Vec<String>,
}

/// Single-quote `s` for a shell, so a printed command pastes as printed
/// whatever the path holds (review 0a5496cb, finding 6).
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `git diff-tree --raw` of one commit, its lines sorted: every path,
/// mode, old blob and new blob, which two commits share only when they
/// make the same change.
fn raw_change(wt: &str, sha: &str) -> Result<Vec<String>> {
    let raw = git(
        wt,
        &[
            "diff-tree",
            "-r",
            "--no-commit-id",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            sha,
        ],
    )?;
    let mut lines: Vec<String> = raw.lines().map(String::from).collect();
    lines.sort();
    Ok(lines)
}

/// Main's newest commit (`onto`, since it parted from `tip`) whose patch
/// text is `sha`'s byte for byte — preferring one that makes the SAME
/// change ([`TwinFound::Same`]) over one that only shares the text — or
/// `None` when main never took that patch text.
///
/// `--verbatim`, NOT `git cherry` (review 9b231beb, finding 2): `git
/// cherry` and `--stable` strip ALL whitespace, so a key a car nests
/// under `a:` and one main adds at the top level of the same YAML file
/// would be named as a twin. `--full-diff`: the path limit picks main's
/// commits, never trims their patches — a train that carried this change
/// AND another has a patch text of its own, so it is never matched here,
/// whatever it holds (backlog 2f2caa8e: say what the match shows).
pub(crate) fn twin_on_main(wt: &str, onto: &str, tip: &str, sha: &str) -> Result<Option<Twin>> {
    // An empty or merge commit has no patch to match.
    let Some((forward, _)) = verbatim_ids(wt, &["-1", sha])?.into_iter().next() else {
        return Ok(None);
    };
    let names = git(
        wt,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            sha,
        ],
    )?;
    let files: Vec<String> = names
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect();
    let not_car = format!("^{tip}");
    let mut args = vec!["--no-merges", "--full-diff", onto, not_car.as_str(), "--"];
    let literal: Vec<String> = files.iter().map(|f| format!(":(literal){f}")).collect();
    args.extend(literal.iter().map(String::as_str));
    let texts: Vec<String> = verbatim_ids(wt, &args)?
        .into_iter()
        .filter(|(id, _)| *id == forward)
        .map(|(_, commit)| commit)
        .collect();
    let Some(newest) = texts.first() else {
        return Ok(None);
    };
    let change = raw_change(wt, sha)?;
    let mut same = None;
    for c in &texts {
        if raw_change(wt, c)? == change {
            same = Some(c.clone());
            break;
        }
    }
    let (found, twin) = match same {
        Some(c) => (TwinFound::Same, c),
        None => (TwinFound::Elsewhere, newest.clone()),
    };
    let subject = git(wt, &["log", "-1", "--format=%s", &twin]).unwrap_or_default();
    Ok(Some(Twin {
        found,
        sha: twin,
        subject,
        files,
    }))
}

/// `git patch-id --verbatim` of each commit `git log -p <log_args>`
/// prints, in log order, as (patch-id, commit). Both sides of a match
/// are read through this one command, so a diff setting reaches both.
fn verbatim_ids(wt: &str, log_args: &[&str]) -> Result<Vec<(String, String)>> {
    use std::io::Write;
    use std::process::Stdio;
    let log = crate::git_auth::command()
        .arg("-C")
        .arg(wt)
        .args([
            "log",
            "-p",
            "--no-color",
            "--no-ext-diff",
            // A diff driver's textconv would compare converted text while
            // the note says byte for byte (review 0a5496cb, finding 5).
            "--no-textconv",
            "--format=commit %H",
        ])
        .args(log_args)
        .output()
        .with_context(|| format!("running git log -p {log_args:?} in {wt}"))?;
    if !log.status.success() {
        bail!(
            "git log -p {log_args:?} failed: {}",
            String::from_utf8_lossy(&log.stderr).trim()
        );
    }
    let mut child = crate::git_auth::command()
        .arg("-C")
        .arg(wt)
        .args(["patch-id", "--verbatim"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git patch-id --verbatim")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("git patch-id has no stdin"))?;
    // Written from a thread: patch-id answers as it reads, and a long
    // main would fill its stdout pipe while this side still writes.
    let patch = log.stdout;
    let writer = std::thread::spawn(move || stdin.write_all(&patch));
    let out = child
        .wait_with_output()
        .context("reading git patch-id --verbatim")?;
    let wrote = writer
        .join()
        .map_err(|_| anyhow!("writing to git patch-id panicked"))?;
    // patch-id's own word first (review 3f14cf26, finding 5): when it
    // dies early the write fails with a broken pipe, which names the
    // symptom and loses the cause.
    if !out.status.success() {
        bail!(
            "git patch-id --verbatim failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    wrote.context("writing to git patch-id")?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(id, commit)| (id.to_string(), commit.trim().to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with a space or a quote is quoted in the printed read.
    #[test]
    fn a_printed_path_is_shell_quoted() {
        assert_eq!(sh_quote("a b.txt"), "'a b.txt'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
