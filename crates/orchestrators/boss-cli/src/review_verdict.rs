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
#[cfg(test)]
pub(crate) fn vouches(run: &Value, car: &Value, head: &str) -> Result<(), String> {
    executor_review_eligibility(car)?;
    vouches_inner(run, car, head)
}

fn vouches_inner(run: &Value, car: &Value, head: &str) -> Result<(), String> {
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

#[async_trait::async_trait]
pub(crate) trait ExecutorRecordReader: Sync {
    async fn executor_read(&self, path: &str) -> Result<Option<Value>>;
    async fn executor_original(
        &self,
        job: uuid::Uuid,
        step: uuid::Uuid,
        key: &str,
    ) -> Result<Option<boss_jobs::first_record::FirstRecord>>;
}

#[async_trait::async_trait]
impl ExecutorRecordReader for Wire {
    async fn executor_read(&self, path: &str) -> Result<Option<Value>> {
        self.call(reqwest::Method::GET, path, None).await
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

pub(crate) fn immutable_record_resource(
    record: &boss_jobs::first_record::FirstRecord,
    job: uuid::Uuid,
    step: uuid::Uuid,
    key: &str,
) -> Result<()> {
    if record.job_id.inner().as_uuid() != &job
        || record.step_id.inner().as_uuid() != &step
        || record.key != key
    {
        bail!("the immutable record response belongs to another resource");
    }
    Ok(())
}

/// Resolve the admitted protocol, never the mutable convenience projection.
pub(crate) async fn pinned_executor_requirement(
    reader: &impl ExecutorRecordReader,
    car: &Value,
) -> Result<boss_jobs::executor_attestation::ExecutorProvenanceRequirement> {
    use boss_jobs::executor_attestation::ExecutorProvenanceRequirement;
    let kind = car
        .get("kind")
        .and_then(Value::as_str)
        .context("the source packet has no protocol kind")?;
    let version = car
        .get("workflow_version")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .context("the source packet has no pinned protocol version")?;
    let row = reader
        .executor_read(&format!("/api/workflows/{kind}/versions/{version}"))
        .await?
        .context("the pinned source protocol is unavailable")?;
    if row["kind"] != kind
        || row["version"] != version
        || !matches!(row["status"].as_str(), Some("active" | "retired"))
    {
        bail!("the source protocol read does not match its immutable pin");
    }
    let step = row
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step["title"] == boss_jobs::car::REVIEW_SLUG)
        })
        .context("the pinned protocol has no review step")?;
    match step.get("agent") {
        None | Some(Value::Null) => Ok(ExecutorProvenanceRequirement::Advisory),
        Some(value) => {
            let agent: boss_jobs::agent_spec::AgentSpec = serde_json::from_value(value.clone())
                .context("the pinned executor declaration is unreadable")?;
            Ok(agent.executor_provenance)
        }
    }
}

/// Authority read during this operation. Neither callers nor metadata can
/// construct it. A verified result retains the original receipt's identity.
#[derive(Debug)]
pub(crate) struct ExecutorReviewEligibility {
    car: String,
    head: String,
    verified: Option<boss_jobs::executor_attestation::VerifiedExecutor>,
    original_verdict: Option<boss_jobs::first_record::FirstRecord>,
    original_receipt: Option<boss_jobs::first_record::FirstRecord>,
}

const ORIGINAL_VERDICT_KEY: &str = "review.verdict";

fn judgment_time(
    value: &Value,
    receipt: &boss_jobs::first_record::FirstRecord,
) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let at = value
        .get("at")
        .and_then(Value::as_str)
        .and_then(|at| at.parse::<chrono::DateTime<chrono::Utc>>().ok())
        .ok_or("the original verdict judgment time is unreadable")?;
    if receipt.recorded_at > at {
        return Err(
            "an executor receipt recorded after judgment cannot be bound to that verdict".into(),
        );
    }
    Ok(at)
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

