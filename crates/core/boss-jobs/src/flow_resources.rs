//! Exact, whole-vector FLOW admission. Storage adapters supply a transactionally
//! consistent capacity/held snapshot; this module never measures capacity.
use boss_core::job::{JobId, StepId};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub const TOKEN_KEY: &str = "resource_claim";
pub const RESERVED: &str = "jobs.step.reserved";
pub const RELEASED: &str = "jobs.step.released";
pub const REFUSED: &str = "jobs.step.resource_refused";

/// The step PUT body key an exit names the acquisition it ends by. One
/// spelling for the server that judges it (`http::steps::update_step`)
/// and every client that sends it (CLAUDE.md §9a; backlog 7b5195ea).
pub const EXIT_CLAIM_KEY: &str = "resource_claim_id";

/// The step PUT's refusal of an exit that names no claim, or another
/// one. It says where the id lives and what frees the slot otherwise,
/// because the reader of a 409 is a writer that has just failed to
/// end its own work (backlog 7b5195ea: four runs read the bare rule and
/// waited out their leases).
pub const EXIT_REFUSAL: &str = "resource exit requires the executor's original resource_claim_id \
     (the step's metadata.resource_claim.id, as the executor read it) or exact complete-if \
     version; a fresh server read is not an acquisition expectation. Without either, the \
     claim's slot frees only when its lease (metadata.resource_claim.lease_until) expires";

/// The id of the claim a step's metadata holds, if it holds one.
pub fn held_claim_id(metadata: &serde_json::Value) -> Option<&str> {
    metadata.get(TOKEN_KEY)?.get("id")?.as_str()
}

/// `body` — a step PUT that may take `step` out of Active — naming the
/// claim `step` held when the writer read it, so the exit frees its
/// slot; a step holding none gets `body` back unchanged.
///
/// WHY EVERY EXITING WRITER CARRIES IT (backlog 7b5195ea, 2026-11-08
/// 05:00-06:10Z). Since agent-run v11 bills a run's `building` against
/// a robots pool, the PUT refuses to take a claimed step out of Active
/// unless the body names that claim, and no writer did: `boss step
/// complete`/`release`, the green's landing rule and the analyst
/// delivery all answered 409, four finished runs held robots:opus 4/4,
/// and every dispatch was refused until the leases expired. The id is
/// the one the writer READ with the step it judged, so it is a compare
/// over the read-to-write window: a step released and claimed again in
/// between holds a new id, and the exit is still refused.
pub fn exiting(step: &serde_json::Value, mut body: serde_json::Value) -> serde_json::Value {
    let held = step.get("metadata").and_then(held_claim_id);
    if let (Some(id), Some(fields)) = (held, body.as_object_mut()) {
        fields.insert(EXIT_CLAIM_KEY.to_string(), id.into());
    }
    body
}

/// Producer-owned resource facts. Lease/STOCK producers extend this roster;
/// the claims lens pins its fold and source selection to the same set.
pub const FACT_KINDS: &[&str] = &[
    RESERVED,
    RELEASED,
    REFUSED,
    crate::events::STATION_RESOURCE_REFUSED,
    crate::pools::PUBLISHED,
    crate::pools::RETIRED,
    crate::resource_leases::IDENTITY_BOUND,
    crate::resource_leases::LEASE_EXPIRED,
    crate::resource_leases::STOCK_RETURNED,
];

/// The acquisition identity is carried by the executor's original step copy.
/// A later read cannot substitute for this generation/holder comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub version: u32,
    pub id: uuid::Uuid,
    pub step_id: StepId,
    pub job_id: JobId,
    pub generation: i64,
    pub holder: String,
    pub workflow_kind: String,
    pub workflow_version: i32,
    pub bill: crate::resources::ResourceBill,
    pub allocations: Vec<Allocation>,
    pub observed: Vec<Demand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl Claim {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.version, 1 | 2)
            || self.id.is_nil()
            || self.generation < 1
            || self.holder.is_empty()
            || self.workflow_version < 1
            || self.workflow_kind.is_empty()
            || self.allocations.is_empty()
            || self.allocations.len() > 4
        {
            return Err("invalid resource acquisition identity/vector".into());
        }
        if self.version == 2 {
            let (Some(at), Some(duration), Some(until)) =
                (self.claimed_at, self.resolved_duration_ms, self.lease_until)
            else {
                return Err("leased acquisition lacks its frozen clock/duration".into());
            };
            if duration < 0
                || duration
                    .checked_mul(2)
                    .and_then(chrono::Duration::try_milliseconds)
                    .and_then(|d| at.checked_add_signed(d))
                    != Some(until)
            {
                return Err("resource lease differs from its frozen duration".into());
            }
        } else if self.claimed_at.is_some()
            || self.resolved_duration_ms.is_some()
            || self.lease_until.is_some()
        {
            return Err("legacy resource acquisition cannot carry new lease fields".into());
        }
        self.bill.validate()?;
        // A claim records the class it READ off the run, never the
        // sentinel: what it holds must be re-derivable from the token.
        if self.bill.takes_model_from_run() {
            return Err("an acquisition's bill must name the model class it resolved".into());
        }
        let mut asked = self
            .bill
            .pools()
            .into_iter()
            .map(|(pool, quantity)| quantity.amount().map(|amount| (pool, amount)))
            .collect::<Result<Vec<_>, _>>()?;
        asked.sort_by(|a, b| a.0.cmp(&b.0));
        let observed: Vec<_> = self
            .observed
            .iter()
            .map(|d| (d.pool.clone(), d.amount.clone()))
            .collect();
        if asked != observed {
            return Err("resource decision vector differs from the pinned protocol bill".into());
        }
        if plan(&self.observed).map_err(|e| e.to_string())? != self.allocations {
            return Err("resource allocation disagrees with its complete decision vector".into());
        }
        let mut previous: Option<&str> = None;
        for row in &self.allocations {
            if row.amount.is_zero()
                || row.version < 1
                || previous.is_some_and(|p| p >= row.pool.as_str())
            {
                return Err(
                    "resource vector must be positive, versioned, unique and ordered".into(),
                );
            }
            previous = Some(&row.pool);
        }
        Ok(())
    }
}

