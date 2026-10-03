//! `boss rerail <car>` — a conflict-skipped car back aboard, with the
//! traps encoded.
//!
//! WHY THIS IS A VERB (e7c86455). When boarding skips a car on a merge
//! conflict, the by-hand recovery is ten steps: fetch, worktree,
//! rebase, resolve, gate the new head, transcribe the receipt, repoint
//! the packet, re-board. Done for 7 cars on 2026-08-15, one on 08-23,
//! one on 08-30 — each time re-walking three traps that have each cost
//! real time:
//!
//!   - THE BRANCH MUST BE NEW. A force-push to the old branch is
//!     classifier-blocked (and rightly: it yanks a ref others hold),
//!     so the rebased tree goes to `<branch>-rerail` — the 08-15
//!     precedent, now the verb's contract.
//!   - THE GATE STEP IS FROZEN. A car's gate receipt vouches for ONE
//!     head; superseding it means writing `metadata.regate_receipt`
//!     on the JOB, never editing the completed step (the boarding
//!     logic and `boss receipt` both prefer regate_receipt).
//!   - THE RECEIPT IS MACHINE-COPIED. Never retyped, never
//!     hand-authored (never-write-a-sha-you-did-not-read): it is read
//!     back from the gate-run packet by `park::receipt_for`, byte for
//!     byte.
//!
//! The verb stops for a human at exactly one place — a real conflict
//! hunk — and hands back the worktree with the remaining sequence
//! printed, finishable with `--finish` once the branch is pushed.
//!
//! `--finish` RUNS AS OFTEN AS THE HEAD MOVES (02165b1d). It used to
//! derive `<car branch>-rerail` unconditionally, which meant it worked
//! exactly once per car: run again on an already-re-railed branch it
//! hunted for `…-rerail-rerail` and refused, leaving no designed way to
//! put a fresh receipt on a car whose head had moved — and the
//! improvised way (re-gate with `--park-*`) filed a twin car. It now
//! asks the forge: a rerail in flight is finished onto its branch, and
//! with no such branch `--finish` REFRESHES the car where it stands.
//! Either way the receipt comes from `park::receipt_for`, so it refuses
//! unless a green vouches for the branch's head right now.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::gate;
use crate::park;

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

/// The car packet for `given` (branch or 8+ chars of id), plus its
/// branch. Reads open ship-a-change packets the same way park does.
pub(crate) async fn find_car(
    http: &boss_core::machine_token::Client,
    given: &str,
) -> Result<(Value, String)> {
    // Read EVERY open car, not just page one: a rerail target opened
    // days ago sorts to the tail of `opened_on DESC`, and a bare
    // `limit=` read left it off page one and reported it "not found"
    // (a-limit-is-not-a-filter). `gate::all_open_cars` pages on `total`.
    let cars = gate::all_open_cars(http).await?;
    select_car(&cars, given)
}

/// The car for `given` (branch, or 8+ chars of id), plus its branch —
/// pure, over the fully-gathered open cars, so branch-or-id resolution
/// (and that it reaches a car past page one) is testable without a live
/// API. Resolves the same way `park` does.
fn select_car(cars: &[Value], given: &str) -> Result<(Value, String)> {
    let by_branch: Vec<&Value> = cars
        .iter()
        .filter(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(given))
        .collect();
    let car = if let [one] = by_branch.as_slice() {
        (*one).clone()
    } else {
        let id = park::resolve_job_id(cars, given)?;
        cars.iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned()
            .ok_or_else(|| anyhow!("resolved {id} but it vanished from the list"))?
    };
    let branch = car
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car carries no metadata.branch — nothing to rerail"))?
        .to_string();
    Ok((car, branch))
}

/// The head the forge carries for this branch, or `None` when it
/// carries no such branch. Asked of the REMOTE, not of a local ref, so
/// it is true without a fetch — `--finish` never fetches.
fn forge_head(branch: &str) -> Result<Option<String>> {
    Ok(git(
        ".",
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )?
    .lines()
    .find_map(|l| l.split_whitespace().next())
    .map(str::to_string))
}

/// Does the forge already carry this branch? The one probe both the
/// rebase path (refusing to cut over an in-flight rerail) and the
/// finish path (deciding whether there IS a rerail to finish) ask —
/// the same read as `forge_head`, so the two cannot disagree.
fn on_forge(branch: &str) -> Result<bool> {
    Ok(forge_head(branch)?.is_some())
}

/// PURE: which branch `--finish` finishes.
///
/// WHY IT IS NOT ALWAYS THE DERIVED ONE (backlog 02165b1d,
/// `third_instance_2026_09_08_2200Z`). `--finish` used to always look
/// for `<car branch>-rerail`, so it could be run exactly once per car:
/// on an already-re-railed car it went hunting for
/// `feat/…-rerail-rerail`, found nothing, and refused. Car 538775dd hit
/// that on 2026-09-08 — its branch head had moved for a renumbered
/// migration, its receipt was stale, boarding correctly refused it, and
/// there was NO designed way to put the fresh receipt on it. The
/// operator re-gated instead, the auto-park handler filed a twin, and
/// the good car was retired by hand.
///
/// So the question is asked of the forge, not of the string: if the
/// derived branch EXISTS, a rerail is in flight and that is what gets
/// finished (the post-conflict path, unchanged). If it does not, there
/// is nothing to rerail to and `--finish` means what the operator
/// needs it to mean — put this car's CURRENT branch's current green
/// receipt on it and clear the skip. That second reading is also the
/// answer for a plain rebase, so one verb covers both and there is no
/// second one to pick wrong.
///
/// It cannot invent a green: the caller transcribes through
/// `park::receipt_for`, which refuses unless a green gate-run vouches
/// for the branch's head RIGHT NOW.
fn finish_target<'a>(car_branch: &'a str, derived: &'a str, derived_on_forge: bool) -> &'a str {
    if derived_on_forge {
        derived
    } else {
        car_branch
    }
}

/// PURE: the note a repoint writes, which differs by what happened.
/// A rerail moved the car to a new branch; a refresh left it where it
/// was and only superseded the receipt. Saying "rerailed from X" on a
/// car that did not move would be a false record.
fn repoint_note(old_branch: &str, new_branch: &str) -> String {
    if old_branch == new_branch {
        format!(
            "receipt refreshed in place by boss rerail --finish: {new_branch}'s head moved \
             (rebase, re-rail or a renumbered migration), so the current green gate-run's \
             receipt was machine-copied to regate_receipt and the stale skip cleared (the \
             frozen gate step stays as the original head's record)"
        )
    } else {
        format!(
            "rerailed from {old_branch} by boss rerail: new branch cut from \
             origin/main, re-gated, receipt machine-copied to regate_receipt \
             (the frozen gate step stays as the original head's record)"
        )
    }
}