fn original_verdict_matches(
    original: &boss_jobs::first_record::FirstRecord,
    run: &Value,
    building: uuid::Uuid,
    receipt: &boss_jobs::first_record::FirstRecord,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let scalar: Value = serde_json::from_str(&original.value_json)
        .map_err(|_| "the original verdict scalar is unreadable")?;
    let canonical = boss_core::job::canonical_json_bytes(&scalar);
    let judged_at = judgment_time(&scalar, receipt)?;
    if original.job_id.to_string() != str_at(run, "/id")
        || original.step_id.to_string() != building.to_string()
        || original.key != ORIGINAL_VERDICT_KEY
        || scalar != original.value
        || canonical != original.canonical
        || hex::encode(Sha256::digest(&canonical)) != original.digest
        || run.pointer(&format!("/metadata/{REVIEW_KEY}")) != Some(&original.value)
        || receipt.job_id != original.job_id
        || receipt.step_id != original.step_id
        || receipt.key != boss_jobs::executor_attestation::EXECUTOR_BINDING_KEY
        || receipt.value.get("actor").and_then(Value::as_str)
            != Some(original.actor.to_string().as_str())
        || receipt.recorded_at > original.recorded_at
        || judged_at > original.recorded_at
        || original.value.get("executor_binding")
            != Some(&json!({"digest":receipt.digest,"event_id":receipt.event_id}))
    {
        return Err(
            "the verdict does not conserve its original receipt, actor, bytes and order".into(),
        );
    }
    Ok(())
}

impl ExecutorReviewEligibility {
    pub(crate) fn record_binding(&self) -> Option<Value> {
        self.verified
            .as_ref()
            .map(|executor| json!({"digest":executor.digest(),"event_id":executor.event_id()}))
    }
}

pub(crate) fn vouches_with_executor(
    run: &Value,
    car: &Value,
    head: &str,
    eligibility: &ExecutorReviewEligibility,
) -> Result<(), String> {
    if eligibility.car != str_at(car, "/id") || eligibility.head != head {
        return Err("executor eligibility belongs to another source claim/head".into());
    }
    let binding = eligibility.record_binding();
    if run.pointer(&format!("/metadata/{REVIEW_KEY}/executor_binding")) != binding.as_ref() {
        return Err("the verdict does not retain the original verified executor receipt".into());
    }
    if eligibility.verified.is_some() {
        let original = eligibility
            .original_verdict
            .as_ref()
            .ok_or("verified review has no original immutable verdict")?;
        let receipt = eligibility
            .original_receipt
            .as_ref()
            .ok_or("verified review has no original immutable receipt")?;
        let building = crate::envelope::steps(run)
            .into_iter()
            .find(|step| step["spec_slug"] == crate::dispatch::BUILDING_SLUG)
            .and_then(|step| step["id"].as_str())
            .and_then(|id| id.parse().ok())
            .ok_or("verified review has no valid Building identity")?;
        original_verdict_matches(original, run, building, receipt)?;
    }
    vouches_inner(run, car, head)
}

