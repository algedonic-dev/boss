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

use crate::car_retire::Door;
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
    http: &crate::machine_http::MachineClient,
    given: &str,
) -> Result<(Value, String)> {
    // Read EVERY open car, not just page one: a rerail target opened
    // days ago sorts to the tail of `opened_on DESC`, and a bare
    // `limit=` read left it off page one and reported it "not found"
    // (a-limit-is-not-a-filter). `crate::sor_client::all_open_cars` pages on `total`.
    let cars = crate::sor_client::all_open_cars(http).await?;
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
    let reference = format!("refs/heads/{branch}");
    let output = git(".", &["ls-remote", "origin", &reference])?;
    Ok(crate::receipt::exact_ref_head(&output, &reference))
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
            "receipt refreshed in place by boss rerail --finish: a current green vouches for \
             {new_branch}'s head, so its receipt was machine-copied to regate_receipt and \
             the stale skip cleared (the \
             completed gate step stays as the original head's record; an unfinished gate \
             is completed with the current green)"
        )
    } else {
        format!(
            "rerailed from {old_branch} by boss rerail: new branch cut from \
             origin/main, re-gated, receipt machine-copied to regate_receipt \
             (a completed gate step stays as the original head's record; an unfinished \
             gate is completed with the current green)"
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

/// The metadata-door body that repoints the car at the re-gated branch:
/// `branch` moves, the fresh receipt rides `regate_receipt` VERBATIM,
/// and the skip_reason is deleted (a PATCH key set to null is removed —
/// the metadata door's documented contract) so the next boarding no
/// longer sees a skip. Built, not sent: `finish` sends it, or — under
/// `--dry-run` — prints it (backlog 1e9aeea9).
///
/// `old_branch == new_branch` is the refresh-in-place case: the branch
/// write is a no-op and only the receipt and the skip move.
fn repoint_patch(
    tree: &dyn Tree,
    new_branch: &str,
    receipt: &boss_jobs::car::Receipt,
    old_branch: &str,
    origins: Option<Vec<Value>>,
    proof: serde_json::Map<String, Value>,
) -> Value {
    // The regate write itself — receipt verbatim, skip cleared — is
    // core's builder, shared with `boss park` and the auto-park handler
    // (a re-gate of a still-parked car refreshes it the same way); the
    // repoint is what rerail adds, because here the branch moved too.
    // The branch moved, so re-classify the channel from the new branch's
    // diff (None if it won't resolve — safe, the key is then left as-is).
    let dc = tree.channel(new_branch);
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
        m.extend(tree.tiers(new_branch));
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
    patch
}

/// What `--finish` reads from git and the forge, apart from what it
/// reads from the system of record (backlog 1e9aeea9). Split out so the
/// whole finishing half — every read it makes and every write it would
/// make — runs against fixtures, which is what lets a test prove that
/// `--dry-run` sends no write at all rather than trust that it does.
/// [`Checkout`] is the live one: the checkout the verb runs in.
trait Tree {
    /// The head the forge carries for `branch` (`gate::resolve_sha`).
    fn head(&self, branch: &str) -> Result<String>;
    /// The forge's head for `branch`, `None` when it has no such branch.
    fn forge_head(&self, branch: &str) -> Result<Option<String>>;
    /// The ops-verb judgement of the diff `head` carries on `branch`.
    fn judge(&self, branch: &str, head: &str) -> crate::mutating_verb::Judgement;
    /// The delivery channel `branch`'s diff classifies as.
    fn channel(&self, branch: &str) -> Option<String>;
    /// The tier stamps `branch`'s diff earns.
    fn tiers(&self, branch: &str) -> serde_json::Map<String, Value>;
    /// Does `rev`'s tree still hold the kept probe's needle?
    fn holds(&self, rev: &str, n: &crate::kept_probe::Needle) -> Option<bool>;
}

/// The checkout `boss rerail` runs in, and the forge its `origin` names.
struct Checkout;

impl Tree for Checkout {
    fn head(&self, branch: &str) -> Result<String> {
        gate::resolve_sha(branch)
    }
    fn forge_head(&self, branch: &str) -> Result<Option<String>> {
        forge_head(branch)
    }
    fn judge(&self, branch: &str, head: &str) -> crate::mutating_verb::Judgement {
        let base = crate::freshness::observe(std::path::Path::new("."), branch).base;
        crate::mutating_verb::judge(std::path::Path::new("."), &base, head)
    }
    fn channel(&self, branch: &str) -> Option<String> {
        crate::channels::delivery_channel_for(branch)
    }
    fn tiers(&self, branch: &str) -> serde_json::Map<String, Value> {
        crate::channels::tier_stamps_for(branch)
    }
    fn holds(&self, rev: &str, n: &crate::kept_probe::Needle) -> Option<bool> {
        crate::kept_probe::holds(".", rev, n)
    }
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
    /// Clean, and left in place: `--dry-run` only says it would go.
    WouldRemove(String),
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
            (Self::Removed(a), Self::Removed(b))
            | (Self::WouldRemove(a), Self::WouldRemove(b))
            | (Self::KeptDirty(a), Self::KeptDirty(b)) => a == b,
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
/// is left and named. Under `--dry-run` a clean one is only named
/// ([`Retired::WouldRemove`]): a dry run removes nothing (1e9aeea9).
fn retire_rerail_worktree(wt: &str, dry: bool) -> Retired {
    let wt = wt.to_string();
    if !std::path::Path::new(&wt).exists() {
        return Retired::Absent;
    }
    match git(&wt, &["status", "--porcelain"]) {
        Ok(s) if !s.trim().is_empty() => Retired::KeptDirty(wt),
        Ok(_) if dry => Retired::WouldRemove(wt),
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
    /// A real hunk — the one human stop. The files, git's words, and —
    /// when main took the stopped commit's patch once — its twin, named
    /// for the human who decides ([`twin_note`]).
    Conflict {
        files: String,
        said: String,
        dropped: Vec<crate::freshness::Dropped>,
        twin: Option<String>,
    },
    /// git stopped with no conflict to read and no empty commit to skip.
    Failed { said: String },
}

/// Cherry-pick `base..tip` onto the checkout at `wt`, skipping and naming
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
///
/// A CONFLICT NAMES ITS TWIN AND DROPS NOTHING (backlog ec05a62f,
/// 2026-10-10). Car dd64f4ad carried e624ebcd, a stale copy of a change
/// that had landed alone as train b147c26c; a later train had rewritten
/// those lines, so the replay CONFLICTED rather than replaying empty, and
/// the car sat 87 windows on the stop (car 9ac6cbaf, 101) while nothing
/// told its reader main had taken that patch once. So the stop now looks
/// for main's twin of the stopped commit — the same change, blob for blob
/// (review 0a5496cb) — and names it, with
/// the command that shows what main did to those files since. It never
/// drops the commit: three adversarial reviews of car fd13ddf2 (runs
/// 9b231beb, 3f14cf26, 1c7dd9c7) each found a commit main no longer held
/// dropped by a twin — reverted, reverted in a squash beside another edit,
/// reverted by hand through an editor that rewrote whitespace, or a twin
/// at another position in the file — and on dd64f4ad's own history a drop
/// would have fired at two trains and not after. A wrong drop boards a car
/// silently without its change; a named twin costs a human seconds.
fn pick_onto(wt: &str, base: &str, tip: &str) -> Result<Picked> {
    use crate::freshness::{Dropped, DroppedHow};
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
    let range = format!("{base}..{tip}");
    let count: usize = git(wt, &["rev-list", "--count", &range])?
        .parse()
        .context("counting the car's commits")?;
    // Nothing past main at all: the branch head is main's ancestor.
    if count == 0 {
        return Ok(Picked::Landed { dropped: vec![] });
    }
    // The main the car is replayed onto, read before the first pick
    // moves HEAD: a twin is looked for there, never among the car's own
    // replayed commits.
    let onto = git(wt, &["rev-parse", "HEAD"])?;
    // A predecessor landed as a squash of a SERIES conflicts at its first
    // commit instead of replaying empty (backlog 48bb0b42): main holds the
    // series' final state blob for blob, so that prefix is skipped and
    // named by the one rule `boss gate --rebase` uses, and only the
    // commits after it are picked.
    let listed = git(wt, &["rev-list", "--reverse", &range])?;
    let commits: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    let skip = crate::freshness::landed_prefix(&|args: &[&str]| run(args), &onto, base, &commits)?;
    let mut dropped: Vec<Dropped> = Vec::new();
    for c in &commits[..skip] {
        let title = git(wt, &["log", "-1", "--format=%s", c])
            .unwrap_or_else(|_| "(title unreadable)".to_string());
        dropped.push(Dropped {
            sha: c.to_string(),
            title,
            how: DroppedHow::LandedPrefix { len: skip },
        });
    }
    if skip == count {
        return Ok(Picked::Landed { dropped });
    }
    let range = if skip > 0 {
        format!("{}..{tip}", commits[skip - 1])
    } else {
        range
    };
    let mut out = run(&["cherry-pick", &range])?;
    while !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let files = git(
            wt,
            &["diff", "--no-renames", "--name-only", "--diff-filter=U"],
        )
        .unwrap_or_default()
        .replace('\n', ", ");
        let picking = git(wt, &["rev-parse", "--verify", "-q", "CHERRY_PICK_HEAD"]).ok();
        if !files.is_empty() {
            return Ok(Picked::Conflict {
                files,
                said,
                dropped,
                twin: picking.and_then(|sha| twin_note(wt, &onto, tip, &sha)),
            });
        }
        let index_is_head = run(&["diff", "--cached", "--quiet", "HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false);
        // Bounded by the commit count: a skip that does not advance is
        // a stop this function does not understand, not a loop.
        let (true, Some(sha), true) = (index_is_head, picking, dropped.len() < count) else {
            return Ok(Picked::Failed { said });
        };
        let title = git(wt, &["log", "-1", "--format=%s", &sha]).unwrap_or_default();
        dropped.push(Dropped {
            sha,
            title,
            how: DroppedHow::EmptyReplay,
        });
        out = run(&["cherry-pick", "--skip"])?;
    }
    Ok(if dropped.len() == count {
        Picked::Landed { dropped }
    } else {
        Picked::Clean { dropped }
    })
}

/// What a conflict stop on `sha` says about main's twin of it, or `None`
/// when main never took its patch text. A twin proves main took the
/// change ONCE, never that it holds it now — so this names the twin and
/// the read that shows what main did since, and the decision stays with
/// the human. Only an identical change is offered `--skip`: a twin of the
/// text alone is named as a match that may or may not be this change,
/// and the stop is the ordinary one. A
/// lookup that fails says so: the stop is the one that was always there.
/// The lookup is [`crate::twin`]'s, shared with `boss gate --rebase`
/// (backlog 6e38a79e); the words are this verb's own.
fn twin_note(wt: &str, onto: &str, tip: &str, sha: &str) -> Option<String> {
    use crate::twin::{Twin, TwinFound, sh_quote, twin_on_main};
    let short = |s: &str| s[..12.min(s.len())].to_string();
    let Twin {
        found,
        sha: twin,
        subject,
        files,
    } = match twin_on_main(wt, onto, tip, sha) {
        Ok(found) => found?,
        Err(e) => {
            return Some(format!(
                "  Whether main holds a twin of {} could not be read: {e:#}",
                short(sha)
            ));
        }
    };
    let title = git(wt, &["log", "-1", "--format=%s", sha]).unwrap_or_default();
    // Not "it is not this commit's twin" (backlog 2f2caa8e, review
    // 22073d6e): main's commit IS the twin when it landed on files main
    // had already changed outside the hunk, and that is likely the common
    // shape, so the note says what the match shows and no more.
    if found == TwinFound::Elsewhere {
        return Some(format!(
            "  Main's {} \"{subject}\" carries the same patch text as the stopped commit {} \
             \"{title}\", but not the same blobs (git diff-tree --raw). So it may be this \
             commit's twin, landed on files main had already changed outside the hunk, or \
             the same text at another place in them; the patch text alone cannot tell which. \
             Read it, then resolve the conflict as usual:\n    \
             git -C {wt} show {}",
            short(&twin),
            short(sha),
            short(&twin),
        ));
    }
    // Whether a skip leaves the car empty (review 0a5496cb, finding 3):
    // the stop is on the car's last commit and nothing before it was
    // replayed, so HEAD after the skip is main itself.
    let last = git(wt, &["rev-list", "--count", &format!("{sha}..{tip}")]).is_ok_and(|n| n == "0");
    let nothing_replayed = git(wt, &["rev-parse", "HEAD"]).is_ok_and(|h| h == onto);
    let branch = tip.strip_prefix("origin/").unwrap_or(tip);
    // The read above is the evidence that main still holds the change.
    // `boss merged` is not: it answers MERGED off `git cherry` once main
    // took the patch, a later revert or not (merged.rs), so offering it
    // as confirmation offered a check that cannot fail (backlog 2f2caa8e).
    let skip = if last && nothing_replayed {
        format!(
            "  If main holds it, this is the only change the car still carries, so the car \
             carries nothing main lacks:\n    \
             git -C {wt} cherry-pick --skip\n  \
             ends the replay with HEAD at main. Then push nothing and gate nothing — none of \
             the steps below apply; close the car as landed, citing the read above. That \
             read is what shows main still holds the change: `boss merged {branch}` answers \
             MERGED once main has taken the patch, even if a later commit took it back out, \
             so it cannot."
        )
    } else {
        format!(
            "  If main holds it, leave the commit out:\n    \
             git -C {wt} cherry-pick --skip\n  \
             and go on with the steps below, leaving out `cherry-pick --continue` if git \
             says no cherry-pick is in progress (the skipped commit was the last)."
        )
    };
    Some(format!(
        "  The stopped commit {} \"{title}\" is main's {} \"{subject}\" byte for byte — the \
         same paths, modes and blobs (git diff-tree --raw): main took this change once. \
         Whether main STILL holds it is yours to decide — read what main did to its files \
         since:\n    \
         git -C {wt} log -p {}..{} -- {}\n\
         {skip}\n  \
         Otherwise resolve the conflict as usual.",
        short(sha),
        short(&twin),
        short(&twin),
        short(onto),
        files
            .iter()
            .map(|f| sh_quote(f))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

/// Main's newest commit touching the files `branch` changed since
/// `base`, as (sha, subject) — for a landed car, the train that landed
/// it, or one that has touched its files since. `None` when git cannot
/// say, which the caller prints as nothing rather than a guess.
fn landed_by(repo: &str, base: &str, branch: &str, main: &str) -> Option<(String, String)> {
    let changed = git(repo, &["diff", "--no-renames", "--name-only", base, branch]).ok()?;
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
async fn car_still_at_the_dock(door: &dyn Door, car: &Value) -> Result<Value> {
    let id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?;
    let now = door
        .send(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .ok_or_else(|| anyhow!("re-reading car {id} answered with no body"))?;
    if let Some(why) = standing_refusal(&now) {
        bail!("boss rerail: REFUSED — {why}");
    }
    Ok(now)
}

type GateFinish = (boss_jobs::car::StepWrite, serde_json::Map<String, Value>);

/// The car's `gate` step, by slug or title.
fn gate_step(car: &Value) -> Option<&Value> {
    car.get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps.iter().find(|s| {
                s.get("spec_slug").and_then(Value::as_str) == Some(boss_jobs::car::GATE_SLUG)
                    || s.get("title").and_then(Value::as_str) == Some(boss_jobs::car::GATE)
            })
        })
}

/// The gate-run an unfinished gate step names as its holder, if any.
fn gate_holder_id(car: &Value) -> Option<&str> {
    gate_step(car)
        .filter(|s| {
            !matches!(
                s.get("status").and_then(Value::as_str),
                Some("completed" | "skipped")
            )
        })
        .and_then(|s| s.pointer(&format!("/metadata/{}", boss_jobs::car::GATE_RUN)))
        .and_then(Value::as_str)
        .filter(|h| !h.trim().is_empty())
}

/// The packet of the gate-run holding the car's unfinished gate step:
/// off the list already read, or by id when it has fallen off that page.
/// `None` when the step names no holder.
async fn gate_holder(door: &dyn Door, car: &Value, gate_runs: &[Value]) -> Result<Option<Value>> {
    let Some(holder) = gate_holder_id(car) else {
        return Ok(None);
    };
    if let Some(p) = gate_runs
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(holder))
    {
        return Ok(Some(p.clone()));
    }
    door.send(reqwest::Method::GET, &format!("/api/jobs/{holder}"), None)
        .await
}

/// A copied green must also finish an unfinished gate (773bad26).
/// Completed steps stay frozen; a pending gate still owes its build.
///
/// NOT A GATE THAT IS STILL RUNNING (backlog 545d9c02, review 08805508
/// N1). `holder` is the packet of the gate-run the step names. While
/// that run has recorded no verdict, the step is its to move: an older
/// green at the same head would complete it, the expectation below
/// would match (the holder still holds it), and the holder's own later
/// verdict — a red included — would then change nothing, because the
/// hand-back moves only an active step. So it is refused, by name. An
/// unreadable holder is refused the same way: no evidence is not a pass.
/// A holder that has a verdict (`lost` is the 773bad26 case) or has
/// closed will never move the step again, and the green finishes it.
fn gate_finish_plan(
    car: &Value,
    receipt: &boss_jobs::car::Receipt,
    holder: Option<&Value>,
) -> Result<Option<GateFinish>> {
    let Some(gate) = gate_step(car) else {
        return Ok(None);
    };
    match gate.get("status").and_then(Value::as_str) {
        Some("completed" | "skipped") => return Ok(None),
        Some("ready" | "active") => {}
        other => {
            bail!("boss rerail: the gate is {other:?}; finish its build before copying a green")
        }
    }
    let step_id = gate
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the unfinished gate has no id"))?;
    if boss_jobs::car::ending_of(car).is_some() {
        bail!("boss rerail: this car's ending already claimed the unfinished gate");
    }
    if let Some(held_by) = gate_holder_id(car) {
        let short = &held_by[..8.min(held_by.len())];
        let running = match holder {
            None => true,
            Some(p) => {
                !matches!(
                    p.get("status").and_then(Value::as_str),
                    Some("closed" | "cancelled")
                ) && crate::gate::own_verdict(held_by, p)?.is_none()
            }
        };
        if running {
            bail!(
                "boss rerail: REFUSED — the gate step is held by gate-run {short}, which has \
                 recorded no verdict yet; a green copied now would finish its step and its own \
                 verdict would then move nothing. Wait for it (`boss gate --wait`), then finish."
            );
        }
    }
    let mut expect = boss_jobs::car::unbound_gate();
    for key in [boss_jobs::car::GATE_RUN, "gated_head"] {
        let value = gate
            .get("metadata")
            .and_then(|md| md.get(key))
            .cloned()
            .unwrap_or(Value::Null);
        expect.insert(key.into(), value);
    }
    let (_, title, metadata) = boss_jobs::car::step_fields(
        "",
        "",
        "",
        "Current green receipt machine-copied by boss rerail --finish",
        receipt,
        boss_clock_client::wall_now(),
    )[2]
    .clone();
    Ok(Some((
        boss_jobs::car::StepWrite {
            step_id: step_id.into(),
            title,
            metadata,
            status_body: json!({"status": "completed"}),
        },
        expect,
    )))
}

async fn finish_gate(door: &dyn Door, car: &str, plan: Option<GateFinish>) -> Result<()> {
    let Some((write, expect)) = plan else {
        return Ok(());
    };
    for (method, path, body) in [
        (
            reqwest::Method::PATCH,
            write.merge_path(car),
            write.metadata.clone(),
        ),
        (
            reqwest::Method::PUT,
            write.status_path(car),
            write.status_body,
        ),
    ] {
        door.send_expecting(method, &path, Some(body), &expect).await
            .context("finishing the recovered gate only while its launch binding is unchanged; receipt copied, but recovery must be retried if another launch took the gate")?;
    }
    Ok(())
}

/// The finishing half, standalone: the new branch exists and has a
/// GREEN gate; transcribe its receipt and repoint the car. Split out
/// so a conflict-interrupted rerail (human resolves, pushes, gates)
/// completes through the same code as the happy path — one definition
/// of the transcription, which is where the hand-typed-sha trap lived.
async fn finish(
    door: &dyn Door,
    tree: &dyn Tree,
    car: &Value,
    old_branch: &str,
    new_branch: &str,
    dry: bool,
) -> Result<()> {
    // The car as it stands now, not as it stood before the gate
    // (b3c2bfb6): a closed or boarded car is refused, never repointed.
    let car = &car_still_at_the_dock(door, car).await?;
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?;
    let id = &car_id[..8.min(car_id.len())];
    let head = tree.head(new_branch)?;
    let gate_runs = crate::train::rows(
        door.send(
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&limit=100&full=true",
            None,
        )
        .await?,
    )?;
    // Machine-copied, newest-verdict-at-the-head (81e5fee6) — every property
    // the by-hand transcription kept getting wrong, in one call.
    let receipt = park::receipt_for(&gate_runs, new_branch, &head)?;
    let holder = gate_holder(door, car, &gate_runs).await?;
    let gate_finish = gate_finish_plan(car, &receipt, holder.as_ref())?;
    // Plan the brake before opening review. This is only a planning
    // view: the pending review's status is NEVER written here. The gate
    // completion opens it through the protocol, with its hold already on.
    let mut hold_car = car.clone();
    if gate_finish.is_some()
        && let Some(review) = hold_car
            .get_mut("steps")
            .and_then(Value::as_array_mut)
            .and_then(|steps| {
                steps.iter_mut().find(|s| {
                    s.get("spec_slug").and_then(Value::as_str) == Some(boss_jobs::car::REVIEW_SLUG)
                        || s.get("title").and_then(Value::as_str) == Some(boss_jobs::car::REVIEW)
                })
            })
        && review.get("status").and_then(Value::as_str) == Some("pending")
    {
        review["status"] = json!("ready");
    }
    // The head the branch we are LEAVING carries right now — read from
    // the forge, before anything else touches it, because that is the
    // content the rerail carries away and the only evidence the sweep
    // will accept for deleting it later. Best effort on purpose: an
    // unreadable head must not fail a rerail that is otherwise done,
    // and an origin recorded without one is refused by name at the
    // sweep rather than deleted on a guess.
    let old_head = match tree.forge_head(old_branch) {
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
    let judged = tree.judge(new_branch, &head);
    // A finding binds the hold to the head it read (`hold_sha`), so only
    // a review releases it; an unread diff does not (backlog b7b02024,
    // review F1) — it is judged again once cleared.
    let bound = judged.is_finding().then_some(head.as_str());
    let hold = match judged.hold_reason() {
        None => None,
        Some(reason) => match crate::mutating_verb::review_hold_write(&hold_car, &reason, bound)
            .map_err(|e| anyhow!("boss rerail: REFUSED — {new_branch} {reason}, and {e}"))?
        {
            None => {
                println!(
                    "boss rerail: {new_branch} {reason} — the car is held already; that hold \
                     stands"
                );
                None
            }
            Some(write) => Some((reason, write)),
        },
    };
    // A REFRESH MOVES A LIVE REVIEW HOLD'S HEAD (backlog 6b12a70f). When
    // this green brings no hold write of its own, a car already held for
    // a review keeps that hold, now bound to the head the green vouches
    // for, so the review of that head releases it. Car eb86ac0c was
    // refreshed onto df139b70 with its hold left at 29943365, and the
    // review of df139b70 was refused at both doors. The decider is
    // `boss_jobs::car::hold_follows_head`, which the auto-park refresh
    // asks too. The write carries the hold and head it was decided on
    // (`hold_as_judged`, backlog cd1f68c6): this read is several calls
    // old by the time it lands, and a hold released or replaced in that
    // window must not have its head moved for it.
    let moved = match &hold {
        Some(_) => None,
        None => crate::steps::holdable(&hold_car).ok().and_then(|step| {
            let md = step.get("metadata").unwrap_or(&Value::Null);
            let step_id = step.get("id").and_then(Value::as_str).unwrap_or_default();
            boss_jobs::car::hold_follows_head(md, &head)
                .filter(|_| !step_id.is_empty())
                .map(|body| {
                    (
                        step_id.to_string(),
                        body,
                        boss_jobs::car::hold_as_judged(md),
                    )
                })
        }),
    };
    let origins = origins_after(car, old_branch, old_head.as_deref(), new_branch);
    // THE PROBE MOVES WITH THE GREEN THAT STATES ONE (backlog 79a17c7a).
    // Read before the repoint, off the car as it stood: a green with
    // park intent replaces the car's probe; a bare one keeps it, and
    // the kept probe is replayed against the head it now vouches for.
    let proof = proof_carried(&gate_runs, new_branch, &receipt.raw);
    // AND A BARE GREEN ON A CAR WITH NO PROBE IS REFUSED (backlog
    // 031a3ca9): this finish can complete an unfinished gate, so it is a
    // park door, and the car it would leave at the dock could never be
    // proven. Before the dry-run stop, so a rehearsal says it too.
    if let Some(why) = boss_jobs::car::unproven_park(Some(car), &proof) {
        bail!("boss rerail --finish: car {id} on {new_branch}: {why}");
    }
    let kept = proof
        .is_empty()
        .then(|| kept_probe_note(tree, car, id, &receipt.head))
        .flatten();
    let took_probe = proof.contains_key(boss_jobs::car::PROOF_PROBE)
        || proof.contains_key(boss_jobs::car::PROOF_EVENT);
    let repoint = (
        format!("/api/jobs/{car_id}/metadata"),
        repoint_patch(tree, new_branch, &receipt, old_branch, origins, proof),
    );
    let stamps = rerail_stamps(&gate_runs, old_branch, new_branch);

    // A DRY RUN STOPS HERE, BEFORE THE FIRST WRITE (backlog 1e9aeea9).
    // Everything above reads; everything below writes. `--finish
    // --dry-run` used to refresh the car for real: `run` read `dry` only
    // on the cut path, after this branch had returned, so a dry run
    // followed by the real call printed the refresh twice, for two
    // different kept-probe heads (car bb81467a, 2026-10-17). It now
    // prints each write it would send, verbatim, and sends none.
    if dry {
        if let Some((write, _)) = &gate_finish {
            println!(
                "boss rerail: DRY — would finish gate {} with the current green, preserving its review hold",
                write.step_id
            );
        }
        if let Some((reason, (path, body))) = &hold {
            println!("boss rerail: DRY — would HOLD {new_branch} {reason}: PATCH {path} {body}");
        }
        if let Some((step_id, body, judged)) = &moved {
            println!(
                "boss rerail: DRY — would carry {new_branch}'s review hold to {}: PATCH \
                 /api/jobs/{car_id}/steps/{step_id}/metadata {body}, only while it reads {}",
                &head[..12.min(head.len())],
                Value::Object(judged.clone())
            );
        }
        let (path, patch) = &repoint;
        println!(
            "boss rerail: DRY — would {} {id}: PATCH {path} {patch}",
            if old_branch == new_branch {
                format!("refresh {new_branch} in place on car")
            } else {
                format!("repoint {old_branch} -> {new_branch} on car")
            }
        );
        for (packet, patch) in &stamps {
            println!(
                "boss rerail: DRY — would stamp gate-run {}: {patch}",
                &packet[..8.min(packet.len())]
            );
        }
        if took_probe {
            println!("boss rerail: DRY — {id} would carry the re-gate's probe");
        }
        if let Some(note) = kept {
            eprintln!("{note}");
        }
        println!("boss rerail: DRY — nothing written; car {id} is as it was");
        return Ok(());
    }

    // Every write is decided above and made below, in this order: the
    // hold first (a hold that cannot land stops the finish), then the
    // repoint, then the best-effort stamps.
    if let Some((reason, (path, body))) = hold {
        door.send(reqwest::Method::PATCH, &path, Some(body))
            .await
            .context("holding the car for its review before repointing it")?;
        println!(
            "boss rerail: HELD — {new_branch} {reason}. It boards only after `boss \
             release`"
        );
    }
    if let Some((step_id, body, judged)) = moved {
        let carried =
            crate::car_retire::patch_step_metadata_expecting(door, car_id, &step_id, body, &judged)
                .await
                .context(
                    "carrying the car's review hold to the refreshed head before repointing it",
                )?;
        if carried == crate::car_retire::Judged::Moved {
            bail!(
                "boss rerail: REFUSED — car {id}'s review step changed after this refresh read \
                 it: its hold or hold_sha is no longer the one judged, so the hold was not \
                 carried to {} and the car was not repointed. Nothing was written. Run `boss \
                 rerail {new_branch} --finish` again to judge the hold it carries now (backlog \
                 cd1f68c6).",
                &head[..12.min(head.len())]
            );
        }
        println!(
            "boss rerail: HELD — {new_branch}'s review hold stands, now at {}: a review of that \
             head releases it (`boss review`, then `boss release --review`)",
            &head[..12.min(head.len())]
        );
    }
    let (path, patch) = repoint;
    door.send(reqwest::Method::PATCH, &path, Some(patch))
        .await?;
    finish_gate(door, car_id, gate_finish).await?;
    // Record the rerail on the gate-runs themselves, so the original
    // branch's green stops reading as stranded. Best effort, after the
    // repoint: the car is already correct, and a missed stamp costs one
    // false amber row, not a car.
    for (packet, patch) in stamps {
        if let Err(e) = door
            .send(
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
    if old_branch == new_branch {
        println!(
            "boss rerail: {id} refreshed {new_branch} in place — current green receipt \
             copied, skip cleared; review holds still brake boarding"
        );
    } else {
        println!(
            "boss rerail: {id} repointed {old_branch} -> {new_branch} — receipt copied, \
             skip cleared; review holds still brake boarding"
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
pub(crate) fn proof_carried(
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
fn kept_probe_note(tree: &dyn Tree, car: &Value, id: &str, new_head: &str) -> Option<String> {
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
            let was = written_for.as_deref().and_then(|h| tree.holds(h, &n));
            let now = tree.holds(new_head, &n);
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
    let http = crate::sor_client::machine_client()?;
    let door = crate::car_retire::Http(&http);
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
        finish(&door, &Checkout, &car, &old_branch, &target, dry).await?;
        // The worktree a conflict left behind, taken back now the car
        // no longer needs it (backlog 3129d98a). Only ever the one the
        // verb ADDED — an in-place rerail ran in the actor's own
        // worktree, which is theirs, so there is nothing to take back
        // there and this finds it absent. Best effort: the car is
        // already correct, and a worktree that cannot be read is named,
        // not guessed at.
        match git(".", &["rev-parse", "--git-common-dir"]) {
            Ok(common) => {
                match retire_rerail_worktree(&temporary_worktree(common.trim(), &new_branch), dry) {
                    Retired::Removed(wt) => println!("boss rerail: removed its worktree {wt}"),
                    Retired::WouldRemove(wt) => {
                        println!("boss rerail: DRY — would remove its worktree {wt}")
                    }
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
    let dropped = match pick_onto(&wt, &base, &format!("origin/{old_branch}"))? {
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
            twin,
        } => {
            for d in &dropped {
                println!("boss rerail: {}", d.line());
            }
            // THE one human stop: a real conflict hunk. Everything after
            // resolution is the same finishing half the happy path uses.
            // A twin on main is named first, so the reader starts there.
            println!(
                "boss rerail: CONFLICT rebasing {old_branch} onto current main, in: {files}.\n{}{}\n  \
                 ({said})",
                twin.map(|t| format!("{t}\n")).unwrap_or_default(),
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
        // No `--despite-hold`: under a gates hold a rerail is refused like
        // any other gate and waits for the release (backlog 95401d82).
        None,
        now,
    )
    .await?;

    finish(&door, &Checkout, &car, &old_branch, &new_branch, dry).await
}

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) fn head_for_exact_ref_test(branch: &str) -> anyhow::Result<Option<String>> {
        let head = super::forge_head(branch)?;
        assert_eq!(super::on_forge(branch)?, head.is_some());
        Ok(head)
    }
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
        // `--finish --dry-run` says it would take the worktree back and
        // leaves it exactly where it was (backlog 1e9aeea9).
        assert_eq!(
            retire_rerail_worktree(&clean, true),
            Retired::WouldRemove(clean.clone())
        );
        assert!(Path::new(&clean).exists(), "a dry run removes no worktree");
        assert_eq!(
            retire_rerail_worktree(&clean, false),
            Retired::Removed(clean.clone())
        );
        assert!(!Path::new(&clean).exists(), "the clean worktree is gone");

        let dirty = format!("{common}/rerail-wt/fix/dirty-rerail");
        git(r, &["worktree", "add", "-q", "--detach", &dirty, "main"]).expect("add dirty");
        std::fs::write(Path::new(&dirty).join("half-resolved.txt"), "x").expect("dirty it");
        assert_eq!(
            retire_rerail_worktree(&dirty, false),
            Retired::KeptDirty(dirty.clone())
        );
        assert!(
            Path::new(&dirty).exists(),
            "a dirty worktree is never removed"
        );

        let never = format!("{common}/rerail-wt/fix/never-conflicted-rerail");
        assert_eq!(retire_rerail_worktree(&never, false), Retired::Absent);
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

    /// The jobs API as `finish` meets it, from fixtures: it answers the
    /// two reads `finish` makes (the car re-read, the gate-run list) and
    /// RECORDS every call, whatever its method. A write is answered and
    /// recorded, never applied — the record of what was sent is the
    /// evidence, so nothing here can make a write look like a read.
    struct Recording {
        car: Value,
        gate_runs: Vec<Value>,
        calls: std::sync::Mutex<Vec<(reqwest::Method, String)>>,
        /// Every write's body, beside its path, in the order sent.
        bodies: std::sync::Mutex<Vec<(String, Value)>>,
    }

    impl Recording {
        fn new(car: Value, gate_runs: Vec<Value>) -> Self {
            Self {
                car,
                gate_runs,
                calls: std::sync::Mutex::new(Vec::new()),
                bodies: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Door for Recording {
        async fn send(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<Option<Value>> {
            self.calls
                .lock()
                .expect("calls")
                .push((method.clone(), path.to_string()));
            if method != reqwest::Method::GET {
                if let Some(body) = body {
                    self.bodies
                        .lock()
                        .expect("bodies")
                        .push((path.to_string(), body));
                }
                return Ok(None);
            }
            let car_id = self.car["id"].as_str().unwrap_or_default();
            if path == format!("/api/jobs/{car_id}") {
                Ok(Some(self.car.clone()))
            } else if path.starts_with("/api/jobs?kind=gate-run") {
                Ok(Some(
                    json!({ "data": self.gate_runs, "total": self.gate_runs.len() }),
                ))
            } else {
                bail!("the fixture has no answer for GET {path}")
            }
        }

        /// A judged write is recorded like any other; this fixture judges
        /// nothing (the race tests below serve the real merge door).
        async fn send_expecting(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
            _expect: &serde_json::Map<String, Value>,
        ) -> Result<Option<Value>> {
            self.send(method, path, body).await
        }
    }

    /// The git half, fixed: the forge carries `head` for every branch and
    /// the diff judges as `judged`. No process is spawned and no network
    /// is touched, so the whole finish runs anywhere a test runs.
    struct Fixed {
        head: String,
        judged: crate::mutating_verb::Judgement,
    }

    #[tokio::test]
    async fn a_finish_completes_an_unfinished_gate_and_keeps_its_pending_review_hold() {
        let head = "abcdef1234567890000000000000000000000000";
        let receipt = json!({"head": head, "mode": "auto", "verdict": "green"}).to_string();
        for (dry, held) in [(true, true), (false, true), (false, false)] {
            let review_md = if held {
                json!({"hold": "waits for its adversarial review", "hold_sha": head})
            } else {
                json!({})
            };
            let door = Recording::new(
                json!({"id": "car", "kind": "ship-a-change", "status": "open",
                "metadata": {"branch": "fix/x", "proof_event": "event-bound — a lost gate"},
                "steps": [
                    {"id": "gate", "spec_slug": "gate", "title": boss_jobs::car::GATE,
                     "status": "ready", "metadata": {"gate_run": "lost", "gated_head": head}},
                    {"id": "review", "spec_slug": "review", "title": boss_jobs::car::REVIEW,
                     "status": "pending", "metadata": review_md}
                ]}),
                vec![
                    json!({"id": "green", "metadata": {"branch": "fix/x"},
                    "steps": [{"metadata": {"verdict": "green", "receipt": receipt}}]}),
                    lost_holder(),
                ],
            );
            let tree = Fixed {
                head: head.into(),
                judged: crate::mutating_verb::Judgement::Touches {
                    paths: vec!["infra/ops/verbs/x.toml".into()],
                    mutating: Vec::new(),
                },
            };
            finish(&door, &tree, &door.car, "fix/x", "fix/x", dry)
                .await
                .unwrap();
            let bodies = door.bodies.lock().unwrap();
            if dry {
                assert!(bodies.is_empty());
                continue;
            }
            assert!(
                bodies
                    .iter()
                    .any(|(p, b)| p == "/api/jobs/car/steps/gate/metadata"
                        && b["receipt"] == receipt)
            );
            assert!(
                bodies.iter().any(|(p, b)| p == "/api/jobs/car/steps/gate"
                    && b == &json!({"status": "completed"}))
            );
            assert!(!bodies.iter().any(|(p, b)| p.contains("review")
                && (b.get("hold").is_some_and(Value::is_null) || b.get("status").is_some())));
            if !held {
                let brake = bodies
                    .iter()
                    .position(|(p, b)| p.contains("review") && b.get("hold").is_some())
                    .unwrap();
                let finish = bodies
                    .iter()
                    .position(|(p, _)| p == "/api/jobs/car/steps/gate")
                    .unwrap();
                assert!(
                    brake < finish,
                    "the new hold lands before the gate opens review"
                );
            }
        }
    }

    /// `--finish` IS A PARK DOOR TOO (backlog 031a3ca9). It can complete
    /// an unfinished gate and so put a car `boss car open` filed — which
    /// carries no probe — at the dock on a bare green that states none.
    /// That car could never be proven, so the finish refuses it, before
    /// any write and in a dry run too, with the text every park door says.
    #[tokio::test]
    async fn a_finish_refuses_to_leave_a_car_with_no_proof_at_the_dock() {
        let head = "abcdef1234567890000000000000000000000000";
        let receipt = json!({"head": head, "mode": "auto", "verdict": "green"}).to_string();
        for dry in [true, false] {
            let door = Recording::new(
                json!({"id": "car", "kind": "ship-a-change", "status": "open",
                "metadata": {"branch": "fix/x"}, "steps": [
                    {"id": "gate", "spec_slug": "gate", "title": boss_jobs::car::GATE,
                     "status": "ready", "metadata": {"gate_run": "lost", "gated_head": head}},
                    {"id": "review", "spec_slug": "review", "title": boss_jobs::car::REVIEW,
                     "status": "pending", "metadata": {}}
                ]}),
                vec![
                    json!({"id": "green", "metadata": {"branch": "fix/x"},
                    "steps": [{"metadata": {"verdict": "green", "receipt": receipt}}]}),
                    lost_holder(),
                ],
            );
            let tree = Fixed {
                head: head.into(),
                judged: crate::mutating_verb::Judgement::Clear,
            };
            let refused = finish(&door, &tree, &door.car, "fix/x", "fix/x", dry)
                .await
                .expect_err("a car with no proof must not reach the dock");
            let said = format!("{refused:#}");
            assert!(said.contains(boss_jobs::car::UNPROVEN_PARK), "{said}");
            assert!(door.bodies.lock().unwrap().is_empty(), "nothing is written");
        }
    }

    /// Backlog 545d9c02 (review 08805508, N1): a gate step ACTIVE under a
    /// gate-run N that is still running at head H must not be completed
    /// with an OLDER green G at the same H. The conditional write matches
    /// (it expects N, and N still holds the step), so only a refusal here
    /// keeps N's later verdict — a red included — from being lost: the
    /// hand-back moves only an active step.
    #[tokio::test]
    async fn a_finish_refuses_a_gate_step_held_by_a_gate_that_is_still_running() {
        let head = "abcdef1234567890000000000000000000000000";
        let running = "4e4e4e4e-0000-4000-8000-000000000001";
        let receipt = json!({"head": head, "mode": "auto", "verdict": "green"}).to_string();
        let door = Recording::new(
            json!({"id": "car", "kind": "ship-a-change", "status": "open",
            "metadata": {"branch": "fix/x"}, "steps": [
                {"id": "gate", "spec_slug": "gate", "title": boss_jobs::car::GATE,
                 "status": "active", "metadata": {"gate_run": running, "gated_head": head}}
            ]}),
            vec![
                json!({"id": running, "status": "open", "metadata": {"branch": "fix/x"},
                    "steps": [{"spec_slug": "record-verdict", "status": "ready",
                               "metadata": {}}]}),
                json!({"id": "47474747-0000-4000-8000-000000000002", "status": "closed",
                    "metadata": {"branch": "fix/x"},
                    "steps": [{"spec_slug": "record-verdict", "status": "completed",
                               "metadata": {"verdict": "green", "receipt": receipt}}]}),
            ],
        );
        let tree = Fixed {
            head: head.into(),
            judged: crate::mutating_verb::Judgement::Clear,
        };
        let refused = finish(&door, &tree, &door.car, "fix/x", "fix/x", false)
            .await
            .expect_err("an older green must not finish a running gate's step");
        let said = format!("{refused:#}");
        assert!(said.contains("4e4e4e4e"), "names the running gate: {said}");
        assert!(door.bodies.lock().unwrap().is_empty(), "nothing is written");
    }

    /// Backlog 81e5fee6 (finding 1): the holder N has RECORDED a red at
    /// head H and been handed back to ready, and an older green G exists
    /// at the same H. N is done, so `gate_finish_plan` lets it pass; the
    /// receipt chosen must be the newest verdict for H — N's — so the
    /// finish refuses, names N, and writes nothing.
    #[tokio::test]
    async fn a_finish_refuses_when_the_newest_verdict_at_the_head_is_red() {
        let head = "abcdef1234567890000000000000000000000000";
        let red_run = "4e4e4e4e-0000-4000-8000-000000000003";
        let green = json!({"head": head, "mode": "auto", "verdict": "green"}).to_string();
        let red = json!({"head": head, "mode": "auto", "verdict": "failed", "fails": ["test"]})
            .to_string();
        let door = Recording::new(
            json!({"id": "car", "kind": "ship-a-change", "status": "open",
            "metadata": {"branch": "fix/x"}, "steps": [
                {"id": "gate", "spec_slug": "gate", "title": boss_jobs::car::GATE,
                 "status": "ready", "metadata": {"gate_run": red_run, "gated_head": head}}
            ]}),
            vec![
                json!({"id": red_run, "status": "closed", "opened_at": "2026-11-11T11:00:00Z",
                    "metadata": {"branch": "fix/x"},
                    "steps": [{"spec_slug": "record-verdict", "status": "completed",
                               "metadata": {"verdict": "failed", "receipt": red}}]}),
                json!({"id": "47474747-0000-4000-8000-000000000004", "status": "closed",
                    "opened_at": "2026-11-11T10:00:00Z", "metadata": {"branch": "fix/x"},
                    "steps": [{"spec_slug": "record-verdict", "status": "completed",
                               "metadata": {"verdict": "green", "receipt": green}}]}),
            ],
        );
        let tree = Fixed {
            head: head.into(),
            judged: crate::mutating_verb::Judgement::Clear,
        };
        let refused = finish(&door, &tree, &door.car, "fix/x", "fix/x", false)
            .await
            .expect_err("an older green must not override the newer red at the same head");
        let said = format!("{refused:#}");
        assert!(said.contains(red_run), "names the red gate-run: {said}");
        assert!(door.bodies.lock().unwrap().is_empty(), "nothing is written");
    }

    /// The packet of gate-run `lost` as the 773bad26 recovery meets it:
    /// its verdict recorded, so it will never move the step again.
    fn lost_holder() -> Value {
        json!({"id": "lost", "status": "closed", "metadata": {"branch": "fix/x"},
            "steps": [{"spec_slug": "record-verdict", "status": "completed",
                       "metadata": {"verdict": "lost"}}]})
    }

    /// CHANGED DELIBERATELY (backlog 545d9c02). This test used to hand
    /// `gate_finish_plan` the car alone and pinned that a READY or ACTIVE
    /// step always yields a plan — which is the defect: an ACTIVE step is
    /// normally held by a gate still running, and finishing it with an
    /// older green loses that gate's verdict. A plan now needs the
    /// holder's own packet to say it is done (a recorded verdict, `lost`
    /// here, or a closed packet); a holder still without a verdict, or one
    /// that cannot be read, is refused by name in either status.
    #[test]
    fn gate_recovery_preserves_frozen_steps_and_expects_the_launch_it_read() {
        let receipt = boss_jobs::car::Receipt {
            raw: "verbatim".into(),
            head: "abc1234".into(),
            mode: "full".into(),
        };
        let lost = lost_holder();
        let running = json!({"id": "lost", "status": "open",
            "steps": [{"spec_slug": "record-verdict", "status": "ready", "metadata": {}}]});
        for status in ["pending", "ready", "active", "completed", "skipped"] {
            let car = json!({"steps": [{"id": "gate", "spec_slug": "gate", "status": status,
                "metadata": {"gate_run": "lost", "gated_head": "old1234", "receipt": "old"}}]});
            let plan = gate_finish_plan(&car, &receipt, Some(&lost));
            match status {
                "pending" => assert!(plan.is_err()),
                "completed" | "skipped" => assert!(plan.unwrap().is_none()),
                _ => {
                    for holder in [Some(&running), None] {
                        let refused = gate_finish_plan(&car, &receipt, holder).unwrap_err();
                        assert!(format!("{refused}").contains("gate-run lost"), "{refused}");
                    }
                    let (write, expect) = plan.unwrap().unwrap();
                    assert_eq!(write.metadata["receipt"], "verbatim");
                    assert_eq!(
                        Value::Object(expect),
                        json!({"gate_run": "lost", "gated_head": "old1234", "ended_before_gate": null})
                    );
                }
            }
        }
        let ended = json!({"steps": [{"id": "gate", "spec_slug": "gate", "status": "ready",
            "metadata": {"ended_before_gate": {"by": "item", "outcome": "withdrawn"}}}]});
        assert!(gate_finish_plan(&ended, &receipt, None).is_err());
    }

    impl Tree for Fixed {
        fn head(&self, _: &str) -> Result<String> {
            Ok(self.head.clone())
        }
        fn forge_head(&self, _: &str) -> Result<Option<String>> {
            Ok(Some(self.head.clone()))
        }
        fn judge(&self, _: &str, _: &str) -> crate::mutating_verb::Judgement {
            self.judged.clone()
        }
        fn channel(&self, _: &str) -> Option<String> {
            None
        }
        fn tiers(&self, _: &str) -> serde_json::Map<String, Value> {
            serde_json::Map::new()
        }
        fn holds(&self, _: &str, _: &crate::kept_probe::Needle) -> Option<bool> {
            None
        }
    }

    /// Run `finish` for `old -> new` against the fixtures and hand back
    /// every call it sent. The car stands at the dock with an open review
    /// and a kept probe; both branches carry a green for `HEAD`; the diff
    /// touches an ops verb, so a real finish holds, repoints and stamps —
    /// every write `finish` can make.
    async fn calls_of_finish(old: &str, new: &str, dry: bool) -> Vec<(reqwest::Method, String)> {
        const HEAD: &str = "845c5896f5b4aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let receipt = json!({ "head": HEAD, "mode": "auto" }).to_string();
        let green = |id: &str, branch: &str| {
            json!({ "id": id, "metadata": { "branch": branch },
                    "steps": [{ "metadata": { "verdict": "green", "receipt": receipt } }] })
        };
        let door = Recording::new(
            json!({
                "id": "bb81467a-b213-4b23-92d6-4b90da170826",
                "kind": "ship-a-change",
                "status": "open",
                "metadata": {
                    "branch": old,
                    "proof_probe": "git show HEAD:a.rs | grep -c 'pub fn a'",
                    "proof_expect": "1",
                },
                "steps": [{ "id": "step-review", "spec_slug": "review",
                            "title": "Open for review", "status": "ready", "metadata": {} }],
            }),
            vec![green("g-old", old), green("g-new", new)],
        );
        let tree = Fixed {
            head: HEAD.to_string(),
            judged: crate::mutating_verb::Judgement::Touches {
                paths: vec!["infra/ops/verbs/x.toml".to_string()],
                mutating: Vec::new(),
            },
        };
        finish(&door, &tree, &door.car.clone(), old, new, dry)
            .await
            .expect("the fixture finishes");
        door.calls.into_inner().expect("calls")
    }

    fn writes(calls: &[(reqwest::Method, String)]) -> Vec<String> {
        calls
            .iter()
            .filter(|(m, _)| *m != reqwest::Method::GET)
            .map(|(m, p)| format!("{m} {p}"))
            .collect()
    }

    /// `--finish --dry-run` SENDS NO WRITE AT ALL (backlog 1e9aeea9).
    /// Measured 2026-10-17 on car bb81467a: a dry run followed by the
    /// real `--finish` printed the refresh twice, the dry one naming a
    /// kept probe written for 3e10f85cbb47 and the real one, straight
    /// after, for 845c5896f5b4 — the dry run had already refreshed the
    /// car. `run` passed `dry` only to the cut path; the finish branch
    /// returned before it was read. Asserted over EVERY method, not the
    /// one write that was seen, so a write added to `finish` later is
    /// caught too; and the control below proves this fixture sees each
    /// write a real finish makes, so the empty list is not a blind one.
    #[tokio::test]
    async fn a_dry_finish_sends_no_write_and_a_real_one_sends_each() {
        let car = "bb81467a-b213-4b23-92d6-4b90da170826";
        for (old, new) in [("fix/x", "fix/x-rerail"), ("fix/x", "fix/x")] {
            let dry = calls_of_finish(old, new, true).await;
            assert_eq!(
                writes(&dry),
                Vec::<String>::new(),
                "a dry {old} -> {new} finish wrote"
            );
            assert_eq!(
                dry.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
                [
                    format!("/api/jobs/{car}").as_str(),
                    "/api/jobs?kind=gate-run&limit=100&full=true"
                ],
                "a dry finish still reads what it would write from"
            );
        }
        // The control: the same fixture, run for real, sends every write.
        assert_eq!(
            writes(&calls_of_finish("fix/x", "fix/x-rerail", false).await),
            [
                format!("PATCH /api/jobs/{car}/steps/step-review/metadata"),
                format!("PATCH /api/jobs/{car}/metadata"),
                "PATCH /api/jobs/g-old/metadata".to_string(),
                "PATCH /api/jobs/g-new/metadata".to_string(),
            ]
        );
        assert_eq!(
            writes(&calls_of_finish("fix/x", "fix/x", false).await),
            [
                format!("PATCH /api/jobs/{car}/steps/step-review/metadata"),
                format!("PATCH /api/jobs/{car}/metadata"),
            ],
            "a refresh in place holds and repoints, and stamps nothing"
        );
    }

    /// A REFRESH THAT MOVES THE CAR'S HEAD MOVES ITS HOLD'S HEAD (backlog
    /// 6b12a70f). The sequence measured on car eb86ac0c, 2026-11-05: a
    /// RELEASE at R (2b1591c1), a rerail by merge to A (29943365), the
    /// conductor's re-judge holding the car "waits for a review of this
    /// head" bound to A, then `boss rerail --finish` refreshing the car in
    /// place onto a green at B (df139b70) whose diff the judge clears. The
    /// finish wrote the receipt and left `hold_sha` at A, so the review of
    /// B was refused by the release rule and by `boss release --review`
    /// ("run 7cb2438c reviewed df139b70, and the car's head is
    /// 29943365"), and no supported door moved it. Asked here as the yard
    /// and the release rule ask it: at the head the hold names.
    #[tokio::test]
    async fn a_refresh_carries_the_review_hold_to_the_head_it_refreshed() {
        const R: &str = "2b1591c1983f0000000000000000000000000000";
        const A: &str = "299433653fb10000000000000000000000000000";
        const B: &str = "df139b701abb0000000000000000000000000000";
        const CAR: &str = "eb86ac0c-1361-4aa1-8bbc-a3633b70852b";
        let branch = "feat/binding-plate-claims-lamps-car5";
        let hold = format!(
            "conductor re-judge at {}: released at {}, head is {} (no dock replay records this \
             head as a replay of that one): waits for a review of this head; boss review, then \
             boss release --review (backlog b7b02024)",
            &A[..12],
            &R[..12],
            &A[..12]
        );
        let held = json!({
            "id": CAR,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": branch, "proof_probe": "echo ok", "proof_expect": "ok" },
            "steps": [{ "id": "step-review", "spec_slug": "review",
                        "title": "Open for review", "status": "ready",
                        "metadata": {
                            "hold": hold,
                            boss_jobs::car::HOLD_SHA: A,
                            boss_jobs::car::RELEASE: { "sha": R, "review": "2c4bad6a" },
                            "agent_run": "7cb2438c-0000-4000-8000-000000000000",
                        } }],
        });
        let receipt = json!({ "head": B, "mode": "auto" }).to_string();
        let door = Recording::new(
            held.clone(),
            vec![json!({ "id": "g-b", "metadata": { "branch": branch },
                         "steps": [{ "metadata": { "verdict": "green", "receipt": receipt } }] })],
        );
        let tree = Fixed {
            head: B.to_string(),
            judged: crate::mutating_verb::Judgement::Clear,
        };
        finish(&door, &tree, &held, branch, branch, false)
            .await
            .expect("the refresh finishes");
        // The reviewer's verdict, recorded at B.
        let run = json!({
            "id": "7cb2438c-0000-4000-8000-000000000000",
            "kind": "agent-run",
            "metadata": { "review": { "car": CAR, "verdict": "release", "reviewed_sha": B } },
        });
        // The control: on the car as it stood, the review of B is refused
        // at the head the hold names, in the words the conductor quoted.
        let stale = boss_jobs::car_release::hold_sha(&held).expect("held at A");
        let why = boss_jobs::car_release::vouches(&run, &held, stale).expect_err("held at A");
        assert!(why.contains("and the car's head is 29943365"), "{why}");
        // The car as the finish left it: every review-step write merged
        // onto the review step's metadata, as the merge door would.
        let step_path = format!("/api/jobs/{CAR}/steps/step-review/metadata");
        let mut after = held.clone();
        for (path, body) in door.bodies.into_inner().expect("bodies") {
            if path == step_path {
                let md = after["steps"][0]["metadata"].as_object_mut().expect("md");
                for (k, v) in body.as_object().expect("a merge body") {
                    md.insert(k.clone(), v.clone());
                }
            }
        }
        assert_eq!(
            boss_jobs::car_release::hold_sha(&after),
            Some(B),
            "the hold that waits for a review of this head names the head the car now carries"
        );
        assert_eq!(
            boss_jobs::stranded::marked(&after["steps"][0]["metadata"], "hold").as_deref(),
            Some(hold.as_str()),
            "the hold's reason stands: the car still waits for its review"
        );
        boss_jobs::car_release::vouches(
            &run,
            &after,
            boss_jobs::car_release::hold_sha(&after).expect("bound"),
        )
        .expect("the review of B releases the car refreshed to B");
    }

    // -- the refresh writes only on the hold it judged (cd1f68c6) -------
    //
    // `finish` reads the car, then makes several more calls (the gate-run
    // list, the forge, the judge) before it carries the hold to the new
    // head. The REAL jobs router over an in-memory store serves the car,
    // so the merge door itself judges the write; the gate-run list is a
    // fixture, and answering it is where a concurrent writer moves the
    // review step — Codex's injection (review 5d605949), placed after the
    // judged read and before the write.

    const HELD_AT: &str = "299433653fb10000000000000000000000000000";
    const REFRESHED_TO: &str = "df139b701abb0000000000000000000000000000";
    const REVIEW_HOLD: &str = "conductor re-judge at 299433653fb1 — waits for a review of this \
                               head; boss review, then boss release --review (backlog b7b02024)";

    /// The real door over the real router, answering the gate-run list
    /// from a fixture and, the first time it does, landing `race` on the
    /// review step through the same router.
    struct Racing {
        wire: crate::steps::Wire,
        gate_runs: Vec<Value>,
        race: Option<(String, Value)>,
        raced: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Door for Racing {
        async fn send(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<Option<Value>> {
            if method == reqwest::Method::GET && path.starts_with("/api/jobs?kind=gate-run") {
                if let Some((step_path, write)) = &self.race
                    && !self.raced.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    self.wire
                        .call(reqwest::Method::PATCH, step_path, Some(write.clone()))
                        .await?;
                }
                return Ok(Some(
                    json!({ "data": self.gate_runs, "total": self.gate_runs.len() }),
                ));
            }
            self.wire.call(method, path, body).await
        }

        async fn send_expecting(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
            expect: &serde_json::Map<String, Value>,
        ) -> Result<Option<Value>> {
            self.wire.call_expecting(method, path, body, expect).await
        }
    }

    /// A held car at the dock, seeded into a fresh store behind the real
    /// router; the base, its id, and its review step's id.
    async fn served_held_car(review_md: Value) -> (String, String, String) {
        use boss_jobs::JobsRepository as _;
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        use std::sync::Arc;
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut job = boss_core::job::Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", "feat/binding-plate-claims-lamps-car5"),
            "A held car",
            "emp-bootstrap-admin",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        );
        job.metadata = json!({
            "branch": "feat/binding-plate-claims-lamps-car5",
            "proof_probe": "echo ok", "proof_expect": "ok",
        });
        job.status = boss_core::job::JobStatus::Open;
        jobs.create_job(&job).await.unwrap();
        let mut step = boss_core::job::Step::new(job.id, "task", boss_jobs::car::REVIEW, 4);
        step.spec_slug = Some(boss_jobs::car::REVIEW_SLUG.into());
        step.status = boss_core::job::StepStatus::Ready;
        step.metadata = review_md;
        jobs.add_step(&step).await.unwrap();
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, job.id.to_string(), step.id.to_string())
    }

    fn held_review_md() -> Value {
        json!({
            "hold": REVIEW_HOLD,
            boss_jobs::car::HOLD_SHA: HELD_AT,
            boss_jobs::car::RELEASE: { "sha": "2b1591c1983f", "review": "2c4bad6a" },
        })
    }

    fn signed_wire(base: String) -> crate::steps::Wire {
        crate::steps::Wire::at(
            base,
            Some(crate::identity::Caller {
                id: "claude@algedonic.dev".into(),
                source: crate::identity::Source::Env,
            }),
        )
        .unwrap()
    }

    /// `finish` refreshing the held car in place onto a clear green at
    /// [`REFRESHED_TO`], with `race` landing after its read. Its answer,
    /// the review step's metadata and the car's own metadata after.
    async fn finish_racing(race: Option<Value>) -> (Result<()>, Value, Value) {
        let branch = "feat/binding-plate-claims-lamps-car5";
        let (base, car_id, step_id) = served_held_car(held_review_md()).await;
        let receipt = json!({ "head": REFRESHED_TO, "mode": "auto" }).to_string();
        let door = Racing {
            wire: signed_wire(base),
            gate_runs: vec![json!({ "id": "g-b", "metadata": { "branch": branch },
                "steps": [{ "metadata": { "verdict": "green", "receipt": receipt } }] })],
            race: race.map(|w| (format!("/api/jobs/{car_id}/steps/{step_id}/metadata"), w)),
            raced: std::sync::atomic::AtomicBool::new(false),
        };
        let judged = crate::car_retire::read_via(&door, &car_id).await.unwrap();
        let tree = Fixed {
            head: REFRESHED_TO.to_string(),
            judged: crate::mutating_verb::Judgement::Clear,
        };
        let answer = finish(&door, &tree, &judged, branch, branch, false).await;
        let after = crate::car_retire::read_via(&door, &car_id).await.unwrap();
        let review = boss_jobs::car_release::review_step(&after)
            .and_then(|s| s.get("metadata"))
            .cloned()
            .unwrap_or_default();
        (answer, review, after["metadata"].clone())
    }

    /// The control: nothing moved after the read, so the refresh carries
    /// the hold to the head it refreshed (6b12a70f) and repoints the car.
    #[tokio::test]
    async fn a_refresh_on_the_hold_it_judged_carries_it_and_repoints_the_car() {
        let (answer, review, car) = finish_racing(None).await;
        answer.expect("the row reads as judged");
        assert_eq!(review[boss_jobs::car::HOLD_SHA], REFRESHED_TO);
        assert_eq!(review["hold"], REVIEW_HOLD);
        assert!(car.get("regate_receipt").is_some(), "repointed: {car}");
    }

    /// The race: the hold the refresh read was released, or replaced by a
    /// person's, before it wrote. The refresh is refused in words and
    /// NOTHING is written — not the hold's head, and not the repoint
    /// behind it — so no `hold_sha` is left beside no hold, and no
    /// person's hold is bound to a head a reviewer could release.
    #[tokio::test]
    async fn a_refresh_does_not_rebind_a_hold_that_moved_after_its_read() {
        for race in [
            json!({ "hold": null,
                    boss_jobs::car::RELEASE: { "sha": HELD_AT, "review": "7cb2438c" } }),
            json!({ "hold": "David reads this one first" }),
        ] {
            let mut expected = held_review_md();
            let md = expected.as_object_mut().unwrap();
            md.extend(race.as_object().unwrap().clone());
            md.retain(|_, v| !v.is_null());
            let (answer, review, car) = finish_racing(Some(race.clone())).await;
            let said = format!("{:#}", answer.expect_err("the hold moved since the read"));
            assert!(
                said.contains("changed") && said.contains("Nothing was written"),
                "{said}"
            );
            assert_eq!(
                review, expected,
                "{race}: the review step is as the race left it"
            );
            assert!(
                car.get("regate_receipt").is_none(),
                "{race}: the car is not repointed: {car}"
            );
        }
    }

    /// THE HELPER EVERY JUDGED HOLD WRITE ADOPTS (backlog cd1f68c6) —
    /// the strike release's `lift_struck_hold` next, with
    /// `car_release::release_write_expectation` as its judgement. Against
    /// the real merge door: a judgement the step no longer holds answers
    /// `Moved` and writes nothing; one it holds answers `Written`; and a
    /// hold reason's em-dash survives the header both ways.
    #[tokio::test]
    async fn a_judged_hold_write_answers_moved_and_writes_nothing_when_the_hold_moved() {
        let (base, car_id, step_id) = served_held_car(held_review_md()).await;
        let wire = signed_wire(base);
        let review = |car: Value| {
            boss_jobs::car_release::review_step(&car)
                .and_then(|s| s.get("metadata"))
                .cloned()
                .unwrap_or_default()
        };
        let lift = json!({ "hold": null, "hold_released": { "strike_release": true } });
        let stale = boss_jobs::car::hold_as_judged(&json!({ "hold": "red train #1571",
            boss_jobs::car::HOLD_SHA: null }));
        let answer = crate::car_retire::patch_step_metadata_expecting(
            &wire,
            &car_id,
            &step_id,
            lift.clone(),
            &stale,
        )
        .await
        .expect("a moved hold is an answer, not an error");
        assert_eq!(answer, crate::car_retire::Judged::Moved);
        let stored = review(crate::car_retire::read_via(&wire, &car_id).await.unwrap());
        assert_eq!(stored, held_review_md(), "nothing written");

        let judged = boss_jobs::car::hold_as_judged(&held_review_md());
        let answer = crate::car_retire::patch_step_metadata_expecting(
            &wire, &car_id, &step_id, lift, &judged,
        )
        .await
        .unwrap();
        assert_eq!(answer, crate::car_retire::Judged::Written);
        let stored = review(crate::car_retire::read_via(&wire, &car_id).await.unwrap());
        assert!(stored.get("hold").is_none(), "{stored}");
        assert_eq!(stored["hold_released"]["strike_release"], true);
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

        let picked = pick_onto(&r, &base, "fix/x").expect("pick");
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

        match pick_onto(&r, &base, "fix/y").expect("pick") {
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

    /// A predecessor landed by squash as a SERIES (backlog 48bb0b42): its
    /// first commit conflicts with main's sum instead of replaying empty.
    /// Main holds the series' final state blob for blob, so the prefix is
    /// skipped as landed, named, and the car's own commit replays.
    #[test]
    fn a_squash_landed_series_before_the_car_is_skipped_not_a_conflict() {
        let r = fixture_repo("rerail-series-pick");
        git(&r, &["checkout", "-q", "-b", "fix/s"]).expect("branch");
        write_commit(&r, "first.txt", "1\n", "first car: add first.txt");
        write_commit(&r, "first.txt", "1\n2\n", "first car: second line");
        write_commit(&r, "second.txt", "2\n", "this car: second.txt");
        git(&r, &["checkout", "-q", "main"]).expect("main");
        write_commit(&r, "first.txt", "1\n2\n", "train: first car lands");
        let base = git(&r, &["merge-base", "main", "fix/s"]).expect("base");
        git(&r, &["checkout", "-q", "--detach", "main"]).expect("detach");

        match pick_onto(&r, &base, "fix/s").expect("pick") {
            Picked::Clean { dropped } => {
                assert_eq!(
                    dropped.iter().map(|d| d.title.as_str()).collect::<Vec<_>>(),
                    ["first car: add first.txt", "first car: second line"]
                );
            }
            other => panic!("expected the landed series skipped and a clean replay, got {other:?}"),
        }
        assert_eq!(
            git(&r, &["log", "-1", "--format=%s"]).expect("head"),
            "this car: second.txt"
        );
        assert!(
            git(&r, &["status", "--porcelain"])
                .expect("status")
                .is_empty(),
            "nothing is left mid-cherry-pick"
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

        match pick_onto(&r, &base, "fix/z").expect("pick") {
            Picked::Conflict { files, twin, .. } => {
                assert_eq!(files, "base.txt");
                assert_eq!(twin, None, "main never took this patch");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    const FIVE: &str = "l1\nl2\nl3\nl4\nl5\n";
    const FEATURE: &str = "l1\nl2\nFEATURE\nl4\nl5\n";

    /// One fixture commit: the files it writes, and its subject.
    type Step<'a> = (&'a [(&'a str, &'a str)], &'a str);

    /// A repo: `start` on main, the car's commits on `fix/c`, then main's
    /// own; HEAD detached at main. Returns (repo, base, main's shas).
    fn shaped(
        name: &str,
        start: &[Step],
        car: &[Step],
        main: &[Step],
    ) -> (String, String, Vec<String>) {
        let r = fixture_repo(name);
        let commit = |(files, msg): &Step| -> String {
            for (f, body) in files.iter() {
                std::fs::write(Path::new(&r).join(f), body).expect("write");
                git(&r, &["add", f]).expect("add");
            }
            let id = ["-c", "user.email=t@t", "-c", "user.name=t"];
            git(&r, &[&id[..], &["commit", "-q", "-m", msg]].concat()).expect("commit");
            git(&r, &["rev-parse", "HEAD"]).expect("sha")
        };
        start.iter().for_each(|s| {
            commit(s);
        });
        git(&r, &["checkout", "-q", "-b", "fix/c"]).expect("branch");
        car.iter().for_each(|s| {
            commit(s);
        });
        git(&r, &["checkout", "-q", "main"]).expect("main");
        let shas = main.iter().map(commit).collect();
        let base = git(&r, &["merge-base", "main", "fix/c"]).expect("base");
        git(&r, &["checkout", "-q", "--detach", "main"]).expect("detach");
        (r, base, shas)
    }

    /// RERAIL NEVER DROPS A COMMIT BY ITS TWIN (backlog ec05a62f). Every
    /// shape three adversarial reviews of car fd13ddf2 built against an
    /// automatic drop (runs 9b231beb, 3f14cf26, 1c7dd9c7) — and car
    /// dd64f4ad's own, which a drop was for — now STOPS as a conflict with
    /// nothing dropped and the commit still being picked, and the stop
    /// names main's byte-for-byte twin when there is one. Main took the
    /// patch once in each; in most it has taken it back out since, which
    /// no diff rule these reviews tried could tell from an amendment.
    #[test]
    fn every_reviewed_shape_stops_keeping_its_commit_and_names_any_twin() {
        // Consts, so the fixture slices below are promoted to 'static.
        const NINE: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";
        const NINE_F: &str = "l1\nl2\nl3\nl4\nFEATURE\nl6\nl7\nl8\nl9\n";
        const ADD: &str = "l1\nl2\nl3\nFEATURE\nl4\nl5\n";
        const CRLF: &str = "l1\r\nl2\r\nl3\r\nl4\r\nl5\r\n";
        const CRLF_F: &str = "l1\r\nl2\r\nFEATURE\r\nl4\r\nl5\r\n";
        // Two identical blocks; FEATURE goes into one or the other.
        const BLOCKS: &str = "head\nk1\nk2\nk3\nk4\nk5\nk6\nmid\nk1\nk2\nk3\nk4\nk5\nk6\ntail\n";
        const IN_ONE: &str =
            "head\nk1\nk2\nk3\nFEATURE\nk4\nk5\nk6\nmid\nk1\nk2\nk3\nk4\nk5\nk6\ntail\n";
        const IN_TWO: &str =
            "head\nk1\nk2\nk3\nk4\nk5\nk6\nmid\nk1\nk2\nk3\nFEATURE\nk4\nk5\nk6\ntail\n";
        const K3X: &str =
            "head\nk1\nk2\nK3X\nk4\nk5\nk6\nmid\nk1\nk2\nk3\nFEATURE\nk4\nk5\nk6\ntail\n";
        let five: Step = (&[("base.txt", FIVE)], "five");
        let feat: Step = (&[("base.txt", FEATURE)], "car: feature");
        let landed: Step = (&[("base.txt", FEATURE)], "train: feature (#1)");
        let w: Step = (&[("w.txt", "w\n")], "car: w.txt");
        // (fixture, start, car, main, index of main's commit named as twin)
        let shapes: Vec<(&str, Vec<Step>, Vec<Step>, Vec<Step>, Option<usize>)> = vec![
            // dd64f4ad: a stale copy of a landed change whose lines a
            // later train amended.
            (
                "dd64f4ad",
                vec![five],
                vec![feat, w],
                vec![
                    landed,
                    (
                        &[("base.txt", "l1\nl2\nFEATURE, amended\nl4\nl5\n")],
                        "train: amend (#2)",
                    ),
                ],
                Some(0),
            ),
            (
                "dd64f4ad-pure-add",
                vec![five],
                vec![(&[("base.txt", ADD)], "car: add"), w],
                vec![
                    (&[("base.txt", ADD)], "train: add (#1)"),
                    (
                        &[("base.txt", "l1\nl2\nl3\nFEATURE2\nl4\nl5\n")],
                        "train: amend (#2)",
                    ),
                ],
                Some(0),
            ),
            // 9b231beb: a twin differing only in whitespace is no twin.
            (
                "whitespace-twin",
                vec![(&[("c.yaml", "a:\n  b: 1\n")], "yaml")],
                vec![(&[("c.yaml", "a:\n  b: 1\n  c: 2\n")], "car: nest c")],
                vec![(
                    &[("c.yaml", "a:\n  b: 1\nc: 2\n")],
                    "train: top-level c (#1)",
                )],
                None,
            ),
            (
                "reverted-then-rewritten",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (&[("base.txt", FIVE)], "train: revert (#2)"),
                    (
                        &[("base.txt", "l1\nl2\nOTHER\nl4\nl5\n")],
                        "train: other (#3)",
                    ),
                ],
                Some(0),
            ),
            (
                "relanded-then-amended",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (&[("base.txt", FIVE)], "train: revert (#2)"),
                    (&[("base.txt", FEATURE)], "train: again (#3)"),
                    (
                        &[("base.txt", "l1\nl2\nFEATURE, amended\nl4\nl5\n")],
                        "train: amend (#4)",
                    ),
                ],
                Some(2),
            ),
            // 3f14cf26: reverts no reverse patch shows.
            (
                "revert-after-context-moved",
                vec![(&[("base.txt", NINE)], "nine")],
                vec![(&[("base.txt", NINE_F)], "car: feature")],
                vec![
                    (&[("base.txt", NINE_F)], "train: feature (#1)"),
                    (
                        &[("base.txt", "l1\nl2\nL3X\nl4\nFEATURE\nl6\nl7\nl8\nl9\n")],
                        "train: l3 (#2)",
                    ),
                    (
                        &[("base.txt", "l1\nl2\nL3X\nl4\nl5\nl6\nl7\nl8\nl9\n")],
                        "train: revert #1 (#3)",
                    ),
                    (
                        &[("base.txt", "l1\nl2\nL3X\nl4\nl5\nL6X\nl7\nl8\nl9\n")],
                        "train: l6 (#4)",
                    ),
                ],
                Some(0),
            ),
            (
                "revert-in-a-multi-car-train",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (
                        &[("base.txt", FIVE), ("other.txt", "other car\n")],
                        "train: (2 changes) (#2)",
                    ),
                    (&[("base.txt", "l1\nl2\nl3\nL4X\nl5\n")], "train: l4 (#3)"),
                ],
                Some(0),
            ),
            (
                "partly-reverted",
                vec![(&[("f1", "p1\np2\np3\n"), ("f2", "q1\nq2\nq3\n")], "f1 f2")],
                vec![(
                    &[("f1", "p1\nP2\np3\n"), ("f2", "q1\nQ2\nq3\n")],
                    "car: P2 and Q2",
                )],
                vec![
                    (
                        &[("f1", "p1\nP2\np3\n"), ("f2", "q1\nQ2\nq3\n")],
                        "train: P2 and Q2 (#1)",
                    ),
                    (&[("f1", "p1\np2\np3\n")], "train: P2 out (#2)"),
                    (&[("f2", "q1\nQ2 amended\nq3\n")], "train: amend Q2 (#3)"),
                ],
                Some(0),
            ),
            (
                "added-line-deleted",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (
                        &[("base.txt", "l1\nl2\nl4\nl5\n")],
                        "train: drop the line (#2)",
                    ),
                ],
                Some(0),
            ),
            // 1c7dd9c7: a squashed revert beside an adjacent edit, a hand
            // revert that rewrote whitespace, a twin elsewhere in the file.
            (
                "revert-squashed-with-adjacent-edit",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (
                        &[("base.txt", "l1\nl2\nL4X\nl5\n")],
                        "train: (2 changes) (#2)",
                    ),
                ],
                Some(0),
            ),
            (
                "pure-add-reverted-beside-an-edit",
                vec![five],
                vec![(&[("base.txt", ADD)], "car: add")],
                vec![
                    (&[("base.txt", ADD)], "train: add (#1)"),
                    (
                        &[("base.txt", "l1\nl2\nL3X\nl4\nl5\n")],
                        "train: (2 changes) (#2)",
                    ),
                ],
                Some(0),
            ),
            (
                "revert-plus-same-line-edit",
                vec![five],
                vec![feat],
                vec![
                    landed,
                    (
                        &[("base.txt", "l1\nl2\nL3Y\nl4\nl5\n")],
                        "train: (2 changes) (#2)",
                    ),
                ],
                Some(0),
            ),
            (
                "crlf-revert-by-hand",
                vec![(&[("base.txt", CRLF)], "crlf")],
                vec![(&[("base.txt", CRLF_F)], "car: feature")],
                vec![
                    (&[("base.txt", CRLF_F)], "train: feature (#1)"),
                    (
                        &[("base.txt", "l1\r\nl2\r\nl3\nl4\r\nl5\r\n")],
                        "train: out by hand (#2)",
                    ),
                ],
                Some(0),
            ),
            (
                "trailing-space-revert-by-hand",
                vec![(&[("base.txt", "l1\nl2\nl3 \nl4\nl5\n")], "five")],
                vec![feat],
                vec![landed, (&[("base.txt", FIVE)], "train: out by hand (#2)")],
                Some(0),
            ),
            (
                "twin-in-another-block",
                vec![(&[("f.txt", BLOCKS)], "blocks")],
                vec![(&[("f.txt", IN_ONE)], "car: block one")],
                vec![
                    (&[("f.txt", IN_TWO)], "train: block two (#1)"),
                    (&[("f.txt", K3X)], "train: k3 (#2)"),
                ],
                // Review 0a5496cb, finding 1: the same patch TEXT in the
                // other block is not this change — its blobs differ — so
                // no byte-for-byte twin is named and no skip offered; the
                // stop names main's commit as a text-only match that may
                // or may not be this one (checked below; backlog 2f2caa8e).
                None,
            ),
        ];
        for (name, start, car, main, want) in shapes {
            let (r, base, shas) = shaped(&format!("rerail-twin-{name}"), &start, &car, &main);
            let picked = pick_onto(&r, &base, "fix/c").expect("pick");
            let Picked::Conflict { dropped, twin, .. } = &picked else {
                panic!("{name}: a twin must never drop or land the commit, got {picked:?}");
            };
            assert!(dropped.is_empty(), "{name}: {dropped:?}");
            // The commit is kept: the stop is ON it, for the human.
            let stopped = git(&r, &["rev-parse", "CHERRY_PICK_HEAD"]).expect("picking");
            let car_shas = git(&r, &["rev-list", &format!("{base}..fix/c")]).expect("car");
            assert!(car_shas.lines().any(|s| s == stopped), "{name}: {stopped}");
            match (want, twin) {
                (None, None) => {}
                // A twin of the text alone is said as such: never "byte
                // for byte", never a skip, and the stop is the ordinary one.
                // Never "not this commit's twin" either: from the text
                // alone the stop cannot tell (backlog 2f2caa8e).
                (None, Some(note)) if name == "twin-in-another-block" => {
                    assert!(note.contains(&shas[0][..12]), "{name}: {note}");
                    assert!(note.contains("cannot tell which"), "{name}: {note}");
                    assert!(!note.contains("not this commit's twin"), "{name}: {note}");
                    assert!(!note.contains("byte for byte"), "{name}: {note}");
                    assert!(!note.contains("--skip"), "{name}: {note}");
                }
                (Some(i), Some(note)) => {
                    assert!(
                        note.contains(&shas[i][..12]),
                        "{name} names its twin: {note}"
                    );
                    assert!(note.contains("yours to decide"), "{name}: {note}");
                    assert!(note.contains("byte for byte"), "{name}: {note}");
                    // dd64f4ad stops on the first of two commits: a skip
                    // goes on to the next, so the steps below still apply.
                    if name == "dd64f4ad" {
                        assert!(note.contains("leave the commit out"), "{note}");
                        assert!(!note.contains("nothing main lacks"), "{note}");
                    }
                }
                (want, got) => panic!("{name}: twin {want:?} expected, the stop said {got:?}"),
            }
        }
    }

    /// The note a stop prints for dd64f4ad's shape names the stopped
    /// commit, the twin and its train subject, the read that shows what
    /// main did since, and the way to leave the commit out.
    #[test]
    fn a_conflict_stop_names_the_twin_and_how_to_read_and_skip_it() {
        let five: Step = (&[("base.txt", FIVE)], "five");
        let (r, base, shas) = shaped(
            "rerail-twin-note",
            &[five],
            &[(&[("base.txt", FEATURE)], "stale copy: feature")],
            &[
                (
                    &[("base.txt", FEATURE)],
                    "train: 2026-10-09 (1 changes) (#1015)",
                ),
                (
                    &[("base.txt", "l1\nl2\nFEATURE2\nl4\nl5\n")],
                    "train: amend (#1016)",
                ),
            ],
        );
        let Picked::Conflict {
            twin: Some(note), ..
        } = pick_onto(&r, &base, "fix/c").expect("pick")
        else {
            panic!("expected a conflict naming its twin");
        };
        // Twelve-character shas and quoted paths (review 0a5496cb,
        // finding 6), so the read pastes as printed.
        let onto = &shas[1][..12];
        let twin = &shas[0][..12];
        for want in [
            "\"stale copy: feature\"",
            "\"train: 2026-10-09 (1 changes) (#1015)\"",
            &format!("git -C {r} log -p {twin}..{onto} -- 'base.txt'"),
            &format!("git -C {r} cherry-pick --skip"),
            "Whether main STILL holds it is yours to decide",
            // The car's only commit: a skip leaves HEAD at main, so the
            // note says the car is empty rather than send the human to
            // `--continue` and a gate (review 0a5496cb, finding 3).
            "the car carries nothing main lacks",
            "push nothing and gate nothing",
            // `boss merged` answers MERGED off one past cherry (merged.rs,
            // `git cherry`), so the read above is the evidence, and the
            // note says why the verb is not (backlog 2f2caa8e).
            "close the car as landed, citing the read above",
            "`boss merged fix/c` answers MERGED once main has taken the patch, even if a \
             later commit took it back out, so it cannot",
        ] {
            assert!(note.contains(want), "missing {want:?} in: {note}");
        }
        assert!(!note.contains("confirm with `boss merged"), "{note}");
        assert!(!note.contains("leave the commit out"), "{note}");
    }

    /// A TEXT-ONLY TWIN MAY BE THIS COMMIT'S TWIN (backlog 2f2caa8e,
    /// review 22073d6e of car fd13ddf2). Main took the car's very change,
    /// but on a file it had already changed outside the hunk's context:
    /// the blobs differ, so only the patch text matches — the ordinary way
    /// a stale copy's twin lands. The stop called main's commit "not this
    /// commit's twin", which here is false; it now says it cannot tell,
    /// and names the read that can.
    #[test]
    fn a_text_only_twin_on_a_moved_preimage_is_not_called_no_twin() {
        const NINE: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";
        const NINE_F: &str = "l1\nl2\nl3\nl4\nFEATURE\nl6\nl7\nl8\nl9\n";
        let (r, base, shas) = shaped(
            "rerail-twin-moved-preimage",
            &[(&[("base.txt", NINE)], "nine")],
            &[(&[("base.txt", NINE_F)], "car: feature")],
            &[
                (
                    &[("base.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nL9X\n")],
                    "train: l9 (#1)",
                ),
                (
                    &[("base.txt", "l1\nl2\nl3\nl4\nFEATURE\nl6\nl7\nl8\nL9X\n")],
                    "train: feature (#2)",
                ),
                (
                    &[("base.txt", "l1\nl2\nl3\nl4\nFEATURE2\nl6\nl7\nl8\nL9X\n")],
                    "train: amend (#3)",
                ),
            ],
        );
        let Picked::Conflict {
            twin: Some(note), ..
        } = pick_onto(&r, &base, "fix/c").expect("pick")
        else {
            panic!("expected a conflict naming main's commit");
        };
        let landed = &shas[1][..12];
        assert!(note.contains(landed), "names train #2: {note}");
        assert!(!note.contains("not this commit's twin"), "{note}");
        assert!(note.contains("may be this commit's twin"), "{note}");
        assert!(note.contains("cannot tell which"), "{note}");
        assert!(
            note.contains(&format!("git -C {r} show {landed}")),
            "{note}"
        );
        assert!(!note.contains("byte for byte"), "{note}");
        assert!(!note.contains("--skip"), "{note}");
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