/// PURE: the `rerail_origins` a repoint records — every branch this car
/// has been re-railed OFF, oldest first, each with the head it carried
/// when the car left it. `None` when there is nothing to record, which
/// OMITS the key: a null would DELETE the origins already on the car
/// (the metadata door's contract, and the `delivery_channel` lesson in
/// `boss_jobs::car::regate_patch`).
///
/// WHY THE CAR CARRIES THIS AT ALL (packet 473fda1b, generator 1). The
/// rerail leaves the original branch on the forge, and that branch is
/// never a car — so the arrival sweep, which iterates a train's boarded
/// cars, had nothing to act on and could never delete it. Permanent by
/// construction: 13 originals accumulated and came off by hand on
/// 2026-09-10. The sweep reads THIS record (`train::recorded_branches`)
/// rather than guessing a `-rerail` suffix off a name, because a name
/// is not a record and the head is what the sweep's guard needs anyway:
/// a commit pushed to the original after the rerail keeps it, exactly
/// as a commit pushed after boarding keeps a car's own branch.
///
/// A refresh in place (`old == new`) moved nothing, so it records
/// nothing. A branch already recorded keeps the head it was first
/// recorded with — that is the head the car actually left it at.
fn origins_after(
    car: &Value,
    old_branch: &str,
    old_head: Option<&str>,
    new_branch: &str,
) -> Option<Vec<Value>> {
    if old_branch.is_empty() || old_branch == new_branch {
        return None;
    }
    let mut out: Vec<Value> = car
        .pointer("/metadata/rerail_origins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if out
        .iter()
        .any(|o| o.get("branch").and_then(Value::as_str) == Some(old_branch))
    {
        return Some(out);
    }
    let mut entry = json!({ "branch": old_branch });
    if let Some(h) = old_head.filter(|h| !h.is_empty()) {
        entry["head"] = json!(h);
    }
    out.push(entry);
    Some(out)
}

/// Repoint the car at the re-gated branch: `branch` moves, the fresh
/// receipt rides `regate_receipt` VERBATIM, and the skip_reason is
/// deleted (a PATCH key set to null is removed — the metadata door's
/// documented contract) so the next boarding no longer sees a skip.
///
/// `old_branch == new_branch` is the refresh-in-place case: the branch
/// write is a no-op and only the receipt and the skip move.
async fn repoint(
    http: &boss_core::machine_token::Client,
    car_id: &str,
    new_branch: &str,
    receipt: &boss_jobs::car::Receipt,
    old_branch: &str,
    origins: Option<Vec<Value>>,
    proof: serde_json::Map<String, Value>,
) -> Result<()> {
    // The regate write itself — receipt verbatim, skip cleared — is
    // core's builder, shared with `boss park` and the auto-park handler
    // (a re-gate of a still-parked car refreshes it the same way); the
    // repoint is what rerail adds, because here the branch moved too.
    // The branch moved, so re-classify the channel from the new branch's
    // diff (None if it won't resolve — safe, the key is then left as-is).
    let dc = crate::channels::delivery_channel_for(new_branch);
    let mut patch = boss_jobs::car::regate_patch(
        receipt,
        &repoint_note(old_branch, new_branch),
        dc.as_deref(),
    );
    patch["branch"] = json!(new_branch);
    // And the tiers the new branch's diff touches (ba429e7f), beside
    // the channel — empty when the diff will not resolve, so nothing is
    // stripped.
    if let Some(m) = patch.as_object_mut() {
        m.extend(crate::channels::tier_stamps_for(new_branch));
        // The probe the green states, when it states one — the same
        // copy the auto-park refresh makes (79a17c7a). Empty for a bare
        // re-gate, which leaves the car's own proof keys where they are.
        m.extend(proof);
    }
    // The branch the car is leaving, so the arrival sweep can take it
    // too (473fda1b). Omitted, never nulled, when nothing moved.
    if let Some(origins) = origins {
        patch["rerail_origins"] = json!(origins);
    }
    gate::api(
        http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{car_id}/metadata"),
        Some(patch),
    )
    .await?;
    Ok(())
}

/// The stamps that record a rerail on the gate-run packets, so the
/// yard's stranded read (and `boss orient`'s) stop counting the
/// original branch's green as a forgotten one (69daaba2: it read as
/// stranded forever, even after the branch was deleted on the forge).
/// Every gate-run naming `old_branch` gets `rerailed_to: <new>`; every
/// gate-run naming `new_branch` gets `rerailed_from: <old>`. Pure —
/// `(packet id, PATCH body)` pairs the caller merges via the metadata
/// door. A packet with no id cannot be stamped and is skipped.
///
/// A REFRESH IN PLACE STAMPS NOTHING. When the branch did not move
/// (`--finish` on an already-re-railed car), there is no old branch
/// whose green went stranded and no new one to point at — stamping
/// `rerailed_to: <itself>` would be a fabricated record, and the yard
/// would then read the branch's own live green as superseded.
pub(crate) fn rerail_stamps(
    gate_runs: &[Value],
    old_branch: &str,
    new_branch: &str,
) -> Vec<(String, Value)> {
    if old_branch == new_branch {
        return Vec::new();
    }
    gate_runs
        .iter()
        .filter_map(|g| {
            let id = g.get("id").and_then(Value::as_str)?;
            let branch = g.pointer("/metadata/branch").and_then(Value::as_str)?;
            let patch = if branch == old_branch {
                json!({ "rerailed_to": new_branch })
            } else if branch == new_branch {
                json!({ "rerailed_from": old_branch })
            } else {
                return None;
            };
            Some((id.to_string(), patch))
        })
        .collect()
}

/// What `--finish` did with the worktree a conflicted rerail left.
#[derive(Debug)]
enum Retired {
    /// Clean, and removed.
    Removed(String),
    /// Uncommitted changes: kept, because they may be the only copy of
    /// a resolution.
    KeptDirty(String),
    /// No such worktree — the rerail never stopped on a conflict.
    Absent,
    /// It exists and git could not say whether it is clean.
    Unreadable(String, anyhow::Error),
}

impl PartialEq for Retired {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Removed(a), Self::Removed(b)) | (Self::KeptDirty(a), Self::KeptDirty(b)) => {
                a == b
            }
            (Self::Absent, Self::Absent) => true,
            (Self::Unreadable(a, _), Self::Unreadable(b, _)) => a == b,
            _ => false,
        }
    }
}

/// TAKE BACK THE WORKTREE A CONFLICT LEFT (backlog 3129d98a). `run`
/// hands a conflicted rerail's worktree to the human and refuses a new
/// rerail while it is present, and nothing took it back: 26 had piled
/// up under `.git/rerail-wt` by 2026-09-23, one stuck mid-cherry-pick.
/// Called by `--finish` once the car is repointed, on the one path the
/// verb adds itself ([`temporary_worktree`]) — an actor's own worktree
/// is never taken. A worktree with uncommitted changes is never removed
/// — it may hold the only copy of a resolution — and an unreadable one
/// is left and named.
fn retire_rerail_worktree(wt: &str) -> Retired {
    let wt = wt.to_string();
    if !std::path::Path::new(&wt).exists() {
        return Retired::Absent;
    }
    match git(&wt, &["status", "--porcelain"]) {
        Ok(s) if !s.trim().is_empty() => Retired::KeptDirty(wt),
        Ok(_) => match git(&wt, &["worktree", "remove", "--force", &wt]) {
            Ok(_) => Retired::Removed(wt),
            Err(e) => Retired::Unreadable(wt, e),
        },
        Err(e) => Retired::Unreadable(wt, e),
    }
}

/// The disposable worktree's path: beside every other rerail's, under
/// the shared git directory. ONE definition, read by the replay that
/// adds it and by `--finish` that takes it back, so the two cannot
/// disagree about where it is.
fn temporary_worktree(common: &str, new_branch: &str) -> String {
    format!("{common}/rerail-wt/{new_branch}")
}

/// Where the cherry-pick runs (backlog ce9a7d8f, fact 2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Workspace {
    /// Added for this rerail under the shared git directory and removed
    /// when it is done — or by `--finish`, if a conflict left it.
    Temporary(String),
    /// A checkout the actor already works in: switched to the replay,
    /// switched back on the happy path, and never removed.
    InPlace(String),
}

impl Workspace {
    fn path(&self) -> &str {
        match self {
            Self::Temporary(p) | Self::InPlace(p) => p,
        }
    }
}

/// PURE: which [`Workspace`] a rerail uses.
///
/// WHY NOT ALWAYS THE SHARED GIT DIRECTORY (ce9a7d8f). A
/// worktree-isolated builder may not run git under the main repository's
/// `.git`, so on car 05ade1cc (2026-09-28) the agent deleted the verb's
/// conflict worktree and repeated the cherry-pick in its own — the verb
/// had put the one human stop exactly where that human could not work.
/// So: a named `--worktree` is used where it stands; with none, a caller
/// in a LINKED worktree works in that worktree; only the main checkout —
/// the operator's, whose tree the pod's doors run from and must never be
/// switched to a replay — gets the disposable one.
fn workspace_for(
    explicit: Option<&str>,
    linked_toplevel: Option<&str>,
    common: &str,
    new_branch: &str,
) -> Workspace {
    match explicit.or(linked_toplevel) {
        Some(p) => Workspace::InPlace(p.to_string()),
        None => Workspace::Temporary(temporary_worktree(common, new_branch)),
    }
}

/// The top level of `dir`'s checkout when it is a LINKED worktree, and
/// `None` when it is the main checkout. Asked of git — a linked
/// worktree's git dir is not the common one — rather than guessed from
/// the shape of a path.
fn linked_worktree_toplevel(dir: &str) -> Result<Option<String>> {
    let absolute = |p: String| -> Result<std::path::PathBuf> {
        let p = std::path::Path::new(dir).join(p);
        std::fs::canonicalize(&p).with_context(|| format!("resolving {}", p.display()))
    };
    let git_dir = absolute(git(dir, &["rev-parse", "--git-dir"])?)?;
    let common = absolute(git(dir, &["rev-parse", "--git-common-dir"])?)?;
    if git_dir == common {
        return Ok(None);
    }
    Ok(Some(git(dir, &["rev-parse", "--show-toplevel"])?))
}

