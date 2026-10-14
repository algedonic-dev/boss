//! THE RELEASE RULE, IN ONE PLACE (backlog ed8e5c85, David 2026-10-09).
//!
//! WHY IT LIVES HERE. Until this module, the rule that frees a car held
//! for its adversarial review lived only in `boss-cli`
//! (`review_verdict.rs`). So the only way to release a car was a pasted
//! `boss release <branch> --review <run>`, one line per car. Measured
//! 2026-10-09T13:36Z: seven reviewed cars sat HELD, each waiting on a
//! line from a chat message. The yard had no way to act on them. A web
//! control could not call a CLI, and a second copy of the rule in the
//! page is the drift CLAUDE.md 9a forbids. The rule therefore moved into
//! core. `POST /api/jobs/{car}/release` (`http/release.rs`) is the one
//! door. `boss release --review` and the yard's Release control both
//! call it, and the yard's held-car rows show the same judgement
//! ([`decision`]) that the door would apply.
//!
//! WHAT THE RULE IS. It did not change in the move; design 7cedfa29 and
//! backlog b7b02024 explain it. A RELEASE IS A REVIEW VERDICT BOUND TO
//! THE HEAD IT JUDGED:
//!
//! - the named run is an agent-run, not the car's builder, and (when it
//!   was dispatched) was opened on THIS car's `review` step;
//! - it records a review of this car, the verdict is RELEASE, and the
//!   head it read is the head the release is given at ([`vouches`]);
//! - the shell releasing is not the car's builder run
//!   ([`may_release_identity`]);
//! - the pinned executor requirement is met ([`read_executor_eligibility`]);
//! - the review step records no release by this same review at that
//!   head already: a hold placed after a release is not that review's to
//!   lift, while a fresh review is (review 4822764d B1 and re-review
//!   6c1788dc B2, the one rule added in the move, [`released_already`]).
//!   A hold written after a release records no head at all (backlog
//!   6c463c66, [`crate::car::hold_body`]), and a request naming none is
//!   then refused with the bare road ([`unbound`]).
//!
//! The record written on the review step is `{sha, review, verdict, by,
//! at, main_sha, reviewer_model, agent_shell, run?}` ([`release_record`],
//! [`reviewer_model`], which counts only the launcher's record of the
//! model it started, [`launch_record`]).
//!
//! WHICH HEAD. The CLI reads the car's head from the forge and sends it
//! (`head`). The jobs API cannot read the forge, so a caller that sends
//! no head (the yard) is judged at the head the car's hold was placed
//! at, `hold_sha` ([`crate::car::HOLD_SHA`]). That loses nothing:
//! the conductor re-reads the LIVE forge head at boarding and holds again
//! any car whose release names another head (`mutating_verb::dock` in
//! boss-cli). A release at a stale head is therefore no release. The
//! same reasoning covers `main_sha`: it is a record only (design
//! 6af5acdf made the train gate the backstop). The yard records it as
//! null, and the CLI still sends the main it read.
//!
//! WHO. The release's `by` is the request's authenticated actor, which
//! for the yard is the passkey session. The handler stamps it and the
//! body cannot supply it. Presence: the stamp is the approval (design
//! 1ce67f7e), so there is no second prompt.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The agent-run packet key a review's verdict lives under.
pub const REVIEW_KEY: &str = "review";

/// The verdict that releases a car.
pub const RELEASE: &str = "release";

/// Every verdict `boss review` records.
pub const VERDICTS: [&str; 2] = [RELEASE, "changes"];

/// The shell variable naming the run a shell is. It has one spelling,
/// which the CLI reads through `gate::AGENT_RUN_ENV`.
pub const AGENT_RUN_ENV: &str = "BOSS_AGENT_RUN";

/// The agent-run key `boss dispatch --report` writes the model its
/// transcript observed under (backlog 2f7b8c00).
pub const MODEL_OBSERVED_KEY: &str = "model_observed";

/// The first record a LAUNCHER writes on a run's Building step, naming
/// the model it handed the harness (backlog e6b05dd6, D2 of design
/// fb9ba887): `{model, harness, via, ...}`, written through the
/// first-record door before the run can act, so the run never writes
/// it. The one key both sides use: `boss dispatch --from-hook` and
/// `--launch` write it, [`reviewer_model`] reads it.
pub const LAUNCHED_MODEL_KEY: &str = "launch.model";

/// An agent-run's own work step, where its immutable receipts live.
pub const BUILDING_SLUG: &str = "building";

/// The release record's key for [`reviewer_model`].
pub const REVIEWER_MODEL_KEY: &str = "reviewer_model";

/// The first-record key an immutable verdict is written under.
pub const ORIGINAL_VERDICT_KEY: &str = "review.verdict";