/// Original immutable records are read from storage. The current production
/// verifier refuses because no authenticated host/tool issuer is installed.
pub(crate) async fn read_executor_eligibility(
    reader: &impl ExecutorRecordReader,
    run: &Value,
    car: &Value,
    head: &str,
) -> Result<ExecutorReviewEligibility> {
    use boss_jobs::executor_attestation::{
        EXECUTOR_BINDING_KEY, ExecutorExpectation, ExecutorProvenanceRequirement, SOURCE_CLAIM_KEY,
        UnavailableLaunchReceiptVerifier, executor_eligibility,
    };
    let requirement = pinned_executor_requirement(reader, car).await?;
    let advisory = || ExecutorReviewEligibility {
        car: str_at(car, "/id").into(),
        head: head.into(),
        verified: None,
        original_verdict: None,
        original_receipt: None,
    };
    let native = str_at(run, "/id");
    let building = crate::envelope::steps(run)
        .into_iter()
        .find(|step| step["spec_slug"] == crate::dispatch::BUILDING_SLUG);
    let Some(building) = building else {
        if requirement == ExecutorProvenanceRequirement::Advisory {
            return Ok(advisory());
        }
        bail!("the native run has no Building claim");
    };
    let building_id = building
        .get("id")
        .and_then(Value::as_str)
        .context("the Building claim has no id")?;
    let native_id: uuid::Uuid = native.parse().context("the native run id is invalid")?;
    let building_uuid: uuid::Uuid = building_id
        .parse()
        .context("the Building claim id is invalid")?;
    let source = reader
        .executor_original(native_id, building_uuid, SOURCE_CLAIM_KEY)
        .await?;
    let Some(source) = source else {
        if requirement == ExecutorProvenanceRequirement::Advisory {
            return Ok(advisory());
        }
        bail!("verified executor requires the original immutable source claim");
    };
    let claim = boss_jobs::executor_attestation::read_source_claim(&source)?;
    let source_step = crate::envelope::steps(car)
        .into_iter()
        .find(|step| step["spec_slug"] == boss_jobs::car::REVIEW_SLUG)
        .context("the source packet has no review step")?;
    let reference = format!("refs/heads/{}", branch_of(car)?);
    if claim.native_run.to_string() != native
        || claim.source_job.to_string() != str_at(car, "/id")
        || claim.source_step.to_string() != str_at(source_step, "/id")
        || claim.source_slug != boss_jobs::car::REVIEW_SLUG
        || Some(i64::from(claim.protocol_version)) != car["workflow_version"].as_i64()
        || claim.executor_provenance != requirement
        || claim
            .source_ref
            .as_deref()
            .is_some_and(|value| value != reference)
        || claim
            .source_head
            .as_deref()
            .is_some_and(|value| value != head)
    {
        bail!(
            "the original source claim cannot be downgraded or reused after its protocol/head changes"
        );
    }
    if requirement == ExecutorProvenanceRequirement::Advisory {
        return Ok(advisory());
    }
    let record = reader
        .executor_original(native_id, building_uuid, EXECUTOR_BINDING_KEY)
        .await?
        .context("verified executor requires an immutable launch receipt")?;
    let mut expectation: ExecutorExpectation = serde_json::from_value(record.value.clone())
        .context("the executor binding is unreadable")?;
    if expectation.native_run.to_string() != native
        || expectation.source_job.to_string() != str_at(car, "/id")
        || expectation.source_step.to_string() != str_at(source_step, "/id")
        || expectation.source_slug != boss_jobs::car::REVIEW_SLUG
        || Some(i64::from(expectation.protocol_version)) != car["workflow_version"].as_i64()
        || expectation.source_ref.as_deref() != Some(reference.as_str())
        || expectation.source_head.as_deref() != Some(head)
        || record.step_id.to_string() != building_id
        || building
            .get("assignee_id")
            .and_then(Value::as_str)
            .is_some_and(|actor| actor != expectation.actor)
        || source_step
            .get("assignee_id")
            .and_then(Value::as_str)
            .is_some_and(|actor| actor != expectation.actor)
    {
        bail!("the immutable executor binding does not match the exact current source claim/head");
    }
    expectation.source_claim = Some(source);
    let verified = executor_eligibility(
        requirement,
        Some(&record),
        Some(&expectation),
        &UnavailableLaunchReceiptVerifier,
    )?;
    let original_verdict = reader
        .executor_original(native_id, building_uuid, ORIGINAL_VERDICT_KEY)
        .await?;
    Ok(ExecutorReviewEligibility {
        car: str_at(car, "/id").into(),
        head: head.into(),
        verified,
        original_verdict,
        original_receipt: Some(record),
    })
}