/// THE SLOT IS THE RUN'S OWN MODEL'S (backlog af5dd547, allocation order
/// item 2; design 012822ea, conductor decision 20261106_1507Z (3)). A
/// bill whose `model` is [`crate::resources::MODEL_FROM_RUN`] is resolved
/// here, at the claim, against the billed packet's own `model` (`job` is
/// that packet's metadata): the class is the one ACTIVE `robots:`
/// pool whose `models` lists that model exactly. Nothing is parsed out of
/// a model id (conductor decision 20261104T0939) and nothing is guessed:
/// a run that declares no model, a model no pool lists, and a model two
/// pools list are each refused by name. Any other bill returns as it is.
///
/// THE CLAIMANT'S CLASS COMES FIRST (backlog 7750d37d). A slot meters the
/// robot that is working, and the run's `model` is what the step DECLARED:
/// on 2026-11-08 agent-codex (agents row `model_class = "sol"`) held two
/// `robots:opus` slots for reviewer runs declaring opus-5-5 while
/// `robots:sol` sat idle. So `claimant_class`, the class the claiming
/// actor's agents row records, decides; it must name an ACTIVE pool or the
/// claim is refused by name, never re-routed. The run's model is read only
/// for a claimant that records no class.
/// Returns the bill the claim records, and the model it was read from
/// (`None` when the claimant's class decided).
pub(crate) fn resolve_bill<'a>(
    bill: &crate::resources::ResourceBill,
    job: Option<&serde_json::Value>,
    claimant_class: Option<&str>,
    pools: impl IntoIterator<Item = &'a crate::pools::PoolSpec>,
) -> Result<(crate::resources::ResourceBill, Option<String>), Refusal> {
    use crate::resources::{MODEL_FROM_RUN, ROBOTS_PREFIX, RUN_MODEL_KEY, robot_pool};
    if !bill.takes_model_from_run() {
        return Ok((bill.clone(), None));
    }
    let refuse = |reason: &str| Refusal {
        pool: robot_pool(MODEL_FROM_RUN),
        reason: reason.into(),
        observed: Vec::new(),
    };
    let active: Vec<&crate::pools::PoolSpec> = pools
        .into_iter()
        .filter(|pool| pool.status == crate::registry::WorkflowStatus::Active)
        .collect();
    if let Some(class) = claimant_class {
        let pool = robot_pool(class);
        return if active.iter().any(|p| p.name == pool) {
            Ok((bill.with_model_class(class), None))
        } else {
            Err(Refusal {
                pool,
                reason: "claimant-class-has-no-pool".into(),
                observed: Vec::new(),
            })
        };
    }
    let model = job
        .and_then(|metadata| metadata.get(RUN_MODEL_KEY))
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| refuse("run-declares-no-model"))?;
    let mut classes: Vec<&str> = active
        .into_iter()
        .filter(|pool| pool.models.iter().any(|served| served == model))
        .filter_map(|pool| pool.name.strip_prefix(ROBOTS_PREFIX))
        .collect();
    classes.sort_unstable();
    classes.dedup();
    match classes.as_slice() {
        [class] => Ok((bill.with_model_class(class), Some(model.into()))),
        [] => Err(refuse("run-model-has-no-class")),
        _ => Err(refuse("run-model-has-two-classes")),
    }
}

/// Resolve the pinned protocol duration, falling back only to the existing
/// kind registry. Unknown duration is a refusal, never a fabricated lease.
pub(crate) fn resolved_duration_ms(
    workflow: &crate::registry::WorkflowSpec,
    step: &boss_core::job::Step,
) -> Result<i64, crate::port::JobsError> {
    let pinned = workflow
        .steps
        .iter()
        .find(|s| Some(s.title.as_str()) == step.spec_slug.as_deref())
        .ok_or_else(|| {
            crate::port::JobsError::Storage(
                "resource duration has no pinned step specification".into(),
            )
        })?;
    let authored = pinned.wall_clock_hours.or(pinned.duration_hours);
    let registry = crate::step_registry::StepRegistry::v1();
    let hours = authored
        .or_else(|| {
            registry
                .get(&pinned.kind)
                .and_then(|kind| kind.typical_duration_hours)
        })
        .ok_or_else(|| {
            crate::port::JobsError::ResourceRefused(Refusal {
                pool: "acquisition".into(),
                reason: "duration-unknown".into(),
                observed: Vec::new(),
            })
        })?;
    let ms = hours * 3_600_000.0;
    if !ms.is_finite() || ms < 0.0 || ms >= i64::MAX as f64 / 2.0 {
        return Err(crate::port::JobsError::Storage(
            "resource duration is outside the representable lease bound".into(),
        ));
    }
    Ok(ms.ceil() as i64)
}

pub(crate) fn lease_until(
    at: chrono::DateTime<chrono::Utc>,
    duration: i64,
) -> Result<chrono::DateTime<chrono::Utc>, crate::port::JobsError> {
    duration
        .checked_mul(2)
        .and_then(chrono::Duration::try_milliseconds)
        .and_then(|d| at.checked_add_signed(d))
        .ok_or_else(|| crate::port::JobsError::Storage("resource lease instant overflow".into()))
}

