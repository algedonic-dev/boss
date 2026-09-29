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
//! NOT HERE. Refusing a release by the actor that built the car, and a
//! human's own-review road through his passkey session, are design
//! 7cedfa29 Q1 — car 3, sequenced behind DR readiness 62dac114. The
//! per-field `writer` declaration for `hold`, `hold_sha` and `release`
//! (D7) waits on a credential door: `field_writer::RESOLVABLE_PRINCIPALS`
//! is empty, and the viability lint refuses a writer no door resolves.

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
pub(crate) async fn release(
    wire: &Wire,
    car: &str,
    review: &str,
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
    let branch = branch_of(&packet)?;
    let head = forge(&format!("refs/heads/{branch}"))?;
    let main = forge("refs/heads/main")?;
    let run = wire.packet(review).await?;
    vouches(&run, &packet, &head).map_err(|why| anyhow!("boss release: REFUSED — {why}"))?;
    let by = wire
        .caller_id()
        .context("an unnamed release is refused before it is sent")?;
    let at = crate::gate::stamp(boss_clock_client::wall_now());
    let rec = release_record(&head, review, by, &at, &main);
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
