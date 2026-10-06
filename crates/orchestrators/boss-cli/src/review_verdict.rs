//! A RELEASE IS A REVIEW VERDICT BOUND TO THE HEAD IT JUDGED (backlog
//! b7b02024 finding 7, design 7cedfa29 D3-D6 and D8).
//!
//! WHY. `boss release` was a bare `{"hold": null}` merge on the car's
//! review step: any actor ran it, the builder that gated the car
//! included, and a raw metadata PATCH did the same. Nothing recorded
//! WHICH review released the car or WHICH head it read, so a release at
//! head H1 survived a repair to H2. And every agent signs as
//! `agent-claude`, while the shared machine token lets anyone type any
//! actor id (f623e425 point 4), so no check keyed on WHO wrote the key
//! can hold.
//!
//! WHAT HOLDS INSTEAD — a sha and an artifact, which a typed actor id
//! cannot supply:
//!
//! - `boss review <car> --verdict release|changes` runs INSIDE the
//!   reviewer's run (`BOSS_AGENT_RUN`) and writes [`REVIEW_KEY`] onto
//!   that agent-run packet: `{car, branch, reviewed_sha, verdict,
//!   findings, at}`, the sha read from the forge, never typed. A
//!   review's verdict was prose in a handback until this; now it is a
//!   record ([`review_record`]).
//! - `boss release <car> --review <run>` writes the release record on
//!   the car's review step — `{sha, review, verdict, by, at, main_sha}`
//!   ([`release_record`]) — and clears the hold, only when that run's
//!   review [`vouches`] for the car at its current forge head.
//! - The conductor, at boarding, reads the same record and asks the same
//!   [`vouches`] of the named run in one GET: a release at another sha
//!   is no release, and the car is held again (`mutating_verb::dock`).
//!
//! THE BUILDER IS A RUN, NOT AN ACTOR (D5): the agent-run the car
//! carries as `agent_run`, which the auto-park handler copies from the
//! gate-run whose green filed or last refreshed it. A review by that run
//! is no review.
//!
//! `main_sha` (D8, folding 23fbe4e3 as a record only) says which main
//! the release was given against, so a later red traced to a stale hold
//! is read off the record rather than re-derived. It is not re-checked:
//! design 6af5acdf (2026-09-26) made the train gate the backstop.
//!
//! THE BUILDER DOES NOT RELEASE EITHER (car 3, design 7cedfa29 Q1,
//! enforcement order design b08725c2 row A, after DR readiness
//! 62dac114): `boss release`, in both its forms, refuses a shell whose
//! `BOSS_AGENT_RUN` is the car's builder run ([`may_release`]), or is
//! not a full run id. The refusal binds runs, never a person: a shell
//! with no run is not refused, so the three-verb human road in `boss
//! release --help` stands, and a person who gated the car inside a run
//! of his own releases from a shell without it.
//!
//! WHAT A RELEASE RECORDS OF ITS SHELL, AND WHAT IT CANNOT (reviews
//! 5aa91689 B1, c91ba45b B1'). Every release — the review's (`release`)
//! and the bare one ([`BARE_RELEASE`]) — carries `agent_shell`
//! (CLAUDECODE set) and, when one is exported, `run`, beside `by`
//! ([`Releaser`]). So the record DISTINGUISHES an agent's shell from a
//! person's, and names the run when there is one. It does NOT
//! distinguish a dispatched builder from the operator session: a builder
//! exports its run only in the one shell it gates from, so its later
//! shells have none, and in the dev pod every agent shell — the
//! operator's included — is a child of one process carrying the same
//! session markers (measured 2026-09-30: the operator's shell reads
//! CLAUDE_CODE_CHILD_SESSION=1 and the same CLAUDE_CODE_SESSION_ID as
//! the subagents'). No field built on those markers is recorded, because
//! it could not mean what its name says. Every field here is what the
//! shell SAYS; a shell that unsets CLAUDECODE is recorded as a person's.
//! The real fix is per-actor credentials (design 7cedfa29 D7).
//!
//! WHAT HOLDS REGARDLESS: a review-held car is released only on a
//! RELEASE verdict recorded by a run that is not the builder's — `boss
//! review` refuses the builder's run, and [`vouches`] refuses a verdict
//! the builder's run recorded — so a builder's self-release can only act
//! on another run's verdict at the car's current head.
//!
//! NOT HERE. A human's own-review road through his passkey session (the
//! rest of Q1) is unbuilt. No server-side writer refuses the builder:
//! the merge door cannot know which run a caller is, and the per-field
//! `writer` declaration for `hold`, `hold_sha` and `release` (D7) waits
//! on a credential door — `field_writer::RESOLVABLE_PRINCIPALS` is
//! empty, and the viability lint refuses a writer no door resolves. The
//! conductor does not read the shell fields at boarding.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::steps::Wire;