pub(crate) fn preserve_token(
    current: &serde_json::Value,
    proposed: &serde_json::Value,
) -> Result<(), crate::port::JobsError> {
    if current.get(TOKEN_KEY) != proposed.get(TOKEN_KEY) {
        return Err(crate::port::JobsError::ImmutableMetadata(
            "resource acquisition token is server-owned; re-read its generation before an exit"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn state_events(
    events: &[boss_core::event::Event],
    step: &boss_core::job::Step,
    stamp: &boss_core::publisher::EventStamp,
) -> Result<Vec<boss_core::event::Event>, crate::port::JobsError> {
    let mut result = Vec::new();
    let mut found = false;
    for original in events {
        let mut event = original.clone();
        if event.kind == crate::events::STEP_UPDATED {
            found = true;
            event.payload = serde_json::to_value(step)
                .map_err(|e| crate::port::JobsError::Storage(e.to_string()))?;
            if let Some(envelope) = original.payload.as_object() {
                // Resolution describes the claim, not the step row. Keep
                // its two declared keys when adding the resource token
                // (review a480ed50 B1), just like the station envelope.
                for (key, value) in envelope.iter().filter(|(key, _)| {
                    key.starts_with('_')
                        || matches!(
                            key.as_str(),
                            crate::events::STATION_RESOLUTION_KEY
                                | crate::events::STATION_RESOLUTION_REASON_KEY
                        )
                }) {
                    event.payload[key] = value.clone();
                }
            }
        }
        result.push(event);
    }
    if !found {
        result.push(stamp.event(crate::events::STEP_UPDATED, serde_json::json!(step)));
    }
    Ok(result)
}

/// The step metadata key a refused claim leaves its wait under.
pub const WAITING_KEY: &str = "waiting_on";
/// Set inside a waiting step's `waiting_on` by the re-offer rule when its
/// pool frees (`jobs.reoffer_waiting`, backlog 587f395a), with the
/// release that freed it under [`REOFFERED_BY`], and dropped by the
/// step's next refusal. A re-offered step is offered by claim-next again;
/// one without it holds its place in the line and is passed over.
pub const REOFFERED_AT: &str = "reoffered_at";
pub const REOFFERED_BY: &str = "reoffered_by";

/// The one refusal a release cures, so the one that stands in a line.
/// An unknown capacity or duration is a declaration to repair: no
/// release will ever re-offer it, so it must not be passed over waiting
/// for one.
pub const CAPACITY_EXHAUSTED: &str = "capacity-exhausted";

/// Whether a `waiting_on` value is a place in a pool's line: the
/// resource shape, refused for exhaustion. A wait on a packet or a
/// person (`step_wait`) is not a pool's line.
fn in_line(wait: &serde_json::Value) -> bool {
    wait.get("resource")
        .is_some_and(serde_json::Value::is_string)
        && wait.get("reason").and_then(serde_json::Value::as_str) == Some(CAPACITY_EXHAUSTED)
}

/// Whether `metadata` holds a place in a pool's waiting line that no
/// release has re-offered yet.
pub fn holds_its_place(metadata: &serde_json::Value) -> bool {
    metadata.get(WAITING_KEY).is_some_and(|wait| {
        in_line(wait)
            && wait
                .get(REOFFERED_AT)
                .is_none_or(serde_json::Value::is_null)
    })
}

/// One ready step in a pool's waiting line, as `GET /api/claims/waiting`
/// states it (backlog 587f395a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waiting {
    pub position: usize,
    pub job_id: JobId,
    pub step_id: StepId,
    pub assignee_id: Option<String>,
    /// The step's `metadata.waiting_on` exactly as stored: what a
    /// re-offer judges its write against.
    pub waiting_on: serde_json::Value,
}

/// One row an adapter read for a pool's line, before it is ordered.
pub type WaitingRow = (JobId, StepId, Option<String>, serde_json::Value);

/// The line, in order: the step that began waiting first is first, and
/// the step id breaks a tie, so both adapters order one set of rows the
/// same way. A wait recorded before `since` existed has no start to read
/// and stands ahead of every dated one — it was refused before any of
/// them. Positions count from 1. Rows whose wait is not exhaustion are
/// not in the line (see [`CAPACITY_EXHAUSTED`]).
pub fn line(rows: Vec<WaitingRow>) -> Vec<Waiting> {
    let mut rows: Vec<_> = rows.into_iter().filter(|row| in_line(&row.3)).collect();
    let since = |wait: &serde_json::Value| {
        wait.get("since")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|at| at.with_timezone(&chrono::Utc))
    };
    rows.sort_by(|a, b| {
        since(&a.3)
            .cmp(&since(&b.3))
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    rows.into_iter()
        .enumerate()
        .map(
            |(index, (job_id, step_id, assignee_id, waiting_on))| Waiting {
                position: index + 1,
                job_id,
                step_id,
                assignee_id,
                waiting_on,
            },
        )
        .collect()
}

/// What a claim refused on `pool` answers about where its step now
/// stands: its position in that pool's line, or why the line could not
/// be read. Never omitted, because an absent position would read as
/// "not queued" (no evidence is not a pass).
pub fn queued_answer(
    pool: &str,
    step: &StepId,
    line: Result<Vec<Waiting>, String>,
) -> serde_json::Value {
    match line {
        Ok(line) => {
            let position = line.iter().find(|w| w.step_id == *step).map(|w| w.position);
            serde_json::json!({
                "pool": pool,
                "position": position,
                "waiting": line.len(),
                "state": if position.is_some() { "queued" } else { "not-in-line" },
            })
        }
        Err(why) => serde_json::json!({
            "pool": pool,
            "position": null,
            "state": "unread",
            "unread": why,
        }),
    }
}

pub(crate) fn refused_step(
    step: &boss_core::job::Step,
    reason: &Refusal,
    stamp: &boss_core::publisher::EventStamp,
) -> Result<(boss_core::job::Step, Vec<boss_core::event::Event>), crate::port::JobsError> {
    let mut refused = step.clone();
    // A step refused again on the SAME pool keeps the instant it began
    // waiting, so its place in that pool's line survives a re-offer it
    // lost (backlog 587f395a); a refusal on another pool starts a new
    // wait. Rebuilt whole, so a re-offer's mark goes with the refusal.
    let since = step
        .metadata
        .get(WAITING_KEY)
        .filter(|wait| {
            wait.get("resource").and_then(serde_json::Value::as_str) == Some(reason.pool.as_str())
        })
        .and_then(|wait| wait.get("since"))
        .filter(|since| since.is_string())
        .cloned()
        .unwrap_or_else(|| serde_json::json!(stamp.timestamp));
    refused
        .metadata
        .as_object_mut()
        .ok_or_else(|| crate::port::JobsError::Storage("step metadata is not an object".into()))?
        .insert(
            WAITING_KEY.into(),
            serde_json::json!({"resource":reason.pool,"reason":reason.reason,"since":since}),
        );
    let invalidated = crate::events::void_stamps_if_moved(stamp, &step.shape_hash(), &mut refused);
    let mut facts=vec![stamp.event(crate::events::STEP_UPDATED,serde_json::json!(refused)),stamp.event(REFUSED,serde_json::json!({"job_id":step.job_id,"step_id":step.id,"reason":reason,"step":refused}))];
    facts.extend(invalidated);
    Ok((refused, facts))
}

pub(crate) fn reserved_event(
    claim: &Claim,
    step: &boss_core::job::Step,
    stamp: &boss_core::publisher::EventStamp,
) -> boss_core::event::Event {
    stamp.event(
        RESERVED,
        serde_json::json!({"job_id":step.job_id,"step_id":step.id,"claim":claim,"step":step}),
    )
}

/// PostgreSQL's timestamp column stores microseconds, whereas a caller may
/// carry nanoseconds. Canonicalize ONLY that documented storage encoding;
/// every other field must equal the row actually written. Preserve authentic
/// event identity and enrichment while copying the observed row, not a guess.
#[cfg(feature = "postgres")]
pub(crate) fn stored_exit_events(
    events: &[boss_core::event::Event],
    proposed: &boss_core::job::Step,
    stored: &boss_core::job::Step,
) -> Result<Vec<boss_core::event::Event>, crate::port::JobsError> {
    let mut encoded = proposed.clone();
    encoded.completed_at = encoded
        .completed_at
        .map(|at| {
            chrono::DateTime::from_timestamp_micros(at.timestamp_micros()).ok_or_else(|| {
                crate::port::JobsError::Storage("completion timestamp cannot be encoded".into())
            })
        })
        .transpose()?;
    if encoded != *stored {
        return Err(crate::port::JobsError::ImmutableMetadata(
            "resource exit candidate differs from the actual stored step".into(),
        ));
    }
    events
        .iter()
        .map(|original| {
            let mut event = original.clone();
            if event.kind == crate::events::STEP_UPDATED {
                let recorded: boss_core::job::Step = serde_json::from_value(event.payload.clone())
                    .map_err(|error| crate::port::JobsError::Storage(error.to_string()))?;
                if recorded != *proposed {
                    return Err(crate::port::JobsError::ImmutableMetadata(
                        "resource exit event differs from its proposed state".into(),
                    ));
                }
                event.payload = serde_json::to_value(stored)
                    .map_err(|error| crate::port::JobsError::Storage(error.to_string()))?;
                if let Some(fields) = original.payload.as_object() {
                    for (key, value) in fields.iter().filter(|(key, _)| key.starts_with('_')) {
                        event.payload[key] = value.clone();
                    }
                }
            }
            Ok(event)
        })
        .collect()
}

pub(crate) fn exit_event(
    claim: &Claim,
    step: &boss_core::job::Step,
    events: &[boss_core::event::Event],
    job_cancelled: bool,
) -> Result<boss_core::event::Event, crate::port::JobsError> {
    let cause = events
        .iter()
        .find(|event| {
            event.source == "jobs"
                && event.payload.get("_actor").is_some()
                && if job_cancelled {
                    event.kind == crate::events::JOB_UPDATED
                        && event.payload["id"] == serde_json::json!(step.job_id)
                        && matches!(
                            event.payload["status"].as_str(),
                            Some("cancelled" | "closed")
                        )
                } else {
                    event.kind == crate::events::STEP_UPDATED
                        && serde_json::from_value::<boss_core::job::Step>(event.payload.clone())
                            .is_ok_and(|recorded| recorded == *step)
                }
        })
        .ok_or_else(|| {
            crate::port::JobsError::Storage(
                "resource exit requires its attributed state event in the same transaction".into(),
            )
        })?;
    let reason = if job_cancelled {
        if cause.payload["status"] == "closed" {
            "job-closed"
        } else {
            "job-cancelled"
        }
    } else {
        match step.status {
            boss_core::job::StepStatus::Completed => "completed",
            boss_core::job::StepStatus::Skipped => "skipped",
            _ => "released",
        }
    };
    let mut payload = serde_json::json!({"job_id":step.job_id,"step_id":step.id,"claim":claim,"step":step,"reason":reason,"cause_event_id":cause.id});
    if let Some(envelope) = cause.payload.as_object() {
        for (key, value) in envelope.iter().filter(|(key, _)| key.starts_with('_')) {
            payload[key] = value.clone();
        }
    }
    Ok(boss_core::event::Event::new(
        "jobs",
        RELEASED,
        payload,
        cause.timestamp,
    ))
}

/// The payload key a release carries when its exit came after the
/// claim's lease ran out and before the sweep reached it.
pub const LEASE_EXPIRED_KEY: &str = "lease_expired";

/// Mark `release` when the exit it records came after `claim`'s lease.
///
/// AN EXPIRED LEASE NO LONGER STRANDS ITS STEP (backlog ad1c3af2,
/// measured 2026-11-09 19:20Z). The exit used to refuse
/// `resource-lease-expired`, before the sweep and after it, and nothing
/// else ends an Active step: runs 056cc3d1 and d659981f stood at
/// `building` 19 h after their cars landed, the merged-car rule, a
/// claim-id PUT and `boss step release` all refused. A lapsed lease
/// fences the claim's RENEWAL (a retry, an identity binding), not its
/// end — ending it gives back what the lease held either way. The exit
/// still passes every check a live claim's does (holder, generation,
/// the original claim id at the door), so a live claim gains no way
/// around it; this key keeps the record from reading as an exit inside
/// the lease.
pub(crate) fn note_lapsed_lease(claim: &Claim, release: &mut boss_core::event::Event) {
    if claim
        .lease_until
        .is_some_and(|until| until <= release.timestamp)
    {
        release.payload[LEASE_EXPIRED_KEY] = serde_json::json!(true);
    }
}

pub(crate) fn exit_required(
    claim: &Claim,
    current: &boss_core::job::Step,
    next: &boss_core::job::Step,
    job_cancelled: bool,
) -> Result<bool, crate::port::JobsError> {
    preserve_token(&current.metadata, &next.metadata)?;
    if current.metadata.get(TOKEN_KEY) != Some(&serde_json::json!(claim))
        || current.id != claim.step_id
        || current.job_id != claim.job_id
        || current.assignee_id.as_deref() != Some(&claim.holder)
    {
        return Err(crate::port::JobsError::ImmutableMetadata(
            "resource generation/holder differs from the stored acquisition".into(),
        ));
    }
    if next.status == boss_core::job::StepStatus::Active && next.assignee_id != current.assignee_id
    {
        return Err(crate::port::JobsError::ImmutableMetadata(
            "an ACTIVE holder cannot be replaced outside the claim door".into(),
        ));
    }
    Ok(job_cancelled || next.status != boss_core::job::StepStatus::Active)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(Decimal);

impl Amount {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 64
            || value.bytes().filter(|byte| *byte == b'.').count() > 1
            || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err("amount must be a bounded nonnegative exact decimal".into());
        }
        let encoded = if value.contains('.') {
            value.trim_end_matches('0').trim_end_matches('.')
        } else {
            value
        };
        Decimal::from_str_exact(encoded)
            .map(Self)
            .map_err(|e| format!("unrepresentable exact amount: {e}"))
    }

    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }
    /// Sum the entire exact coefficient set before checking the final value's
    /// bounded Decimal representation. Pairwise rounding or row-order-dependent
    /// intermediate refusals are not an aggregate. No public precision changes.
    pub(crate) fn sum<'a>(values: impl IntoIterator<Item = &'a Self>) -> Result<Self, String> {
        let coefficient = values
            .into_iter()
            .fold(num_bigint::BigInt::from(0), |total, value| {
                let normalized = value.0.normalize();
                total
                    + num_bigint::BigInt::from(normalized.mantissa())
                        * num_bigint::BigInt::from(10).pow(28 - normalized.scale())
            });
        let mut digits = coefficient.to_string();
        if digits.len() <= 28 {
            digits = format!("{}{}", "0".repeat(29 - digits.len()), digits);
        }
        let split = digits.len() - 28;
        Self::parse(&format!("{}.{}", &digits[..split], &digits[split..]))
    }
    pub fn checked_add(&self, other: &Self) -> Result<Self, String> {
        let (a, b, scale) = self.aligned(other)?;
        Self::from_coefficient(a.checked_add(b).ok_or("exact amount overflow")?, scale)
    }
    pub fn checked_sub(&self, other: &Self) -> Result<Self, String> {
        let (a, b, scale) = self.aligned(other)?;
        let value = a
            .checked_sub(b)
            .filter(|v| *v >= 0)
            .ok_or("exact amount underflow")?;
        Self::from_coefficient(value, scale)
    }
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }
    fn aligned(&self, other: &Self) -> Result<(i128, i128, u32), String> {
        let a = self.0.normalize();
        let b = other.0.normalize();
        let scale = a.scale().max(b.scale());
        let expand = |value: Decimal| {
            value
                .mantissa()
                .checked_mul(10_i128.pow(scale - value.scale()))
                .ok_or_else(|| "unrepresentable exact amount arithmetic".to_owned())
        };
        Ok((expand(a)?, expand(b)?, scale))
    }
    fn from_coefficient(mut value: i128, mut scale: u32) -> Result<Self, String> {
        while scale > 0 && value % 10 == 0 {
            value /= 10;
            scale -= 1;
        }
        Decimal::try_from_i128_with_scale(value, scale)
            .map(Self)
            .map_err(|e| format!("unrepresentable exact amount arithmetic: {e}"))
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.normalize().fmt(f)
    }
}
impl Serialize for Amount {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "amount", rename_all = "kebab-case")]
pub enum Capacity {
    #[serde(rename = "configured")]
    Known(Amount),
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Demand {
    pub pool: String,
    pub version: i32,
    pub amount: Amount,
    pub capacity: Capacity,
    pub held: Amount,
    pub stock: bool,
    /// The station share this demand draws within the pool, when it draws
    /// one: a run's vCPU on `host:<node>` is bounded by its owner's
    /// FLOATING share of the box, not the box (backlog 5608c1a2). Recorded
    /// on the decision, so the refusal and the claim name the numbers the
    /// station rows said at the claim. `capacity` stays the pool's own,
    /// the number the replay holds every decision to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<Share>,
}
impl Demand {
    pub fn flow(
        pool: &str,
        version: i32,
        amount: Amount,
        capacity: Capacity,
        held: Amount,
    ) -> Self {
        Self {
            pool: pool.into(),
            version,
            amount,
            capacity,
            held,
            stock: false,
            share: None,
        }
    }
}

/// What a station floats of one host pool: what it holds of the box,
/// less what each station standing in it reserves of the same box
/// (docs/design/station-tree.md §3: "A child's reserved allocation is
/// subtracted from its parent's floating capacity").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub station: String,
    pub allocated: Amount,
    /// Each child's reservation of the same pool, by the child's name.
    pub reserved: std::collections::BTreeMap<String, Amount>,
}

