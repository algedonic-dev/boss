//! `boss car unland <car> --merge-ref <sha>` — close a car whose landing
//! forge main LOST, and open the successor that rides again (backlog
//! f9256445, design d812f1b7 D2, Q1 answered 2026-09-25).
//!
//! WHY. Train 2026-09-25 20:04 (PR #687) merged car dec3136a as squash
//! commit c85941b4 at 20:10:41Z. By 20:11:14Z forge main was back at
//! 777a5888, and the 20:34 train was assembled on that real main. The
//! car read landed — "landed on main as c85941b423bc" on its review step,
//! `merged: "true"` on the job — and its `proven` step stood ready for a
//! change main does not carry. Nothing in the protocol could end it
//! truthfully; the pure half of why is `boss_jobs::car_unland`.
//!
//! THE READING IS THE VERB'S OWN. It does not trust the caller's word
//! that main lost the merge — not an operator's, not the conductor's.
//! [`read_main`] reads forge main with `git ls-remote origin
//! refs/heads/main` and asks `git merge-base --is-ancestor <merge>
//! <main>` in the clone it runs in. Exit 1 is the only NOT; exit 0 is a
//! refusal (the landing stands), and any other answer — no forge, an
//! object the clone cannot read, a crash — is a refusal to judge, never
//! a verdict. A wrong target answers instead of erroring, so it is the
//! answer, not the absence of one, that is read.
//!
//! WHAT IT WRITES, IN THIS ORDER, EACH SKIPPED ON A RE-RUN THAT FINDS IT
//! DONE: the SUCCESSOR car for the same branch (`supersedes` naming this
//! one, scope and build carried, first open work `gate`); a CORRECTION
//! beside the review step's landing note through the corrections door
//! (the step stays as it was — both facts stay in the log); then the
//! three writes that close this car through `unlanded`, read back. The
//! successor comes first because the terminal requires it: a terminal
//! naming a successor that does not exist would be a claim.
//!
//! THE SECOND READING, A MERGE MAIN REVERTED (backlog 7f4694eb). Car
//! 526974b0 merged in train #1784, failed every converge, and main
//! REVERTED it (car 6c272983, train #1787); its intent landed as car
//! bfa6f7be. A revert leaves the merge an ancestor of main, so the
//! reading above refused it, and the car stood open at `proven` for four
//! days holding its item open. `--reverted-by <revert car|sha>
//! --superseded-by <car>` closes it through the same `unlanded` terminal
//! on a reading of its own — the merge IS on main, the revert is on main
//! and descends from it, and [`read_revert`] finds every one of the car's
//! own paths undone — and NAMES the successor rather than opening one
//! ([`unland_reverted`]).
//!
//! ONE WRITER, TWO CALLERS. The conductor's merge-lost arm calls
//! [`unland_car`] for every car of a train that ends `merge-lost`,
//! through its own client ([`crate::car_retire::Door`]); an operator
//! calls it through this verb. Neither has a second copy of the writes.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use boss_jobs::car;
use boss_jobs::car_unland::{
    self as core, UNLANDED_SLUG, Unlanding, review_correction, successor_body, successor_of,
    successor_writes, unland_writes,
};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{Value, json};

use crate::car_retire::{Door, Http, apply_via, read_via};
use crate::git_auth::ForgeAuth;

/// What git read about forge main and one merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainReading {
    /// Forge main at the read, as `ls-remote` answered it.
    pub main: String,
    /// The merge, resolved to its full sha in the clone.
    pub merge: String,
    /// RFC3339, when the read was made.
    pub read_at: String,
    /// Does main carry the merge (`merge-base --is-ancestor` exit 0)?
    pub carries: bool,
}

impl MainReading {
    /// The reading as the record states it: both reads, the command and
    /// its answer.
    pub(crate) fn evidence(&self) -> String {
        format!(
            "forge main was {} (git ls-remote origin refs/heads/main) at {}; git merge-base \
             --is-ancestor {} {} exited {}",
            self.main,
            self.read_at,
            self.merge,
            self.main,
            if self.carries { 0 } else { 1 }
        )
    }
}