/// The agent-run packet key a review's verdict lives under.
pub(crate) const REVIEW_KEY: &str = "review";

/// The verdict that releases a car.
pub(crate) const RELEASE: &str = "release";

/// Every verdict `boss review` records.
pub(crate) const VERDICTS: [&str; 2] = [RELEASE, "changes"];

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// The run that built this car: its `agent_run`, or `None` for a car
/// gated by hand.
pub(crate) fn builder_run(car: &Value) -> Option<&str> {
    Some(str_at(car, "/metadata/agent_run")).filter(|s| !s.is_empty())
}

/// PURE: does `run`'s recorded review vouch for `car` at `head`? `Ok`
/// only when every fact holds: the run is an agent-run that is not the
/// car's builder, it records a review OF THIS CAR, the verdict is
/// RELEASE, and it read exactly `head`. Each refusal names the fact.
pub(crate) fn vouches(run: &Value, car: &Value, head: &str) -> Result<(), String> {
    let run_id = str_at(run, "/id");
    let car_id = str_at(car, "/id");
    if run_id.is_empty() {
        return Err("the named review run carries no id".into());
    }
    if str_at(run, "/kind") != "agent-run" {
        return Err(format!(
            "{} is a {} packet, not an agent-run — a review verdict is recorded on the \
             reviewer's run by boss review",
            short(run_id),
            str_at(run, "/kind")
        ));
    }
    if builder_run(car) == Some(run_id) {
        return Err(format!(
            "run {} built this car (its agent_run) — the builder's review is no review",
            short(run_id)
        ));
    }
    // A DISPATCHED run names the packet and step it claimed (`boss
    // dispatch` writes both). Held to them, a run opened on car A's
    // review cannot vouch for car B (review 0545d1b1, 2026-09-30). A
    // hand-filed run names none, and is judged as before.
    let opened_on = str_at(run, "/metadata/packet");
    if !opened_on.is_empty() {
        if opened_on != car_id {
            return Err(format!(
                "run {} was opened on {}, not on car {} — a reviewer vouches only for the \
                 car it was dispatched to",
                short(run_id),
                short(opened_on),
                short(car_id)
            ));
        }
        let step = str_at(run, "/metadata/step");
        if step != boss_jobs::car::REVIEW_SLUG {
            return Err(format!(
                "run {} was opened on step `{step}` of car {}, not on its `{}` step",
                short(run_id),
                short(car_id),
                boss_jobs::car::REVIEW_SLUG
            ));
        }
    }
    let Some(review) = run
        .pointer(&format!("/metadata/{REVIEW_KEY}"))
        .filter(|r| r.is_object())
    else {
        return Err(format!(
            "run {} records no review verdict — the reviewer runs boss review <car> --verdict \
             release inside its own run",
            short(run_id)
        ));
    };
    let reviewed = str_at(review, "/car");
    if reviewed != car_id {
        return Err(format!(
            "run {}'s review is of car {}, not {}",
            short(run_id),
            short(reviewed),
            short(car_id)
        ));
    }
    let verdict = str_at(review, "/verdict");
    if verdict != RELEASE {
        return Err(format!(
            "run {}'s verdict is {:?}, not {RELEASE:?}",
            short(run_id),
            verdict
        ));
    }
    let sha = str_at(review, "/reviewed_sha");
    if head.is_empty() || sha != head {
        return Err(format!(
            "run {} reviewed {}, and the car's head is {} — a review of another head is no \
             review of this one",
            short(run_id),
            short(sha),
            short(head)
        ));
    }
    Ok(())
}