/// What the checkout at `dir` is on, to switch back to: its branch, or
/// its commit when detached.
fn current_checkout(dir: &str) -> Result<String> {
    git(dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .or_else(|_| git(dir, &["rev-parse", "HEAD"]))
}

/// Give the workspace back once the replay no longer needs it: the
/// disposable worktree is removed, and an actor's own is switched back
/// to what it was on.
fn release_workspace(ws: &Workspace, return_to: Option<&str>) -> Result<()> {
    match (ws, return_to) {
        (Workspace::Temporary(p), _) => git(".", &["worktree", "remove", "--force", p]).map(drop),
        (Workspace::InPlace(p), Some(r)) => git(p, &["checkout", "-q", r]).map(drop),
        (Workspace::InPlace(_), None) => Ok(()),
    }
}

/// PURE: the sequence a human finishes a stopped rerail with.
///
/// The gate carries `--rebase` (ce9a7d8f): a resolution takes minutes,
/// the widest window a train has to land in, and a gate launched on the
/// main the verb fetched before the human started is refused as behind.
/// An in-place rerail also names the way back to the checkout it moved
/// the actor off.
fn conflict_steps(
    ws: &Workspace,
    new_branch: &str,
    given: &str,
    return_to: Option<&str>,
) -> String {
    let wt = ws.path();
    let back = match (ws, return_to) {
        (Workspace::InPlace(_), Some(r)) => format!(
            "\n    git -C {wt} checkout {r}    (back to what this worktree was on, once pushed)"
        ),
        _ => String::new(),
    };
    format!(
        "  The worktree is left at {wt} — resolve it, then:\n    \
         git -C {wt} cherry-pick --continue\n    \
         git -C {wt} push origin HEAD:refs/heads/{new_branch}\n    \
         boss gate {new_branch} --wait --rebase --mode auto\n    \
         boss rerail {given} --finish{back}"
    )
}

/// How the replay of a car's commits onto current main ended.
#[derive(Debug, PartialEq, Eq)]
enum Picked {
    /// Every commit applied; the ones main already held replayed empty
    /// and were dropped, each named.
    Clean {
        dropped: Vec<crate::freshness::Dropped>,
    },
    /// EVERY commit replayed empty: main already holds the car.
    Landed {
        dropped: Vec<crate::freshness::Dropped>,
    },
    /// A real hunk — the one human stop. The files, and git's words.
    Conflict {
        files: String,
        said: String,
        dropped: Vec<crate::freshness::Dropped>,
    },
    /// git stopped with no conflict to read and no empty commit to skip.
    Failed { said: String },
}

/// Cherry-pick `range` onto the checkout at `wt`, skipping and naming
/// every commit that replays EMPTY.
///
/// WHY AN EMPTY PICK IS NOT A CONFLICT (backlog b3c2bfb6, 2026-09-24).
/// Train #612 landed a car while its rerail waited; git stopped with
/// "the previous cherry-pick is now empty", and the verb printed
/// CONFLICT, "needs a human", and left its worktree mid-cherry-pick. An
/// empty commit is main already holding that patch under another sha —
/// the landed-twin signal — so it is skipped (`--empty=drop` is git
/// 2.45; the pod has 2.39) the way `boss gate --rebase` skips one, and
/// a car whose every commit is empty is `Landed`.
///
/// Empty is told apart by what git left: no unmerged path, an index
/// equal to HEAD, AND a readable `CHERRY_PICK_HEAD` — a pick refused
/// before it applied anything leaves the first two and not the third.
fn pick_onto(wt: &str, range: &str) -> Result<Picked> {
    use crate::freshness::Dropped;
    // The committer is this verb, set explicitly as `boss gate --rebase`
    // sets it: a gate runner's or a test's environment has no identity.
    let run = |args: &[&str]| -> Result<std::process::Output> {
        crate::git_auth::command()
            .arg("-C")
            .arg(wt)
            .args(args)
            .env("GIT_COMMITTER_NAME", "boss rerail")
            .env("GIT_COMMITTER_EMAIL", "boss@algedonic.dev")
            .output()
            .with_context(|| format!("running git {args:?} in {wt}"))
    };
    let count: usize = git(wt, &["rev-list", "--count", range])?
        .parse()
        .context("counting the car's commits")?;
    // Nothing past main at all: the branch head is main's ancestor.
    if count == 0 {
        return Ok(Picked::Landed { dropped: vec![] });
    }
    let mut out = run(&["cherry-pick", range])?;
    let mut dropped: Vec<Dropped> = Vec::new();
    while !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let files = git(wt, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default()
            .replace('\n', ", ");
        if !files.is_empty() {
            return Ok(Picked::Conflict {
                files,
                said,
                dropped,
            });
        }
        let index_is_head = run(&["diff", "--cached", "--quiet", "HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let picking = git(wt, &["rev-parse", "--verify", "-q", "CHERRY_PICK_HEAD"]).ok();
        // Bounded by the commit count: a skip that does not advance is
        // a stop this function does not understand, not a loop.
        let (true, Some(sha), true) = (index_is_head, picking, dropped.len() < count) else {
            return Ok(Picked::Failed { said });
        };
        let title = git(wt, &["log", "-1", "--format=%s", &sha]).unwrap_or_default();
        dropped.push(Dropped { sha, title });
        out = run(&["cherry-pick", "--skip"])?;
    }
    Ok(if dropped.len() == count {
        Picked::Landed { dropped }
    } else {
        Picked::Clean { dropped }
    })
}

/// Main's newest commit touching the files `branch` changed since
/// `base`, as (sha, subject) — for a landed car, the train that landed
/// it, or one that has touched its files since. `None` when git cannot
/// say, which the caller prints as nothing rather than a guess.
fn landed_by(repo: &str, base: &str, branch: &str, main: &str) -> Option<(String, String)> {
    let changed = git(repo, &["diff", "--name-only", base, branch]).ok()?;
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();
    if files.is_empty() {
        return None;
    }
    let range = format!("{base}..{main}");
    let mut args = vec!["log", "-1", "--format=%H%x09%s", &range, "--"];
    args.extend(files);
    let line = git(repo, &args).ok()?;
    let (sha, subject) = line.split_once('\t')?;
    Some((sha.to_string(), subject.to_string()))
}

/// PURE: why this car must not be re-railed as it stands, or `None`.
///
/// A CLOSED OR BOARDED CAR IS NOT REROUTED (backlog b3c2bfb6). On
/// 2026-09-24 a sequential rerail queue re-railed a car train #616 had
/// already landed — cutting `…-rerail-rerail-rerail` and spending a
/// gate on it — and `finish` then repointed CLOSED car be13790d at that
/// branch, so the car record no longer named what shipped. Asked before
/// the cut and again, of the car re-read, before the repoint: a gate
/// takes minutes, and a train can board or land the car inside them.
fn standing_refusal(car: &Value) -> Option<String> {
    let short = |s: &str| s[..8.min(s.len())].to_string();
    let id = short(car.get("id").and_then(Value::as_str).unwrap_or("?"));
    let branch = car
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .unwrap_or("<branch>");
    let train = car
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(short);
    match (car.get("status").and_then(Value::as_str), train) {
        (Some("open"), None) => None,
        (Some("open"), Some(t)) => Some(format!(
            "car {id} is aboard train {t} — a rerail would move a car mid-transit. Wait for \
             the train: it lands the car, or releases it to the dock to be re-railed then."
        )),
        (status, train) => Some(format!(
            "car {id} is {}{} — it is no longer at the dock, and repointing it would leave its \
             record naming a branch that never shipped. Confirm what landed with `boss merged \
             {branch}`.",
            status.unwrap_or("of no readable status"),
            train
                .map(|t| format!(" (it rode train {t})"))
                .unwrap_or_default()
        )),
    }
}

/// The car as it stands NOW, refused if it has left the dock. `finish`
/// asks this before it repoints anything (b3c2bfb6, third instance).
async fn car_still_at_the_dock(
    http: &boss_core::machine_token::Client,
    car: &Value,
) -> Result<Value> {
    let id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?;
    let now = gate::api(http, reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .ok_or_else(|| anyhow!("re-reading car {id} answered with no body"))?;
    if let Some(why) = standing_refusal(&now) {
        bail!("boss rerail: REFUSED — {why}");
    }
    Ok(now)
}

/// The finishing half, standalone: the new branch exists and has a
/// GREEN gate; transcribe its receipt and repoint the car. Split out
/// so a conflict-interrupted rerail (human resolves, pushes, gates)
/// completes through the same code as the happy path — one definition
/// of the transcription, which is where the hand-typed-sha trap lived.
async fn finish(
    http: &boss_core::machine_token::Client,
    car: &Value,
    old_branch: &str,
    new_branch: &str,
) -> Result<()> {
    // The car as it stands now, not as it stood before the gate
    // (b3c2bfb6): a closed or boarded car is refused, never repointed.
    let car = &car_still_at_the_dock(http, car).await?;
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?;
    let head = gate::resolve_sha(new_branch);
    let gate_runs = crate::train::rows(
        gate::api(
            http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&limit=100&full=true",
            None,
        )
        .await?,
    )?;
    // Machine-copied, green-preferring, head-matched — every property
    // the by-hand transcription kept getting wrong, in one call.
    let receipt = park::receipt_for(&gate_runs, new_branch, &head)?;
    // The head the branch we are LEAVING carries right now — read from
    // the forge, before anything else touches it, because that is the
    // content the rerail carries away and the only evidence the sweep
    // will accept for deleting it later. Best effort on purpose: an
    // unreadable head must not fail a rerail that is otherwise done,
    // and an origin recorded without one is refused by name at the
    // sweep rather than deleted on a guess.
    let old_head = match forge_head(old_branch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "boss rerail: could not read {old_branch}'s head on the forge, \
                 recording the origin without one (the sweep will refuse it by \
                 name rather than guess): {e:#}"
            );
            None
        }
    };
    // A CAR THAT TOUCHES A MUTATING VERB IS HELD BEFORE IT IS MADE
    // BOARDABLE (backlog fdbb447e part 3). The fresh receipt is what
    // lets it board, so the hold lands first, and a hold that cannot
    // land stops the finish: a bare re-gate carries no park intent, so
    // no gate-side hold ever reached this car. Judged off the diff the
    // receipt vouches for, by the one predicate `boss gate` reads.
    let base = crate::freshness::observe(std::path::Path::new("."), new_branch).base;
    let judged = crate::mutating_verb::judge(std::path::Path::new("."), &base, &head);
    // A finding binds the hold to the head it read (`hold_sha`), so only
    // a review releases it; an unread diff does not (backlog b7b02024,
    // review F1) — it is judged again once cleared.
    let bound = judged.is_finding().then_some(head.as_str());
    if let Some(reason) = judged.hold_reason() {
        match crate::mutating_verb::review_hold_write(car, &reason, bound)
            .map_err(|e| anyhow!("boss rerail: REFUSED — {new_branch} {reason}, and {e}"))?
        {
            None => println!(
                "boss rerail: {new_branch} {reason} — the car is held already; that hold stands"
            ),
            Some((path, body)) => {
                gate::api(http, reqwest::Method::PATCH, &path, Some(body))
                    .await
                    .context("holding the car for its review before repointing it")?;
                println!(
                    "boss rerail: HELD — {new_branch} {reason}. It boards only after `boss \
                     release`"
                );
            }
        }
    }
    let origins = origins_after(car, old_branch, old_head.as_deref(), new_branch);
    // THE PROBE MOVES WITH THE GREEN THAT STATES ONE (backlog 79a17c7a).
    // Read before the repoint, off the car as it stood: a green with
    // park intent replaces the car's probe; a bare one keeps it, and
    // the kept probe is replayed against the head it now vouches for.
    let proof = proof_carried(&gate_runs, new_branch, &receipt.raw);
    let kept = proof
        .is_empty()
        .then(|| kept_probe_note(car, &car_id[..8.min(car_id.len())], &receipt.head))
        .flatten();
    let took_probe = proof.contains_key(boss_jobs::car::PROOF_PROBE)
        || proof.contains_key(boss_jobs::car::PROOF_EVENT);
    repoint(
        http, car_id, new_branch, &receipt, old_branch, origins, proof,
    )
    .await?;
    // Record the rerail on the gate-runs themselves, so the original
    // branch's green stops reading as stranded. Best effort, after the
    // repoint: the car is already correct, and a missed stamp costs one
    // false amber row, not a car.
    for (packet, patch) in rerail_stamps(&gate_runs, old_branch, new_branch) {
        if let Err(e) = gate::api(
            http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(patch),
        )
        .await
        {
            eprintln!(
                "boss rerail: could not stamp gate-run {}: {e:#}",
                &packet[..8.min(packet.len())]
            );
        }
    }
    let id = &car_id[..8.min(car_id.len())];
    if old_branch == new_branch {
        println!(
            "boss rerail: {id} refreshed {new_branch} in place — current green receipt \
             copied, skip cleared; the next boarding takes it"
        );
    } else {
        println!(
            "boss rerail: {id} repointed {old_branch} -> {new_branch} — receipt copied, \
             skip cleared; the next boarding takes it"
        );
    }
    if took_probe {
        println!(
            "boss rerail: {id} carries the re-gate's probe — its park intent replaced the old one"
        );
    }
    if let Some(note) = kept {
        eprintln!("{note}");
    }
    Ok(())
}

/// PURE: the proof intent the green `--finish` copies states — the
/// park keys of the gate-run whose step holds `receipt_raw` — read by
/// the one function the auto-park refresh reads it with. Empty for a
/// bare re-gate, and for a receipt no gate-run on `branch` holds.
///
/// WHY (backlog 79a17c7a). `--finish` wrote the receipt and never the
/// probe, so every car it refreshed kept its FIRST gate's probe
/// whatever the re-gate said: 11 of 11 on 2026-09-27/28, one of them
/// (bfb219b4) proven FAILING on correct code.
fn proof_carried(
    gate_runs: &[Value],
    branch: &str,
    receipt_raw: &str,
) -> serde_json::Map<String, Value> {
    gate_runs
        .iter()
        .filter(|g| g.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch))
        .find(|g| {
            g.get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|s| {
                    s.pointer("/metadata/receipt").and_then(Value::as_str) == Some(receipt_raw)
                })
        })
        .and_then(|g| g.get("metadata"))
        .and_then(Value::as_object)
        .map(boss_jobs::car::proof_intent_of_park)
        .unwrap_or_default()
}