/// The git every read here runs: the forge credential on it the way
/// `sh_in` puts it on the conductor's clone, fetch and push. Bare, the
/// conductor pod's `ls-remote origin` asked for a username and the
/// merge-lost arm never judged (backlog cb3d8952, 2026-09-26). With no
/// token file — `boss car unland` on the dev pod — it is a plain git and
/// the clone's own credential helper answers, as before.
fn git_command(repo: &Path, auth: Option<&ForgeAuth>) -> Command {
    let mut cmd = crate::git_auth::command_with(auth);
    cmd.arg("-C").arg(repo);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Result<Output, String> {
    git_command(repo, crate::git_auth::forge_auth().as_ref())
        .args(args)
        .output()
        .map_err(|e| format!("git {} could not run: {e}", args.join(" ")))
}

/// Run git and require exit 0; the stdout, trimmed.
fn git_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status
                .code()
                .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The full sha `rev` names in this clone, if it names a commit here.
fn commit_here(repo: &Path, rev: &str) -> Option<String> {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

/// The twelve characters a sha is spelled with on the record.
fn s12(s: &str) -> &str {
    &s[..12.min(s.len())]
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Read whether forge main carries `merge`, from the clone at `repo`
/// whose `origin` is the forge. `Err` is a refusal to judge — every git
/// failure lands there, and none of them is a NOT.
///
/// A merge main lost may never have been fetched into this clone: main
/// moved back within 32 seconds on 2026-09-25, inside one conductor
/// pass. So an unknown merge named by its FULL sha is fetched by sha
/// (the forge still holds the object, measured by triage d081da27); an
/// abbreviated one the clone cannot resolve is refused, naming the fix.
pub(crate) fn read_main(
    repo: &Path,
    merge: &str,
    now: DateTime<Utc>,
) -> Result<MainReading, String> {
    let merge = merge.trim();
    let line = git_ok(repo, &["ls-remote", "origin", "refs/heads/main"])?;
    let main = crate::receipt::exact_ref_head(&line, "refs/heads/main")
        .filter(|s| is_full_sha(s))
        .ok_or_else(|| format!("the forge answered no main (ls-remote said {line:?})"))?;
    if commit_here(repo, &main).is_none() {
        git_ok(repo, &["fetch", "-q", "origin", "refs/heads/main"])?;
    }
    if commit_here(repo, &main).is_none() {
        return Err(format!(
            "forge main {main} is not readable in this clone even after a fetch"
        ));
    }
    let merge_full = match commit_here(repo, merge) {
        Some(sha) => sha,
        None if is_full_sha(merge) => {
            git_ok(repo, &["fetch", "-q", "origin", merge])?;
            commit_here(repo, merge).ok_or_else(|| {
                format!("the merge {merge} is not readable in this clone even after a fetch")
            })?
        }
        None => {
            return Err(format!(
                "the merge {merge} is not a commit this clone holds, and an abbreviated sha \
                 cannot be fetched — name it by its full sha (the forge's merge_commit_sha \
                 for the train's PR)"
            ));
        }
    };
    let carries = is_ancestor(repo, &merge_full, &main)?;
    Ok(MainReading {
        main,
        merge: merge_full,
        read_at: crate::sor_client::stamp(now),
        carries,
    })
}

/// `git merge-base --is-ancestor a b`: exit 0 is yes, exit 1 is no, and
/// anything else is no answer — a refusal, never a NOT.
fn is_ancestor(repo: &Path, a: &str, b: &str) -> Result<bool, String> {
    let out = git(repo, &["merge-base", "--is-ancestor", a, b])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        other => Err(format!(
            "git merge-base --is-ancestor {a} {b} gave no answer (exit {}): {}",
            other.map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// The full sha of `rev` in this clone — fetched by sha when it is a
/// full sha the clone has not seen (a revert car's merge on a clone that
/// has not fetched since), refused when it is abbreviated and unknown.
fn commit_or_fetch(repo: &Path, rev: &str) -> Result<String, String> {
    match commit_here(repo, rev) {
        Some(sha) => Ok(sha),
        None if is_full_sha(rev) => {
            git_ok(repo, &["fetch", "-q", "origin", rev])?;
            commit_here(repo, rev)
                .ok_or_else(|| format!("{rev} is not readable in this clone even after a fetch"))
        }
        None => Err(format!(
            "{rev} is not a commit this clone holds, and an abbreviated sha cannot be fetched \
             — name it by its full sha"
        )),
    }
}

/// `git diff --name-only` of a range, one path a line, renames off so a
/// moved file reads as both of its paths.
fn changed_paths(repo: &Path, range: &[&str]) -> Result<Vec<String>, String> {
    let mut args = vec!["diff", "--no-renames", "--name-only"];
    args.extend_from_slice(range);
    Ok(git_ok(repo, &args)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The bytes of `git diff a b -- path`, for patch-id.
fn diff_of(repo: &Path, a: &str, b: &str, path: &str) -> Result<Vec<u8>, String> {
    let args = [
        "diff",
        "--no-renames",
        "--no-ext-diff",
        "--no-color",
        a,
        b,
        "--",
        path,
    ];
    let out = git(repo, &args)?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {:?}: {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// `git patch-id --stable` of a diff: the hash of the change with line
/// numbers and whitespace left out. `None` for an empty diff — two empty
/// diffs are not the same change, they are no change.
fn patch_id(repo: &Path, diff: &[u8]) -> Result<Option<String>, String> {
    use std::io::Write;
    use std::process::Stdio;
    if diff.is_empty() {
        return Ok(None);
    }
    let mut child = git_command(repo, crate::git_auth::forge_auth().as_ref())
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git patch-id could not run: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git patch-id took no stdin")?
        .write_all(diff)
        .map_err(|e| format!("feeding git patch-id: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git patch-id: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git patch-id exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string))
}

/// What git read about a REVERT of one car's merge (backlog 7f4694eb):
/// whether main carries the revert, whether the revert descends from the
/// merge, and, path by path over the car's OWN paths, whether it undid
/// the car's change. Every field is a reading; [`RevertReading::refusal`]
/// is the judgement. Reusable by any reader that must tell a merge main
/// still carries from one main took back — `boss dispatch`'s ALREADY ON
/// MAIN refusal reads the merge alone and cannot (backlog 5314fb42).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RevertReading {
    /// Forge main the reading was judged against.
    pub main: String,
    /// The car's merge, full sha.
    pub merge: String,
    /// The revert, full sha.
    pub revert: String,
    /// `git merge-base --is-ancestor <revert> <main>` exited 0.
    pub on_main: bool,
    /// `git merge-base --is-ancestor <merge> <revert>` exited 0, and the
    /// revert is not the merge itself.
    pub descends: bool,
    /// Where the car's own paths came from — its boarded diff, or the
    /// merge's whole diff when no boarded head is readable here.
    pub paths_from: String,
    /// The car's own paths: the merge's, restricted to its boarded diff
    /// (a multi-car train's other cars are not this car's question).
    pub paths: Vec<String>,
    /// `(path, how it reads reverted)` for each path the revert undid.
    pub reverted: Vec<(String, String)>,
    /// The car's paths main still carries the car's change to.
    pub unreverted: Vec<String>,
}

impl RevertReading {
    /// Does main carry a revert of ALL of this car?
    pub(crate) fn total(&self) -> bool {
        self.refusal().is_none()
    }

    /// Why this is not a total revert of the car, or `None` when it is.
    pub(crate) fn refusal(&self) -> Option<String> {
        let (r, m, main) = (s12(&self.revert), s12(&self.merge), s12(&self.main));
        if !self.on_main {
            return Some(format!(
                "the revert {r} is not an ancestor of forge main {main} (git merge-base \
                 --is-ancestor exited 1): main does not carry it"
            ));
        }
        if !self.descends {
            return Some(format!(
                "the revert {r} does not descend from the merge {m} (git merge-base \
                 --is-ancestor {m} {r} exited 1, or it is the merge itself): it cannot have \
                 reverted it"
            ));
        }
        if self.paths.is_empty() {
            return Some(format!(
                "the merge {m} changed none of the car's own paths ({}), so nothing says \
                 what a revert of it must undo",
                self.paths_from
            ));
        }
        if !self.unreverted.is_empty() {
            return Some(format!(
                "the revert {r} is partial: main still carries the car's change to {} — \
                 neither git diff {m}^1 {r} -- <path> is empty nor do the patch-ids match",
                self.unreverted.join(", ")
            ));
        }
        None
    }

    /// The reading as the record states it.
    pub(crate) fn evidence(&self) -> String {
        format!(
            "git merge-base --is-ancestor {r} {main} exited {}; git merge-base --is-ancestor \
             {m} {r} exited {}; the car's own paths ({}, from {}): {}",
            if self.on_main { 0 } else { 1 },
            if self.descends { 0 } else { 1 },
            self.paths.len(),
            self.paths_from,
            self.reverted
                .iter()
                .map(|(p, how)| format!("{p} ({how})"))
                .chain(
                    self.unreverted
                        .iter()
                        .map(|p| format!("{p} (NOT reverted)"))
                )
                .collect::<Vec<_>>()
                .join("; "),
            r = self.revert,
            m = self.merge,
            main = self.main,
        )
    }
}

/// Read whether `revert` undoes the car that landed as `merge`, against
/// forge main `main` (full shas, read by [`read_main`]). The car's own
/// paths are the merge's, restricted to `git diff <merge>^1...<boarded>`
/// when the boarded head is readable, so another car in the same train
/// is not asked about; unreadable, the merge's whole diff is asked, which
/// can only refuse more. A path reads reverted when `git diff <merge>^1
/// <revert> -- <path>` is empty, or when the patch-id of the car's change
/// undone (`git diff <merge> <merge>^1 -- <path>`) equals the revert's
/// own (`git diff <revert>^1 <revert> -- <path>`) — the second because a
/// later train may have changed the same file elsewhere (triage 59f08557
/// read 526974b0's revert both ways). `Err` is a git failure only.
pub(crate) fn read_revert(
    repo: &Path,
    main: &str,
    merge: &str,
    revert: &str,
    boarded: Option<&str>,
) -> Result<RevertReading, String> {
    let merge = commit_or_fetch(repo, merge)?;
    let revert = commit_or_fetch(repo, revert)?;
    let on_main = is_ancestor(repo, &revert, main)?;
    let descends = revert != merge && is_ancestor(repo, &merge, &revert)?;
    let before = format!("{merge}^1");
    let merged = changed_paths(repo, &[&before, &merge])?;
    let boarded = boarded
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| (b, commit_or_fetch(repo, b)));
    let (paths, paths_from): (Vec<String>, String) = match boarded {
        Some((_, Ok(head))) => {
            let own = changed_paths(repo, &[&format!("{before}...{head}")])?;
            (
                merged.into_iter().filter(|p| own.contains(p)).collect(),
                format!("its boarded head {}", s12(&head)),
            )
        }
        Some((b, Err(e))) => (
            merged,
            format!("the merge's whole diff — its boarded head {b} is not readable here: {e}"),
        ),
        None => (
            merged,
            "the merge's whole diff — the car records no boarded head".to_string(),
        ),
    };
    let revert_before = format!("{revert}^1");
    let mut reverted = Vec::new();
    let mut unreverted = Vec::new();
    for p in &paths {
        if diff_of(repo, &before, &revert, p)?.is_empty() {
            reverted.push((
                p.clone(),
                format!("git diff {}^1 {} is empty", s12(&merge), s12(&revert)),
            ));
            continue;
        }
        let undone = patch_id(repo, &diff_of(repo, &merge, &before, p)?)?;
        let theirs = patch_id(repo, &diff_of(repo, &revert_before, &revert, p)?)?;
        match (undone, theirs) {
            (Some(a), Some(b)) if a == b => {
                reverted.push((p.clone(), format!("patch-id {} both ways", s12(&a))))
            }
            _ => unreverted.push(p.clone()),
        }
    }
    Ok(RevertReading {
        main: main.to_string(),
        merge,
        revert,
        on_main,
        descends,
        paths_from,
        paths,
        reverted,
        unreverted,
    })
}

/// What an unlanding left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unlanded {
    /// The car closed through `unlanded`.
    pub car: String,
    /// Its successor's full id.
    pub successor: String,
    /// The reading it closed on (None when a pass before this one had
    /// already closed it).
    pub evidence: Option<String>,
}

fn id_of(v: &Value) -> Result<String> {
    v.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("a packet without an id")
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// Open the successor, or find the one a pass before this one opened,
/// and bring it to `gate`.
async fn successor(door: &dyn Door, pred: &Value, lost: &str, at: &str) -> Result<String> {
    let pid = id_of(pred)?;
    let filter = json!({ core::SUPERSEDES: pid }).to_string();
    let listed = door
        .send(
            Method::GET,
            &format!(
                "/api/jobs?kind=ship-a-change&limit=20&metadata={}",
                percent_encoding::utf8_percent_encode(&filter, crate::job::QUERY_VALUE)
            ),
            None,
        )
        .await?;
    let cars = crate::train::rows(listed).context("listing the car's successors")?;
    let sid = match successor_of(&cars, &pid) {
        Some(found) => id_of(found)?,
        None => {
            let body = successor_body(pred).map_err(|e| anyhow!(e))?;
            let made = door
                .send(Method::POST, "/api/jobs", Some(body))
                .await?
                .context("opening the successor answered no body")?;
            id_of(&made)?
        }
    };
    let fresh = read_via(door, &sid).await?;
    for w in successor_writes(&fresh, pred, lost, at).map_err(|e| anyhow!(e))? {
        door.send(Method::PATCH, &w.merge_path(&sid), Some(w.metadata.clone()))
            .await
            .with_context(|| format!("carrying `{}` onto the successor", w.title))?;
        door.send(
            Method::PUT,
            &w.status_path(&sid),
            Some(w.status_body.clone()),
        )
        .await
        .with_context(|| format!("completing `{}` on the successor", w.title))?;
    }
    Ok(sid)
}

/// A car a pass before this one already closed through `unlanded`: the
/// successor it names, and no second reading.
fn already_unlanded(car_json: &Value, id: &str) -> Option<Unlanded> {
    (car_json
        .pointer("/metadata/outcome")
        .and_then(Value::as_str)
        == Some(UNLANDED_SLUG))
    .then(|| Unlanded {
        car: id.to_string(),
        successor: car_json
            .pointer("/metadata/superseded_by")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        evidence: None,
    })
}

/// The checks both readings make BEFORE git is asked anything: the car
/// is unlandable for `merge`, and the terminal exists on its pinned
/// version — a successor written for a car that then cannot close would
/// be a twin riding beside a car that still reads landed.
fn admit(car_json: &Value, merge: &str) -> Result<()> {
    core::unlandable(car_json, merge).map_err(|e| anyhow!("REFUSED — {e}"))?;
    let probe = Unlanding {
        merge_ref: merge.to_string(),
        evidence: "-".into(),
        superseded_by: "-".into(),
        completed_at: String::new(),
    };
    unland_writes(car_json, &probe).map_err(|e| anyhow!("REFUSED — {e}"))?;
    Ok(())
}

/// The commit `--reverted-by` names, and the revert car when it named
/// one. A full sha is a commit. Anything shorter is asked of BOTH the
/// jobs API (a car's id prefix → that car's merge) and the clone (an
/// abbreviated sha); exactly one may answer, because picking between a
/// car and a commit that share eight characters is how the wrong revert
/// gets recorded.
async fn resolve_revert(
    door: &dyn Door,
    repo: &Path,
    given: &str,
) -> Result<(String, Option<String>)> {
    let given = given.trim();
    if is_full_sha(given) {
        return Ok((given.to_string(), None));
    }
    let as_car = match read_via(door, given).await {
        Ok(job) if job.get("kind").and_then(Value::as_str) == Some("ship-a-change") => Some(job),
        _ => None,
    };
    let as_commit = commit_here(repo, given);
    match (as_car, as_commit) {
        (Some(revert_car), None) => {
            let merge =
                core::revert_car_merge(&revert_car).map_err(|e| anyhow!("REFUSED — {e}"))?;
            Ok((merge.to_string(), Some(id_of(&revert_car)?)))
        }
        (None, Some(sha)) => Ok((sha, None)),
        (Some(revert_car), Some(sha)) => bail!(
            "REFUSED — `{given}` names both car {} and commit {} — give the full sha or the \
             full car id",
            short(&id_of(&revert_car)?),
            s12(&sha)
        ),
        (None, None) => bail!(
            "REFUSED — `{given}` is neither a car the jobs API holds nor a commit this clone \
             holds — name the revert's car or its merge sha"
        ),
    }
}

/// `boss car unland`'s two readings, from its flags: `None` is the
/// lost-merge reading; `--reverted-by` and `--superseded-by` together are
/// the reverted-merge reading, and one without the other is refused — a
/// revert with no successor named would leave the item nothing to close
/// with, and a successor with no revert names no reason the car ended.
pub(crate) fn mode<'a>(
    reverted_by: Option<&'a str>,
    superseded_by: Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>> {
    match (reverted_by, superseded_by) {
        (None, None) => Ok(None),
        (Some(r), Some(s)) => Ok(Some((r, s))),
        _ => bail!(
            "boss car unland: REFUSED — --reverted-by and --superseded-by name one reading \
             together: the revert main carries AND the car that carries the intent"
        ),
    }
}

/// THE SECOND READING (backlog 7f4694eb): unland `car_id`, landed as
/// `merge`, whose merge forge main still carries but REVERTED in
/// `reverted_by` (a revert car, or the revert's sha), its intent carried
/// by the NAMED car `superseded_by`. Every reading is the verb's own and
/// each failure is a refusal before anything is written: main carries
/// the merge (else it is the lost-merge reading), the revert resolves to
/// a commit main carries that descends from the merge, and it undoes
/// every one of the car's own paths ([`read_revert`]). No successor is
/// opened: the named one gains `supersedes` if it lacks it. Then the same
/// correction door and the same `unland_writes` as the first reading,
/// with `reverted_by` named on the job. Idempotent.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn unland_reverted(
    door: &dyn Door,
    repo: &Path,
    car_id: &str,
    merge: &str,
    reverted_by: &str,
    superseded_by: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<Unlanded> {
    let car_json = read_via(door, car_id).await?;
    let id = id_of(&car_json)?;
    if let Some(done) = already_unlanded(&car_json, &id) {
        return Ok(done);
    }
    admit(&car_json, merge)?;

    let main = read_main(repo, merge, now)
        .map_err(|e| anyhow!("REFUSED — could not read whether main carries {merge}: {e}"))?;
    if !main.carries {
        bail!(
            "REFUSED — forge main {} does not carry {} (git merge-base --is-ancestor exited 1): \
             main LOST this merge rather than reverting it — run boss car unland without \
             --reverted-by, which reads that and opens the successor",
            short(&main.main),
            short(&main.merge)
        );
    }
    let successor = read_via(door, superseded_by)
        .await
        .map_err(|e| anyhow!("REFUSED — the successor `{superseded_by}` cannot be read: {e}"))?;
    let sid = id_of(&successor)?;
    let carried =
        core::reverted_successor(&car_json, &successor).map_err(|e| anyhow!("REFUSED — {e}"))?;
    let (revert, revert_car) = resolve_revert(door, repo, reverted_by).await?;
    let boarded = crate::train::boarded_head(&car_json);
    let reading = read_revert(repo, &main.main, &main.merge, &revert, boarded)
        .map_err(|e| anyhow!("REFUSED — could not read the revert {revert}: {e}"))?;
    if !reading.total() {
        bail!("REFUSED — {}", reading.refusal().unwrap_or_default());
    }
    let named = revert_car
        .as_deref()
        .map(|c| format!(" (car {}'s merge_ref)", short(c)))
        .unwrap_or_default();
    let evidence = format!(
        "{}; main REVERTED it in {}{named}: {}; {carried}",
        main.evidence(),
        reading.revert,
        reading.evidence()
    );
    let recorded = car_json
        .pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .unwrap_or(merge);
    if dry_run {
        println!(
            "  DRY — main reverted {} in {}{named}: would link car {} as its successor, \
             correct the landing note and close car {} through `{UNLANDED_SLUG}`",
            short(&reading.merge),
            s12(&reading.revert),
            short(&sid),
            short(&id)
        );
        return Ok(Unlanded {
            car: id,
            successor: sid,
            evidence: Some(evidence),
        });
    }

    // The successor's back-edge first: the terminal names it, and a
    // terminal naming a successor that does not name it back is half an
    // edge (`successor_of` reads `supersedes`).
    if let Some(patch) = core::supersedes_patch(&id, &successor) {
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{sid}/metadata"),
            Some(patch),
        )
        .await
        .context("linking the successor back to this car")?;
    }

    if let Some((reads, should_read)) = review_correction(
        &car_json,
        &format!(
            "merged as {recorded}, then main reverted this merge in {}{named}; its intent {}",
            s12(&reading.revert),
            // Landed only on a record of the successor's merge; an open
            // car carries the intent (review b279aa0c).
            core::successor_standing(&successor)
        ),
    ) && let Some(review) = car::find_step(&car_json, car::REVIEW_SLUG, car::REVIEW)
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
    {
        door.send(
            Method::POST,
            &format!("/api/jobs/{id}/steps/{review}/corrections"),
            Some(crate::correct::correction_body(
                "note",
                &reads,
                &should_read,
                Some(
                    "forge main reverted the train's merge (boss car unland --reverted-by, \
                     backlog 7f4694eb)",
                ),
            )),
        )
        .await
        .context("correcting the landing note")?;
    }

    if let Some(patch) = core::reverted_by_patch(&car_json, &reading.revert, revert_car.as_deref())
    {
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(patch),
        )
        .await
        .context("naming the revert on the car")?;
    }

    let current = read_via(door, &id).await?;
    let writes = unland_writes(
        &current,
        &Unlanding {
            merge_ref: recorded.to_string(),
            evidence: evidence.clone(),
            superseded_by: sid.clone(),
            completed_at: main.read_at.clone(),
        },
    )
    .map_err(|e| anyhow!("REFUSED — {e}"))?;
    apply_via(
        door,
        &id,
        &current,
        &writes,
        UNLANDED_SLUG,
        "boss car unland",
    )
    .await?;
    Ok(Unlanded {
        car: id,
        successor: sid,
        evidence: Some(evidence),
    })
}

/// THE WRITER: unland `car_id`, landed as `merge`, reading forge main
/// from the clone at `repo`. Idempotent — a car already closed through
/// `unlanded` answers with the successor it names, and every write below
/// is skipped when a pass before this one made it.
pub(crate) async fn unland_car(
    door: &dyn Door,
    repo: &Path,
    car_id: &str,
    merge: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<Unlanded> {
    let car_json = read_via(door, car_id).await?;
    let id = id_of(&car_json)?;
    if let Some(done) = already_unlanded(&car_json, &id) {
        return Ok(done);
    }
    admit(&car_json, merge)?;

    let reading = read_main(repo, merge, now)
        .map_err(|e| anyhow!("REFUSED — could not read whether main carries {merge}: {e}"))?;
    if reading.carries {
        bail!(
            "REFUSED — forge main {} carries {} (git merge-base --is-ancestor exited 0): the \
             landing stands, and there is nothing to unland",
            short(&reading.main),
            short(&reading.merge)
        );
    }
    let recorded = car_json
        .pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .unwrap_or(merge);
    let lost = format!(
        "merged as {recorded}, then lost: forge main {} at {} does not carry it — not on main",
        short(&reading.main),
        reading.read_at
    );
    if dry_run {
        println!(
            "  DRY — main does not carry {}: would open the successor for {}, correct the \
             landing note and close car {} through `{UNLANDED_SLUG}`",
            short(&reading.merge),
            car_json
                .pointer("/metadata/branch")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            short(&id)
        );
        return Ok(Unlanded {
            car: id,
            successor: String::new(),
            evidence: Some(reading.evidence()),
        });
    }

    let sid = successor(door, &car_json, recorded, &reading.read_at).await?;

    if let Some((reads, should_read)) =
        review_correction(&car_json, &format!("{lost}; rides again as car {sid}"))
        && let Some(review) = car::find_step(&car_json, car::REVIEW_SLUG, car::REVIEW)
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
    {
        door.send(
            Method::POST,
            &format!("/api/jobs/{id}/steps/{review}/corrections"),
            Some(crate::correct::correction_body(
                "note",
                &reads,
                &should_read,
                Some("forge main lost the train's merge (boss car unland, backlog f9256445)"),
            )),
        )
        .await
        .context("correcting the landing note")?;
    }

    let current = read_via(door, &id).await?;
    let writes = unland_writes(
        &current,
        &Unlanding {
            merge_ref: recorded.to_string(),
            evidence: reading.evidence(),
            superseded_by: sid.clone(),
            completed_at: reading.read_at.clone(),
        },
    )
    .map_err(|e| anyhow!("REFUSED — {e}"))?;
    apply_via(
        door,
        &id,
        &current,
        &writes,
        UNLANDED_SLUG,
        "boss car unland",
    )
    .await?;
    Ok(Unlanded {
        car: id,
        successor: sid,
        evidence: Some(reading.evidence()),
    })
}

/// `boss car unland`.
pub(crate) async fn unland(
    given: &str,
    merge_ref: &str,
    reverted_by: Option<&str>,
    superseded_by: Option<&str>,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let reverted = mode(reverted_by, superseded_by)?;
    let http = crate::sor_client::machine_client()?;
    // The actor FIRST: a write nobody names is refused, and the refusal
    // costs a line rather than a half-unlanded car (5083d6f5).
    if !dry_run {
        crate::identity::sign(&Method::PATCH, "/api/jobs")?;
    }
    let (found, branch) = crate::rerail::find_car(&http, given).await?;
    let id = id_of(&found)?;
    println!("boss car unland: car {} ({branch})", short(&id));
    // A car pinned to a version without the terminal is MOVED first —
    // through the conversion door, which previews, refuses an unsafe
    // move by step, and records the re-pin on the packet (7cf202a9).
    if car::find_step(&found, UNLANDED_SLUG, "").is_none() {
        println!(
            "  car {} is pinned to v{} with no `{UNLANDED_SLUG}` terminal — converting it",
            short(&id),
            found["workflow_version"]
        );
        crate::job::convert(&id, None, dry_run).await?;
        if dry_run {
            println!("boss car unland: DRY — nothing written past the conversion preview");
            return Ok(());
        }
    }
    if let Some((reverted_by, superseded_by)) = reverted {
        let done = unland_reverted(
            &Http(&http),
            Path::new("."),
            &id,
            merge_ref,
            reverted_by,
            superseded_by,
            dry_run,
            now,
        )
        .await
        .map_err(|e| anyhow!("boss car unland: {e}"))?;
        if let Some(evidence) = &done.evidence {
            println!("  evidence: {evidence}");
        }
        if !dry_run {
            println!(
                "  closed through `{UNLANDED_SLUG}`; its intent rides as car {} — the item \
                 it answered closes with that car, not with this one",
                short(&done.successor)
            );
        }
        return Ok(());
    }
    let done = unland_car(&Http(&http), Path::new("."), &id, merge_ref, dry_run, now)
        .await
        .map_err(|e| anyhow!("boss car unland: {e}"))?;
    if let Some(evidence) = &done.evidence {
        println!("  evidence: {evidence}");
    }
    if !dry_run {
        println!(
            "  successor: car {} for {branch} — its first open work is `gate`: rebase or \
             merge it onto current main and gate it with the park flags (boss gate {branch} \
             --park-file …), and the auto-park handler adopts the green onto it",
            short(&done.successor)
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn git_in(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The forge (a bare repo whose main is at `base`) and a clone of it
    /// that has fetched a merge on top — then main is rewound to `base`
    /// with a force push, the way 2026-09-25's main lost c85941b4.
    /// Returns (clone, base, merge).
    pub(crate) fn forge_that_lost_a_merge(tag: &str) -> (std::path::PathBuf, String, String) {
        let root = boss_testing::scratch::scratch_dir(tag);
        let forge = root.join("forge.git");
        let clone = root.join("clone");
        let work = root.join("work");
        std::fs::create_dir_all(&forge).unwrap();
        git_in(&forge, &["init", "-q", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&work).unwrap();
        git_in(&work, &["init", "-q", "-b", "main"]);
        boss_testing::scratch::write_file(&work.join("a.txt"), "base\n");
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "the 19:26 train"]);
        let base = git_in(&work, &["rev-parse", "HEAD"]);
        git_in(&work, &["remote", "add", "origin", forge.to_str().unwrap()]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        std::fs::create_dir_all(&clone).unwrap();
        git_in(&clone, &["clone", "-q", forge.to_str().unwrap(), "."]);
        boss_testing::scratch::write_file(&work.join("a.txt"), "base\nthe car\n");
        git_in(&work, &["commit", "-qam", "train 20:04 (#687)"]);
        let merge = git_in(&work, &["rev-parse", "HEAD"]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        (clone, base, merge)
    }

    pub(crate) fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-25T21:19:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The git the merge-lost arm reads forge main with carries the
    /// conductor's forge credential. Backlog cb3d8952, measured
    /// 2026-09-26 01:10Z: `fn git` was a bare `git`, so inside the
    /// conductor pod — which has only the token file git_auth reads —
    /// every `ls-remote origin` exited 128 "could not read Username",
    /// the arm never judged, and train c94d5d39 could not close. The
    /// same pass's `fetch origin` succeeded through `sh_in`, which builds
    /// on git_auth: the credential is host-scoped, so the remote's name
    /// was never the fault. The tests above use a local bare forge that
    /// asks no credential, which is why they could not see it.
    #[test]
    fn the_merge_lost_arm_reads_forge_main_with_the_forge_credential() {
        let root = boss_testing::scratch::scratch_dir("unland-auth");
        git_in(&root, &["init", "-q"]);
        let auth = crate::git_auth::ForgeAuth {
            forge: "http://forge.test/".to_string(),
            value: "Authorization: token t".to_string(),
        };
        let out = git_command(&root, Some(&auth))
            .args(["config", "--get", &auth.header_key()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the credential is not on the arm's git: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Authorization: token t"
        );
        // And `-C <repo>` still lands it in the clone it was given.
        let top = git_command(&root, None)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .unwrap();
        assert_eq!(
            std::path::PathBuf::from(String::from_utf8_lossy(&top.stdout).trim()),
            root.canonicalize().unwrap().join(".git")
        );
    }

    /// The pin for the half the test above cannot reach (CLAUDE.md 9a):
    /// every git this module runs outside its tests is built by
    /// `git_command`, so no second, bare spawner can come back.
    #[test]
    fn no_git_in_this_module_is_spawned_bare() {
        let src = include_str!("car_unland.rs");
        let live = src.split("#[cfg(test)]").next().unwrap();
        let bare = format!("Command::new({:?})", "git");
        assert!(
            !live.contains(&bare),
            "car_unland.rs spawns a bare git outside its tests — build it with git_command, \
             which carries the forge credential (backlog cb3d8952)"
        );
        assert!(live.contains("crate::git_auth::command_with("));
    }

    /// While main carries the merge the reading says so; after the
    /// rewind it reads NOT — and it reads it even though the clone never
    /// fetched the merge, because the full sha is fetched by sha.
    #[test]
    fn a_merge_main_lost_reads_not_carried_and_a_carried_one_reads_carried() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-read");
        let carried = read_main(&clone, &merge, now()).unwrap();
        assert!(carried.carries, "{carried:?}");
        assert_eq!(carried.main, merge);

        // The rewind: main written back to `base`, a force push.
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let fresh = clone.parent().unwrap().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git_in(
            &fresh,
            &[
                "clone",
                "-q",
                "--no-local",
                clone.parent().unwrap().join("forge.git").to_str().unwrap(),
                ".",
            ],
        );
        // `fresh` never saw the merge: the full sha is fetched by sha.
        let lost = read_main(&fresh, &merge, now()).unwrap();
        assert!(!lost.carries, "{lost:?}");
        assert_eq!(lost.main, base);
        assert_eq!(lost.merge, merge);
        assert!(
            lost.evidence().contains(&format!(
                "git merge-base --is-ancestor {merge} {base} exited 1"
            )),
            "{}",
            lost.evidence()
        );
        assert!(lost.evidence().contains("at 2026-09-25T21:19:00Z"));
    }

    /// A git failure is a refusal to judge, never a NOT: an abbreviated
    /// merge the clone cannot resolve, and a clone with no forge.
    #[test]
    fn a_reading_git_cannot_make_is_refused_not_read_as_lost() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-refuse");
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        // A clone made after the rewind never saw the merge, and its
        // 12-character spelling cannot be fetched. `--no-local`: a local
        // clone copies the object directory whole, unreachable merge and
        // all, and would answer the question this test asks.
        let fresh = clone.parent().unwrap().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git_in(
            &fresh,
            &[
                "clone",
                "-q",
                "--no-local",
                clone.parent().unwrap().join("forge.git").to_str().unwrap(),
                ".",
            ],
        );
        let e = read_main(&fresh, &merge[..12], now()).unwrap_err();
        assert!(e.contains("name it by its full sha"), "{e}");

        let orphan = clone.parent().unwrap().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        git_in(&orphan, &["init", "-q", "-b", "main"]);
        let e = read_main(&orphan, &merge, now()).unwrap_err();
        assert!(e.contains("ls-remote"), "{e}");
    }

    // -- the writer, end to end ---------------------------------------
    //
    // The real jobs router with the real platform bundle on a local
    // socket, and a real forge that lost a merge: what `unland_car`
    // writes is read back off the packets, not off a stub's log.

    struct AdminRoster;

    #[async_trait::async_trait]
    impl boss_jobs::owner_resolution::RosterLookup for AdminRoster {
        async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
            Ok(match role {
                "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
                _ => Vec::new(),
            })
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(id == "emp-bootstrap-admin")
        }
    }

    /// The one robot [`serve`] registers: it records the model class the
    /// bundle's backlog-item `build` demands, so a test whose executor
    /// claims that step signs as a robot the claim door admits.
    pub(crate) const BUILDER_ROBOT: &str = "agent-builder";

    pub(crate) async fn serve() -> String {
        use boss_core::publish::PublishMode;
        use boss_core::publisher::EventStamp;
        use boss_jobs::WorkflowRegistry;
        use boss_jobs::agents::AgentsRegistry;
        use boss_jobs::pools::PoolRegistry;
        use boss_jobs::step_executor::{ExecutorDemand, demanded_by};
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        use std::sync::Arc;
        let kinds = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
        let platform = boss_jobs::registry::seedable_platform_workflows();
        // THE EXECUTORS THE BUNDLE NAMES (design ccfe87ff pass item 1,
        // backlog 446ab559). backlog-item `build` names a model class, so
        // the claim door admits only a robot whose agents row records it,
        // judged against the active `robots:<class>` pools. The bundle
        // is served with the pools the tree declares and one robot of the
        // class the row demands — read off the row, so the fixture follows
        // a change of class rather than pinning a second copy of it.
        let class = platform
            .iter()
            .find(|w| w.kind == "backlog-item")
            .and_then(|w| demanded_by(w, Some("build")).expect("a well-formed declaration"))
            .and_then(|demand| match demand {
                // A set admits a robot of any class in it; the first is one.
                ExecutorDemand::Model(classes) => classes.into_iter().next(),
                _ => None,
            });
        for spec in platform {
            kinds.seed(spec).expect("seed platform kind");
        }
        let test = boss_core::actor::ActorId::Automation("test".into());
        let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
        for spec in boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
            .expect("the platform pools parse")
        {
            pools
                .publish_declared(spec, &test, chrono::Utc::now())
                .await
                .expect("seed platform pool");
        }
        let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new());
        agents
            .publish(
                &[boss_jobs::agents::types::AgentInput {
                    factory_rotation: Some(true),
                    id: BUILDER_ROBOT.into(),
                    display_name: "Builder".into(),
                    default_model: "opus-5-5".into(),
                    model_class: class,
                    aliases: Vec::new(),
                    role: Some("platform-admin".into()),
                    department: None,
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: None,
                    stations: None,
                }],
                PublishMode::Take,
                &EventStamp::new("jobs", test),
            )
            .await
            .expect("the builder robot registers");
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new().with_resource_pools(pools));
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Create, Resource::job()),
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
            // The hand close, as platform-admin holds it in the policy
            // defaults: the reverted-merge fixture closes its revert car
            // and its successor merged (backlog 7f4694eb).
            (Action::Close, Resource::job()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            roster: Some(Arc::new(AdminRoster)),
            agent_budget: Some(Arc::new(boss_jobs::agent_budget::BudgetDoor {
                agents,
                runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(Vec::new())),
            })),
            ..boss_jobs::http::JobsApiState::minimal(
                jobs,
                bus,
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = boss_jobs::http::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// A car the conductor landed as `merge`: scope, build, gate and
    /// review completed (review with the conductor's landing note), and
    /// the `merged` marker on the job.
    pub(crate) async fn landed(door: &dyn Door, branch: &str, merge: &str) -> String {
        let made = door
            .send(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "ship-a-change",
                    "subject": {"subject_kind": "custom", "id": branch},
                    "title": "A stamp cannot land on a moved shape",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "tags": [],
                    "metadata": {"branch": branch, "summary": "A stamp cannot land."},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        for (slug, md) in [
            (
                "scope",
                json!({"summary": "A stamp cannot land.", "excludes": "the UI"}),
            ),
            ("build", json!({"test": "12 tests"})),
            ("gate", json!({"gates": "auto", "verified": "green"})),
            (
                "review",
                json!({"pr_url": "x/pulls/687",
                       "note": format!("landed on main as {}", &merge[..12])}),
            ),
        ] {
            let car = read_via(door, &id).await.unwrap();
            let sid = car::find_step(&car, slug, "").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            door.send(
                Method::PATCH,
                &format!("/api/jobs/{id}/steps/{sid}/metadata"),
                Some(md),
            )
            .await
            .unwrap();
            door.send(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{sid}"),
                Some(json!({"status": "completed"})),
            )
            .await
            .unwrap();
        }
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(json!({"merged": "true", "merge_ref": &merge[..12]})),
        )
        .await
        .unwrap();
        id
    }

    /// dec3136a's repair, end to end: the verb reads main NOT carrying
    /// the merge, opens the successor at `gate`, corrects the landing
    /// note, and closes the car through `unlanded` — and a second run
    /// writes nothing new and names the same successor.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lost_landing_is_unlanded_once_with_a_successor_at_gate() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-e2e");
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let wire = crate::steps::Wire::at(serve().await, admin_caller()).unwrap();
        let branch = "fix/a-stamp-cannot-land-on-a-moved-shape";
        let id = landed(&wire, branch, &merge).await;

        // By its FULL sha: this clone was made before the merge and never
        // fetched it — the conductor clone's own shape on 2026-09-25,
        // when main lost the merge inside one pass — so it is fetched by
        // sha. The car's 12-character `merge_ref` still names it.
        let done = unland_car(&wire, &clone, &id, &merge, false, now())
            .await
            .expect("unlanded");
        assert_eq!(done.car, id);
        let car_after = read_via(&wire, &id).await.unwrap();
        assert_eq!(car_after["status"], "closed", "{car_after:#}");
        assert_eq!(car_after["metadata"]["outcome"], "unlanded");
        assert_eq!(car_after["metadata"]["merged"], "true", "both facts stay");
        assert_eq!(
            car_after["metadata"]["superseded_by"],
            done.successor.as_str()
        );
        let step = car::find_step(&car_after, "unlanded", "").unwrap();
        assert!(
            step["metadata"]["evidence"]
                .as_str()
                .unwrap()
                .contains("exited 1"),
            "{step:#}"
        );
        let review = car::find_step(&car_after, "review", "").unwrap();
        assert_eq!(
            review["corrections"][0]["field"], "note",
            "the landing note is corrected beside itself: {review:#}"
        );

        let next = read_via(&wire, &done.successor).await.unwrap();
        assert_eq!(next["metadata"]["supersedes"], id.as_str());
        assert_eq!(next["metadata"]["branch"], branch);
        assert_eq!(
            car::find_step(&next, "gate", "").unwrap()["status"],
            "ready"
        );

        let again = unland_car(&wire, &clone, &id, &merge, false, now())
            .await
            .expect("a re-run answers");
        assert_eq!(again.successor, done.successor, "no second successor");
        assert_eq!(again.evidence, None, "and no second reading");
    }

    /// The verb does not take the caller's word: while main carries the
    /// merge it refuses, and nothing is written.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_landing_main_still_carries_is_refused_and_nothing_is_written() {
        let (clone, _base, merge) = forge_that_lost_a_merge("unland-carried");
        let wire = crate::steps::Wire::at(serve().await, admin_caller()).unwrap();
        let id = landed(&wire, "fix/still-on-main", &merge).await;
        let e = unland_car(&wire, &clone, &id, &merge[..12], false, now())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("the landing stands"), "{e}");
        let untouched = read_via(&wire, &id).await.unwrap();
        assert_eq!(untouched["status"], "open");
        assert!(untouched["metadata"].get("superseded_by").is_none());
    }

    // -- the second reading: a merge main REVERTED (backlog 7f4694eb) ----

    /// How much of the car the fixture's revert undoes.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Undo {
        /// Both of the car's paths: `a.txt`'s line back, `c.txt` gone.
        All,
        /// Only `a.txt`: `c.txt` still stands on main.
        OnlyA,
    }

    /// A forge whose main carries a merge AND a later revert of it — car
    /// 526974b0's shape (#1784 merged, #1787 reverted). The merge is a
    /// squash of TWO cars: ours (`a.txt` line 2, new `c.txt`, boarded
    /// from branch `fix/car`) and another car's `d.txt`, which the revert
    /// leaves alone. A train between them changes `a.txt` line 9, so
    /// `a.txt` at the revert differs from before the merge and only the
    /// patch-id reading can see that its line 2 was undone. A side branch
    /// carries a full revert main never took.
    struct Reverted {
        clone: std::path::PathBuf,
        base: String,
        merge: String,
        boarded: String,
        revert: String,
        side: String,
    }

    fn forge_that_reverted_a_merge(tag: &str, undo: Undo) -> Reverted {
        let root = boss_testing::scratch::scratch_dir(tag);
        let (forge, work, clone) = (
            root.join("forge.git"),
            root.join("work"),
            root.join("clone"),
        );
        std::fs::create_dir_all(&forge).unwrap();
        git_in(&forge, &["init", "-q", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&work).unwrap();
        git_in(&work, &["init", "-q", "-b", "main"]);
        let lines =
            |two: &str, nine: &str| format!("1\n{two}\n3\n4\n5\n6\n7\n8\n{nine}\n10\n11\n12\n");
        let write = |p: &str, s: &str| boss_testing::scratch::write_file(&work.join(p), s);
        write("a.txt", &lines("2", "9"));
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "the 03:14 train"]);
        let base = git_in(&work, &["rev-parse", "HEAD"]);
        // Our car, on its own branch: the head the train boarded.
        git_in(&work, &["checkout", "-qb", "fix/car"]);
        write("a.txt", &lines("two", "9"));
        write("c.txt", "the car's new file\n");
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "pin postgres-0 to w-3"]);
        let boarded = git_in(&work, &["rev-parse", "HEAD"]);
        // The train: both cars squashed into one merge on main.
        git_in(&work, &["checkout", "-q", "main"]);
        git_in(&work, &["checkout", "fix/car", "--", "a.txt", "c.txt"]);
        write("d.txt", "another car's file\n");
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "train 03:48 (#1784)"]);
        let merge = git_in(&work, &["rev-parse", "HEAD"]);
        // A train between the merge and the revert.
        write("a.txt", &lines("two", "nine"));
        git_in(&work, &["commit", "-qam", "train 05:00 (#1786)"]);
        // A full revert main never took.
        git_in(&work, &["checkout", "-qb", "side"]);
        write("a.txt", &lines("2", "nine"));
        git_in(&work, &["rm", "-q", "c.txt"]);
        git_in(&work, &["commit", "-qam", "a revert on a side branch"]);
        let side = git_in(&work, &["rev-parse", "HEAD"]);
        // The revert main took.
        git_in(&work, &["checkout", "-q", "main"]);
        write("a.txt", &lines("2", "nine"));
        if undo == Undo::All {
            git_in(&work, &["rm", "-q", "c.txt"]);
        }
        git_in(
            &work,
            &["commit", "-qam", "train 07:20 (#1787): revert #1784's car"],
        );
        let revert = git_in(&work, &["rev-parse", "HEAD"]);
        write("e.txt", "a later train\n");
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "train 18:51 (#1798)"]);
        git_in(&work, &["remote", "add", "origin", forge.to_str().unwrap()]);
        git_in(&work, &["push", "-q", "origin", "main", "fix/car", "side"]);
        std::fs::create_dir_all(&clone).unwrap();
        git_in(&clone, &["clone", "-q", forge.to_str().unwrap(), "."]);
        Reverted {
            clone,
            base,
            merge,
            boarded,
            revert,
            side,
        }
    }

    fn forge_main(fx: &Reverted) -> String {
        git_in(&fx.clone, &["rev-parse", "origin/main"])
    }

    /// THE REUSABLE READING: a revert main carries, descending from the
    /// merge, undoing every path of the car's OWN boarded diff — `c.txt`
    /// by an empty diff against before the merge, `a.txt` by patch-id
    /// (a later train changed it), and the other car's `d.txt` outside
    /// the question.
    #[test]
    fn a_total_revert_of_the_cars_own_paths_reads_total() {
        let fx = forge_that_reverted_a_merge("unland-revert-total", Undo::All);
        let r = read_revert(
            &fx.clone,
            &forge_main(&fx),
            &fx.merge,
            &fx.revert[..12],
            Some(&fx.boarded),
        )
        .unwrap();
        assert!(r.total(), "{r:?}");
        assert_eq!(r.revert, fx.revert);
        assert_eq!(r.paths, vec!["a.txt".to_string(), "c.txt".to_string()]);
        assert_eq!(r.refusal(), None);
        let e = r.evidence();
        assert!(e.contains("a.txt (patch-id "), "{e}");
        assert!(e.contains("c.txt (git diff"), "{e}");
        assert!(!e.contains("d.txt"), "another car's path is not ours: {e}");
        assert!(
            e.contains(&format!("boarded head {}", &fx.boarded[..12])),
            "{e}"
        );

        // With no boarded head the whole merge is the question, and the
        // other car's d.txt still stands: refused, naming it.
        let whole = read_revert(&fx.clone, &forge_main(&fx), &fx.merge, &fx.revert, None).unwrap();
        assert_eq!(whole.unreverted, vec!["d.txt".to_string()]);
        assert!(whole.refusal().unwrap().contains("d.txt"));
    }

    /// A revert that undoes only part of the car is refused, naming the
    /// path main still carries.
    #[test]
    fn a_partial_revert_is_refused_naming_the_path_main_still_carries() {
        let fx = forge_that_reverted_a_merge("unland-revert-partial", Undo::OnlyA);
        let r = read_revert(
            &fx.clone,
            &forge_main(&fx),
            &fx.merge,
            &fx.revert,
            Some(&fx.boarded),
        )
        .unwrap();
        assert!(!r.total());
        assert_eq!(r.unreverted, vec!["c.txt".to_string()]);
        let e = r.refusal().unwrap();
        assert!(e.contains("partial") && e.contains("c.txt"), "{e}");
    }

    /// A revert main does not carry, and a commit that does not descend
    /// from the merge (it cannot have reverted it), are each refused.
    #[test]
    fn a_revert_off_main_or_before_the_merge_is_refused() {
        let fx = forge_that_reverted_a_merge("unland-revert-where", Undo::All);
        let main = forge_main(&fx);
        let off = read_revert(&fx.clone, &main, &fx.merge, &fx.side, Some(&fx.boarded)).unwrap();
        assert!(!off.on_main && off.descends, "{off:?}");
        assert!(
            off.refusal()
                .unwrap()
                .contains("not an ancestor of forge main")
        );

        let before = read_revert(&fx.clone, &main, &fx.merge, &fx.base, Some(&fx.boarded)).unwrap();
        assert!(before.on_main && !before.descends, "{before:?}");
        assert!(
            before
                .refusal()
                .unwrap()
                .contains("does not descend from the merge")
        );

        let itself =
            read_revert(&fx.clone, &main, &fx.merge, &fx.merge, Some(&fx.boarded)).unwrap();
        assert!(!itself.descends, "a merge does not revert itself");
    }

    /// `--reverted-by` and `--superseded-by` come as a pair: the clap
    /// line refuses one alone, and so does the verb's own mode reading.
    #[test]
    fn reverted_by_without_superseded_by_is_refused() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            let mut argv = vec![
                "boss",
                "car",
                "unland",
                "526974b0",
                "--merge-ref",
                "d6c3a70378e8",
            ];
            argv.extend_from_slice(extra);
            crate::Cli::try_parse_from(argv)
        };
        assert!(parse(&[]).is_ok(), "the lost-merge line parses unchanged");
        assert!(parse(&["--reverted-by", "6c272983", "--superseded-by", "bfa6f7be"]).is_ok());
        assert!(parse(&["--reverted-by", "6c272983"]).is_err());
        assert!(parse(&["--superseded-by", "bfa6f7be"]).is_err());

        assert_eq!(mode(None, None).unwrap(), None);
        assert_eq!(
            mode(Some("6c272983"), Some("bfa6f7be")).unwrap(),
            Some(("6c272983", "bfa6f7be"))
        );
        let e = mode(Some("6c272983"), None).unwrap_err().to_string();
        assert!(e.contains("--superseded-by"), "{e}");
    }

    /// A packet the in-memory router holds, closed with `outcome` — the
    /// hand close, the one write that names an outcome with the close.
    async fn close_as(door: &dyn Door, id: &str, outcome: &str) {
        let mut job = read_via(door, id).await.unwrap();
        job["status"] = json!("closed");
        job["metadata"]["outcome"] = json!(outcome);
        door.send(Method::PUT, &format!("/api/jobs/{id}"), Some(job))
            .await
            .unwrap();
    }

    async fn file_item(door: &dyn Door) -> String {
        let made = door
            .send(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "backlog-item", "title": "postgres runs on the data node",
                    "owner_id": "emp-bootstrap-admin",
                    "subject": {"subject_kind": "custom", "id": "postgres-placement"},
                    "priority": "urgent", "status": "open", "tags": [], "metadata": {},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        made["id"].as_str().unwrap().to_string()
    }

    async fn car_count(door: &dyn Door) -> usize {
        let page = door
            .send(Method::GET, "/api/jobs?kind=ship-a-change&limit=100", None)
            .await
            .unwrap()
            .unwrap();
        crate::train::rows(Some(page)).unwrap().len()
    }

    /// 526974b0, its revert car and its landed successor, on the real
    /// router: (car, revert car, successor).
    async fn reverted_cars(door: &dyn Door, fx: &Reverted) -> (String, String, String) {
        let item = file_item(door).await;
        let id = landed(door, "fix/postgres-runs-on-the-data-node", &fx.merge).await;
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(json!({"backlog_item": item, "boarded_head": fx.boarded})),
        )
        .await
        .unwrap();
        let revert = landed(door, "fix/revert-the-pin", &fx.revert).await;
        close_as(door, &revert, "merged").await;
        let successor = landed(door, "fix/pin-postgres-by-purpose", &fx.revert).await;
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{successor}/metadata"),
            Some(json!({"backlog_item": item})),
        )
        .await
        .unwrap();
        close_as(door, &successor, "merged").await;
        (id, revert, successor)
    }

    /// 526974b0's repair, end to end: the merge IS on main, a landed
    /// revert car undid all of it, and the landed successor is NAMED —
    /// the car closes through `unlanded` naming both, the successor gains
    /// its `supersedes` back-edge, no second car is opened, and a re-run
    /// writes nothing new.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_merge_main_reverted_closes_through_unlanded_naming_its_landed_successor() {
        let fx = forge_that_reverted_a_merge("unland-reverted-e2e", Undo::All);
        let wire = crate::steps::Wire::at(serve().await, admin_caller()).unwrap();
        let (id, revert_car, successor) = reverted_cars(&wire, &fx).await;
        let before = car_count(&wire).await;

        let done = unland_reverted(
            &wire,
            &fx.clone,
            &id,
            &fx.merge[..12],
            &revert_car[..8],
            &successor[..8],
            false,
            now(),
        )
        .await
        .expect("unlanded");
        assert_eq!(done.successor, successor, "the NAMED successor, full id");
        assert_eq!(car_count(&wire).await, before, "no second car is opened");

        let car_after = read_via(&wire, &id).await.unwrap();
        assert_eq!(car_after["status"], "closed", "{car_after:#}");
        assert_eq!(car_after["metadata"]["outcome"], "unlanded");
        assert_eq!(car_after["metadata"]["merged"], "true", "both facts stay");
        assert_eq!(car_after["metadata"]["superseded_by"], successor.as_str());
        assert_eq!(car_after["metadata"]["reverted_by"], fx.revert.as_str());
        assert_eq!(
            car_after["metadata"]["reverted_by_car"],
            revert_car.as_str()
        );
        let step = car::find_step(&car_after, "unlanded", "").unwrap();
        let evidence = step["metadata"]["evidence"].as_str().unwrap();
        for want in [
            "exited 0",
            &fx.revert[..12],
            &format!("car {}", &revert_car[..8]),
            "a.txt (patch-id ",
            "c.txt",
            &format!("car {}", &successor[..8]),
        ] {
            assert!(evidence.contains(want), "{want:?} not in {evidence}");
        }
        let review = car::find_step(&car_after, "review", "").unwrap();
        assert_eq!(review["corrections"][0]["field"], "note", "{review:#}");
        let correction = review["corrections"][0].to_string();
        assert!(
            correction.contains("main reverted this merge in"),
            "{review:#}"
        );
        // Closed merged IS the record of a landing, so the note may say it.
        assert!(
            correction.contains(&format!("landed as car {}", &successor[..8])),
            "{review:#}"
        );
        let next = read_via(&wire, &successor).await.unwrap();
        assert_eq!(next["metadata"]["supersedes"], id.as_str());
        assert_eq!(next["status"], "closed", "the successor is left as it was");

        let again = unland_reverted(
            &wire,
            &fx.clone,
            &id,
            &fx.merge[..12],
            &revert_car[..8],
            &successor[..8],
            false,
            now(),
        )
        .await
        .expect("a re-run answers");
        assert_eq!(again.successor, successor);
        assert_eq!(again.evidence, None, "and no second reading");
        assert_eq!(car_count(&wire).await, before);
    }

    /// A successor that has NOT landed is admitted — a fresh car filed for
    /// the same item, no `merged` marker, no merge_ref — and the record
    /// says only what is true of it: its intent rides as that car. Review
    /// b279aa0c (CHANGES, 2026-11-20) measured the correction stating
    /// "its intent landed as car <successor>" for exactly this car, an
    /// immutable claim of a delivery nothing had observed.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_open_unmerged_successor_is_not_recorded_as_landed() {
        let fx = forge_that_reverted_a_merge("unland-reverted-open", Undo::All);
        let wire = crate::steps::Wire::at(serve().await, admin_caller()).unwrap();
        // reverted_cars also lands a successor; this test names a fresh one.
        let (id, revert_car, _landed) = reverted_cars(&wire, &fx).await;
        let item = read_via(&wire, &id).await.unwrap()["metadata"]["backlog_item"]
            .as_str()
            .unwrap()
            .to_string();
        let fresh = wire
            .send(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "ship-a-change",
                    "subject": {"subject_kind": "custom", "id": "fix/pin-postgres-again"},
                    "title": "Pin postgres by purpose, again",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "tags": [],
                    "metadata": {"branch": "fix/pin-postgres-again", "backlog_item": item},
                })),
            )
            .await
            .unwrap()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let done = unland_reverted(
            &wire,
            &fx.clone,
            &id,
            &fx.merge[..12],
            &revert_car[..8],
            &fresh,
            false,
            now(),
        )
        .await
        .expect("an open successor is admitted");
        assert_eq!(done.successor, fresh);

        let next = read_via(&wire, &fresh).await.unwrap();
        assert_eq!(next["status"], "open", "the successor is left as it was");
        assert!(next["metadata"].get("merged").is_none(), "{next:#}");

        let car_after = read_via(&wire, &id).await.unwrap();
        let review = car::find_step(&car_after, "review", "").unwrap();
        let correction = review["corrections"][0].to_string();
        let evidence = car::find_step(&car_after, "unlanded", "").unwrap()["metadata"]["evidence"]
            .as_str()
            .unwrap()
            .to_string();
        for said in [&correction, &evidence] {
            assert!(
                !said.contains("landed as car"),
                "an unmerged car is not a landing: {said}"
            );
            assert!(
                said.contains(&format!("rides as car {}", &fresh[..8])),
                "{said}"
            );
        }
    }

    /// Each refusal writes nothing: a partial revert (naming the path), a
    /// revert main does not carry, a commit that does not descend from
    /// the merge — and a merge main LOST, which is the other reading.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revert_that_does_not_undo_the_car_is_refused_and_nothing_is_written() {
        let partial = forge_that_reverted_a_merge("unland-reverted-partial", Undo::OnlyA);
        let wire = crate::steps::Wire::at(serve().await, admin_caller()).unwrap();
        let (id, _revert_car, successor) = reverted_cars(&wire, &partial).await;
        for (revert, want) in [
            (&partial.revert, "c.txt"),
            (&partial.side, "not an ancestor of forge main"),
            (&partial.base, "does not descend from the merge"),
        ] {
            let e = unland_reverted(
                &wire,
                &partial.clone,
                &id,
                &partial.merge,
                revert,
                &successor,
                false,
                now(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(e.contains(want), "{want:?} not in {e}");
            let car_now = read_via(&wire, &id).await.unwrap();
            assert_eq!(car_now["status"], "open", "{e}");
            assert!(car_now["metadata"].get("superseded_by").is_none(), "{e}");
            assert!(car_now["metadata"].get("reverted_by").is_none(), "{e}");
            let next = read_via(&wire, &successor).await.unwrap();
            assert!(next["metadata"].get("supersedes").is_none(), "{e}");
        }

        // A merge main LOST is the first reading's, and this one says so.
        let (clone, base, merge) = forge_that_lost_a_merge("unland-reverted-lost");
        git_in(
            &clone.parent().unwrap().join("work"),
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let lost = landed(&wire, "fix/lost", &merge).await;
        let e = unland_reverted(
            &wire,
            &clone,
            &lost,
            &merge,
            &base,
            &successor,
            false,
            now(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("without --reverted-by"), "{e}");
    }

    /// The signer the roster and the policy know.
    fn admin_caller() -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: "emp-bootstrap-admin".into(),
            source: crate::identity::Source::Env,
        })
    }
}