/// The review-step key a BARE release records itself under: `{hold, by,
/// at, agent_shell, run?}` — what was lifted, and who and what kind of
/// shell lifted it (review 5aa91689 N2). One record: the next bare
/// release overwrites it, and earlier ones survive in the audit log's
/// PATCH events. Not `boss_jobs::car::RELEASE`: the conductor reads that
/// key as a review's release and checks the review it names, and a bare
/// release names none — nor may it overwrite one a review gave.
pub(crate) const BARE_RELEASE: &str = "hold_released";

/// The shell a release is given from, as its environment says: the run
/// it is (`BOSS_AGENT_RUN`), and whether it is an agent's shell at all
/// (`CLAUDECODE`). That tells an agent's shell from a person's; it does
/// NOT tell a dispatched builder from the operator session, whose shells
/// share one process's markers in the dev pod (review c91ba45b, measured
/// 2026-09-30) — so none of those markers is recorded. Recorded, not
/// trusted: a shell can unset either, which is the forgeability design
/// 7cedfa29 D7 leaves until per-actor credentials land.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Releaser {
    /// `BOSS_AGENT_RUN`, when set and not blank.
    pub run: Option<String>,
    /// `CLAUDECODE` is set and not blank: an agent's (Claude Code) shell.
    pub agent_shell: bool,
}

impl Releaser {
    /// The shell this process runs in.
    pub(crate) fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// PURE: a shell read through `get`, so a test hands in its own.
    pub(crate) fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Self {
            run: set(crate::gate::AGENT_RUN_ENV),
            agent_shell: set("CLAUDECODE").is_some(),
        }
    }

    /// The run this shell is, if any.
    pub(crate) fn run(&self) -> Option<&str> {
        self.run.as_deref()
    }

    /// What every release records of its shell: `agent_shell` always,
    /// `run` when set.
    pub(crate) fn provenance(&self) -> Value {
        let mut out = serde_json::Map::new();
        if let Some(run) = self.run() {
            out.insert("run".into(), json!(run));
        }
        out.insert("agent_shell".into(), json!(self.agent_shell));
        Value::Object(out)
    }
}

/// The record a bare release writes ([`BARE_RELEASE`]).
pub(crate) fn bare_release_record(hold: &str, by: &str, at: &str, releaser: &Releaser) -> Value {
    let mut rec = releaser.provenance();
    rec["hold"] = json!(hold);
    rec["by"] = json!(by);
    rec["at"] = json!(at);
    rec
}