impl Share {
    pub fn floating(&self) -> Result<Amount, String> {
        self.allocated
            .checked_sub(&Amount::sum(self.reserved.values())?)
            .map_err(|_| {
                format!(
                    "the stations under {} reserve more than it holds",
                    self.station
                )
            })
    }
}

/// The floating share of every host pool the station whose `id` is
/// `owner` allocates — the station that owns the run's protocol, so the
/// one whose capacity executes it (agent-run names engineering). Ordered
/// by pool name; empty when no tree row has that id or it holds no host.
pub fn floating_shares(
    stations: &[crate::stations::StationSpec],
    owner: &str,
) -> Result<Vec<(String, Share)>, String> {
    let rows = crate::station_tree::tree_rows(stations);
    let Some(station) = rows.iter().find(|r| r.id.as_deref() == Some(owner)) else {
        return Ok(Vec::new());
    };
    station
        .allocation
        .iter()
        .filter(|(pool, _)| crate::station_tree::host_of(pool).is_some())
        .map(|(pool, held)| {
            let reserved = rows
                .iter()
                .filter(|child| child.parent.as_deref() == Some(owner))
                .filter_map(|child| child.allocation.get(pool).map(|q| (child, q)))
                .map(|(child, q)| Ok((child.name.clone(), q.amount()?)))
                .collect::<Result<_, String>>()?;
            Ok((
                pool.clone(),
                Share {
                    station: station.name.clone(),
                    allocated: held.amount()?,
                    reserved,
                },
            ))
        })
        .collect()
}

