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
//! - `boss release <car> --review <run>` asks the jobs API's release
//!   door, `POST /api/jobs/{car}/release`, with the head and main it
//!   read from the forge and the shell it runs in ([`release`]). The
//!   RULE is `boss_jobs::car_release` (backlog ed8e5c85). It lived here
//!   until the yard needed the same act, and the yard cannot run a CLI.
//!   The door writes `{sha, review, verdict, by, at, main_sha,
//!   reviewer_model, agent_shell, run?}` on the car's review step and
//!   clears the hold, only when that run's review vouches for the car at
//!   that head. `by` is the caller the API authenticated.
//! - The conductor, at boarding, reads the same record and asks the same
//!   [`vouches_with_executor`] of the named run in one GET: a release at
//!   another sha is no release, and the car is held again
//!   (`mutating_verb::dock`).
//!
//! THE BUILDER IS A RUN, NOT AN ACTOR (D5): the agent-run the car
//! carries as `agent_run`, which the auto-park handler copies from the
//! gate-run whose green filed or last refreshed it. A review by that run
//! is no review, and that run does not release the car in either form
//! (`car_release::may_release_identity`, car 3 of design 7cedfa29). The
//! refusal binds runs, never a person: a shell with no run is not
//! refused, so the three-verb human road in `boss release --help`
//! stands.
//!
//! WHAT A RELEASE RECORDS OF ITS SHELL, AND WHAT IT CANNOT (reviews
//! 5aa91689 B1, c91ba45b B1'). Every release carries `agent_shell`
//! (CLAUDECODE set) and, when one is exported, `run`, beside `by`
//! ([`Releaser`]). So the record DISTINGUISHES an agent's shell from a
//! person's, and names the run when there is one. It does NOT
//! distinguish a dispatched builder from the operator session: in the
//! dev pod every agent shell is a child of one process carrying the same
//! session markers (measured 2026-09-30). Every field here is what the
//! shell SAYS. The real fix is per-actor credentials (design 7cedfa29
//! D7).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::steps::Wire;

// The rule's one copy, in core (backlog ed8e5c85). Re-exported so the
// conductor, `boss review` and the bare release read it where they
// always have.
pub(crate) use boss_jobs::car_release::{
    ExecutorRecordReader, ORIGINAL_VERDICT_KEY, RELEASE, REVIEW_KEY, VERDICTS, branch_of,
    builder_run, immutable_record_resource, judgment_time, may_release, original_verdict_matches,
    pinned_executor_requirement, read_executor_eligibility, vouches_with_executor,
};
#[cfg(test)]
pub(crate) use boss_jobs::car_release::{release_record, reviewer_model, vouches};

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

#[async_trait::async_trait]
impl ExecutorRecordReader for Wire {
    async fn protocol_version(&self, kind: &str, version: i64) -> Result<Option<Value>> {
        self.call(
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions/{version}"),
            None,
        )
        .await
    }
    async fn executor_original(
        &self,
        job: uuid::Uuid,
        step: uuid::Uuid,
        key: &str,
    ) -> Result<Option<boss_jobs::first_record::FirstRecord>> {
        self.immutable_record(job, step, key).await
    }
}

fn select_original_verdict(original: Option<&Value>, candidate: &Value) -> Result<Value, String> {
    let Some(original) = original else {
        return Ok(candidate.clone());
    };
    let mut original_judgment = original.clone();
    let mut candidate_judgment = candidate.clone();
    original_judgment
        .as_object_mut()
        .ok_or("original verdict is not an object")?
        .remove("at");
    candidate_judgment
        .as_object_mut()
        .ok_or("candidate verdict is not an object")?
        .remove("at");
    if original_judgment != candidate_judgment {
        return Err(
            "the original immutable verdict cannot be replaced; use a distinct review run".into(),
        );
    }
    Ok(original.clone())
}