/// PURE: may the run this shell is (`releaser`, its `BOSS_AGENT_RUN`)
/// release `car`? Refused when it is not a full run id — the record
/// carries the id that was checked, never whatever the variable held
/// (review 5aa91689 N5) — and when it is the car's builder run
/// (backlog b7b02024 car 3, design 7cedfa29 Q1, design b08725c2 row A).
///
/// THE BUILDER RUN is the car's `agent_run`: the run the gate that filed
/// the car stamped, re-stamped by every later green that carries a run
/// (the auto-park refresh). A green with NO run — a gate launched by
/// hand, or the dock's own replay — keeps the one before, so after a
/// hand repair the refusal still binds the run that built the car, not
/// the repairer (review 5aa91689 N1: fails open, locks no one out).
///
/// No run in the shell, another run, or a car with no builder run is
/// not refused: the refusal binds agent runs, never a person, and a
/// person's road is the release from a shell that is not the builder
/// run. The error is the whole refusal — the car, the run and that road
/// — because a refusal that leaves its reader to find the way round is
/// a dead end at the wrong moment.
pub(crate) fn may_release(car: &Value, releaser: Option<&str>) -> Result<(), String> {
    let env = crate::gate::AGENT_RUN_ENV;
    let id = str_at(car, "/id");
    let branch = str_at(car, "/metadata/branch");
    let target = if branch.is_empty() { short(id) } else { branch };
    let Some(run) = releaser.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(());
    };
    if !crate::job::looks_like_uuid(run) {
        return Err(format!(
            "boss release: REFUSED — {env}={run:?} is not a full run id (36 characters, \
             8-4-4-4-12), and a release records the run that gave it. Nothing was written. \
             Export the run's full id, or release from a shell that is no run: `env -u {env} \
             boss release {target} …`."
        ));
    }
    let Some(builder) = builder_run(car) else {
        return Ok(());
    };
    if !run.eq_ignore_ascii_case(builder) {
        return Ok(());
    }
    Err(format!(
        "boss release: REFUSED — this shell is run {builder} ({env}), the run that built car \
         {} {branch} (the car's agent_run, stamped by the green that filed or last refreshed \
         it), and the run that built a car does not release it (backlog b7b02024 car 3). \
         Nothing was written; the car stays held. The road around it: any other actor \
         releases it on its review, from a shell that is not this run — `boss release \
         {target} --review <the reviewer's run, full id>`, the three verbs `boss release \
         --help` names. A person who gated the car inside a run of his own releases from a \
         shell without it: `env -u {env} boss release {target} --review <run>`.",
        short(id)
    ))
}

/// The review a reviewer's run records ([`REVIEW_KEY`]).
pub(crate) fn review_record(
    car: &Value,
    reviewed_sha: &str,
    verdict: &str,
    findings: &str,
    at: &str,
) -> Value {
    json!({
        "car": str_at(car, "/id"),
        "branch": str_at(car, "/metadata/branch"),
        "reviewed_sha": reviewed_sha,
        "verdict": verdict,
        "findings": findings,
        "at": at,
    })
}

/// The release record on the review step (`boss_jobs::car::RELEASE`).
pub(crate) fn release_record(sha: &str, review: &str, by: &str, at: &str, main_sha: &str) -> Value {
    json!({
        "sha": sha,
        "review": review,
        "verdict": RELEASE,
        "by": by,
        "at": at,
        "main_sha": main_sha,
    })
}

/// The release a car's review step records, as `(sha, review)` — or
/// `None` when it records none.
pub(crate) fn release_on(car: &Value) -> Option<Release<'_>> {
    let review =
        boss_jobs::car::find_step(car, boss_jobs::car::REVIEW_SLUG, boss_jobs::car::REVIEW)?;
    let release = review
        .pointer(&format!("/metadata/{}", boss_jobs::car::RELEASE))
        .filter(|r| r.is_object())?;
    let sha = str_at(release, "/sha");
    Some(Release {
        sha,
        review: str_at(release, "/review"),
        reviewed: Some(str_at(release, "/reviewed_sha"))
            .filter(|s| !s.is_empty())
            .unwrap_or(sha),
        record: release,
    })
}

/// A release as the dock reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release<'a> {
    /// The car head the release stands at.
    pub sha: &'a str,
    /// The review run it names.
    pub review: &'a str,
    /// The head that review READ — `sha`, unless the release was carried
    /// forward over a mechanical replay (`carried_forward`).
    pub reviewed: &'a str,
    /// The record as stored.
    pub record: &'a Value,
}

/// The release a dock replay CARRIES FORWARD to `head` (backlog
/// b7b02024, review F3). The dock's re-gate replays a released car onto
/// main, which gives it a new head; the review read the old one. When
/// the conductor has proved the replay changed nothing of the car's own
/// — its re-gate stamp records the replay from `release.sha`, and the
/// car's diff from its merge-base has the same patch-id on both heads —
/// the release moves to `head`, keeping the head the review READ as
/// `reviewed_sha` and naming the head it moved from and why. Everything
/// else in the record stands, so the review is still checked against
/// the head it read.
pub(crate) fn carried_forward(release: &Release<'_>, head: &str, patch_id: &str) -> Value {
    let mut rec = release.record.clone();
    rec["sha"] = json!(head);
    rec["reviewed_sha"] = json!(release.reviewed);
    rec["carried_from"] = json!(release.sha);
    rec["carried_by"] = json!(format!(
        "the dock's replay onto main: the car's own diff is unchanged (patch-id {patch_id})"
    ));
    rec
}