/// A RUN THAT BINDS NO WORKER IS BILLED NO vCPU (backlog db88f6a9).
/// Measured 2026-11-12 15:08Z on GET /api/claims: host:w-1 bound, six
/// claims of 2.5 held 15 of engineering's 16 floating, two of them triage
/// analyst runs that bind no dev-build worker and run no cargo, while a
/// builder's run was refused 'host:w-1 exhausted'. The agent-run row is
/// one bill for every profile, so it names the profiles that never bind a
/// worker, and the claim reads the billed packet's `profile` (`job` is
/// that packet's metadata) against it, before the host is placed: a
/// listed profile's bill drops its `vcpu`. Every bill is returned with
/// the list consumed, so a claim records what it drew and never the rule;
/// a packet with no profile, or none at all, keeps its vCPU.
pub(crate) fn waive_workerless(
    bill: &crate::resources::ResourceBill,
    job: Option<&serde_json::Value>,
) -> crate::resources::ResourceBill {
    let profile = job
        .and_then(|metadata| metadata.get(crate::resources::RUN_PROFILE_KEY))
        .and_then(serde_json::Value::as_str);
    let waived = profile.is_some_and(|p| bill.workerless_profiles.iter().any(|w| w == p));
    crate::resources::ResourceBill {
        vcpu: if waived { None } else { bill.vcpu.clone() },
        workerless_profiles: Vec::new(),
        ..bill.clone()
    }
}

/// Whether the claim must place `bill` on a host before it plans: a
/// from-run bill billing vCPU that names no host yet. Only then does an
/// adapter read the station rows.
pub(crate) fn draws_a_host(bill: &crate::resources::ResourceBill) -> bool {
    bill.takes_model_from_run() && bill.vcpu.is_some() && bill.host.is_none()
}