pub(crate) fn executor_review_eligibility(car: &Value) -> Result<(), String> {
    use boss_jobs::executor_attestation::{
        ExecutorProvenanceRequirement, UnavailableLaunchReceiptVerifier, executor_eligibility,
    };
    let declaration = crate::envelope::steps(car)
        .into_iter()
        .find(|step| step["spec_slug"] == boss_jobs::car::REVIEW_SLUG)
        .and_then(|step| step["metadata"].get(boss_jobs::agent_spec::EXECUTOR_PROVENANCE_KEY));
    let requirement = match declaration {
        None => ExecutorProvenanceRequirement::Advisory,
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| "the pinned executor provenance declaration is unreadable".to_string())?,
    };
    // There is no authenticated host/tool receipt adapter in this deployment.
    // An actor header, model metadata or self-attestation must not stand in
    // for that authority. The same domain predicate keeps every door closed.
    executor_eligibility(requirement, None, None, &UnavailableLaunchReceiptVerifier)
        .map(|_| ())
        .map_err(|why| format!("the pinned FORMAL executor requirement is unmet: {why}; no authenticated host/tool receipt adapter is available"))
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
    executor_review_eligibility(car)?;
    may_release_identity(car, releaser)
}

fn may_release_identity(car: &Value, releaser: Option<&str>) -> Result<(), String> {
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
    let executor = read_executor_eligibility(wire, &run_packet, &packet, &head).await.context("boss review: FORMAL executor eligibility refused; advisory findings remain reportable on the run")?;
    let at = crate::gate::stamp(boss_clock_client::wall_now());
    let mut rec = review_record(&packet, &head, verdict, findings.trim(), &at);
    if let Some(binding) = executor.record_binding() {
        rec["executor_binding"] = binding;
    }
    if executor.verified.is_some() {
        rec = select_original_verdict(
            executor
                .original_verdict
                .as_ref()
                .map(|record| &record.value),
            &rec,
        )
        .map_err(|why| anyhow!(why))?;
        let receipt = executor
            .original_receipt
            .as_ref()
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
    may_release_identity(&packet, releaser.run()).map_err(|why| anyhow!("{why}"))?;
    let branch = branch_of(&packet)?;
    let head = forge(&format!("refs/heads/{branch}"))?;
    let main = forge("refs/heads/main")?;
    let run = wire.packet(review).await?;
    let executor = read_executor_eligibility(wire, &run, &packet, &head)
        .await
        .context("boss release: pinned executor eligibility refused")?;
    vouches_with_executor(&run, &packet, &head, &executor)
        .map_err(|why| anyhow!("boss release: REFUSED — {why}"))?;
    let by = wire
        .caller_id()
        .context("an unnamed release is refused before it is sent")?;
    let at = crate::gate::stamp(boss_clock_client::wall_now());
    // Which run and which shell released it, beside `by`: every agent
    // signs as agent-claude, so the actor alone cannot say (car 3,
    // review 5aa91689 B1).
    let mut rec = release_record(&head, review, by, &at, &main);
    if let Some(binding) = executor.record_binding() {
        rec["executor_binding"] = binding;
    }
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

    #[test]
    fn an_advisory_result_cannot_supply_or_retarget_a_verified_receipt_reference() {
        let eligibility = ExecutorReviewEligibility {
            car: CAR.into(),
            head: H1.into(),
            verified: None,
            original_verdict: None,
            original_receipt: None,
        };
        let mut reviewer = run(REVIEWER, release_at(H1));
        assert!(vouches_with_executor(&reviewer, &car(), H1, &eligibility).is_ok());
        assert!(vouches_with_executor(&reviewer, &car(), H2, &eligibility).is_err());
        reviewer["metadata"][REVIEW_KEY]["executor_binding"] =
            json!({"digest":"self-declared","event_id":REVIEWER});
        assert!(vouches_with_executor(&reviewer, &car(), H1, &eligibility).is_err());
        let mut another_car = car();
        another_car["id"] = json!("another-car");
        assert!(vouches_with_executor(&reviewer, &another_car, H1, &eligibility).is_err());
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
}