/// A ref's sha on the forge (`origin` in the cwd's clone), read live.
pub(crate) fn forge_sha(refname: &str) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "origin", refname])
        .output()
        .context("running git ls-remote")?;
    if !out.status.success() {
        bail!(
            "git ls-remote origin {refname}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .filter(|s| s.len() == 40)
        .ok_or_else(|| anyhow!("the forge holds no {refname}"))
}

/// A car's branch, or a refusal naming the car.
fn branch_of(car: &Value) -> Result<&str> {
    Some(str_at(car, "/metadata/branch"))
        .filter(|b| !b.is_empty())
        .ok_or_else(|| anyhow!("car {} names no branch", short(str_at(car, "/id"))))
}

/// `boss review <car> --verdict <v>`: record the verdict of the run this
/// shell is (`BOSS_AGENT_RUN`) on that run's own packet, at the car's
/// forge head. `forge` reads a ref's sha (the live one is [`forge_sha`]).
pub(crate) async fn record(
    wire: &Wire,
    car: &str,
    verdict: &str,
    findings: &str,
    run: Option<&str>,
    forge: impl Fn(&str) -> Result<String>,
) -> Result<()> {
    if !VERDICTS.contains(&verdict) {
        bail!("--verdict {verdict:?}: one of {}", VERDICTS.join(" | "));
    }
    let run_id = run
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "boss review: REFUSED — no {} in this shell. A verdict is recorded on the \
             reviewer's own run, so it runs inside that run: export {}=<the run's full id>",
                crate::gate::AGENT_RUN_ENV,
                crate::gate::AGENT_RUN_ENV
            )
        })?;
    if !crate::job::looks_like_uuid(run_id) {
        bail!("boss review: {run_id:?} is not a full run id");
    }
    let packet = wire.car(car).await?;
    if builder_run(&packet) == Some(run_id) {
        bail!(
            "boss review: REFUSED — run {} built car {} (its agent_run); the builder's review \
             is no review",
            short(run_id),
            short(str_at(&packet, "/id"))
        );
    }
    let run_packet = wire.packet(run_id).await?;
    if str_at(&run_packet, "/kind") != "agent-run" {
        bail!(
            "boss review: {} is a {} packet, not an agent-run",
            short(run_id),
            str_at(&run_packet, "/kind")
        );
    }
    let branch = branch_of(&packet)?;
    let head = forge(&format!("refs/heads/{branch}"))?;
    let at = crate::gate::stamp(boss_clock_client::wall_now());
    let rec = review_record(&packet, &head, verdict, findings.trim(), &at);
    wire.patch_job_metadata(run_id, json!({ REVIEW_KEY: rec.clone() }))
        .await
        .context("writing the review onto the reviewer's run")?;
    let after = wire.packet(run_id).await?;
    if after.pointer(&format!("/metadata/{REVIEW_KEY}")) != Some(&rec) {
        bail!("the API answered the write, but run {run_id} does not read back the review sent");
    }
    println!(
        "boss review: run {} — {verdict} for car {} {branch} at {}{}",
        short(run_id),
        short(str_at(&packet, "/id")),
        short(&head),
        if verdict == RELEASE {
            format!("\n  the operator releases it: boss release {branch} --review {run_id}")
        } else {
            String::new()
        }
    );
    Ok(())
}