async fn persist_original_verdict(
    wire: &Wire,
    native: uuid::Uuid,
    building: uuid::Uuid,
    value: &Value,
) -> Result<boss_jobs::first_record::FirstRecord> {
    use boss_jobs::first_record::FirstRecordResult;
    let response = wire
        .call(
            reqwest::Method::POST,
            &format!("/api/jobs/{native}/steps/{building}/metadata/records"),
            Some(json!({"key":ORIGINAL_VERDICT_KEY,"value":value,"expected_absence":true})),
        )
        .await?
        .context("the immutable verdict write returned no record")?;
    let original = match serde_json::from_value::<FirstRecordResult>(response)
        .context("the immutable verdict write returned an unreadable record")?
    {
        FirstRecordResult::Recorded(record) | FirstRecordResult::Replayed(record) => record,
        _ => bail!("the immutable verdict write refused; no mutable verdict was projected"),
    };
    immutable_record_resource(&original, native, building, ORIGINAL_VERDICT_KEY)?;
    use sha2::{Digest, Sha256};
    let scalar: Value = serde_json::from_str(&original.value_json)
        .context("the immutable verdict scalar is unreadable")?;
    if !original.matches(value)
        || original.value != scalar
        || boss_core::job::canonical_json_bytes(&scalar) != original.canonical
        || hex::encode(Sha256::digest(&original.canonical)) != original.digest
    {
        bail!("the immutable verdict write returned a different judgment");
    }
    Ok(original)
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
    let executor = read_executor_eligibility(wire, &run_packet, &packet, &head).await.context("boss review: FORMAL executor eligibility refused; advisory findings remain reportable on the run")?;
    let at = crate::sor_client::stamp(boss_clock_client::wall_now());
    let mut rec = review_record(&packet, &head, verdict, findings.trim(), &at);
    if let Some(binding) = executor.record_binding() {
        rec["executor_binding"] = binding;
    }
    if executor.is_verified() {
        rec = select_original_verdict(
            executor.original_verdict().map(|record| &record.value),
            &rec,
        )
        .map_err(|why| anyhow!(why))?;
        let receipt = executor
            .original_receipt()
            .context("verified review has no original receipt")?;
        judgment_time(&rec, receipt).map_err(|why| anyhow!(why))?;
        let original = persist_original_verdict(
            wire,
            receipt.job_id.to_string().parse()?,
            receipt.step_id.to_string().parse()?,
            &rec,
        )
        .await?;
        let mut projected_run = run_packet.clone();
        projected_run["metadata"][REVIEW_KEY] = original.value.clone();
        original_verdict_matches(
            &original,
            &projected_run,
            receipt.step_id.to_string().parse()?,
            receipt,
        )
        .map_err(|why| anyhow!(why))?;
        rec = original.value;
    }
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
            format!(
                "\n  the release rule asks the release door on this verdict now (backlog \
                 0cfc57e6); if the door refuses, the car's review step says why (auto_release) \
                 and the operator releases it: boss release {branch} --review {run_id}"
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

/// `boss release <car> --review <run>`: ask the jobs API's release door
/// (`POST /api/jobs/{car}/release`, backlog ed8e5c85) to release the car
/// on that review's verdict at the head the forge holds now. The door
/// owns the rule (`boss_jobs::car_release::judge`) and the write, and
/// it reads the release back before it answers. This verb supplies what
/// only a clone can read, the branch head and main, plus the shell it
/// runs in ([`Releaser`]), whose builder run the door refuses before it
/// writes anything.
pub(crate) async fn release(
    wire: &Wire,
    car: &str,
    review: &str,
    releaser: &Releaser,
    forge: impl Fn(&str) -> Result<String>,
) -> Result<()> {
    let packet = wire.car(car).await?;
    let jid = str_at(&packet, "/id").to_string();
    let branch = branch_of(&packet)?.to_string();
    let head = forge(&format!("refs/heads/{branch}"))?;
    let main = forge("refs/heads/main")?;
    let mut ask = json!({
        "review": review.trim(),
        "head": head,
        "main_sha": main,
        "agent_shell": releaser.agent_shell,
    });
    if let Some(run) = releaser.run() {
        ask["run"] = json!(run);
    }
    let answer = wire
        .call(
            reqwest::Method::POST,
            &format!("/api/jobs/{jid}/release"),
            Some(ask),
        )
        // The door's refusal is the error as it came: its status line and
        // its sentence, which name why and the road around it.
        .await?
        .context("boss release: the release door answered with no body")?;
    let model_says = answer
        .pointer(&format!(
            "/release/{}/says",
            boss_jobs::car_release::REVIEWER_MODEL_KEY
        ))
        .and_then(Value::as_str)
        .unwrap_or_default();
    println!(
        "boss release: {} {branch} — released at {} on review run {} (main {})\n  reviewer \
         model: {model_says} (recorded on the release, not refused)\n  it boards at the next \
         tick while its head stays {}",
        short(&jid),
        short(&head),
        short(review.trim()),
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
    fn an_original_verdict_cannot_acquire_a_later_receipt_or_changed_projection() {
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        use boss_jobs::first_record::FirstRecord;
        let building = uuid::Uuid::from_u128(44);
        let stamp = |at: &str| {
            let mut s = EventStamp::new("test", ActorId::automation("launch"));
            s.timestamp = at.parse().unwrap();
            s
        };
        let receipt = FirstRecord::new(
            JobId::from_uuid(REVIEWER.parse().unwrap()),
            StepId::from_uuid(building),
            "executor.binding",
            &json!({"actor":"automation:launch"}),
            &stamp("2026-09-28T19:59:59Z"),
            uuid::Uuid::from_u128(45),
        );
        let binding = json!({"digest":receipt.digest,"event_id":receipt.event_id});
        let mut value = release_at(H1);
        value["executor_binding"] = binding;
        let native = run(REVIEWER, value.clone());
        let original = FirstRecord::new(
            JobId::from_uuid(REVIEWER.parse().unwrap()),
            StepId::from_uuid(building),
            "review.verdict",
            &value,
            &stamp("2026-09-28T20:00:00Z"),
            uuid::Uuid::from_u128(46),
        );
        assert!(original_verdict_matches(&original, &native, building, &receipt).is_ok());
        let mut late = receipt.clone();
        late.recorded_at = "2026-09-28T20:00:01Z".parse().unwrap();
        assert!(
            original_verdict_matches(&original, &native, building, &late).is_err(),
            "later evidence cannot retrofit an old verdict"
        );
        let mut later_original = original.clone();
        later_original.recorded_at = "2026-09-28T20:01:00Z".parse().unwrap();
        assert!(
            original_verdict_matches(&later_original, &native, building, &late).is_err(),
            "a late immutable write cannot retrofit a receipt after the original judgment time"
        );
        let mut changed = native.clone();
        changed["metadata"][REVIEW_KEY]["findings"] = json!("replacement");
        assert!(
            original_verdict_matches(&original, &changed, building, &receipt).is_err(),
            "mutable review replacement must not inherit an original judgment"
        );
        let mut damaged = original.clone();
        damaged.digest = "0".repeat(64);
        assert!(original_verdict_matches(&damaged, &native, building, &receipt).is_err());
    }

    fn fixture_stamp_at(
        actor: boss_core::actor::ActorId,
        at: &str,
    ) -> boss_core::publisher::EventStamp {
        let mut stamp = boss_core::publisher::EventStamp::new("fixture", actor);
        stamp.timestamp = at.parse().unwrap();
        stamp
    }

    #[test]
    fn the_issuer_actor_is_not_the_executor_who_records_the_verdict() {
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
        };
        use boss_jobs::first_record::FirstRecord;
        let job = JobId::from_uuid(REVIEWER.parse().unwrap());
        let step = StepId::from_uuid(uuid::Uuid::from_u128(44));
        let issuer = fixture_stamp_at(
            ActorId::automation("trusted-launch"),
            "2026-09-28T19:59:59Z",
        );
        let receipt = FirstRecord::new(
            job,
            step,
            "executor.binding",
            &json!({"actor":"agent-codex"}),
            &issuer,
            uuid::Uuid::from_u128(45),
        );
        let judgment = fixture_stamp_at(
            ActorId::RegisteredAgent("agent-codex".into()),
            "2026-09-28T20:00:00Z",
        );
        let mut value = release_at(H1);
        value["executor_binding"] = json!({"digest":receipt.digest,"event_id":receipt.event_id});
        let original = FirstRecord::new(
            job,
            step,
            ORIGINAL_VERDICT_KEY,
            &value,
            &judgment,
            uuid::Uuid::from_u128(46),
        );
        let native = run(REVIEWER, value);
        assert!(
            original_verdict_matches(&original, &native, uuid::Uuid::from_u128(44), &receipt)
                .is_ok()
        );
        let mut other = original;
        other.actor = ActorId::RegisteredAgent("agent-other".into());
        assert!(
            original_verdict_matches(&other, &native, uuid::Uuid::from_u128(44), &receipt).is_err()
        );
    }

    #[test]
    fn an_immutable_verdict_retry_keeps_first_time_and_refuses_replacement() {
        let original = release_at(H1);
        let mut retry = original.clone();
        retry["at"] = json!("2026-09-28T20:01:00Z");
        assert_eq!(
            select_original_verdict(Some(&original), &retry).unwrap(),
            original
        );
        retry["findings"] = json!("changed judgment");
        assert!(select_original_verdict(Some(&original), &retry).is_err());
        retry = original.clone();
        retry["executor_binding"] = json!({"digest":"new receipt"});
        assert!(select_original_verdict(Some(&original), &retry).is_err());
        assert_eq!(select_original_verdict(None, &original).unwrap(), original);
    }

    #[tokio::test]
    async fn immutable_verdict_write_refuses_an_internally_damaged_receipt() {
        use axum::{Router, extract::Json};
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        use boss_jobs::first_record::{FirstRecord, FirstRecordResult};
        let native = REVIEWER.parse().unwrap();
        let building = uuid::Uuid::from_u128(44);
        let value = release_at(H1);
        let mut record = FirstRecord::new(
            JobId::from_uuid(native),
            StepId::from_uuid(building),
            ORIGINAL_VERDICT_KEY,
            &value,
            &EventStamp::new("jobs", ActorId::automation("launch")),
            uuid::Uuid::from_u128(48),
        );
        record.value["findings"] = json!("damaged projection");
        let app = Router::new().fallback(move || {
            let record = record.clone();
            async move { Json(FirstRecordResult::Recorded(record)) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let wire = Wire::at(
            base,
            crate::identity::resolve_from(Some("agent-codex".into()), None),
        )
        .unwrap();
        assert!(
            persist_original_verdict(&wire, native, building, &value)
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn immutable_verdict_write_keeps_the_server_record_and_refuses_a_changed_response() {
        use axum::{Router, extract::Json};
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        use boss_jobs::first_record::{FirstRecord, FirstRecordResult};
        let native = REVIEWER.parse().unwrap();
        let building = uuid::Uuid::from_u128(44);
        let value = release_at(H1);
        let mut stamp = EventStamp::new("jobs", ActorId::automation("launch"));
        stamp.timestamp = "2026-09-28T20:00:01Z".parse().unwrap();
        let original = FirstRecord::new(
            JobId::from_uuid(native),
            StepId::from_uuid(building),
            ORIGINAL_VERDICT_KEY,
            &value,
            &stamp,
            uuid::Uuid::from_u128(47),
        );
        let served = original.clone();
        let app = Router::new().fallback(move |Json(body): Json<Value>| {
            let mut record = served.clone();
            async move {
                assert_eq!(body["key"], ORIGINAL_VERDICT_KEY);
                assert_eq!(body["expected_absence"], true);
                if body["value"]["findings"] != record.value["findings"] {
                    record.value["findings"] = json!("server replacement");
                }
                Json(FirstRecordResult::Replayed(record))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let wire = Wire::at(
            base,
            crate::identity::resolve_from(Some("agent-codex".into()), None),
        )
        .unwrap();
        assert_eq!(
            persist_original_verdict(&wire, native, building, &value)
                .await
                .unwrap(),
            original
        );
        let mut changed = value.clone();
        changed["findings"] = json!("changed judgment");
        assert!(
            persist_original_verdict(&wire, native, building, &changed)
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_formal_verdict_reads_original_records_and_never_mutable_receipt_metadata() {
        use axum::{Router, body::Body, extract::Request, http::StatusCode, response::Response};
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = calls.clone();
        let mut packet = car();
        packet["status"] = json!("open");
        packet["workflow_version"] = json!(7);
        packet["steps"] = json!([{"id":"00000000-0000-0000-0000-000000000003", "spec_slug":"review","metadata":{"agent_executor_provenance":"verified"}}]);
        let native = json!({"id":REVIEWER,"kind":"agent-run","metadata":{"executor.binding":{"assurance":"verified"}},"steps":[{"id":"00000000-0000-0000-0000-000000000004","spec_slug":"building","status":"active"}]});
        let app = Router::new().fallback(move |request: Request| {
            let calls = observed.clone(); let packet = packet.clone(); let native = native.clone();
            async move {
                let path = request.uri().path().to_string();
                calls.lock().unwrap().push(format!("{} {path}", request.method()));
                let answer = if request.method() != reqwest::Method::GET { None }
                    else if path == "/api/jobs" { Some(json!({"data":[packet],"total":1})) }
                    else if path == format!("/api/jobs/{CAR}") { Some(packet) }
                    else if path == format!("/api/jobs/{REVIEWER}") { Some(native) }
                    else if path == "/api/workflows/ship-a-change/versions/7" { Some(json!({"kind":"ship-a-change","version":7,"status":"active","steps":[{"title":"review","agent":{"profile":"reviewer","model":"gpt-6.1-sol","budget_usd":1,"effort":"low","executor_provenance":"verified"}}]})) }
                    else { None };
                match answer {
                    Some(value) => Response::builder().status(StatusCode::OK).header("content-type","application/json").body(Body::from(value.to_string())).unwrap(),
                    None => Response::builder().status(StatusCode::NOT_FOUND).body(Body::from("no original record")).unwrap(),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let wire = Wire::at(
            base,
            crate::identity::resolve_from(Some("agent-codex".into()), None),
        )
        .unwrap();
        assert!(
            record(&wire, CAR, RELEASE, "review", Some(REVIEWER), |_| Ok(
                H1.into()
            ))
            .await
            .is_err()
        );
        let calls = calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|path| path.ends_with("/records/executor.source_claim")),
            "FORMAL must read the original immutable record: {calls:?}"
        );
        assert!(
            calls.iter().all(|path| path.starts_with("GET ")),
            "missing authority refuses all writes"
        );
        server.abort();
    }

    #[test]
    fn verified_formal_review_requires_authenticated_execution_not_matching_model_metadata() {
        let mut car = car();
        car["steps"] =
            json!([{"spec_slug":"review","metadata":{"agent_executor_provenance":"verified"}}]);
        let mut run = run(REVIEWER, release_at(H1));
        run["metadata"]["model"] = json!("gpt-6.1-sol");
        run["metadata"]["observed_model"] = json!("gpt-6.1-sol");
        assert!(
            vouches(&run, &car, H1).is_err(),
            "matching caller metadata is no authenticated launch receipt"
        );
    }

    #[test]
    fn a_bare_release_cannot_bypass_verified_executor_provenance() {
        let mut car = car();
        car["steps"] =
            json!([{"spec_slug":"review","metadata":{"agent_executor_provenance":"verified"}}]);
        assert!(
            may_release(&car, None).is_err(),
            "a human shell cannot erase the pinned executor requirement"
        );
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

    /// WHICH MODEL REVIEWED IT, said at the release (backlog ad82090a),
    /// and since backlog e6b05dd6 (D2 of design fb9ba887) decided ONLY by
    /// the launcher's first record. The incident's shape — a run whose
    /// own report observed a model other than the one it declared, and
    /// no launcher record — no longer reads DIFFER off the run's word
    /// about itself: unknown, with the observation beside it as the
    /// cross-check. A launcher's record decides; nothing here refuses.
    #[test]
    fn a_release_says_which_model_the_review_declared_and_which_ran() {
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        let mut differs = run(REVIEWER, release_at(H1));
        differs["metadata"]["model"] = json!("opus-5[1m]");
        differs["metadata"]["model_observed"] = json!({"declared": "opus-5[1m]",
            "observed": "opus-5-5", "says": "declared opus-5[1m], observed opus-5-5 — they DIFFER"});
        let said = reviewer_model(&differs, &Ok(None));
        assert_eq!(said["declared"], json!("opus-5[1m]"));
        assert_eq!(said["observed"], json!("opus-5-5"));
        assert_eq!(said["launched"], Value::Null);
        assert_eq!(
            said["matches"],
            Value::Null,
            "the run's own word decides nothing"
        );

        // The launcher's record of a Codex launch decides it.
        let launched = |model: &str| {
            boss_jobs::first_record::FirstRecord::new(
                JobId::from_uuid(uuid::Uuid::from_u128(1)),
                StepId::from_uuid(uuid::Uuid::from_u128(2)),
                boss_jobs::car_release::LAUNCHED_MODEL_KEY,
                &json!({"model": model, "harness": "codex-exec", "via": "argv"}),
                &EventStamp::new(
                    "jobs.record",
                    ActorId::RegisteredAgent("agent-claude".into()),
                ),
                uuid::Uuid::from_u128(3),
            )
        };
        let mut same = run(REVIEWER, release_at(H1));
        same["metadata"]["model"] = json!("gpt-6.1-sol");
        same["metadata"]["model_observed"] =
            json!({"declared": "gpt-6.1-sol", "observed": "gpt-6.1-sol"});
        let said = reviewer_model(&same, &Ok(Some(launched("gpt-6.1-sol"))));
        assert_eq!(said["matches"], json!(true));
        assert_eq!(said["cross_check"], json!(true));
        assert_eq!(
            said["says"],
            json!("declared gpt-6.1-sol, launched gpt-6.1-sol; observed gpt-6.1-sol")
        );
        let said = reviewer_model(&same, &Ok(Some(launched("opus-5-5"))));
        assert_eq!(said["matches"], json!(false));

        // An unread record is unknown with its reason, never a match.
        let said = reviewer_model(&same, &Err("the jobs API was dark".into()));
        assert_eq!(said["matches"], Value::Null);
        assert!(
            said["says"]
                .as_str()
                .unwrap()
                .starts_with("declared gpt-6.1-sol, launched unknown (the jobs API was dark)"),
            "{said}"
        );

        // A run that declares nothing (the human road files one bare).
        let said = reviewer_model(&run(REVIEWER, release_at(H1)), &Ok(None));
        assert_eq!(said["declared"], Value::Null);
        assert_eq!(said["matches"], Value::Null);
    }
}