/// The reviewer's own one-line answer to "may this car go without
/// David?". Reviewer briefs ask for a line that starts with it, and the
/// yard shows that line beside the Release control. It is shown and
/// never judged: the verdict is the record the door checks.
pub const SAFE_LINE_PREFIX: &str = "SAFE TO RELEASE WITHOUT DAVID";

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// A packet's steps, as the jobs API serves them.
fn steps(row: &Value) -> Vec<&Value> {
    row.get("steps")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// The full 36-character id shape (8-4-4-4-12): the record carries the
/// id that was checked, never whatever was typed.
pub fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The run that built this car: its `agent_run`, or `None` for a car
/// gated by hand.
pub fn builder_run(car: &Value) -> Option<&str> {
    Some(str_at(car, "/metadata/agent_run")).filter(|s| !s.is_empty())
}

/// The car's review step: the step the hold and the release live on.
pub fn review_step(car: &Value) -> Option<&Value> {
    crate::car::find_step(car, crate::car::REVIEW_SLUG, crate::car::REVIEW)
}

/// The head a car's review hold was placed at, when the hold recorded one.
pub fn hold_sha(car: &Value) -> Option<&str> {
    review_step(car)
        .map(|s| str_at(s, &format!("/metadata/{}", crate::car::HOLD_SHA)))
        .filter(|s| !s.is_empty())
}

/// The run the car's review step was dispatched to. A held car's
/// reviewer is named there by `boss dispatch`.
pub fn dispatched_reviewer(car: &Value) -> Option<&str> {
    review_step(car)
        .map(|s| str_at(s, "/metadata/agent_run"))
        .filter(|s| !s.is_empty())
}

/// PURE: may a release free this car's review step at all? Only while
/// the step is open, which means the car stands at the dock. This is the
/// CLI's `steps::holdable` rule, asked by the door.
pub fn holdable(car: &Value) -> Result<&Value, String> {
    let id = short(str_at(car, "/id"));
    let review = review_step(car).ok_or_else(|| {
        format!(
            "car {id} ({}) has no review step — a hold brakes a car at the dock, and this \
             packet is not one",
            str_at(car, "/kind")
        )
    })?;
    match str_at(review, "/status") {
        "ready" | "active" => Ok(review),
        other => Err(format!(
            "car {id}'s review is {other} — a hold brakes a car standing at the dock (review \
             open)"
        )),
    }
}

/// PURE: does `run`'s recorded review vouch for `car` at `head`? `Ok`
/// only when every fact holds: the run is an agent-run that is not the
/// car's builder, it records a review OF THIS CAR, the verdict is
/// RELEASE, and it read exactly `head`. Each refusal names the fact.
pub fn vouches(run: &Value, car: &Value, head: &str) -> Result<(), String> {
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
        if step != crate::car::REVIEW_SLUG {
            return Err(format!(
                "run {} was opened on step `{step}` of car {}, not on its `{}` step",
                short(run_id),
                short(car_id),
                crate::car::REVIEW_SLUG
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

/// What the release rule reads besides the two packets: the pinned
/// protocol row and the immutable first records. The jobs API answers
/// from its own stores (`http/release.rs`). The CLI and the conductor
/// answer over HTTP.
#[async_trait::async_trait]
pub trait ExecutorRecordReader: Sync {
    /// The protocol row `kind` v`version`, as `GET
    /// /api/workflows/{kind}/versions/{version}` serves it.
    async fn protocol_version(&self, kind: &str, version: i64) -> Result<Option<Value>>;
    async fn executor_original(
        &self,
        job: uuid::Uuid,
        step: uuid::Uuid,
        key: &str,
    ) -> Result<Option<crate::first_record::FirstRecord>>;
}

pub fn immutable_record_resource(
    record: &crate::first_record::FirstRecord,
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
pub async fn pinned_executor_requirement(
    reader: &impl ExecutorRecordReader,
    car: &Value,
) -> Result<crate::executor_attestation::ExecutorProvenanceRequirement> {
    use crate::executor_attestation::ExecutorProvenanceRequirement;
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
        .protocol_version(kind, version)
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
                .find(|step| step["title"] == crate::car::REVIEW_SLUG)
        })
        .context("the pinned protocol has no review step")?;
    match step.get("agent") {
        None | Some(Value::Null) => Ok(ExecutorProvenanceRequirement::Advisory),
        Some(value) => {
            let agent: crate::agent_spec::AgentSpec = serde_json::from_value(value.clone())
                .context("the pinned executor declaration is unreadable")?;
            Ok(agent.executor_provenance)
        }
    }
}

/// Authority read during this operation. Neither callers nor metadata can
/// construct it. A verified result retains the original receipt's identity.
#[derive(Debug)]
pub struct ExecutorReviewEligibility {
    car: String,
    head: String,
    verified: Option<crate::executor_attestation::VerifiedExecutor>,
    original_verdict: Option<crate::first_record::FirstRecord>,
    original_receipt: Option<crate::first_record::FirstRecord>,
}

pub fn judgment_time(
    value: &Value,
    receipt: &crate::first_record::FirstRecord,
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

pub fn original_verdict_matches(
    original: &crate::first_record::FirstRecord,
    run: &Value,
    building: uuid::Uuid,
    receipt: &crate::first_record::FirstRecord,
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
        || receipt.key != crate::executor_attestation::EXECUTOR_BINDING_KEY
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
    pub fn record_binding(&self) -> Option<Value> {
        self.verified
            .as_ref()
            .map(|executor| json!({"digest":executor.digest(),"event_id":executor.event_id()}))
    }

    /// Whether an authenticated launch receipt verified the executor.
    pub fn is_verified(&self) -> bool {
        self.verified.is_some()
    }

    /// The immutable verdict already on record, if any.
    pub fn original_verdict(&self) -> Option<&crate::first_record::FirstRecord> {
        self.original_verdict.as_ref()
    }

    /// The immutable launch receipt this eligibility was read from.
    pub fn original_receipt(&self) -> Option<&crate::first_record::FirstRecord> {
        self.original_receipt.as_ref()
    }
}

pub fn vouches_with_executor(
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
        let building = steps(run)
            .into_iter()
            .find(|step| step["spec_slug"] == BUILDING_SLUG)
            .and_then(|step| step["id"].as_str())
            .and_then(|id| id.parse().ok())
            .ok_or("verified review has no valid Building identity")?;
        original_verdict_matches(original, run, building, receipt)?;
    }
    vouches_inner(run, car, head)
}

/// A car's branch, or a refusal naming the car.
pub fn branch_of(car: &Value) -> Result<&str> {
    Some(str_at(car, "/metadata/branch"))
        .filter(|b| !b.is_empty())
        .ok_or_else(|| anyhow!("car {} names no branch", short(str_at(car, "/id"))))
}

/// Original immutable records are read from storage. The current production
/// verifier refuses because no authenticated host/tool issuer is installed.
pub async fn read_executor_eligibility(
    reader: &impl ExecutorRecordReader,
    run: &Value,
    car: &Value,
    head: &str,
) -> Result<ExecutorReviewEligibility> {
    use crate::executor_attestation::{
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
    let building = steps(run)
        .into_iter()
        .find(|step| step["spec_slug"] == BUILDING_SLUG);
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
    let claim = crate::executor_attestation::read_source_claim(&source)?;
    let source_step = review_step(car).context("the source packet has no review step")?;
    let reference = format!("refs/heads/{}", branch_of(car)?);
    if claim.native_run.to_string() != native
        || claim.source_job.to_string() != str_at(car, "/id")
        || claim.source_step.to_string() != str_at(source_step, "/id")
        || claim.source_slug != crate::car::REVIEW_SLUG
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
        || expectation.source_slug != crate::car::REVIEW_SLUG
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

pub fn executor_review_eligibility(car: &Value) -> Result<(), String> {
    use crate::executor_attestation::{
        ExecutorProvenanceRequirement, UnavailableLaunchReceiptVerifier, executor_eligibility,
    };
    let declaration = review_step(car)
        .and_then(|step| step["metadata"].get(crate::agent_spec::EXECUTOR_PROVENANCE_KEY));
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

/// PURE: may the run this shell is (`releaser`, its `BOSS_AGENT_RUN`)
/// release `car`? Refused when it is not a full run id, and when it is
/// the car's builder run (backlog b7b02024 car 3, design 7cedfa29 Q1,
/// design b08725c2 row A). Asked of both release forms. The bare form
/// asks it through [`may_release`], and the review form asks it at the
/// door.
///
/// THE BUILDER RUN is the car's `agent_run`. That is the run the gate
/// that filed the car stamped, re-stamped by every later green that
/// carries a run (the auto-park refresh). A green with NO run keeps the
/// one before, so after a hand repair the refusal still binds the run
/// that built the car (review 5aa91689 N1: fails open, locks no one out).
///
/// No run in the shell, another run, or a car with no builder run is
/// not refused. The refusal binds agent runs and never a person. The
/// yard's control is a person's (its request carries no run), so it is
/// never refused here.
pub fn may_release(car: &Value, releaser: Option<&str>) -> Result<(), String> {
    executor_review_eligibility(car)?;
    may_release_identity(car, releaser)
}

pub fn may_release_identity(car: &Value, releaser: Option<&str>) -> Result<(), String> {
    let env = AGENT_RUN_ENV;
    let id = str_at(car, "/id");
    let branch = str_at(car, "/metadata/branch");
    let target = if branch.is_empty() { short(id) } else { branch };
    let Some(run) = releaser.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(());
    };
    if !looks_like_uuid(run) {
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

/// The release record on the review step (`crate::car::RELEASE`).
pub fn release_record(sha: &str, review: &str, by: &str, at: &str, main_sha: &str) -> Value {
    json!({
        "sha": sha,
        "review": review,
        "verdict": RELEASE,
        "by": by,
        "at": at,
        "main_sha": main_sha,
    })
}

/// One model line: what was declared and what ran, and whether they
/// differ. Exact equality of the rate card's spellings, with no alias
/// table (backlog ad82090a).
pub fn model_line(declared: &str, observed: Option<&str>, why: Option<&str>) -> String {
    match observed {
        Some(o) if o == declared => format!("declared {declared}, observed {o}"),
        Some(o) => format!("declared {declared}, observed {o} — they DIFFER"),
        None => format!(
            "declared {declared}, observed unknown ({})",
            why.unwrap_or("the transcript was not read")
        ),
    }
}

/// What the launcher recorded on `run`: the immutable first record under
/// [`LAUNCHED_MODEL_KEY`] on its Building step, read from storage through
/// `reader` — the stored receipt, never the step's metadata, which is a
/// projection of it. `Ok(None)` is a run no launcher recorded a model on;
/// `Err` says why the record could not be read, or why a record there
/// cannot be a launch (its step does not declare the key as written
/// before the claim, [`crate::records_before_claim`]), and is never
/// absence.
pub async fn launch_record(
    reader: &impl ExecutorRecordReader,
    run: &Value,
) -> Result<Option<crate::first_record::FirstRecord>, String> {
    let building = steps(run)
        .into_iter()
        .find(|step| step["spec_slug"] == BUILDING_SLUG)
        .ok_or("the run has no building step, where a launcher records the model it started")?;
    // ONLY WHERE THE DOOR COULD REFUSE THE RUN'S OWN (review fd7f1030 F1).
    // A launcher and the run it started sign as the same agent; what tells
    // their writes apart is that the launcher's lands before the claim.
    // The first-record door holds that only on a step whose protocol
    // declares the key `records_before_claim`, so a record on any other
    // Building step — a run admitted under an older agent-run version —
    // may be the run describing itself after its claim, and is no launch.
    if !crate::records_before_claim::declares(
        building.get("metadata").unwrap_or(&Value::Null),
        LAUNCHED_MODEL_KEY,
    ) {
        return Err(format!(
            "this run's building step does not declare {LAUNCHED_MODEL_KEY} in {}, so nothing \
             refused a record written after its claim — it was admitted under an agent-run \
             version older than backlog e6b05dd6's",
            crate::records_before_claim::KEY
        ));
    }
    // ...AND ONLY WHERE A CLAIM CLOSED IT (third review 85c11887 N2). The
    // declaration says the door SHOULD refuse a post-claim record; the
    // mark says a claim ran that made it refuse. Without the mark the run
    // was never claimed, or was claimed by a jobs API older than the mark
    // (a declaring agent-run version published before that image), and
    // nothing refused a record the run wrote itself. Both orders of
    // deploy then read unknown, never a believed launch.
    if building["metadata"]
        .get(crate::records_before_claim::CLAIMED_KEY)
        .is_none()
    {
        return Err(format!(
            "this run's building step carries no {}: no claim closed its before-claim records, \
             so nothing refused a {LAUNCHED_MODEL_KEY} written after the run began",
            crate::records_before_claim::CLAIMED_KEY
        ));
    }
    let job: uuid::Uuid = str_at(run, "/id")
        .parse()
        .map_err(|_| "the run's id is not a uuid".to_string())?;
    let step: uuid::Uuid = str_at(building, "/id")
        .parse()
        .map_err(|_| "the run's building step id is not a uuid".to_string())?;
    let record = reader
        .executor_original(job, step, LAUNCHED_MODEL_KEY)
        .await
        .map_err(|e| format!("the launch record could not be read: {e:#}"))?;
    if let Some(r) = &record {
        immutable_record_resource(r, job, step, LAUNCHED_MODEL_KEY).map_err(|e| e.to_string())?;
    }
    Ok(record)
}

/// PURE: which model the review run DECLARED, which its LAUNCHER started
/// it on, and which its own report OBSERVED, as `{declared, launched,
/// launched_by, observed, matches, cross_check, says}`. The release
/// records this beside the verdict it releases on (backlog ad82090a),
/// and the yard shows `says` and `matches`.
///
/// WHY. Two reviews released cars on a declared `opus-5[1m]` that never
/// ran (runs 81840889, 0cfcae19; train #931 cancelled before merge).
///
/// ONLY THE LAUNCHER'S RECORD COUNTS (backlog e6b05dd6, David on design
/// fb9ba887 D2, 2026-10-09). `matches` compares the declaration with
/// `launch`, the first record the launcher wrote before the run could
/// act ([`launch_record`]). It used to compare it with
/// `metadata.model_observed`, which the run's own `--report` writes from
/// a transcript found on its own host: the agent describing itself, so
/// a refusal built on it would add friction without adding trust (D1).
/// The observation is now a CROSS-CHECK: `cross_check` says whether the
/// transcript agrees with the launch, and never moves `matches`.
///
/// EXACT EQUALITY, NO ALIAS TABLE (backlog ad82090a, decision 2). A
/// launch that handed the harness a name the rate card does not price —
/// Claude Code's `opus`, which that harness resolves to a model itself —
/// did not pin a model, so `matches` is `null`, never `true` and never
/// `false`: unknown is never a match. So is a missing or unread record.
///
/// RECORDED, NOT REFUSED. D1 stays record-only until David decides it
/// again on this record.
pub fn reviewer_model(
    run: &Value,
    launch: &Result<Option<crate::first_record::FirstRecord>, String>,
) -> Value {
    let declared = Some(str_at(run, "/metadata/model")).filter(|s| !s.is_empty());
    let record = launch.as_ref().ok().and_then(Option::as_ref);
    let launched = record
        .and_then(|r| r.value.get("model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let pinned = launched.filter(|l| crate::agent_spec::known_models().iter().any(|m| m == l));
    let report = run.pointer(&format!("/metadata/{MODEL_OBSERVED_KEY}"));
    let observed = report
        .and_then(|r| r.get("observed"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let matches = declared.zip(pinned).map(|(d, l)| d == l);
    let cross_check = pinned.zip(observed).map(|(l, o)| l == o);
    let d = declared.unwrap_or("nothing");
    let launch_part = match (launch, record, pinned) {
        (_, Some(_), Some(l)) if matches == Some(false) => {
            format!("declared {d}, launched {l} — they DIFFER")
        }
        (_, Some(_), Some(l)) => format!("declared {d}, launched {l}"),
        (_, Some(r), None) => {
            let who = r
                .value
                .get("harness")
                .and_then(Value::as_str)
                .unwrap_or("the harness");
            match launched {
                Some(l) => format!(
                    "declared {d}, launch pinned no model ({who} was handed `{l}`, which the \
                     rate card does not price, so it chose the model itself)"
                ),
                None => format!(
                    "declared {d}, launch pinned no model ({who} was handed none, so it chose \
                     one itself)"
                ),
            }
        }
        (Ok(_), None, _) => format!(
            "declared {d}, launched unknown (no launcher recorded a model on this run: it was \
             started before backlog e6b05dd6, or by a hand no launcher saw)"
        ),
        (Err(why), None, _) => format!("declared {d}, launched unknown ({why})"),
    };
    let observed_part = match (observed, cross_check, report) {
        (Some(o), Some(false), _) => {
            format!("observed {o} — the transcript DIFFERS from the launch")
        }
        (Some(o), _, _) => format!("observed {o}"),
        (None, _, None) => "observed unknown (the run has not reported)".to_string(),
        (None, _, Some(_)) => {
            "observed unknown (its report read no model from the transcript)".to_string()
        }
    };
    json!({
        "declared": declared,
        "launched": launched,
        "launched_by": record.map(|r| r.actor.to_string()),
        "observed": observed,
        "matches": matches,
        "cross_check": cross_check,
        "says": format!("{launch_part}; {observed_part}"),
    })
}

/// The reviewer's [`SAFE_LINE_PREFIX`] line, read from its findings. The
/// first such line wins. `None` when the review wrote none. Shown, never
/// judged.
pub fn safe_line(run: &Value) -> Option<String> {
    run.pointer(&format!("/metadata/{REVIEW_KEY}/findings"))
        .and_then(Value::as_str)?
        .lines()
        .map(str::trim)
        .map(|l| l.trim_start_matches(['*', '#', '-', '>', ' ']))
        .find(|l| l.starts_with(SAFE_LINE_PREFIX))
        // Bold markers are the report's markdown, not the reviewer's words.
        .map(|l| l.replace("**", ""))
}

/// What a release request asks: the review it rests on and the shell it
/// came from. `by` and `at` are never part of it; the door stamps them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAsk {
    /// The reviewer's agent-run, as a full id.
    pub review: String,
    /// The car head the release is given at, read by the caller from the
    /// forge. When it is absent, the head the hold was placed at
    /// (`hold_sha`) is used.
    #[serde(default)]
    pub head: Option<String>,
    /// The main the release was given against: a record only.
    #[serde(default)]
    pub main_sha: Option<String>,
    /// The run the releasing shell is (`BOSS_AGENT_RUN`), when it is one.
    /// The car's builder run is refused.
    #[serde(default)]
    pub run: Option<String>,
    /// The releasing shell is an agent's (`CLAUDECODE` set). Recorded,
    /// never trusted (review 5aa91689 B1).
    #[serde(default)]
    pub agent_shell: bool,
}

/// A release the rule grants: the review step it is written on, and the
/// one merge body that writes the record and lifts the hold in one act.
#[derive(Debug, Clone, PartialEq)]
pub struct Granted {
    pub step_id: String,
    pub head: String,
    pub record: Value,
    pub patch: Value,
}

/// THE RULE, as the door asks it: may `ask` release `car` on `run`'s
/// review, and what is written if so. Every refusal is the whole
/// sentence a reader needs. The checks run in the CLI's order: the step
/// is open, the shell is not the builder, there is a head, no release is
/// recorded at it already, the executor is eligible, and the review
/// vouches.
pub async fn judge(
    reader: &impl ExecutorRecordReader,
    car: &Value,
    run: &Value,
    ask: &ReleaseAsk,
    by: &str,
    at: &str,
) -> Result<Granted, String> {
    let review = ask.review.trim();
    if !looks_like_uuid(review) {
        return Err(format!(
            "--review {review:?} is not a full run id — the record carries the id that was \
             checked, never the characters that were typed"
        ));
    }
    let step = holdable(car)?;
    may_release_identity(car, ask.run.as_deref())?;
    let head = ask
        .head
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .or_else(|| hold_sha(car))
        .ok_or_else(|| unbound(car, step))?
        .to_string();
    released_already(car, step, &head, review)?;
    let executor = read_executor_eligibility(reader, run, car, &head)
        .await
        .map_err(|e| format!("pinned executor eligibility refused: {e:#}"))?;
    vouches_with_executor(run, car, &head, &executor)?;
    let main = ask
        .main_sha
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let mut rec = release_record(&head, review, by, at, main.unwrap_or_default());
    if main.is_none() {
        rec["main_sha"] = Value::Null;
    }
    if let Some(binding) = executor.record_binding() {
        rec["executor_binding"] = binding;
    }
    rec[REVIEWER_MODEL_KEY] = reviewer_model(run, &launch_record(reader, run).await);
    if let Some(r) = ask.run.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        rec["run"] = json!(r);
    }
    rec["agent_shell"] = json!(ask.agent_shell);
    let patch = json!({ "hold": Value::Null, crate::car::RELEASE: rec.clone() });
    Ok(Granted {
        step_id: str_at(step, "/id").to_string(),
        head,
        record: rec,
        patch,
    })
}

/// PURE: why a request that names no head is refused when the hold
/// records none either. A hold written after a release is judged at no
/// head, because its writer deletes the `hold_sha` that release lifted
/// ([`crate::car::hold_body`], backlog 6c463c66). It is not a review's
/// hold, so this refusal names the bare road that lifts it, the same
/// road [`released_already`] names to the CLI at the forge head. A step
/// with no release keeps the old answer: read the head from the forge.
fn unbound(car: &Value, step: &Value) -> String {
    let id = str_at(car, "/id");
    let md = step.get("metadata").unwrap_or(&Value::Null);
    let release = md.get(crate::car::RELEASE).filter(|r| r.is_object());
    let (Some(release), Some(hold)) = (release, crate::stranded::hold_reason(md)) else {
        return format!(
            "car {} records no head its hold was judged at (hold_sha), and the request named \
             none — release it with boss release, which reads the forge",
            short(id)
        );
    };
    let branch = str_at(car, "/metadata/branch");
    let target = if branch.is_empty() { short(id) } else { branch };
    format!(
        "car {} {target}: the hold it carries now ({hold:?}) was placed after its release at {} \
         (on review {}, by {}, at {}) and was judged at no head, so it is not a review's to lift \
         (backlog 6c463c66). Nothing was written. Once that hold's reason is answered, lift it \
         with a bare `boss release {target}`: the conductor boards the car on the release it \
         already has, or holds it again for a review if its head has moved. A car that must \
         change is rebuilt, re-gated and its new head reviewed.",
        short(id),
        short(str_at(release, "/sha")),
        short(str_at(release, "/review")),
        str_at(release, "/by"),
        str_at(release, "/at"),
    )
}

/// PURE: a review releases a car ONCE at a head (review 4822764d B1). When
/// the review step already records a release at `head`, whatever holds
/// the car now was placed AFTER that release: the conductor's red-train
/// hold and a hand `boss hold` write `hold` alone and leave the old
/// `release`, `hold_sha` and `agent_run` in place. The review never
/// judged that hold, so naming it again would lift the hold "on review R"
/// and record a provenance R never gave. Measured by the reviewer: a
/// red-train hold over a release at the same sha read releasable, and
/// Release-all would have lifted it in bulk.
///
/// The road is the bare release, which clears the marker and leaves the
/// release it already has; the conductor then boards the car on that
/// release (`Dock::Check`) or holds it again if the head moved. A re-review
/// at a NEW head is untouched: the conductor's re-hold writes the new
/// head as `hold_sha`, which no earlier release names. Why a stale release
/// survives on the step at all is backlog 6c463c66, not this rule.
///
/// A CARRIED release counts at the head its review READ as well
/// (`reviewed_sha`, backlog 6c463c66). The dock's replay moves a
/// release to the new head (H2) and leaves `hold_sha` at H1, so a hold
/// written over it the old way, `hold` alone, was judged at H1. That
/// missed this rule and was granted on the same review, which vouches at
/// H1: measured by test, a red-train hold read releasable in the yard.
/// The writer now deletes that `hold_sha`; this covers steps written
/// before it did.
///
/// Only the SAME review is refused (re-review 6c1788dc B2). When the
/// conductor's own boarding check fails at the head the release names, it
/// holds the car at that head and asks for "boss review, then boss release
/// --review"; a fresh review, dispatched with that hold in view, is a new
/// judgement and is granted. Refusing it too left no road but a rebuild:
/// the bare release sent the car back into the check that had just failed.
fn released_already(car: &Value, step: &Value, head: &str, review: &str) -> Result<(), String> {
    let md = step.get("metadata").unwrap_or(&Value::Null);
    let Some(release) = md.get(crate::car::RELEASE).filter(|r| r.is_object()) else {
        return Ok(());
    };
    let at = str_at(release, "/sha");
    let read = str_at(release, "/reviewed_sha");
    let at_head = at == head || (!read.is_empty() && read == head);
    // The id compares without case: `looks_like_uuid` admits upper-case
    // hex, and the same run typed in upper case is the same review.
    if !at_head || !str_at(release, "/review").eq_ignore_ascii_case(review) {
        return Ok(());
    }
    let id = str_at(car, "/id");
    let branch = str_at(car, "/metadata/branch");
    let target = if branch.is_empty() { short(id) } else { branch };
    let Some(hold) = crate::stranded::hold_reason(md) else {
        // N2 (re-review 6c1788dc): no hold remains, so name none.
        return Err(format!(
            "car {} {target}: already released at {} on review {}; nothing to do (by {}, at \
             {}). Nothing was written.",
            short(id),
            short(at),
            short(review),
            str_at(release, "/by"),
            str_at(release, "/at"),
        ));
    };
    Err(format!(
        "car {} {target}: its review step already records a release at {} (on review {}, by \
         {}, at {}), so the hold it carries now ({hold:?}) was placed after that release, and \
         it is not this review's to lift — a review releases a car once at a head, and it never \
         judged what held the car again (review 4822764d B1). Nothing was written. Once that \
         hold's reason is answered, lift it with a bare `boss release {target}`: the conductor \
         boards the car on the release it already has. A car that must change is rebuilt, \
         re-gated and its new head reviewed.",
        short(id),
        short(at),
        short(review),
        str_at(release, "/by"),
        str_at(release, "/at"),
    ))
}

/// What a held car's release rests on, for the yard's row: the review,
/// its verdict, the head it read against the head the hold was placed
/// at, the reviewer's SAFE line, the models, and the door's own answer.
/// `releasable` and `refusal` come from [`judge`] itself, asked as a
/// person with no run, so the row can never promise what the door
/// refuses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// The reviewer's agent-run the release would name: the run the
    /// review step was dispatched to.
    pub review: Option<String>,
    pub verdict: Option<String>,
    pub reviewed_sha: Option<String>,
    /// The head the hold was placed at (`hold_sha`).
    pub head: Option<String>,
    /// `reviewed_sha == head`, when both are known.
    pub head_matches: Option<bool>,
    pub safe_line: Option<String>,
    pub reviewer_model: Value,
    /// The door would grant a release asked now.
    pub releasable: bool,
    /// Why it would not, in the door's own words.
    pub refusal: Option<String>,
}

/// [`Decision`] for `car`. `run` is the reviewer's packet, read by the
/// caller, or `Err` with why it could not be read.
pub async fn decision(
    reader: &impl ExecutorRecordReader,
    car: &Value,
    run: Result<&Value, String>,
) -> Decision {
    let review = dispatched_reviewer(car).map(str::to_string);
    let head = hold_sha(car).map(str::to_string);
    let opt = |v: &str| Some(v.to_string()).filter(|s| !s.is_empty());
    let (verdict, reviewed_sha, safe, model, outcome) = match (&review, run) {
        (None, _) => (
            None,
            None,
            None,
            Value::Null,
            Err("no reviewer has been dispatched to its review step yet".to_string()),
        ),
        (Some(_), Err(why)) => (None, None, None, Value::Null, Err(why)),
        (Some(id), Ok(run)) => {
            let ask = ReleaseAsk {
                review: id.clone(),
                ..ReleaseAsk::default()
            };
            let outcome = judge(reader, car, run, &ask, "", "").await.map(|_| ());
            (
                opt(str_at(run, &format!("/metadata/{REVIEW_KEY}/verdict"))),
                opt(str_at(run, &format!("/metadata/{REVIEW_KEY}/reviewed_sha"))),
                safe_line(run),
                reviewer_model(run, &launch_record(reader, run).await),
                outcome,
            )
        }
    };
    Decision {
        head_matches: reviewed_sha
            .as_ref()
            .zip(head.as_ref())
            .map(|(r, h)| r == h),
        review,
        verdict,
        reviewed_sha,
        head,
        safe_line: safe,
        reviewer_model: model,
        releasable: outcome.is_ok(),
        refusal: outcome.err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAR: &str = "c0ffee00-0000-4000-8000-000000000000";
    const BUILDER: &str = "b0b0b0b0-0000-4000-8000-000000000000";
    const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";
    const STEP: &str = "00000000-0000-4000-8000-000000000003";
    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    /// A held car, standing at the dock, held at `H1`, on the
    /// ship-a-change protocol v7.
    fn held_car() -> Value {
        json!({"id": CAR, "kind": "ship-a-change", "workflow_version": 7,
               "metadata": {"branch": "feat/x", "agent_run": BUILDER},
               "steps": [{"id": STEP, "spec_slug": "review", "title": crate::car::REVIEW,
                          "status": "active",
                          "metadata": {"hold": "area policy", "hold_sha": H1,
                                       "agent_run": REVIEWER}}]})
    }

    fn review_at(sha: &str, verdict: &str, findings: &str) -> Value {
        json!({"car": CAR, "branch": "feat/x", "reviewed_sha": sha, "verdict": verdict,
               "findings": findings, "at": "2026-10-09T13:00:00Z"})
    }

    fn reviewer(review: Value) -> Value {
        json!({"id": REVIEWER, "kind": "agent-run",
               "metadata": {REVIEW_KEY: review, "model": "opus-5[1m]",
                            "model_observed": {"observed": "opus-5-5"}}})
    }

    /// The pinned protocol answers advisory, and there are no first
    /// records: the shape every live car had on 2026-10-09.
    struct Advisory;

    #[async_trait::async_trait]
    impl ExecutorRecordReader for Advisory {
        async fn protocol_version(&self, kind: &str, version: i64) -> Result<Option<Value>> {
            Ok(Some(
                json!({"kind": kind, "version": version, "status": "active",
                           "steps": [{"title": "review"}]}),
            ))
        }
        async fn executor_original(
            &self,
            _: uuid::Uuid,
            _: uuid::Uuid,
            _: &str,
        ) -> Result<Option<crate::first_record::FirstRecord>> {
            Ok(None)
        }
    }

    fn ask(head: Option<&str>) -> ReleaseAsk {
        ReleaseAsk {
            review: REVIEWER.into(),
            head: head.map(str::to_string),
            ..ReleaseAsk::default()
        }
    }

    /// THE YARD'S ROAD (backlog ed8e5c85): a request that names no head
    /// is judged at the head the hold was placed at, `by` is the actor
    /// the door stamps, and the one merge body lifts the hold and writes
    /// the record together, as `boss release` always has.
    #[tokio::test]
    async fn a_release_with_no_head_is_judged_at_the_head_the_hold_was_placed_at() {
        let run = reviewer(review_at(
            H1,
            RELEASE,
            "SAFE TO RELEASE WITHOUT DAVID: yes.",
        ));
        let granted = judge(&Advisory, &held_car(), &run, &ask(None), "emp-david", "t")
            .await
            .expect("granted");
        assert_eq!(granted.step_id, STEP);
        assert_eq!(granted.head, H1);
        assert_eq!(granted.patch["hold"], Value::Null);
        let rec = &granted.patch[crate::car::RELEASE];
        assert_eq!(rec["sha"], json!(H1));
        assert_eq!(rec["review"], json!(REVIEWER));
        assert_eq!(rec["verdict"], json!(RELEASE));
        assert_eq!(rec["by"], json!("emp-david"));
        assert_eq!(rec["main_sha"], Value::Null, "the yard reads no main");
        assert_eq!(rec["agent_shell"], json!(false));
        assert!(rec.get("run").is_none());
        // The run's transcript observed a model other than the declared
        // one, and no launcher recorded one: the observation is a
        // cross-check and never the verdict, so the match is unknown
        // (backlog e6b05dd6).
        assert_eq!(rec[REVIEWER_MODEL_KEY]["matches"], Value::Null);
        assert_eq!(rec[REVIEWER_MODEL_KEY]["observed"], json!("opus-5-5"));
    }

    const RUN_ID: &str = "a1a1a1a1-0000-4000-8000-000000000000";
    const BUILDING: &str = "b1b1b1b1-0000-4000-8000-000000000000";

    /// Advisory, plus ONE first record: what a launcher wrote under
    /// [`LAUNCHED_MODEL_KEY`] on [`BUILDING`], signed by `actor`. Every
    /// other read answers as [`Advisory`] does.
    struct Launched {
        value: Value,
        actor: &'static str,
    }

    #[async_trait::async_trait]
    impl ExecutorRecordReader for Launched {
        async fn protocol_version(&self, kind: &str, version: i64) -> Result<Option<Value>> {
            Advisory.protocol_version(kind, version).await
        }
        async fn executor_original(
            &self,
            job: uuid::Uuid,
            step: uuid::Uuid,
            key: &str,
        ) -> Result<Option<crate::first_record::FirstRecord>> {
            if key != LAUNCHED_MODEL_KEY || step.to_string() != BUILDING {
                return Ok(None);
            }
            Ok(Some(crate::first_record::FirstRecord::new(
                boss_core::job::JobId::from_uuid(job),
                boss_core::job::StepId::from_uuid(step),
                key,
                &self.value,
                &boss_core::publisher::EventStamp::new(
                    "jobs.record",
                    boss_core::actor::ActorId::RegisteredAgent(self.actor.into()),
                ),
                uuid::Uuid::from_u128(7),
            )))
        }
    }

    /// A review run with a Building step that declares the launch record
    /// as one written before the claim (as agent-run's protocol does),
    /// declaring `declared` and with a report that observed `observed`.
    fn launched_run(declared: &str, observed: Option<&str>) -> Value {
        let mut run = json!({"id": RUN_ID, "kind": "agent-run",
            "metadata": {"model": declared},
            "steps": [{"id": BUILDING, "spec_slug": BUILDING_SLUG, "status": "completed",
                       "metadata": {"records_before_claim": [LAUNCHED_MODEL_KEY],
                                    "records_before_claim_closed_at": "2026-10-10T00:00:00Z"}}]});
        if let Some(o) = observed {
            run["metadata"][MODEL_OBSERVED_KEY] = json!({"observed": o});
        }
        run
    }

    /// ONLY THE LAUNCHER'S RECORD COUNTS (backlog e6b05dd6, D2 of design
    /// fb9ba887). Each shape a review run is in when it is released:
    #[tokio::test]
    async fn the_reviewer_model_counts_the_launchers_record_and_cross_checks_the_transcript() {
        let said = |reader: Launched, run: Value| async move {
            reviewer_model(&run, &launch_record(&reader, &run).await)
        };
        let codex = |model: &str| Launched {
            value: json!({"model": model, "harness": "codex-exec", "via": "argv --model"}),
            actor: "agent-claude",
        };

        // A launcher started the declared model and the transcript agrees.
        let m = said(
            codex("gpt-6.1-sol"),
            launched_run("gpt-6.1-sol", Some("gpt-6.1-sol")),
        )
        .await;
        assert_eq!(m["launched"], json!("gpt-6.1-sol"));
        assert_eq!(m["launched_by"], json!("agent-claude"));
        assert_eq!(m["matches"], json!(true));
        assert_eq!(m["cross_check"], json!(true));
        assert_eq!(
            m["says"],
            json!("declared gpt-6.1-sol, launched gpt-6.1-sol; observed gpt-6.1-sol")
        );

        // The launch differs from the declaration: `matches` is false
        // whatever the transcript says — the run's report cannot repair
        // what its launcher started.
        let m = said(
            codex("gpt-6.1-sol"),
            launched_run("opus-5-5", Some("opus-5-5")),
        )
        .await;
        assert_eq!(m["matches"], json!(false));
        assert_eq!(m["cross_check"], json!(false));
        assert_eq!(
            m["says"],
            json!(
                "declared opus-5-5, launched gpt-6.1-sol — they DIFFER; observed opus-5-5 — the \
                 transcript DIFFERS from the launch"
            )
        );

        // ...and the other way round: a transcript that disagrees with a
        // launch that matched is a cross-check failure, not a mismatch.
        let m = said(
            codex("opus-5-5"),
            launched_run("opus-5-5", Some("gpt-6.1-sol")),
        )
        .await;
        assert_eq!(m["matches"], json!(true));
        assert_eq!(m["cross_check"], json!(false));

        // Claude Code is handed `opus`, which it resolves itself: the
        // launch pinned no model, so the match is unknown — never true,
        // and never a DIFFER the record cannot stand behind.
        let claude = Launched {
            value: json!({"model": "opus", "via": "the Agent tool call",
                          "subagent_type": "effort-high"}),
            actor: "agent-claude",
        };
        let m = said(claude, launched_run("opus-5-5", Some("opus-5-5"))).await;
        assert_eq!(m["launched"], json!("opus"));
        assert_eq!(m["matches"], Value::Null);
        assert_eq!(m["cross_check"], Value::Null);
        assert_eq!(
            m["says"],
            json!(
                "declared opus-5-5, launch pinned no model (the harness was handed `opus`, which \
                 the rate card does not price, so it chose the model itself); observed opus-5-5"
            )
        );

        // A launcher that handed the harness no model at all (an Agent
        // call naming none inherits one): pinned nothing, decides nothing.
        let m = said(
            Launched {
                value: json!(null),
                actor: "agent-claude",
            },
            launched_run("opus-5-5", Some("gpt-6.1-sol")),
        )
        .await;
        assert_eq!(m["launched"], Value::Null);
        assert_eq!(m["matches"], Value::Null);
        assert!(
            m["says"].as_str().unwrap().contains("was handed none"),
            "{m}"
        );
        // No launcher recorded anything: the transcript alone decides
        // nothing.
        let no_record = reviewer_model(&launched_run("opus-5-5", None), &Ok(None));
        assert_eq!(no_record["matches"], Value::Null);
        assert!(
            no_record["says"]
                .as_str()
                .unwrap()
                .starts_with("declared opus-5-5, launched unknown (no launcher recorded"),
            "{no_record}"
        );
        assert!(
            no_record["says"]
                .as_str()
                .unwrap()
                .ends_with("observed unknown (the run has not reported)"),
            "{no_record}"
        );

        // A RECORD THE DOOR COULD NOT HAVE REFUSED IS NOT A LAUNCH (review
        // fd7f1030 F1). A Building step that does not declare the key as
        // written before the claim — a run admitted under an agent-run
        // version older than the declaration — let the run claim its step
        // and then write `launch.model` itself, signed as the same agent a
        // launcher signs as. The record is there and says the declared
        // model; it counts for nothing.
        let mut claimed = launched_run("opus-5-5", Some("opus-5-5"));
        claimed["steps"][0]["status"] = json!("active");
        claimed["steps"][0]["metadata"] = json!({});
        let m = said(codex("opus-5-5"), claimed).await;
        assert_eq!(m["launched"], Value::Null, "{m}");
        assert_eq!(m["matches"], Value::Null, "{m}");
        assert!(
            m["says"]
                .as_str()
                .unwrap()
                .starts_with("declared opus-5-5, launched unknown (")
                && m["says"].as_str().unwrap().contains("records_before_claim"),
            "{m}"
        );

        // A step that declares the key but that no claim ever closed (third
        // review 85c11887 N2): never claimed, or claimed by a jobs API
        // older than the mark — either way nothing refused a record written
        // after the run began. It counts for nothing either.
        let mut unclosed = launched_run("opus-5-5", Some("opus-5-5"));
        unclosed["steps"][0]["metadata"] =
            json!({crate::records_before_claim::KEY: [LAUNCHED_MODEL_KEY]});
        let m = said(codex("opus-5-5"), unclosed).await;
        assert_eq!(m["launched"], Value::Null, "{m}");
        assert_eq!(m["matches"], Value::Null, "{m}");
        assert!(
            m["says"]
                .as_str()
                .unwrap()
                .contains(crate::records_before_claim::CLAIMED_KEY),
            "{m}"
        );

        // A run with no Building step (a hand-filed one) is unknown with
        // its reason, never an error.
        let bare = reviewer(review_at(H1, RELEASE, ""));
        let read = launch_record(&Advisory, &bare).await;
        assert!(read.is_err(), "{read:?}");
        let m = reviewer_model(&bare, &read);
        assert_eq!(m["matches"], Value::Null);
        assert!(
            m["says"].as_str().unwrap().contains("no building step"),
            "{m}"
        );
    }

    /// It refuses where `boss release` refuses, each in words a reader
    /// can act on: another head, a changes verdict, the builder's shell,
    /// a typed short id, a closed review, and a car with no head at all.
    #[tokio::test]
    async fn the_door_refuses_where_the_cli_refused() {
        let car = held_car();
        let refused = |r: Result<Granted, String>, want: &str| {
            let why = r.expect_err(want);
            assert!(why.contains(want), "{want} in {why}");
        };
        let at_h2 = reviewer(review_at(H2, RELEASE, ""));
        refused(
            judge(&Advisory, &car, &at_h2, &ask(None), "d", "t").await,
            "a review of another head",
        );
        let changes = reviewer(review_at(H1, "changes", ""));
        refused(
            judge(&Advisory, &car, &changes, &ask(None), "d", "t").await,
            "not \"release\"",
        );
        let ok = reviewer(review_at(H1, RELEASE, ""));
        let mut builder = ask(None);
        builder.run = Some(BUILDER.into());
        refused(
            judge(&Advisory, &car, &ok, &builder, "d", "t").await,
            "the run that built car",
        );
        let mut typed = ask(None);
        typed.review = "5eed5eed".into();
        refused(
            judge(&Advisory, &car, &ok, &typed, "d", "t").await,
            "not a full run id",
        );
        let mut closed = held_car();
        closed["steps"][0]["status"] = json!("completed");
        refused(
            judge(&Advisory, &closed, &ok, &ask(None), "d", "t").await,
            "review is completed",
        );
        let mut unbound = held_car();
        unbound["steps"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("hold_sha");
        refused(
            judge(&Advisory, &unbound, &ok, &ask(None), "d", "t").await,
            "records no head",
        );
        // The CLI's forge head wins over the hold's when it names one.
        let granted = judge(&Advisory, &unbound, &ok, &ask(Some(H1)), "d", "t")
            .await
            .expect("the forge head the CLI read");
        assert_eq!(granted.head, H1);
    }

    /// The yard's row is the door's own answer, plus what a person reads
    /// before pressing it: the verdict, both heads, the SAFE line and the
    /// models.
    #[tokio::test]
    async fn the_rows_decision_is_the_doors_answer() {
        let findings = "## Findings\n### VERDICT: release\n**SAFE TO RELEASE WITHOUT DAVID: yes.** \
                        It narrows the gap.\n";
        let run = reviewer(review_at(H1, RELEASE, findings));
        let d = decision(&Advisory, &held_car(), Ok(&run)).await;
        assert!(d.releasable, "{d:?}");
        assert_eq!(d.refusal, None);
        assert_eq!(d.review.as_deref(), Some(REVIEWER));
        assert_eq!(d.verdict.as_deref(), Some(RELEASE));
        assert_eq!(d.head_matches, Some(true));
        assert_eq!(
            d.safe_line.as_deref(),
            Some("SAFE TO RELEASE WITHOUT DAVID: yes. It narrows the gap.")
        );
        assert_eq!(d.reviewer_model["observed"], json!("opus-5-5"));

        let stale = reviewer(review_at(H2, RELEASE, ""));
        let d = decision(&Advisory, &held_car(), Ok(&stale)).await;
        assert!(!d.releasable);
        assert_eq!(d.head_matches, Some(false));
        assert!(d.refusal.unwrap().contains("another head"));

        let mut undispatched = held_car();
        undispatched["steps"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("agent_run");
        let d = decision(&Advisory, &undispatched, Err("unread".into())).await;
        assert!(!d.releasable);
        assert!(
            d.refusal
                .unwrap()
                .contains("no reviewer has been dispatched")
        );
    }

    /// A HOLD PLACED AFTER A RELEASE IS NOT THE REVIEW'S TO LIFT (review
    /// 4822764d B1). A released car's review step keeps its `release`,
    /// `hold_sha` and `agent_run`; the conductor's red-train hold and a
    /// hand `boss hold` write `hold` alone. Before this, the row read such
    /// a car releasable and Release-all lifted the red-train hold "on
    /// review R", a review that never judged that red. The door now
    /// refuses a release at a head the step already records a release
    /// at, from the row, from Release-all and from the CLI alike, and
    /// names the bare release as the road.
    #[tokio::test]
    async fn a_hold_placed_after_a_release_at_this_head_is_not_the_reviews_to_lift() {
        let run = reviewer(review_at(
            H1,
            RELEASE,
            "SAFE TO RELEASE WITHOUT DAVID: yes.",
        ));
        let earlier = release_record(H1, REVIEWER, "emp-david", "t0", "");
        let held_after = |hold: &str| {
            let mut car = held_car();
            car["steps"][0]["metadata"]["hold"] = json!(hold);
            car["steps"][0]["metadata"][crate::car::RELEASE] = earlier.clone();
            car
        };
        for hold in [
            "held by the conductor: train deadbeef was judged red; rebuild it on current main, \
             re-gate, then boss release it",
            "held for the restart",
        ] {
            let car = held_after(hold);
            for head in [None, Some(H1)] {
                let why = judge(&Advisory, &car, &run, &ask(head), "emp-david", "t")
                    .await
                    .expect_err("a hold placed after the release at this head");
                assert!(why.contains("already records a release at"), "{why}");
                assert!(
                    why.contains("boss release feat/x`"),
                    "names the bare road: {why}"
                );
            }
            let d = decision(&Advisory, &car, Ok(&run)).await;
            assert!(!d.releasable, "the row offers no Release: {d:?}");
            assert!(
                d.refusal.unwrap().contains("not this review's to lift"),
                "the row says why"
            );
        }

        // KEPT: a first release is unaffected (the car carries no release).
        assert!(
            judge(&Advisory, &held_car(), &run, &ask(None), "d", "t")
                .await
                .is_ok()
        );
        // KEPT: a re-review at a new head. The conductor re-held the car at
        // H2 (its new hold_sha) because the release names H1; a review of
        // H2 releases it.
        let mut moved = held_after("conductor re-judge at 222222222222: released at H1");
        moved["steps"][0]["metadata"][crate::car::HOLD_SHA] = json!(H2);
        let at_h2 = reviewer(review_at(H2, RELEASE, ""));
        let granted = judge(&Advisory, &moved, &at_h2, &ask(None), "d", "t")
            .await
            .expect("a review of the new head");
        assert_eq!(granted.head, H2);
        assert!(decision(&Advisory, &moved, Ok(&at_h2)).await.releasable);

        // REFUSED, AND SAYS SO PLAINLY (re-review 6c1788dc N2): the same
        // review asked again at the head it already released names no
        // hold, because none remains.
        let mut done = held_car();
        done["steps"][0]["metadata"]["hold"] = Value::Null;
        done["steps"][0]["metadata"][crate::car::RELEASE] = earlier.clone();
        let why = judge(&Advisory, &done, &run, &ask(Some(H1)), "d", "t")
            .await
            .expect_err("the same review, again, at the same head");
        assert!(
            why.contains("already released at 11111111 on review 5eed5eed; nothing to do"),
            "{why}"
        );
        assert!(
            !why.contains("hold"),
            "names no hold that is not there: {why}"
        );
    }

    /// The merge door's own rule, for a body written onto `md`: a key
    /// sent as null is deleted, any other key is set.
    fn merged(md: &Value, body: &Value) -> Value {
        let mut out = md.as_object().cloned().unwrap_or_default();
        for (k, v) in body.as_object().into_iter().flatten() {
            if v.is_null() {
                out.remove(k);
            } else {
                out.insert(k.clone(), v.clone());
            }
        }
        Value::Object(out)
    }

    /// THE ROOT UNDER B1 (backlog 6c463c66). A hold written after a
    /// release is judged at no head, because the writer now deletes the
    /// `hold_sha` that release lifted ([`crate::car::hold_body`]). The
    /// head-less request (the yard's row and Release-all) is refused and
    /// told the bare road, as the CLI is at the forge head. The case
    /// that was LIVE on main: a release CARRIED over the dock's replay to
    /// H2 keeps the old `hold_sha` H1, and the yard judged a red-train
    /// hold over it at H1. That missed the B1 rule (no release at H1)
    /// and was granted on its old review, which vouches at H1.
    #[tokio::test]
    async fn a_hold_written_after_a_release_is_judged_at_no_head_and_names_the_bare_road() {
        let run = reviewer(review_at(
            H1,
            RELEASE,
            "SAFE TO RELEASE WITHOUT DAVID: yes.",
        ));
        let red = "held by the conductor: train deadbeef was judged red; rebuild it on current \
                   main, re-gate, then boss release it";
        let mut carried = release_record(H2, REVIEWER, "emp-david", "t0", "m");
        carried["reviewed_sha"] = json!(H1);
        for (release, cli_head) in [
            (release_record(H1, REVIEWER, "emp-david", "t0", ""), H1),
            (carried, H2),
        ] {
            // Released and boarded: no hold, the release, the old head.
            let mut car = held_car();
            let md = &mut car["steps"][0]["metadata"];
            md["hold"] = Value::Null;
            md[crate::car::RELEASE] = release;
            let before = md.clone();
            *md = merged(&before, &crate::car::hold_body(&before, red));
            assert!(md.get(crate::car::HOLD_SHA).is_none(), "{md}");

            let why = judge(&Advisory, &car, &run, &ask(None), "emp-david", "t")
                .await
                .expect_err("a hold placed after a release is not the review's to lift");
            assert!(why.contains("placed after its release"), "{why}");
            assert!(
                why.contains("boss release feat/x`"),
                "names the bare road: {why}"
            );
            let d = decision(&Advisory, &car, Ok(&run)).await;
            assert!(!d.releasable, "the row offers no Release: {d:?}");
            assert_eq!(d.head, None, "the row shows no head the hold was judged at");
            // The CLI, at the forge head, meets the B1 rule as before.
            let why = judge(&Advisory, &car, &run, &ask(Some(cli_head)), "d", "t")
                .await
                .expect_err("the same review at the forge head");
            assert!(why.contains("not this review's to lift"), "{why}");
        }
    }

    /// A STEP ALREADY WRITTEN THE OLD WAY. Cars at the dock today may carry
    /// a release carried to H2, the old `hold_sha` H1, and a red-train
    /// hold written as `hold` alone. The same review asked again at the
    /// head it READ is the release it already gave, carried, so it is
    /// refused like one at the head the release names.
    #[tokio::test]
    async fn the_same_review_at_the_head_a_carried_release_read_is_refused() {
        let run = reviewer(review_at(H1, RELEASE, ""));
        let mut car = held_car();
        let md = &mut car["steps"][0]["metadata"];
        let mut carried = release_record(H2, REVIEWER, "emp-david", "t0", "m");
        carried["reviewed_sha"] = json!(H1);
        md[crate::car::RELEASE] = carried;
        md["hold"] = json!("held by the conductor: train deadbeef was judged red");
        let why = judge(&Advisory, &car, &run, &ask(None), "emp-david", "t")
            .await
            .expect_err("the carried release's own review, at the head it read");
        assert!(why.contains("not this review's to lift"), "{why}");
        assert!(!decision(&Advisory, &car, Ok(&run)).await.releasable);
    }

    /// THE CONDUCTOR'S OWN ROAD (re-review 6c1788dc B2). When its boarding
    /// check fails at the head the release names, the conductor holds the
    /// car at that same head, keeps the old `release`, and asks for "boss
    /// review, then boss release --review". A FRESH review, dispatched
    /// after that hold, is a new judgement made with the hold in view, so
    /// the door grants it; the bare release would only send the car back
    /// into the check that just failed. Only the SAME review asked again
    /// at that head stays refused (B1).
    #[tokio::test]
    async fn a_fresh_review_after_a_failed_boarding_check_at_the_release_head_releases() {
        const R2: &str = "7e707e70-0000-4000-8000-000000000000";
        let fresh = |sha: &str| {
            let mut r = reviewer(review_at(
                sha,
                RELEASE,
                "SAFE TO RELEASE WITHOUT DAVID: yes.",
            ));
            r["id"] = json!(R2);
            r
        };
        let as_r2 = |head: Option<&str>| ReleaseAsk {
            review: R2.into(),
            head: head.map(str::to_string),
            ..ReleaseAsk::default()
        };

        // Dock::Check: the release's review no longer vouches at H1.
        let mut car = held_car();
        let md = &mut car["steps"][0]["metadata"];
        md[crate::car::RELEASE] = release_record(H1, REVIEWER, "emp-david", "t0", "m");
        md["hold"] = json!(
            "conductor re-judge at 111111111111: its release names review run 5eed5eed, \
             which does not vouch for this head; boss review, then boss release --review"
        );
        md["agent_run"] = json!(R2);
        let r2 = fresh(H1);
        for head in [None, Some(H1)] {
            let granted = judge(&Advisory, &car, &r2, &as_r2(head), "emp-david", "t")
                .await
                .expect("a fresh review of the release's head");
            assert_eq!(granted.record["review"], json!(R2));
            assert_eq!(granted.head, H1);
        }
        assert!(decision(&Advisory, &car, Ok(&r2)).await.releasable);

        // A carried release (H1 replayed to H2) whose same-change proof
        // later fails: held at H2, the head the carried release names.
        let mut car = held_car();
        let md = &mut car["steps"][0]["metadata"];
        let mut carried = release_record(H2, REVIEWER, "emp-david", "t0", "m");
        carried["reviewed_sha"] = json!(H1);
        md[crate::car::RELEASE] = carried;
        md["hold"] = json!(
            "conductor re-judge at 222222222222: waits for a review of this head; boss review, \
             then boss release --review"
        );
        md[crate::car::HOLD_SHA] = json!(H2);
        md["agent_run"] = json!(R2);
        let r2 = fresh(H2);
        for head in [None, Some(H2)] {
            judge(&Advisory, &car, &r2, &as_r2(head), "emp-david", "t")
                .await
                .expect("a fresh review of the carried head");
        }
        assert!(decision(&Advisory, &car, Ok(&r2)).await.releasable);

        // KEPT: the old review, asked again over that hold, is refused.
        let old = reviewer(review_at(H2, RELEASE, ""));
        let mut by_r = car.clone();
        by_r["steps"][0]["metadata"]["agent_run"] = json!(REVIEWER);
        let why = judge(&Advisory, &by_r, &old, &ask(None), "emp-david", "t")
            .await
            .expect_err("the review that already released at H2");
        assert!(why.contains("not this review's to lift"), "{why}");
        // ...and so is that review typed in upper case: the same run.
        let mut upper = ask(None);
        upper.review = REVIEWER.to_ascii_uppercase();
        let why = judge(&Advisory, &by_r, &old, &upper, "emp-david", "t")
            .await
            .expect_err("the same review, upper case");
        assert!(why.contains("not this review's to lift"), "{why}");
    }

    #[test]
    fn an_advisory_result_cannot_supply_or_retarget_a_verified_receipt_reference() {
        let car = json!({"id": CAR, "metadata": {"branch": "feat/x", "agent_run": BUILDER}});
        let eligibility = ExecutorReviewEligibility {
            car: CAR.into(),
            head: H1.into(),
            verified: None,
            original_verdict: None,
            original_receipt: None,
        };
        let mut reviewer = reviewer(review_at(H1, RELEASE, ""));
        assert!(vouches_with_executor(&reviewer, &car, H1, &eligibility).is_ok());
        assert!(vouches_with_executor(&reviewer, &car, H2, &eligibility).is_err());
        reviewer["metadata"][REVIEW_KEY]["executor_binding"] =
            json!({"digest":"self-declared","event_id":REVIEWER});
        assert!(vouches_with_executor(&reviewer, &car, H1, &eligibility).is_err());
        let mut another_car = car.clone();
        another_car["id"] = json!("another-car");
        assert!(vouches_with_executor(&reviewer, &another_car, H1, &eligibility).is_err());
    }
}