/// `boss release <car> --review <run>`: release the car on a review
/// verdict of RELEASE at its current forge head, recording the release.
/// `releaser` is the shell it is given from: the car's builder run is
/// refused ([`may_release`]) before anything is read from the forge or
/// written, and the shell's [`Releaser::provenance`] rides on the record.
pub(crate) async fn release(
    wire: &Wire,
    car: &str,
    review: &str,
    releaser: &Releaser,
    forge: impl Fn(&str) -> Result<String>,
) -> Result<()> {
    if !crate::job::looks_like_uuid(review) {
        bail!(
            "boss release: --review {review:?} is not a full run id — the record carries the id \
             that was checked, never the characters that were typed"
        );
    }
    let packet = wire.car(car).await?;
    let step = crate::steps::holdable(&packet).map_err(|e| anyhow!("{e}"))?;
    may_release(&packet, releaser.run()).map_err(|why| anyhow!("{why}"))?;
    let branch = branch_of(&packet)?;
    let head = forge(&format!("refs/heads/{branch}"))?;
    let main = forge("refs/heads/main")?;
    let run = wire.packet(review).await?;
    vouches(&run, &packet, &head).map_err(|why| anyhow!("boss release: REFUSED — {why}"))?;
    let by = wire
        .caller_id()
        .context("an unnamed release is refused before it is sent")?;
    let at = crate::gate::stamp(boss_clock_client::wall_now());
    // Which run and which shell released it, beside `by`: every agent
    // signs as agent-claude, so the actor alone cannot say (car 3,
    // review 5aa91689 B1).
    let mut rec = release_record(&head, review, by, &at, &main);
    if let (Some(fields), Value::Object(shell)) = (rec.as_object_mut(), releaser.provenance()) {
        fields.extend(shell);
    }
    let jid = str_at(&packet, "/id");
    let sid = str_at(step, "/id");
    wire.call(
        reqwest::Method::PATCH,
        &format!("/api/jobs/{jid}/steps/{sid}/metadata"),
        Some(json!({ "hold": Value::Null, boss_jobs::car::RELEASE: rec })),
    )
    .await
    .context("writing the release onto the car's review step")?;
    let after = wire.packet(jid).await?;
    let held =
        boss_jobs::car::find_step(&after, boss_jobs::car::REVIEW_SLUG, boss_jobs::car::REVIEW)
            .and_then(|s| s.get("metadata"))
            .and_then(boss_jobs::stranded::hold_reason);
    if let Some(h) = held {
        bail!("the API answered the write but the review step still reads as held: {h:?}");
    }
    if release_on(&after).map(|r| r.sha) != Some(head.as_str()) {
        bail!("the API answered the write but the car does not read back a release at {head}");
    }
    println!(
        "boss release: {} {branch} — released at {} on review run {} (main {})\n  it boards \
         at the next tick while its head stays {}",
        short(jid),
        short(&head),
        short(review),
        short(&main),
        short(&head)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAR: &str = "c0ffee00-0000-4000-8000-000000000000";
    const BUILDER: &str = "b0b0b0b0-0000-4000-8000-000000000000";
    const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";
    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    fn car() -> Value {
        json!({"id": CAR, "kind": "ship-a-change",
               "metadata": {"branch": "feat/x", "agent_run": BUILDER}})
    }

    fn run(id: &str, review: Value) -> Value {
        json!({"id": id, "kind": "agent-run", "metadata": {REVIEW_KEY: review}})
    }

    fn release_at(sha: &str) -> Value {
        review_record(&car(), sha, RELEASE, "no findings", "2026-09-28T20:00:00Z")
    }

    #[test]
    fn a_review_vouches_only_for_its_car_at_the_head_it_read() {
        assert_eq!(vouches(&run(REVIEWER, release_at(H1)), &car(), H1), Ok(()));
        let refused = |r: Value, head: &str, want: &str| {
            let why = vouches(&r, &car(), head).expect_err(want);
            assert!(why.contains(want), "{want} in {why}");
        };
        refused(run(REVIEWER, release_at(H1)), H2, "reviewed 11111111");
        refused(run(REVIEWER, release_at(H1)), "", "another head");
        refused(run(BUILDER, release_at(H1)), H1, "built this car");
        let mut changes = release_at(H1);
        changes["verdict"] = json!("changes");
        refused(run(REVIEWER, changes), H1, "not \"release\"");
        let mut other = release_at(H1);
        other["car"] = json!("0ther000-0000-4000-8000-000000000000");
        refused(run(REVIEWER, other), H1, "not c0ffee00");
        refused(
            json!({"id": REVIEWER, "kind": "agent-run", "metadata": {}}),
            H1,
            "records no review verdict",
        );
        let mut gate = run(REVIEWER, release_at(H1));
        gate["kind"] = json!("gate-run");
        refused(gate, H1, "not an agent-run");
    }

    /// A DISPATCHED REVIEWER VOUCHES ONLY FOR THE CAR IT WAS OPENED ON
    /// (review 0545d1b1 of car bc9ef34f, 2026-09-30). `boss dispatch`
    /// files a reviewer run naming the packet and step it claimed
    /// (`metadata.packet`, `metadata.step`). Its recorded review names a
    /// car too, and nothing held the two equal: a run opened on car A's
    /// review could record a verdict on car B, and car B's release would
    /// accept it. A run that names a packet is held to it — the car, at
    /// its `review` step. A run that names none (the three-verb human
    /// road, `boss job file --kind agent-run`) is judged as before.
    #[test]
    fn a_dispatched_review_vouches_only_for_the_car_it_was_opened_on() {
        let opened_on = |packet: &str, step: &str| {
            let mut r = run(REVIEWER, release_at(H1));
            r["metadata"]["packet"] = json!(packet);
            r["metadata"]["step"] = json!(step);
            r
        };
        assert_eq!(vouches(&opened_on(CAR, "review"), &car(), H1), Ok(()));
        let why = vouches(
            &opened_on("0ther000-0000-4000-8000-000000000000", "review"),
            &car(),
            H1,
        )
        .expect_err("a run opened on another car");
        assert!(why.contains("opened on 0ther000"), "{why}");
        let why = vouches(&opened_on(CAR, "build"), &car(), H1)
            .expect_err("a run opened on another step");
        assert!(why.contains("`build`"), "{why}");
        // No packet named: the hand-filed road is untouched.
        assert_eq!(vouches(&run(REVIEWER, release_at(H1)), &car(), H1), Ok(()));
    }

    /// Car 3 (design b08725c2 row A): the RELEASER is judged too, not
    /// only the review it names. The run that gated the car's head is
    /// refused, the refusal names the car, that run and the road around
    /// it; every other shell — another run, or none (a person with no
    /// agent running) — is not.
    #[test]
    fn the_run_that_gated_the_car_does_not_release_it() {
        let why = may_release(&car(), Some(BUILDER)).expect_err("the builder releasing");
        for want in [
            "REFUSED",
            "c0ffee00",
            "feat/x",
            BUILDER,
            "boss release feat/x --review",
            "env -u BOSS_AGENT_RUN",
        ] {
            assert!(why.contains(want), "{want} in {why}");
        }
        // Spelled with stray case or space, it is still that run.
        assert!(may_release(&car(), Some(" B0B0B0B0-0000-4000-8000-000000000000 ")).is_err());
        assert_eq!(may_release(&car(), Some(REVIEWER)), Ok(()));
        assert_eq!(may_release(&car(), None), Ok(()));
        assert_eq!(may_release(&car(), Some("  ")), Ok(()));
        // A car gated with no run (a person, by hand) has no builder
        // run to refuse: whoever releases it is judged on the review.
        let mut by_hand = car();
        by_hand["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("agent_run");
        assert_eq!(may_release(&by_hand, Some(BUILDER)), Ok(()));
        // A padded agent_run on a hand-edited car is still that run
        // (review 5aa91689 N4): both sides are trimmed.
        let mut padded = car();
        padded["metadata"]["agent_run"] = json!(format!("  {BUILDER} "));
        assert!(may_release(&padded, Some(BUILDER)).is_err());
    }

    /// A run that is not a full id is refused before anything is
    /// written, naming the shape (review 5aa91689 N5): the record carries
    /// the id that was checked, never whatever the variable held.
    #[test]
    fn a_releasing_run_that_is_not_a_full_id_is_refused_by_its_shape() {
        for typo in [
            "b0b0b0b0",
            "not-a-run",
            "b0b0b0b0-0000-4000-8000-00000000000",
        ] {
            let why = may_release(&car(), Some(typo)).expect_err(typo);
            assert!(why.contains("not a full run id"), "{why}");
            assert!(why.contains("env -u BOSS_AGENT_RUN"), "{why}");
        }
    }

    /// What a release records of its shell (reviews 5aa91689 B1 and
    /// c91ba45b B1'): `agent_shell` — an agent's shell or a person's —
    /// and `run` when one is exported. NOTHING that claims to tell a
    /// dispatched builder from the operator session: in the dev pod every
    /// agent shell, the operator's included, is a child of one process
    /// and carries the same CLAUDE_CODE_CHILD_SESSION=1 and session id
    /// (measured 2026-09-30), so no field built on them is recorded.
    #[test]
    fn a_releasers_provenance_is_read_from_its_shell() {
        let shell = |vars: &'static [(&'static str, &'static str)]| {
            Releaser::from_vars(|k| {
                vars.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        let agent = shell(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_CHILD_SESSION", "1"),
            (
                "CLAUDE_CODE_SESSION_ID",
                "f267c9a2-1e61-4860-bc7b-9f9d19684876",
            ),
        ]);
        assert_eq!(agent.run(), None);
        assert_eq!(
            agent.provenance(),
            json!({"agent_shell": true}),
            "the shared session markers are not recorded as if they were an identity"
        );
        let person = shell(&[]);
        assert_eq!(person.provenance(), json!({"agent_shell": false}));
        assert_eq!(
            shell(&[("CLAUDECODE", "  ")]).provenance(),
            json!({"agent_shell": false}),
            "a blank marker is no marker"
        );
        let run = shell(&[("BOSS_AGENT_RUN", REVIEWER), ("CLAUDECODE", "1")]);
        assert_eq!(run.run(), Some(REVIEWER));
        assert_eq!(
            run.provenance(),
            json!({"run": REVIEWER, "agent_shell": true})
        );
    }

    /// A car gated by hand has no builder run to refuse; its reviewer
    /// is judged on the verdict and the head alone.
    #[test]
    fn a_car_with_no_builder_run_is_released_on_the_review_alone() {
        let mut by_hand = car();
        by_hand["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("agent_run");
        assert_eq!(builder_run(&by_hand), None);
        assert_eq!(
            vouches(&run(REVIEWER, release_at(H1)), &by_hand, H1),
            Ok(())
        );
    }

    #[test]
    fn the_release_record_names_the_head_the_review_and_main() {
        let rec = release_record(H1, REVIEWER, "emp-david", "2026-09-28T20:00:00Z", H2);
        assert_eq!(
            rec,
            json!({"sha": H1, "review": REVIEWER, "verdict": "release", "by": "emp-david",
                   "at": "2026-09-28T20:00:00Z", "main_sha": H2})
        );
        let on = json!({"id": CAR, "steps": [{"id": "r", "spec_slug": "review",
            "title": boss_jobs::car::REVIEW, "status": "ready",
            "metadata": {boss_jobs::car::RELEASE: rec}}]});
        let r = release_on(&on).expect("a release");
        assert_eq!((r.sha, r.review, r.reviewed), (H1, REVIEWER, H1));
        assert_eq!(release_on(&car()), None);

        // Carried over a dock replay: the release stands at the new head
        // and still names the head its review READ.
        let fwd = carried_forward(&r, H2, "abc123");
        assert_eq!(fwd["sha"], json!(H2));
        assert_eq!(fwd["reviewed_sha"], json!(H1));
        assert_eq!(fwd["carried_from"], json!(H1));
        assert_eq!(fwd["review"], json!(REVIEWER));
        assert!(fwd["carried_by"].as_str().unwrap().contains("abc123"));
        let moved = json!({"id": CAR, "steps": [{"id": "r", "spec_slug": "review",
            "title": boss_jobs::car::REVIEW, "status": "ready",
            "metadata": {boss_jobs::car::RELEASE: fwd}}]});
        let m = release_on(&moved).expect("a release");
        assert_eq!((m.sha, m.reviewed), (H2, H1));
    }
}