/// What `--finish` says about the probe a bare green leaves on the car:
/// its tree needles replayed against the head the car's receipt named
/// (what the probe was written for) and the head the fresh receipt names,
/// in the checkout the verb runs in. See `kept_probe`.
fn kept_probe_note(car: &Value, id: &str, new_head: &str) -> Option<String> {
    let probe = car
        .pointer(&format!("/metadata/{}", boss_jobs::car::PROOF_PROBE))
        .and_then(Value::as_str);
    let written_for = crate::receipt::select_receipt(car)
        .and_then(|r| r.get("head").and_then(Value::as_str).map(str::to_string));
    let checked: Vec<_> = probe
        .map(crate::kept_probe::needles)
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            let was = written_for
                .as_deref()
                .and_then(|h| crate::kept_probe::holds(".", h, &n));
            let now = crate::kept_probe::holds(".", new_head, &n);
            (n, was, now)
        })
        .collect();
    crate::kept_probe::kept_probe_report(id, probe, written_for.as_deref(), new_head, &checked)
}

pub async fn run(
    given: &str,
    finish_only: bool,
    worktree: Option<&str>,
    dry: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = crate::gate::machine_client()?;
    let (car, old_branch) = find_car(&http, given).await?;
    let new_branch = format!("{old_branch}-rerail");
    // Before anything is cut, fetched or finished (b3c2bfb6): a car a
    // train has boarded or landed is not the verb's to move.
    if let Some(why) = standing_refusal(&car) {
        bail!("boss rerail: REFUSED — {why}");
    }

    if finish_only {
        // A rerail in flight is finished onto its new branch; with no
        // such branch on the forge there is nothing to rerail TO, and
        // `--finish` refreshes the car where it stands. See
        // `finish_target` for the car this was measured on.
        let target = finish_target(&old_branch, &new_branch, on_forge(&new_branch)?).to_string();
        if target == old_branch {
            println!(
                "boss rerail: no {new_branch} on the forge — refreshing {old_branch}'s car \
                 from its current green receipt instead"
            );
        }
        finish(&http, &car, &old_branch, &target).await?;
        // The worktree a conflict left behind, taken back now the car
        // no longer needs it (backlog 3129d98a). Only ever the one the
        // verb ADDED — an in-place rerail ran in the actor's own
        // worktree, which is theirs, so there is nothing to take back
        // there and this finds it absent. Best effort: the car is
        // already correct, and a worktree that cannot be read is named,
        // not guessed at.
        match git(".", &["rev-parse", "--git-common-dir"]) {
            Ok(common) => {
                match retire_rerail_worktree(&temporary_worktree(common.trim(), &new_branch)) {
                    Retired::Removed(wt) => println!("boss rerail: removed its worktree {wt}"),
                    Retired::KeptDirty(wt) => eprintln!(
                        "boss rerail: kept {wt} — it has uncommitted changes, which may be the \
                         only copy of a resolution; remove it by hand once they are safe"
                    ),
                    Retired::Absent => {}
                    Retired::Unreadable(wt, e) => {
                        eprintln!("boss rerail: left {wt} in place — could not read it: {e:#}")
                    }
                }
            }
            Err(e) => eprintln!("boss rerail: could not find the rerail worktrees: {e:#}"),
        }
        return Ok(());
    }

    // The rebase, cut from the CURRENT trunk.
    git(".", &["fetch", "origin"])?;
    if on_forge(&new_branch)? {
        bail!(
            "{new_branch} already exists on the forge — a previous rerail is in \
             flight. Finish it (`boss rerail {given} --finish`) or delete the \
             branch before cutting a fresh one."
        );
    }
    let base = git(
        ".",
        &["merge-base", "origin/main", &format!("origin/{old_branch}")],
    )?;
    let range = format!("{base}..origin/{old_branch}");
    // THE SHARED GIT DIRECTORY, ASKED FOR RATHER THAN SPELLED.
    //
    // This was the literal `.git/rerail-wt/{new_branch}` until
    // 2026-09-21 (backlog 112106e4), which works only from the main
    // checkout. In a LINKED WORKTREE `.git` is a FILE — it holds a
    // single `gitdir:` line pointing at the real directory — so the
    // path asks git to create a directory under a file, and git says:
    //
    //   fatal: could not create leading directories of
    //   '.git/rerail-wt/<branch>/.git': Not a directory
    //
    // which names a path the operator never typed and says nothing
    // about worktrees. The knowledge needed to read it — that a
    // worktree's `.git` is a file — is exactly what someone reaching
    // for `rerail` may not have (§Diagnosis: a verdict someone must
    // re-derive is not a verdict).
    //
    // `--git-common-dir` answers from either place: `.git` from the
    // main checkout, an absolute path from a worktree, and both are
    // usable as written. The disposable worktree lands beside every
    // other rerail's whichever checkout the verb was run from, which is
    // also where `--finish` looks for it later.
    let common = git(".", &["rev-parse", "--git-common-dir"]).context(
        "asking git for the shared git directory — rerail needs it to place its temporary \
         worktree, and without it the path would only be right from the main checkout",
    )?;
    // WHERE THE REPLAY RUNS (ce9a7d8f): where the actor already works
    // when that is a linked worktree or a named `--worktree`, and the
    // disposable worktree only from the main checkout. See
    // `workspace_for`.
    let linked = linked_worktree_toplevel(".").unwrap_or_else(|e| {
        eprintln!(
            "boss rerail: could not tell whether this is a linked worktree ({e:#}) — using a \
             disposable one"
        );
        None
    });
    let ws = workspace_for(worktree, linked.as_deref(), common.trim(), &new_branch);

    if dry {
        let commits = git(".", &["rev-list", "--count", &range])?;
        println!(
            "boss rerail: DRY — would cut {new_branch} from origin/main in {}, \
             cherry-pick {commits} commit(s) ({range}), push, gate with --rebase, and \
             repoint the car",
            ws.path()
        );
        return Ok(());
    }

    let return_to = match &ws {
        Workspace::Temporary(wt) => {
            git(".", &["worktree", "add", "--detach", wt, "origin/main"])?;
            None
        }
        Workspace::InPlace(wt) => {
            // An actor's own worktree is switched only when clean: the
            // replay must not carry, or strand, work that is not the car's.
            let dirty = git(wt, &["status", "--porcelain", "--untracked-files=no"])?;
            if !dirty.is_empty() {
                bail!(
                    "boss rerail: REFUSED — {wt} has uncommitted changes, and the rerail would \
                     switch it to origin/main to replay the car there. Commit or set them aside, \
                     or name a clean checkout with --worktree <path>.\n{dirty}"
                );
            }
            let was = current_checkout(wt)?;
            git(wt, &["checkout", "-q", "--detach", "origin/main"])?;
            println!(
                "boss rerail: replaying in {wt} (it was on {was}; the rerail switches it back)"
            );
            Some(was)
        }
    };
    let wt = ws.path().to_string();
    let dropped = match pick_onto(&wt, &range)? {
        Picked::Clean { dropped } => dropped,
        Picked::Landed { dropped } => {
            // THE CAR IS ALREADY ON MAIN (b3c2bfb6). Nothing to cut,
            // push or gate: the workspace is given back and the rerail
            // refuses, naming main's commit that holds the car.
            release_workspace(&ws, return_to.as_deref())?;
            let main_head = git(".", &["rev-parse", "origin/main"])?;
            let named = dropped
                .iter()
                .map(|d| format!("{} \"{}\"", &d.sha[..8.min(d.sha.len())], d.title))
                .collect::<Vec<_>>()
                .join(", ");
            let by = landed_by(".", &base, &format!("origin/{old_branch}"), "origin/main")
                .map(|(sha, subject)| {
                    format!(
                        " Landed by (main's newest commit touching its files): {} \"{subject}\".",
                        &sha[..8.min(sha.len())]
                    )
                })
                .unwrap_or_default();
            bail!(
                "boss rerail: REFUSED — {old_branch} is already on main: every commit it carries \
                 ({}) replays empty onto origin/main@{}.{by} No {new_branch} was cut or pushed, \
                 and the car was not touched. Confirm with `boss merged {old_branch}`.",
                if named.is_empty() {
                    "none past main".to_string()
                } else {
                    named
                },
                &main_head[..8.min(main_head.len())]
            );
        }
        Picked::Conflict {
            files,
            said,
            dropped,
        } => {
            for d in &dropped {
                println!("boss rerail: {}", d.line());
            }
            // THE one human stop: a real conflict hunk. Everything after
            // resolution is the same finishing half the happy path uses.
            println!(
                "boss rerail: CONFLICT rebasing {old_branch} onto current main, in: {files}.\n{}\n  \
                 ({said})",
                conflict_steps(&ws, &new_branch, given, return_to.as_deref())
            );
            bail!("conflict needs a human — the worktree and next steps are above");
        }
        Picked::Failed { said } => {
            println!(
                "boss rerail: the cherry-pick of {old_branch} onto current main STOPPED with no \
                 conflict to read and no empty commit to skip — git said: {said}\n{}",
                conflict_steps(&ws, &new_branch, given, return_to.as_deref())
            );
            bail!("the cherry-pick stopped — the worktree and next steps are above");
        }
    };
    for d in &dropped {
        println!("boss rerail: {}", d.line());
    }
    git(
        &wt,
        &["push", "origin", &format!("HEAD:refs/heads/{new_branch}")],
    )?;
    release_workspace(&ws, return_to.as_deref())?;
    println!("boss rerail: {new_branch} cut from origin/main and pushed — gating");

    // Gate the new head through the existing verb — one definition of
    // launching a gate, waits to a verdict, and a refusal cleans up its
    // own packet (the ed7f1355 fix rides the same binary). Derive the
    // scope from the NEW tree, just like an ordinary car: an omitted
    // mode silently spent 48 minutes on the yard rerail's full test
    // phase, versus four on its scoped gate (a5462105). The assembled
    // train still owns its full gate; no old receipt scope is copied.
    gate::run(
        &new_branch,
        Some("auto".to_string()),
        None,
        "boss-dev",
        true,
        false,
        gate::ParkIntent::default(),
        None,
        // No `--stale-base-anyway`: a stale base is never forced past.
        None,
        // `--rebase` (backlog ce9a7d8f). This branch was cut from the
        // origin/main fetched above, and it was assumed current by
        // construction — until car 05ade1cc (2026-09-28), when train
        // #778 landed between the fetch and the gate's own base read and
        // the guard refused the fresh branch as one commit behind, for a
        // hand re-base and a second gate. The gate's replay onto the
        // main IT sees is the same act as this verb's, and a no-op when
        // the base is still current.
        true,
        None,
        now,
    )
    .await?;

    finish(&http, &car, &old_branch, &new_branch).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Measured 2026-09-23 (backlog 3129d98a): 26 worktrees under
    /// `.git/rerail-wt`, one stuck mid-cherry-pick, because a conflict
    /// leaves the verb's worktree for the human and `--finish` never
    /// took it back — and `run` refuses a rerail while its worktree is
    /// present. So `--finish` retires it: a clean one is removed, a
    /// dirty one is kept and named (it may hold the only copy of a
    /// resolution), and an absent one is nothing to do.
    #[test]
    fn finish_retires_the_worktree_a_conflict_left_and_keeps_a_dirty_one() {
        let repo = boss_testing::scratch_dir("rerail-retire-worktree");
        let r = repo.to_str().expect("utf-8 path");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        ] {
            git(r, &args).expect("fixture git");
        }
        let common = git(r, &["rev-parse", "--git-common-dir"]).expect("common dir");
        let common = if Path::new(common.trim()).is_absolute() {
            common.trim().to_string()
        } else {
            repo.join(common.trim()).display().to_string()
        };

        let clean = format!("{common}/rerail-wt/fix/clean-rerail");
        git(r, &["worktree", "add", "-q", "--detach", &clean, "main"]).expect("add clean");
        assert_eq!(
            retire_rerail_worktree(&clean),
            Retired::Removed(clean.clone())
        );
        assert!(!Path::new(&clean).exists(), "the clean worktree is gone");

        let dirty = format!("{common}/rerail-wt/fix/dirty-rerail");
        git(r, &["worktree", "add", "-q", "--detach", &dirty, "main"]).expect("add dirty");
        std::fs::write(Path::new(&dirty).join("half-resolved.txt"), "x").expect("dirty it");
        assert_eq!(
            retire_rerail_worktree(&dirty),
            Retired::KeptDirty(dirty.clone())
        );
        assert!(
            Path::new(&dirty).exists(),
            "a dirty worktree is never removed"
        );

        let never = format!("{common}/rerail-wt/fix/never-conflicted-rerail");
        assert_eq!(retire_rerail_worktree(&never), Retired::Absent);
    }

    /// The rerail stamps: the old branch's gate-runs (every one — a
    /// branch gated twice has two) get `rerailed_to`, the new branch's
    /// get `rerailed_from`, other branches are untouched, and a packet
    /// without an id is skipped rather than PATCHed at an empty id.
    #[test]
    fn rerail_stamps_the_old_and_new_gate_runs_and_nothing_else() {
        let gate_runs = vec![
            json!({ "id": "old-1", "metadata": { "branch": "fix/x" } }),
            json!({ "id": "old-2", "metadata": { "branch": "fix/x" } }),
            json!({ "id": "new-1", "metadata": { "branch": "fix/x-rerail" } }),
            json!({ "id": "other", "metadata": { "branch": "feat/other" } }),
            json!({ "metadata": { "branch": "fix/x" } }),
        ];
        assert_eq!(
            rerail_stamps(&gate_runs, "fix/x", "fix/x-rerail"),
            vec![
                (
                    "old-1".to_string(),
                    json!({ "rerailed_to": "fix/x-rerail" })
                ),
                (
                    "old-2".to_string(),
                    json!({ "rerailed_to": "fix/x-rerail" })
                ),
                ("new-1".to_string(), json!({ "rerailed_from": "fix/x" })),
            ]
        );
    }

    /// A LIMIT IS NOT A FILTER (memory: a-limit-is-not-a-filter). A
    /// conflict-skipped car sits at the tail of `opened_on DESC` once
    /// open cars fill more than a page; the old bare `limit=200` read
    /// left it off page one and `boss rerail` reported it "not found".
    /// The read now pages on `total` (via `train::list_all_pages`, whose
    /// page-two behaviour is pinned there), so the branch lookup must
    /// reach a car gathered from page two.
    #[tokio::test]
    async fn a_car_on_page_two_is_rerailable_not_not_found() {
        let mut all: Vec<Value> = (0..crate::train::PAGE_LIMIT + 40)
            .map(|i| json!({ "id": format!("decoy-{i}"), "metadata": { "branch": format!("decoy-{i}") } }))
            .collect();
        all.push(json!({ "id": "car-tail", "metadata": { "branch": "fix/tail-car" } }));
        let all_ref = &all;
        let gathered = crate::train::list_all_pages(|offset| async move {
            let page: Vec<Value> = all_ref
                .iter()
                .skip(offset)
                .take(crate::train::PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({ "data": page, "total": all_ref.len() })))
        })
        .await
        .unwrap();
        let (car, branch) = select_car(&gathered, "fix/tail-car")
            .expect("the car on page two must be found, not reported not-found");
        assert_eq!(branch, "fix/tail-car");
        assert_eq!(car.get("id").and_then(Value::as_str), Some("car-tail"));
    }

    /// `--finish` RUNS TWICE (02165b1d, third instance). Car 538775dd
    /// was already on `…-rerail` when its head moved again; the old
    /// suffix-deriving finish looked for `…-rerail-rerail`, refused,
    /// and left the operator with no designed way to refresh the
    /// receipt — so they re-gated and got a twin car instead.
    ///
    /// The forge answers the question now: a rerail in flight is
    /// finished onto its branch; with no such branch, the car is
    /// refreshed where it stands.
    #[test]
    fn finish_targets_the_rerail_branch_only_when_one_exists() {
        // The post-conflict path: the human pushed feat/x-rerail.
        assert_eq!(
            finish_target("feat/x", "feat/x-rerail", true),
            "feat/x-rerail"
        );
        // Car 538775dd: already re-railed, head moved again. There is no
        // feat/…-rerail-rerail and there never will be — refresh in
        // place rather than refuse.
        assert_eq!(
            finish_target(
                "feat/arrival-runs-the-probe-rerail",
                "feat/arrival-runs-the-probe-rerail-rerail",
                false
            ),
            "feat/arrival-runs-the-probe-rerail"
        );
        // The same reading covers a car that was never re-railed at all
        // and simply had a rebase — one verb, no second one to pick
        // wrong.
        assert_eq!(finish_target("fix/y", "fix/y-rerail", false), "fix/y");
    }

    /// THE ORIGIN THE REPOINT RECORDS (packet 473fda1b, generator 1).
    /// A rerail leaves the branch it moved off on the forge, and that
    /// branch is never a car — so the arrival sweep, which iterates a
    /// train's boarded cars, could never see it: 13 originals
    /// accumulated and came off by hand on 2026-09-10. The car's record
    /// now names the branch AND the head it carried when the car left
    /// it, which is what lets the sweep delete only what the rerail
    /// actually carried away (the same head guard a car's own branch
    /// gets).
    /// THE PROBE RIDES WITH THE GREEN THAT STATES ONE (backlog 79a17c7a).
    /// `--finish` wrote the receipt and never the probe, so car bfb219b4
    /// kept its first draft's probe after a review fold rewrote the line
    /// it grepped, and was proven FAILING on correct code. The green it
    /// copies is found by its receipt, and its park keys become the
    /// car's proof keys — the auto-park refresh's own reading; a bare
    /// green, or a receipt on another packet, states nothing.
    #[test]
    fn finish_takes_the_probe_from_the_green_it_copies_and_only_from_it() {
        let runs = [
            json!({ "metadata": { "branch": "fix/x", "park_probe": "echo old", "park_expect": "old" },
                    "steps": [{ "metadata": { "verdict": "green", "receipt": "R1" } }] }),
            json!({ "metadata": { "branch": "fix/x", "park_probe": "git show HEAD:a | grep -c 'new'",
                                  "park_expect": "1", "park_summary": "prose" },
                    "steps": [{ "metadata": { "verdict": "green", "receipt": "R2" } }] }),
            json!({ "metadata": { "branch": "fix/x" },
                    "steps": [{ "metadata": { "verdict": "green", "receipt": "R3" } }] }),
            json!({ "metadata": { "branch": "fix/other", "park_probe": "echo no" },
                    "steps": [{ "metadata": { "verdict": "green", "receipt": "R4" } }] }),
        ];
        let p = proof_carried(&runs, "fix/x", "R2");
        assert_eq!(
            Value::Object(p),
            json!({ "proof_probe": "git show HEAD:a | grep -c 'new'", "proof_expect": "1" })
        );
        assert!(
            proof_carried(&runs, "fix/x", "R3").is_empty(),
            "a bare green states no probe"
        );
        assert!(
            proof_carried(&runs, "fix/x", "R4").is_empty(),
            "another branch's green"
        );
        assert!(
            proof_carried(&runs, "fix/x", "R9").is_empty(),
            "no packet holds it"
        );
    }

    #[test]
    fn a_rerail_records_the_branch_it_moved_off_with_its_head() {
        let car = json!({ "id": "car-1", "metadata": { "branch": "feat/x" } });
        assert_eq!(
            origins_after(&car, "feat/x", Some("abc123"), "feat/x-rerail"),
            Some(vec![json!({ "branch": "feat/x", "head": "abc123" })])
        );
    }

    /// An unreadable head is recorded as absent, never guessed: the
    /// sweep then refuses the branch by name (`SweepGuard::NoRecord`),
    /// which is a line an operator can act on. A fabricated head would
    /// be a deletion with no evidence behind it.
    #[test]
    fn an_unreadable_old_head_is_recorded_as_absent() {
        let car = json!({ "id": "car-1", "metadata": { "branch": "feat/x" } });
        assert_eq!(
            origins_after(&car, "feat/x", None, "feat/x-rerail"),
            Some(vec![json!({ "branch": "feat/x" })])
        );
    }

    /// A refresh in place moved no branch, so there is no original to
    /// record — and `None` OMITS the key rather than nulling it, which
    /// the metadata door would read as "delete the origins this car
    /// already recorded" (the `delivery_channel` lesson in
    /// `boss_jobs::car::regate_patch`).
    #[test]
    fn a_refresh_in_place_records_no_origin() {
        let car = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x-rerail",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(&car, "feat/x-rerail", Some("def456"), "feat/x-rerail"),
            None
        );
    }

    /// A CAR RE-RAILED TWICE keeps both originals: the chain is
    /// appended to, oldest first, so the second rerail does not drop
    /// the first original back into the leak. A branch already recorded
    /// is not recorded twice, and its first-recorded head — the one it
    /// carried when the car actually left it — stands.
    #[test]
    fn a_second_rerail_appends_the_branch_it_moved_off() {
        let car = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x-rerail",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(
                &car,
                "feat/x-rerail",
                Some("def456"),
                "feat/x-rerail-rerail"
            ),
            Some(vec![
                json!({ "branch": "feat/x", "head": "abc123" }),
                json!({ "branch": "feat/x-rerail", "head": "def456" }),
            ])
        );
        // And again, with the first branch somehow re-offered: recorded
        // once, with the head it carried the first time.
        let twice = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(&twice, "feat/x", Some("zzz999"), "feat/x-rerail-2"),
            Some(vec![json!({ "branch": "feat/x", "head": "abc123" })])
        );
    }

    /// A refresh in place did not move the branch, so it stamps no
    /// gate-run: `rerailed_to: <itself>` would be a fabricated record
    /// and would make the yard read the branch's own live green as
    /// superseded.
    #[test]
    fn a_refresh_in_place_stamps_no_gate_run() {
        let gate_runs = vec![
            json!({ "id": "g1", "metadata": { "branch": "feat/x-rerail" } }),
            json!({ "id": "g2", "metadata": { "branch": "feat/x-rerail" } }),
        ];
        assert!(
            rerail_stamps(&gate_runs, "feat/x-rerail", "feat/x-rerail").is_empty(),
            "the branch did not move — nothing was superseded"
        );
    }

    /// A git fixture: a repo on `main` with one base commit, committer
    /// identity set on every call so it runs as the gate's uid (which has
    /// none). Returns the repo path.
    fn fixture_repo(name: &str) -> String {
        let repo = boss_testing::scratch_dir(name);
        let r = repo.to_str().expect("utf-8 path").to_string();
        git(&r, &["init", "-q", "-b", "main"]).expect("init");
        write_commit(&r, "base.txt", "base\n", "base");
        r
    }

    fn write_commit(repo: &str, file: &str, body: &str, msg: &str) {
        std::fs::write(Path::new(repo).join(file), body).expect("write");
        git(repo, &["add", file]).expect("add");
        git(
            repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                msg,
            ],
        )
        .expect("commit");
    }

    /// THE GATE TAKES MAIN AS IT STANDS AT LAUNCH (backlog ce9a7d8f).
    /// Car 05ade1cc, 2026-09-28: rerail fetched once, cut its branch,
    /// and train #778 landed before the gate observed the base, so the
    /// base guard REFUSED the fresh branch as one commit behind and the
    /// agent re-based and re-gated by hand. A rerail is a replay onto
    /// the main it fetched; `--rebase` replays onto the main the gate
    /// sees, and is a no-op when the two are the same.
    ///
    /// Held to `gate::run`'s OWN signature rather than a remembered
    /// position: the parameter named `rebase` is found in gate.rs, and
    /// the argument in that position in rerail's call must be `true`.
    #[test]
    fn the_rerail_gate_is_launched_with_rebase() {
        let params = |sig: &str| -> Vec<String> {
            sig.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        };
        let gate_src = include_str!("gate.rs");
        let s = gate_src.find("pub async fn run(\n").expect("gate::run");
        let sig = &gate_src[s + "pub async fn run(\n".len()..];
        let sig = &sig[..sig.find(") -> Result<()>").expect("signature end")];
        let at = params(sig)
            .iter()
            .position(|p| p.starts_with("rebase:"))
            .expect("gate::run takes a rebase parameter");

        let src = include_str!("rerail.rs");
        let s = src
            .find("    gate::run(\n")
            .expect("rerail calls gate::run");
        let call = &src[s + "    gate::run(\n".len()..];
        let call = &call[..call.find("\n    )\n    .await").expect("call end")];
        // Comments carry commas; only the arguments count.
        let args: String = call
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            params(&args).get(at).map(String::as_str),
            Some("true"),
            "rerail's gate::run call must pass rebase = true: {args}"
        );
        let mode_at = params(sig)
            .iter()
            .position(|p| p.starts_with("mode:"))
            .expect("gate::run takes a mode parameter");
        assert_eq!(
            params(&args).get(mode_at).map(String::as_str),
            Some("Some(\"auto\".to_string())"),
            "rerail must ask the gate to derive scope from the NEW tree, not default to full: {args}"
        );
    }

    /// The conflict path's printed gate carries `--rebase` too: a human
    /// resolution takes minutes, which is the widest window a train has
    /// to land in (ce9a7d8f). And it names the way back to the branch an
    /// in-place rerail moved the actor off.
    #[test]
    fn the_conflict_steps_gate_with_rebase_and_name_the_way_back() {
        let temp = conflict_steps(
            &Workspace::Temporary("/r/.git/rerail-wt/fix/x-rerail".into()),
            "fix/x-rerail",
            "fix/x",
            None,
        );
        assert!(
            temp.contains("boss gate fix/x-rerail --wait --rebase"),
            "{temp}"
        );
        assert!(
            temp.contains("git -C /r/.git/rerail-wt/fix/x-rerail cherry-pick --continue"),
            "{temp}"
        );
        assert!(temp.contains("boss rerail fix/x --finish"), "{temp}");
        assert!(temp.contains("--mode auto"), "{temp}");

        let here = conflict_steps(
            &Workspace::InPlace("/w/agent-1".into()),
            "fix/x-rerail",
            "fix/x",
            Some("worktree-agent-1"),
        );
        assert!(
            here.contains("boss gate fix/x-rerail --wait --rebase"),
            "{here}"
        );
        assert!(here.contains("--mode auto"), "{here}");
        assert!(
            here.contains("git -C /w/agent-1 checkout worktree-agent-1"),
            "an in-place rerail names the checkout it moved the actor off: {here}"
        );
    }

    /// WHERE THE REPLAY HAPPENS (ce9a7d8f, fact 2). A worktree-isolated
    /// builder may not run git under the main repository's `.git`, which
    /// is where `<common>/rerail-wt/…` lives — so on car 05ade1cc the
    /// agent deleted the verb's worktree and repeated the cherry-pick in
    /// its own. A named `--worktree` is used where it stands; with none,
    /// a caller in a LINKED worktree works in that worktree; only the
    /// main checkout (the operator's, whose tree the pod's doors run
    /// from) gets the disposable one under the shared git directory.
    #[test]
    fn the_replay_happens_where_the_actor_works() {
        assert_eq!(
            workspace_for(
                Some("/w/mine"),
                Some("/w/agent-1"),
                "/r/.git",
                "fix/x-rerail"
            ),
            Workspace::InPlace("/w/mine".into())
        );
        assert_eq!(
            workspace_for(None, Some("/w/agent-1"), "/r/.git", "fix/x-rerail"),
            Workspace::InPlace("/w/agent-1".into())
        );
        assert_eq!(
            workspace_for(None, None, "/r/.git", "fix/x-rerail"),
            Workspace::Temporary("/r/.git/rerail-wt/fix/x-rerail".into())
        );
    }

    /// The default is decided by git, not guessed from a path: the main
    /// checkout answers None, a linked worktree answers its own top level.
    #[test]
    fn a_linked_worktree_is_told_apart_from_the_main_checkout() {
        let r = fixture_repo("rerail-linked-worktree");
        assert_eq!(linked_worktree_toplevel(&r).expect("main checkout"), None);
        let linked = format!("{r}-linked");
        let _ = std::fs::remove_dir_all(&linked);
        git(&r, &["worktree", "add", "-q", "--detach", &linked, "main"]).expect("add linked");
        let top = linked_worktree_toplevel(&linked).expect("linked worktree");
        let want = std::fs::canonicalize(&linked).expect("canonical");
        assert_eq!(
            top.map(|t| std::fs::canonicalize(t).expect("canonical top")),
            Some(want)
        );
        // And from a subdirectory of it, the top level still.
        std::fs::create_dir_all(Path::new(&linked).join("sub")).expect("subdir");
        assert!(
            linked_worktree_toplevel(&format!("{linked}/sub"))
                .expect("subdir")
                .is_some()
        );
    }

    /// A car whose patch main already holds (b3c2bfb6, 2026-09-24):
    /// train #612 landed feat/the-yard-floor-deck… while its rerail
    /// waited, git said "the previous cherry-pick is now empty", and the
    /// verb printed CONFLICT, needs a human, and left its worktree
    /// mid-cherry-pick. An empty pick is the landed-twin signal: every
    /// commit replaying empty is `Landed`, naming main's newest commit
    /// touching the car's files — the train that landed it.
    #[test]
    fn a_car_main_already_holds_is_landed_not_a_conflict() {
        let r = fixture_repo("rerail-empty-pick");
        git(&r, &["checkout", "-q", "-b", "fix/x"]).expect("branch");
        write_commit(&r, "x.txt", "x\n", "car: x.txt");
        git(&r, &["checkout", "-q", "main"]).expect("main");
        // The train squashes the car onto main under another sha.
        write_commit(
            &r,
            "x.txt",
            "x\n",
            "train: 2026-09-24 06:52 (3 changes) (#612)",
        );
        let base = git(&r, &["merge-base", "main", "fix/x"]).expect("base");
        git(&r, &["checkout", "-q", "--detach", "main"]).expect("detach");

        let picked = pick_onto(&r, &format!("{base}..fix/x")).expect("pick");
        match &picked {
            Picked::Landed { dropped } => {
                assert_eq!(dropped.len(), 1, "{picked:?}");
                assert_eq!(dropped[0].title, "car: x.txt", "{picked:?}");
            }
            other => panic!("an empty pick is a landed car, not {other:?}"),
        }
        assert!(
            git(&r, &["status", "--porcelain"])
                .expect("status")
                .is_empty(),
            "nothing is left mid-cherry-pick"
        );
        let (_, subject) = landed_by(&r, &base, "fix/x", "main").expect("the landing commit");
        assert_eq!(subject, "train: 2026-09-24 06:52 (3 changes) (#612)");
    }

    /// Only SOME commits empty — a predecessor car landed by squash — is
    /// not a landed car: the empty ones are dropped and named, the rest
    /// replay, and the rerail goes on.
    #[test]
    fn a_partly_landed_car_drops_the_empty_commits_and_replays_the_rest() {
        let r = fixture_repo("rerail-partial-pick");
        git(&r, &["checkout", "-q", "-b", "fix/y"]).expect("branch");
        write_commit(&r, "first.txt", "1\n", "first car: first.txt");
        write_commit(&r, "second.txt", "2\n", "this car: second.txt");
        git(&r, &["checkout", "-q", "main"]).expect("main");
        write_commit(&r, "first.txt", "1\n", "train: first car");
        let base = git(&r, &["merge-base", "main", "fix/y"]).expect("base");
        git(&r, &["checkout", "-q", "--detach", "main"]).expect("detach");

        match pick_onto(&r, &format!("{base}..fix/y")).expect("pick") {
            Picked::Clean { dropped } => {
                assert_eq!(dropped.len(), 1);
                assert_eq!(dropped[0].title, "first car: first.txt");
            }
            other => panic!("expected a clean replay with one drop, got {other:?}"),
        }
        assert_eq!(
            git(&r, &["log", "-1", "--format=%s"]).expect("head"),
            "this car: second.txt"
        );
    }

    /// And a real hunk is still the one human stop, with the file named.
    #[test]
    fn a_real_hunk_is_still_a_conflict() {
        let r = fixture_repo("rerail-conflict-pick");
        git(&r, &["checkout", "-q", "-b", "fix/z"]).expect("branch");
        write_commit(&r, "base.txt", "car\n", "car: base.txt");
        git(&r, &["checkout", "-q", "main"]).expect("main");
        write_commit(&r, "base.txt", "main\n", "train: base.txt");
        let base = git(&r, &["merge-base", "main", "fix/z"]).expect("base");
        git(&r, &["checkout", "-q", "--detach", "main"]).expect("detach");

        match pick_onto(&r, &format!("{base}..fix/z")).expect("pick") {
            Picked::Conflict { files, .. } => assert_eq!(files, "base.txt"),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    /// A CLOSED OR BOARDED CAR IS NOT REROUTED (b3c2bfb6, second and
    /// third instances): a sequential rerail queue re-railed a car train
    /// #616 had already landed, and `finish` then repointed CLOSED car
    /// be13790d at a branch that never landed — the car record no longer
    /// named what shipped. Checked before the cut and again before the
    /// repoint, against the car as it stands then.
    #[test]
    fn a_closed_or_boarded_car_is_refused_by_name() {
        let closed = json!({
            "id": "be13790d-0000", "status": "closed",
            "metadata": { "branch": "fix/a", "train": "7a7a7a7a-train", "outcome": "merged" }
        });
        let why = standing_refusal(&closed).expect("a closed car is refused");
        assert!(why.contains("be13790d") && why.contains("closed"), "{why}");
        assert!(why.contains("7a7a7a7a"), "names the train it rode: {why}");
        assert!(why.contains("boss merged fix/a"), "{why}");

        let aboard = json!({
            "id": "c0565f48-0000", "status": "open",
            "metadata": { "branch": "fix/b", "train": "61261261-train" }
        });
        let why = standing_refusal(&aboard).expect("a boarded car is refused");
        assert!(why.contains("aboard train 61261261"), "{why}");

        let parked = json!({
            "id": "05ade1cc-0000", "status": "open",
            "metadata": { "branch": "fix/c", "train": null }
        });
        assert_eq!(standing_refusal(&parked), None);
    }

    /// And the note the car carries says which of the two happened. A
    /// refresh that claimed "rerailed from X" would put a move in the
    /// record that never occurred.
    #[test]
    fn the_note_records_a_refresh_as_a_refresh() {
        let moved = repoint_note("feat/x", "feat/x-rerail");
        assert!(moved.contains("rerailed from feat/x"), "{moved}");

        let in_place = repoint_note("feat/x-rerail", "feat/x-rerail");
        assert!(in_place.contains("refreshed in place"), "{in_place}");
        assert!(
            !in_place.contains("rerailed from"),
            "a car that did not move was not rerailed: {in_place}"
        );
        // Both say where the receipt came from — the copy contract is
        // the same either way.
        for n in [&moved, &in_place] {
            assert!(n.contains("regate_receipt"), "{n}");
        }
    }
}