/// The share a demand on `pool` draws: the one `bill.host` names when
/// `pool` is that host's pool, else none. A bill naming a host whose
/// share is not among `shares` is refused on the pool, never planned
/// against the whole box.
pub(crate) fn share_of(
    bill: &crate::resources::ResourceBill,
    pool: &str,
    shares: &[(String, Share)],
) -> Result<Option<Share>, Refusal> {
    let Some(host) = &bill.host else {
        return Ok(None);
    };
    if crate::station_tree::host_of(pool) != Some(host.node.as_str()) {
        return Ok(None);
    }
    shares
        .iter()
        .find(|(p, s)| p == pool && s.station == host.station)
        .map(|(_, s)| Some(s.clone()))
        .ok_or_else(|| Refusal {
            pool: pool.into(),
            reason: "host-share-unknown".into(),
            observed: Vec::new(),
        })
}

/// THE RUN'S vCPU IS DRAWN FROM A HOST (backlog 5608c1a2). Measured
/// 2026-11-08 10:05-10:42Z: a Codex reviewer's worker waited 300 s on the
/// scheduler for w-1's CPU and memory while robots:opus held 4 of 6 —
/// the robot slot was admitted and no box had been asked. A from-run bill
/// that bills `vcpu` therefore names the box at the claim: the host pool,
/// among those its owner floats a share of (`shares`, from
/// [`floating_shares`]), with the most room left — share less what
/// `held` reads held on the pool — and the lower node name on a tie, so
/// both adapters choose alike. The draw is then bounded by that share in
/// [`plan`]. Any other bill returns as it is; an owner that floats no
/// host is refused by name, never placed on the root's slack.
pub(crate) fn resolve_host(
    bill: &crate::resources::ResourceBill,
    shares: &[(String, Share)],
    held: impl Fn(&str) -> Result<Amount, String>,
) -> Result<crate::resources::ResourceBill, Refusal> {
    if !draws_a_host(bill) {
        return Ok(bill.clone());
    }
    let unrepresentable = |why: String| Refusal {
        pool: "host".into(),
        reason: format!("host-share-unreadable: {why}"),
        observed: Vec::new(),
    };
    let mut best: Option<(Amount, &String, &Share)> = None;
    for (pool, share) in shares {
        let floating = share.floating().map_err(unrepresentable)?;
        let room = floating
            .checked_sub(&held(pool).map_err(unrepresentable)?)
            .unwrap_or_else(|_| Amount::zero());
        // Shares arrive in pool order, so keeping the first of equals
        // keeps the lower node name.
        if best.as_ref().is_none_or(|(most, _, _)| room > *most) {
            best = Some((room, pool, share));
        }
    }
    let (_, pool, share) = best.ok_or_else(|| Refusal {
        pool: "host".into(),
        reason: "owner-floats-no-host".into(),
        observed: Vec::new(),
    })?;
    Ok(crate::resources::ResourceBill {
        host: crate::station_tree::host_of(pool).map(|node| crate::resources::HostDraw {
            node: node.into(),
            station: share.station.clone(),
        }),
        ..bill.clone()
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub pool: String,
    pub version: i32,
    pub amount: Amount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub pool: String,
    pub reason: String,
    pub observed: Vec<Demand>,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.pool, self.reason)
    }
}

impl Refusal {
    /// The numbers behind an exhausted station share, as one sentence a
    /// dispatcher reads instead of a worker's scheduling timeout (backlog
    /// 5608c1a2): what the station holds of the box, what each child
    /// reserves, what floats, what is held and what was asked. `None`
    /// for any other refusal, whose pool and reason already say it all.
    pub fn sentence(&self) -> Option<String> {
        if self.reason != CAPACITY_EXHAUSTED {
            return None;
        }
        let demand = self.observed.iter().find(|d| d.pool == self.pool)?;
        let share = demand.share.as_ref()?;
        let reserved: String = share
            .reserved
            .iter()
            .map(|(child, amount)| format!(", {amount} to {child}"))
            .collect();
        Some(format!(
            "{} exhausted: {} allocated to {}{reserved}, {} floating; {} held by running claims, \
             this claim asks {}",
            self.pool,
            share.allocated,
            share.station,
            share.floating().ok()?,
            demand.held,
            demand.amount
        ))
    }
}

pub fn plan(demands: &[Demand]) -> Result<Vec<Allocation>, Refusal> {
    if demands.len() > 4 {
        return Err(Refusal {
            pool: "vector".into(),
            reason: "resource-vector-too-wide".into(),
            observed: Vec::new(),
        });
    }
    let mut ordered: Vec<_> = demands.iter().collect();
    ordered.sort_by(|a, b| a.pool.cmp(&b.pool));
    let observed: Vec<_> = ordered.iter().map(|d| (*d).clone()).collect();
    let mut result = Vec::new();
    let mut previous = None;
    for demand in ordered {
        let reason = if previous == Some(&demand.pool) {
            Some("duplicate-pool")
        } else if demand.amount.is_zero() {
            None
        } else if demand.version < 1 {
            Some("pool-version-unknown")
        } else {
            // A share bounds the draw below the pool's own capacity: the
            // box may have room that is another station's (backlog 5608c1a2).
            let floating = demand.share.as_ref().map(Share::floating);
            match (&demand.capacity, floating) {
                (Capacity::Unknown, _) => Some("capacity-unknown"),
                (_, Some(Err(_))) => Some("share-unrepresentable"),
                (Capacity::Known(cap), floating) => match demand.held.checked_add(&demand.amount) {
                    Ok(total)
                        if total <= *cap
                            && floating
                                .as_ref()
                                .is_none_or(|f| f.as_ref().is_ok_and(|f| total <= *f)) =>
                    {
                        None
                    }
                    Ok(_) => Some(CAPACITY_EXHAUSTED),
                    Err(_) => Some("quantity-unrepresentable"),
                },
            }
        };
        if let Some(reason) = reason {
            return Err(Refusal {
                pool: demand.pool.clone(),
                reason: reason.into(),
                observed,
            });
        }
        previous = Some(&demand.pool);
        if !demand.amount.is_zero() {
            result.push(Allocation {
                pool: demand.pool.clone(),
                version: demand.version,
                amount: demand.amount.clone(),
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod exit_tests {
    use super::{EXIT_CLAIM_KEY, exiting};
    use serde_json::json;

    #[test]
    fn an_exit_names_the_claim_the_step_held_and_only_then() {
        let claimed = json!({"metadata": {"resource_claim": {"id": "c-1"}, "result": "x"}});
        assert_eq!(
            exiting(&claimed, json!({"status": "completed"})),
            json!({"status": "completed", EXIT_CLAIM_KEY: "c-1"})
        );
        for unclaimed in [
            json!({"metadata": {}}),
            json!({}),
            json!({"metadata": {"resource_claim": {}}}),
        ] {
            assert_eq!(
                exiting(&unclaimed, json!({"status": "ready"})),
                json!({"status": "ready"}),
                "{unclaimed}"
            );
        }
    }
}

#[cfg(test)]
mod from_run_tests {
    //! The pure half of a from-run bill (backlog af5dd547); the claim
    //! itself is pinned through the router on both adapters in
    //! tests/flow_resource_claims.rs.
    use super::resolve_bill;
    use crate::pools::PoolSpec;
    use crate::resources::ResourceBill;
    use serde_json::json;

    fn pool(name: &str, status: &str, models: &[&str]) -> PoolSpec {
        serde_json::from_value(
            json!({"name":name,"version":1,"status":status,"unit":"slot",
            "kind":"flow","capacity":{"state":"unknown"},"models":models}),
        )
        .unwrap()
    }
    fn from_run() -> ResourceBill {
        serde_json::from_value(json!({"model":"from-run","vcpu":"1"})).unwrap()
    }

    #[test]
    fn the_class_is_the_one_active_pool_that_lists_the_runs_model() {
        let pools = [
            pool("robots:opus", "active", &["opus-5-5"]),
            pool("robots:sol", "active", &["gpt-6.1-sol"]),
            // A retired row's list is not the registry's answer.
            pool("robots:fable", "retired", &["opus-5-5"]),
        ];
        let (bill, model) = resolve_bill(
            &from_run(),
            Some(&json!({"model":"opus-5-5"})),
            None,
            &pools,
        )
        .unwrap();
        assert_eq!(bill.model.as_deref(), Some("opus"));
        assert_eq!(bill.vcpu, from_run().vcpu, "the rest of the bill is kept");
        assert_eq!(model.as_deref(), Some("opus-5-5"));
    }

    #[test]
    fn no_class_is_guessed_and_two_classes_are_refused() {
        let two = [
            pool("robots:opus", "active", &["opus-5-5"]),
            pool("robots:fable", "active", &["opus-5-5"]),
        ];
        for (job, pools, reason) in [
            (None, &two[..1], "run-declares-no-model"),
            (
                Some(json!({"model":null})),
                &two[..1],
                "run-declares-no-model",
            ),
            (
                Some(json!({"model":" "})),
                &two[..1],
                "run-declares-no-model",
            ),
            // `opus-5-5[1m]` is not `opus-5-5`: exact ids, never a parse.
            (
                Some(json!({"model":"opus-5-5[1m]"})),
                &two[..1],
                "run-model-has-no-class",
            ),
            (
                Some(json!({"model":"opus-5-5"})),
                &two[..],
                "run-model-has-two-classes",
            ),
        ] {
            let refused = resolve_bill(&from_run(), job.as_ref(), None, pools).unwrap_err();
            assert_eq!(refused.pool, "robots:from-run");
            assert_eq!(refused.reason, reason, "{job:?}");
        }
    }

    #[test]
    fn the_claimants_class_decides_and_the_runs_model_is_its_fallback() {
        let pools = [
            pool("robots:opus", "active", &["opus-5-5"]),
            pool("robots:sol", "active", &["gpt-6.1-sol"]),
            pool("robots:fable", "retired", &[]),
        ];
        let run = json!({"model":"opus-5-5"});
        // agent-codex records sol: its opus-declared run holds a sol slot.
        let (bill, model) = resolve_bill(&from_run(), Some(&run), Some("sol"), &pools).unwrap();
        assert_eq!(bill.model.as_deref(), Some("sol"));
        assert!(model.is_none(), "no model was read off the run");
        // Even a run that declares nothing: the class is the executor's.
        let (bill, _) = resolve_bill(&from_run(), None, Some("opus"), &pools).unwrap();
        assert_eq!(bill.model.as_deref(), Some("opus"));
        // A class whose pool is not active is refused by name, not re-routed.
        let refused = resolve_bill(&from_run(), Some(&run), Some("fable"), &pools).unwrap_err();
        assert_eq!(refused.pool, "robots:fable");
        assert_eq!(refused.reason, "claimant-class-has-no-pool");
        // No class recorded: the declared model decides.
        let (bill, _) = resolve_bill(&from_run(), Some(&run), None, &pools).unwrap();
        assert_eq!(bill.model.as_deref(), Some("opus"));
    }

    #[test]
    fn a_fixed_bill_reads_nothing_off_the_run() {
        let fixed: ResourceBill = serde_json::from_value(json!({"model":"opus"})).unwrap();
        let (bill, model) = resolve_bill(&fixed, None, Some("sol"), &[]).unwrap();
        assert_eq!(bill, fixed);
        assert!(model.is_none());
    }
}

#[cfg(test)]
mod host_tests {
    //! A run's vCPU is drawn from a HOST, out of the floating share of the
    //! station that owns the run's protocol (backlog 5608c1a2,
    //! station-tree.md §3). The claim itself is pinned through the router
    //! on both adapters in tests/flow_resource_claims.rs.
    use super::{
        Amount, CAPACITY_EXHAUSTED, Capacity, Demand, Share, draws_a_host, floating_shares, plan,
        resolve_host, waive_workerless,
    };
    use crate::resources::ResourceBill;
    use crate::stations::StationSpec;
    use serde_json::json;

    fn row(
        name: &str,
        id: &str,
        parent: Option<&str>,
        allocation: serde_json::Value,
    ) -> StationSpec {
        serde_json::from_value(
            json!({"name":name,"version":1,"status":"active","title":name,
            "kind":"group","predicate":{},"id":id,"parent":parent,"mode":"standing",
            "allocation":allocation,"created_at":"2026-11-08T00:00:00Z"}),
        )
        .unwrap()
    }
    /// Algedonic's numbers on 2026-11-08: engineering holds 28 of w-1,
    /// its gates reserve 12 of those (engineering.toml v3, gates.toml v4).
    fn tree(extra: serde_json::Value) -> Vec<StationSpec> {
        let mut engineering = json!({"host:w-1":"28"});
        for (k, v) in extra.as_object().unwrap() {
            engineering[k] = v.clone();
        }
        vec![
            row("algedonic", "root", None, json!({})),
            row("engineering", "eng", Some("root"), engineering),
            row("gates", "gat", Some("eng"), json!({"host:w-1":"12"})),
            row("operations", "ops", Some("root"), json!({"host:w-2":"2"})),
        ]
    }
    fn a(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }
    fn run_bill(vcpu: Option<&str>) -> ResourceBill {
        serde_json::from_value(json!({"model":"from-run","vcpu":vcpu})).unwrap()
    }

    #[test]
    fn the_owners_floating_share_is_its_allocation_less_its_childrens() {
        let shares = floating_shares(&tree(json!({})), "eng").unwrap();
        assert_eq!(shares.len(), 1, "{shares:?}");
        let (pool, share) = &shares[0];
        assert_eq!(pool, "host:w-1");
        assert_eq!(share.station, "engineering");
        assert_eq!(share.allocated, a("28"));
        assert_eq!(share.reserved.get("gates"), Some(&a("12")));
        assert_eq!(share.floating().unwrap(), a("16"));
        // A station that is not the owner, or no station, floats nothing.
        assert!(
            floating_shares(&tree(json!({})), "nobody")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_run_draws_its_vcpu_from_the_owners_host_with_the_most_room() {
        let shares = floating_shares(&tree(json!({"host:w-2":"4"})), "eng").unwrap();
        // w-1 floats 16 with 15 held (1 free); w-2 floats 4 with none.
        let held = |pool: &str| Ok(a(if pool == "host:w-1" { "15" } else { "0" }));
        let bill = resolve_host(&run_bill(Some("2.5")), &shares, held).unwrap();
        let host = bill.host.as_ref().expect("the claim names its host");
        assert_eq!(
            (host.node.as_str(), host.station.as_str()),
            ("w-2", "engineering")
        );
        assert!(
            bill.pools()
                .iter()
                .any(|(p, q)| p == "host:w-2" && q.to_string() == "2.5"),
            "the vCPU bills the host, not the summed HUD pool: {:?}",
            bill.pools()
        );
        // Equal room: the lower node name, so two adapters choose alike.
        let even = |_: &str| Ok(a("0"));
        let shares = floating_shares(&tree(json!({"host:w-2":"16"})), "eng").unwrap();
        let bill = resolve_host(&run_bill(Some("2.5")), &shares, even).unwrap();
        assert_eq!(bill.host.unwrap().node, "w-1");
    }

    #[test]
    fn a_bill_billing_no_vcpu_or_not_from_the_run_draws_no_host() {
        let shares = floating_shares(&tree(json!({})), "eng").unwrap();
        let none = |_: &str| Ok(a("0"));
        assert_eq!(
            resolve_host(&run_bill(None), &shares, none).unwrap(),
            run_bill(None)
        );
        let fixed: ResourceBill = serde_json::from_value(json!({"vcpu":"1"})).unwrap();
        assert_eq!(resolve_host(&fixed, &shares, none).unwrap(), fixed);
    }

    // An analyst run binds no worker (backlog db88f6a9): the bill's
    // `workerless_profiles` names the run profiles whose runs it charges
    // no vCPU, read off the billed packet's `profile` at the claim.
    #[test]
    fn a_run_whose_profile_binds_no_worker_bills_no_vcpu() {
        let bill: ResourceBill = serde_json::from_value(
            json!({"model":"from-run","vcpu":"2.5","workerless_profiles":["analyst"]}),
        )
        .unwrap();
        let waived = waive_workerless(&bill, Some(&json!({"profile":"analyst"})));
        assert_eq!(waived, run_bill(None), "no vCPU, and no list on the record");
        assert!(!draws_a_host(&waived), "so it names no host");
        // Any other profile, a run naming none, and no packet at all bill
        // the vCPU: only a profile the row names is waived.
        for job in [
            Some(json!({"profile":"builder"})),
            Some(json!({"profile":"reviewer"})),
            Some(json!({})),
            None,
        ] {
            let kept = waive_workerless(&bill, job.as_ref());
            assert_eq!(kept, run_bill(Some("2.5")), "{job:?}");
            assert!(draws_a_host(&kept), "{job:?}");
        }
        // A bill with no list is returned as it is.
        assert_eq!(
            waive_workerless(&run_bill(Some("2.5")), Some(&json!({"profile":"analyst"}))),
            run_bill(Some("2.5"))
        );
    }

    #[test]
    fn a_workerless_list_must_name_profiles_on_a_bill_that_bills_vcpu() {
        let bill = |v: serde_json::Value| serde_json::from_value::<ResourceBill>(v).unwrap();
        assert!(
            bill(json!({"model":"from-run","vcpu":"2.5","workerless_profiles":["analyst"]}))
                .validate()
                .is_ok()
        );
        for refused in [
            json!({"model":"from-run","workerless_profiles":["analyst"]}),
            json!({"model":"from-run","vcpu":"2.5","workerless_profiles":[""]}),
            json!({"model":"from-run","vcpu":"2.5","workerless_profiles":["Analyst"]}),
        ] {
            assert!(bill(refused.clone()).validate().is_err(), "{refused}");
        }
    }

    #[test]
    fn an_owner_floating_no_host_is_refused_by_name() {
        let refused = resolve_host(&run_bill(Some("2.5")), &[], |_| Ok(a("0"))).unwrap_err();
        assert_eq!(refused.pool, "host");
        assert_eq!(refused.reason, "owner-floats-no-host");
    }

    fn on_w1(held: &str) -> Demand {
        Demand {
            share: Some(Share {
                station: "engineering".into(),
                allocated: a("28"),
                reserved: [("gates".to_string(), a("12"))].into(),
            }),
            ..Demand::flow("host:w-1", 2, a("2.5"), Capacity::Known(a("32")), a(held))
        }
    }

    #[test]
    fn a_draw_past_the_floating_share_is_refused_naming_the_host_pool() {
        // 13.5 + 2.5 = 16: the share is full, and it fits.
        assert_eq!(plan(&[on_w1("13.5")]).unwrap().len(), 1);
        // 15 + 2.5 > 16, though the box's 32 has room: the share decides.
        let refused = plan(&[on_w1("15")]).unwrap_err();
        assert_eq!(refused.pool, "host:w-1");
        assert_eq!(refused.reason, CAPACITY_EXHAUSTED);
        assert_eq!(
            refused.sentence().as_deref(),
            Some(
                "host:w-1 exhausted: 28 allocated to engineering, 12 to gates, 16 floating; \
                 15 held by running claims, this claim asks 2.5"
            )
        );
        // A refusal on a pool with no share says nothing more than its pool.
        let plain = Demand::flow("robots:opus", 1, a("1"), Capacity::Known(a("1")), a("1"));
        assert_eq!(plan(&[plain]).unwrap_err().sentence(), None);
    }
}
