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

/// The newest acquisition version a reader accepts: 3 records where each
/// allocation was drawn — the claim station's line, and of each amount
/// the part its own reservation held and the part borrowed above it
/// (backlog 30c1efe8). Versions 1 and 2 still replay and validate.
pub const CLAIM_VERSION: u32 = 3;

/// The acquisition version every claim door WRITES — still 2, a version
/// below what readers accept, so READERS GO FIRST (review 6fdc1391 P1):
/// a version-3 token persisted by a new replica would be unreadable to
/// an old one's release, identity binding, expiry and replay, whose
/// `Claim` reader refuses version 3 and whose `Demand` and `Allocation`
/// deny the new fields — during a rolling deploy and after a rollback
/// alike. This car ships the version-3 reader; the claim door writes
/// version 3 only in the car after it, once every reader in the estate
/// is this one (backlog 30c1efe8). Held equal to what the old reader
/// accepts by `a_written_claim_is_read_by_the_reader_before_version_three`.
pub const WRITTEN_CLAIM_VERSION: u32 = 2;

impl Claim {
    /// The name of the station whose line this claim's draw on `pool`
    /// stands at: the first level a version-3 decision recorded, the
    /// share a version-2 host draw recorded, else none — a claim that
    /// recorded no line drew at the root.
    pub fn drawn_at(&self, pool: &str) -> Option<&str> {
        let demand = self.observed.iter().find(|d| d.pool == pool)?;
        demand
            .levels
            .first()
            .map(|level| level.station.as_str())
            .or_else(|| demand.share.as_ref().map(|share| share.station.as_str()))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.version, 1..=CLAIM_VERSION)
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
        // A line is version 3's record, and a share is version 2's: each
        // version carries only the decision record its own rule wrote.
        if self.observed.iter().any(|d| {
            (self.version < 3 && !d.levels.is_empty()) || (self.version >= 3 && d.share.is_some())
        }) {
            return Err("resource decision record differs from its acquisition version".into());
        }
        if self.version >= 2 {
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
    /// THE CLAIM STATION'S LINE, as the station rows and running claims
    /// stood at the claim (backlog 30c1efe8): the station the door
    /// resolved first, then each parent up to the root, last. [`plan`]
    /// judges every pool on it by ONE rule — the station's own
    /// reservation first, then borrowing up the line, each level within
    /// its borrow limit — and splits the allocation into what was
    /// reserved and what was borrowed. Recorded on the decision, so the
    /// split is re-derivable from the token alone. Empty where no tree
    /// stands: the pool's capacity bounds the draw by itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<Level>,
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
            levels: Vec::new(),
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

// -- stations reserve, then borrow up their line (backlog 30c1efe8) ----------
//
// David 2026-11-19: "Stations should reserve any resources they need/want
// permanently from their parent station's resource pool. Then they can
// always request resources dynamically from their parent as well, but
// they might have to wait in line is all. Dynamic requests then need to be
// returned to the parent as soon as done. Stations may also have internal
// limits on the dynamic resources they request for their own reasons."
//
// A station's RESERVATION is its row's `allocation` — the one fact, never
// a second key. What it holds less what its children reserve is its
// FLOATING share: its own work draws there first, and so does whatever
// its children borrow past their own reservations. Past its floating, the
// draw is BORROWED from its parent, bounded by the station's
// `borrow_limit` for the pool, and so on up to the root, which holds each
// pool whole and borrows from nothing. A claim returns what it borrowed
// when it exits, because a draw is a function of the claims running now:
// nothing is lent by a record that outlives its claim.
//
// So, per station T and pool: drawn(T) = the claims standing at T, plus
// what each child borrows; borrowed(T) = drawn(T) beyond T's floating;
// and the tree is admissible while every borrowed(T) is within T's limit
// and the root borrows nothing. That is order-free — a release never makes
// it worse — so admission asks only whether ONE more claim keeps it so.
// The host shares of backlog 5608c1a2 (`floating_shares`, a run's vCPU
// bounded by its owner's floating) are this rule at engineering, with
// engineering borrowing none of host:w-1 (`borrow_limit` 0 on its row).
//
// READERS GO FIRST (review 6fdc1391 P1). This is the rule a version-3
// token records and every reader re-derives ([`Claim::validate`] through
// [`plan`]); the claim door still WRITES version 2
// ([`WRITTEN_CLAIM_VERSION`]) and judges by the owner's share, so a
// replica that predates this reader never meets a token it cannot read.
// The car after this one writes version 3 on the line.

/// One station's book of one pool, as a claim read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Level {
    /// The station's name.
    pub station: String,
    /// What it holds of the pool: its reservation (`allocation`, zero when
    /// it names none), or the pool's capacity at the root.
    pub holds: Amount,
    /// Each child's reservation of the same pool, by the child's name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub reserved: std::collections::BTreeMap<String, Amount>,
    /// What draws on its floating share now: the running claims standing
    /// at it, and what the stations standing in it borrow.
    pub drawn: Amount,
    /// The most it borrows of the pool from its parent; none is no limit
    /// of its own. Never set on the root, which borrows from nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_limit: Option<Amount>,
}

fn less(a: &Amount, b: &Amount) -> Amount {
    a.checked_sub(b).unwrap_or_else(|_| Amount::zero())
}

impl Level {
    /// What it holds less what its children reserve.
    pub fn floating(&self) -> Result<Amount, String> {
        self.holds
            .checked_sub(&Amount::sum(self.reserved.values())?)
            .map_err(|_| {
                format!(
                    "the stations under {} reserve more than it holds",
                    self.station
                )
            })
    }
    /// What of its floating share is drawn: by its own claims and lent
    /// to the stations standing in it.
    pub fn lent(&self) -> Result<Amount, String> {
        let floating = self.floating()?;
        Ok(if self.drawn > floating {
            floating
        } else {
            self.drawn.clone()
        })
    }
    /// What of its floating share no claim draws.
    pub fn free(&self) -> Result<Amount, String> {
        Ok(less(&self.floating()?, &self.drawn))
    }
    /// What it borrows from its parent: its draw past its floating share.
    pub fn borrowed(&self) -> Result<Amount, String> {
        Ok(less(&self.drawn, &self.floating()?))
    }
}

/// One running draw on a pool: the name of the station it stands at
/// ([`Claim::drawn_at`]; none, or a name the tree does not hold, is the
/// root) and its amount.
pub type Draw = (Option<String>, Amount);

/// THE BOOKS of one pool at every station of the tree, the root first and
/// each station before the stations standing in it. `capacity` is what
/// the root holds. Empty where the tree has no single root. A station's
/// reservation, its children's, its draw and what it borrows are each a
/// function of the rows and the running `draws`, so for every station
///
///   sum(reserved) + lent + free = holds
///
/// and while the tree is admissible, borrowed is within each station's
/// limit and the root's is zero (conservation, backlog 30c1efe8).
pub fn books(
    stations: &[crate::stations::StationSpec],
    pool: &str,
    capacity: &Amount,
    draws: &[Draw],
) -> Result<Vec<Level>, String> {
    let rows = crate::station_tree::tree_rows(stations);
    let mut roots = rows.iter().copied().filter(|r| r.parent.is_none());
    let (Some(root), None) = (roots.next(), roots.next()) else {
        return Ok(Vec::new());
    };
    // Breadth-first from the root, each row once: a parent always comes
    // before its children, and a cycle cannot loop the read.
    let mut order = vec![(root, None::<usize>)];
    let mut at = 0;
    while at < order.len() {
        let id = order[at].0.id.as_deref();
        let mut children: Vec<_> = rows
            .iter()
            .copied()
            .filter(|c| c.parent.is_some() && c.parent.as_deref() == id)
            .filter(|c| !order.iter().any(|(seen, _)| std::ptr::eq(*seen, *c)))
            .collect();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        order.extend(children.into_iter().map(|c| (c, Some(at))));
        at += 1;
    }
    let held_by = |row: &crate::stations::StationSpec| -> Result<Amount, String> {
        row.allocation
            .get(pool)
            .map_or_else(|| Ok(Amount::zero()), |q| q.amount())
    };
    let mut levels = order
        .iter()
        .enumerate()
        .map(|(i, (row, _))| {
            let reserved = order
                .iter()
                .filter(|(_, parent)| *parent == Some(i))
                .filter(|(child, _)| child.allocation.contains_key(pool))
                .map(|(child, _)| Ok((child.name.clone(), held_by(child)?)))
                .collect::<Result<_, String>>()?;
            Ok(Level {
                station: row.name.clone(),
                holds: if i == 0 {
                    capacity.clone()
                } else {
                    held_by(row)?
                },
                reserved,
                drawn: Amount::zero(),
                borrow_limit: if i == 0 {
                    None
                } else {
                    row.borrow_limit.get(pool).map(|q| q.amount()).transpose()?
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut drawn: Vec<Vec<Amount>> = vec![Vec::new(); levels.len()];
    for (station, amount) in draws {
        let at = station
            .as_deref()
            .and_then(|name| levels.iter().position(|l| l.station == name))
            .unwrap_or(0);
        drawn[at].push(amount.clone());
    }
    // Children before parents: what a station borrows is part of its
    // parent's draw.
    for i in (0..levels.len()).rev() {
        levels[i].drawn = Amount::sum(drawn[i].iter())?;
        if let Some(parent) = order[i].1 {
            let borrowed = levels[i].borrowed()?;
            drawn[parent].push(borrowed);
        }
    }
    Ok(levels)
}

/// The books of `pool` along the LINE of the claim station whose id is
/// `station` ([`crate::station_tree::line_of`]): that station first, the
/// root last. Empty where no tree stands.
pub fn line_levels(
    stations: &[crate::stations::StationSpec],
    station: Option<&str>,
    pool: &str,
    capacity: &Amount,
    draws: &[Draw],
) -> Result<Vec<Level>, String> {
    let line = crate::station_tree::line_of(stations, station);
    if line.is_empty() {
        return Ok(Vec::new());
    }
    let mut all = books(stations, pool, capacity, draws)?;
    line.iter()
        .map(|row| {
            all.iter()
                .position(|l| l.station == row.name)
                .map(|i| all.swap_remove(i))
                .ok_or_else(|| format!("station {} stands outside the tree's books", row.name))
        })
        .collect()
}

/// THE ONE ADMISSION RULE. A claim of `amount` standing at the first of
/// `levels` draws on that station's floating share first; what does not
/// fit rises to its parent as borrowing, and so on up the line. `Ok` with
/// the part the claim station's own reservation held (none at the root,
/// which reserves nothing from anyone); `Err(Some(i))` when level `i`
/// would borrow past its limit — the root's is zero; `Err(None)` when an
/// amount is unrepresentable. A level whose borrowing the claim does not
/// raise is not judged, so a limit lowered under running claims refuses
/// only the claims that would borrow more.
fn walk(levels: &[Level], amount: &Amount) -> Result<Amount, Option<usize>> {
    let last = levels.len().checked_sub(1).ok_or(None)?;
    let mut rising = amount.clone();
    let mut reserved = Amount::zero();
    for (i, level) in levels.iter().enumerate() {
        let before = level.borrowed().map_err(|_| None)?;
        let after = Level {
            drawn: level.drawn.checked_add(&rising).map_err(|_| None)?,
            ..level.clone()
        }
        .borrowed()
        .map_err(|_| None)?;
        let passed = after.checked_sub(&before).map_err(|_| None)?;
        if i == 0 && i != last {
            reserved = rising.checked_sub(&passed).map_err(|_| None)?;
        }
        if passed.is_zero() {
            return Ok(reserved);
        }
        let limit = if i == last {
            Some(Amount::zero())
        } else {
            level.borrow_limit.clone()
        };
        if limit.is_some_and(|limit| after > limit) {
            return Err(Some(i));
        }
        rising = passed;
    }
    Err(None)
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
    /// Of `amount`, what the claim station's own reservation held, and
    /// what was borrowed up its line (backlog 30c1efe8). Written by a
    /// version-3 decision that read a line; absent on an older token or
    /// one drawn where no tree stands, and then never serialized, so an
    /// older token's allocation reads back exactly as it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrowed: Option<Amount>,
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
    ///
    /// On a line (backlog 30c1efe8) it names the station whose borrowing
    /// the claim could not raise, and the numbers of THAT station's book:
    /// for a builder's vCPU, engineering's share of the build node, which
    /// borrows none of the root's slack.
    pub fn sentence(&self) -> Option<String> {
        if self.reason != CAPACITY_EXHAUSTED {
            return None;
        }
        let demand = self.observed.iter().find(|d| d.pool == self.pool)?;
        let reserved = |map: &std::collections::BTreeMap<String, Amount>| -> String {
            map.iter()
                .map(|(child, amount)| format!(", {amount} to {child}"))
                .collect()
        };
        if !demand.levels.is_empty() {
            let Err(Some(at)) = walk(&demand.levels, &demand.amount) else {
                return None;
            };
            let level = &demand.levels[at];
            let limit = match &level.borrow_limit {
                Some(limit) if !limit.is_zero() && at + 1 < demand.levels.len() => {
                    format!(", borrowing at most {limit} from its parent")
                }
                _ => String::new(),
            };
            return Some(format!(
                "{} exhausted: {} allocated to {}{}, {} floating{limit}; {} held by running \
                 claims, this claim asks {}",
                self.pool,
                level.holds,
                level.station,
                reserved(&level.reserved),
                level.floating().ok()?,
                level.drawn,
                demand.amount
            ));
        }
        let share = demand.share.as_ref()?;
        Some(format!(
            "{} exhausted: {} allocated to {}{}, {} floating; {} held by running claims, \
             this claim asks {}",
            self.pool,
            share.allocated,
            share.station,
            reserved(&share.reserved),
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
        // What the claim station's own reservation holds of this draw,
        // when the draw stands on a line (backlog 30c1efe8).
        let mut reserved = None;
        let reason = if previous == Some(&demand.pool) {
            Some("duplicate-pool")
        } else if demand.amount.is_zero() {
            None
        } else if demand.version < 1 {
            Some("pool-version-unknown")
        } else {
            // A share bounds the draw below the pool's own capacity: the
            // box may have room that is another station's (backlog 5608c1a2).
            // A version-2 record; a line's rule is judged below.
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
                        if demand.levels.is_empty() {
                            None
                        } else {
                            match walk(&demand.levels, &demand.amount) {
                                Ok(part) => {
                                    reserved = Some(part);
                                    None
                                }
                                Err(Some(_)) => Some(CAPACITY_EXHAUSTED),
                                Err(None) => Some("share-unrepresentable"),
                            }
                        }
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
            let borrowed = match &reserved {
                Some(part) => Some(demand.amount.checked_sub(part).map_err(|_| Refusal {
                    pool: demand.pool.clone(),
                    reason: "share-unrepresentable".into(),
                    observed: observed.clone(),
                })?),
                None => None,
            };
            result.push(Allocation {
                pool: demand.pool.clone(),
                version: demand.version,
                amount: demand.amount.clone(),
                reserved,
                borrowed,
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
    //! A run's vCPU is drawn from a HOST (backlog 5608c1a2), and every
    //! pool is judged by ONE rule on the claim station's line: its own
    //! reservation first, then borrowing up the parent chain, each level
    //! within its borrow limit (backlog 30c1efe8). The claim itself is
    //! pinned through the router on both adapters in
    //! tests/flow_resource_claims.rs.
    use super::{
        Allocation, Amount, CAPACITY_EXHAUSTED, Capacity, Claim, Demand, Draw, Share, books,
        draws_a_host, floating_shares, line_levels, plan, resolve_host, waive_workerless,
    };
    use crate::resources::ResourceBill;
    use crate::stations::StationSpec;
    use serde_json::json;

    fn row(
        name: &str,
        id: &str,
        parent: Option<&str>,
        allocation: serde_json::Value,
        borrow_limit: serde_json::Value,
    ) -> StationSpec {
        serde_json::from_value(
            json!({"name":name,"version":1,"status":"active","title":name,
            "kind":"group","predicate":{},"id":id,"parent":parent,"mode":"standing",
            "allocation":allocation,"borrow_limit":borrow_limit,
            "created_at":"2026-11-08T00:00:00Z"}),
        )
        .unwrap()
    }
    /// Algedonic's numbers on 2026-11-19, as this car leaves them:
    /// engineering holds 28 of w-1 and borrows none of the root's rest,
    /// its gates reserve 12, build reserves none, and no station reserves
    /// a robot slot (engineering.toml v8, gates.toml, build.toml v5).
    fn tree() -> Vec<StationSpec> {
        vec![
            row("algedonic", "root", None, json!({}), json!({})),
            row(
                "engineering",
                "eng",
                Some("root"),
                json!({"host:w-1":"28","robots:opus":"0"}),
                json!({"host:w-1":"0"}),
            ),
            row(
                "build",
                "bld",
                Some("eng"),
                json!({"robots:opus":"0"}),
                json!({}),
            ),
            row(
                "gates",
                "gat",
                Some("eng"),
                json!({"host:w-1":"12"}),
                json!({}),
            ),
            row(
                "operations",
                "ops",
                Some("root"),
                json!({"host:w-2":"2"}),
                json!({}),
            ),
        ]
    }
    fn a(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }
    fn at(station: &str, amount: &str) -> Draw {
        (Some(station.into()), a(amount))
    }
    fn run_bill(vcpu: Option<&str>) -> ResourceBill {
        serde_json::from_value(json!({"model":"from-run","vcpu":vcpu})).unwrap()
    }
    /// The decision a claim of `amount` at `station` makes on `pool`.
    fn demand(
        stations: &[StationSpec],
        station: Option<&str>,
        pool: &str,
        capacity: &str,
        draws: &[Draw],
        amount: &str,
    ) -> Demand {
        let held = Amount::sum(draws.iter().map(|(_, amount)| amount)).unwrap();
        Demand {
            levels: line_levels(stations, station, pool, &a(capacity), draws).unwrap(),
            ..Demand::flow(pool, 1, a(amount), Capacity::Known(a(capacity)), held)
        }
    }

    /// SPEC (5) OF BACKLOG 30c1efe8, FROM TODAY'S NUMBERS: host pools are
    /// unchanged. A builder's claim stands at build, which reserves none of
    /// w-1, so its 2.5 vCPU borrows engineering's floating share — 28 less
    /// the gates' 12, 16 — and engineering borrows none of the root's 4.
    /// Six fit (15 held), the seventh is refused with the sentence the
    /// live door answered on 2026-11-19, and the box's own 32 is not the
    /// bound.
    #[test]
    fn a_builders_vcpu_is_bounded_by_engineerings_floating_share() {
        let tree = tree();
        let six: Vec<Draw> = (0..6).map(|_| at("build", "2.5")).collect();
        let fits = demand(&tree, Some("bld"), "host:w-1", "32", &six[..5], "2.5");
        let engineering = &fits.levels[1];
        assert_eq!(
            (
                fits.levels[0].station.as_str(),
                engineering.station.as_str()
            ),
            ("build", "engineering")
        );
        assert_eq!(engineering.floating().unwrap(), a("16"));
        let held = plan(&[fits]).unwrap();
        assert_eq!(
            (held[0].reserved.clone(), held[0].borrowed.clone()),
            (Some(a("0")), Some(a("2.5"))),
            "build reserves no vCPU: all of it is borrowed"
        );
        let refused =
            plan(&[demand(&tree, Some("bld"), "host:w-1", "32", &six, "2.5")]).unwrap_err();
        assert_eq!(refused.pool, "host:w-1");
        assert_eq!(refused.reason, CAPACITY_EXHAUSTED);
        assert_eq!(
            refused.sentence().as_deref(),
            Some(
                "host:w-1 exhausted: 28 allocated to engineering, 12 to gates, 16 floating; \
                 15 held by running claims, this claim asks 2.5"
            )
        );
        // 13.5 + 2.5 = 16: the share is full, and it fits.
        let full = [at("build", "13.5")];
        assert!(plan(&[demand(&tree, Some("bld"), "host:w-1", "32", &full, "2.5")]).is_ok());
    }

    /// SPEC (2): no station reserves a robot slot, so a robot claim
    /// borrows from the root, first come first served wherever it stands —
    /// at build, or at no station at all.
    #[test]
    fn robot_slots_are_borrowed_from_the_root_first_come_first_served() {
        let tree = tree();
        let held = [
            at("build", "1"),
            at("build", "1"),
            at("build", "1"),
            (None, a("1")),
            (Some("review".into()), a("1")),
        ];
        for station in [Some("bld"), None] {
            let d = demand(&tree, station, "robots:opus", "6", &held, "1");
            let slot = plan(&[d]).unwrap();
            assert_eq!(slot[0].borrowed, Some(a("1")), "{station:?}");
        }
        let six: Vec<Draw> = held.iter().cloned().chain([at("build", "1")]).collect();
        for station in [Some("bld"), None] {
            let refused =
                plan(&[demand(&tree, station, "robots:opus", "6", &six, "1")]).unwrap_err();
            assert_eq!(refused.reason, CAPACITY_EXHAUSTED, "{station:?}");
        }
    }

    /// A reservation is used first by its own station's work; past it the
    /// station borrows its parent's floating share (what the parent holds
    /// less every child's reservation), then past that the parent borrows
    /// from the root within its own limit. A sibling's idle reservation is
    /// never lent.
    #[test]
    fn a_station_draws_its_reservation_first_then_borrows_within_each_limit() {
        // p: capacity 10. eng reserves 6 and borrows at most 2; under it
        // build reserves 3 and gates 1, so eng floats 2 and the root 4.
        let tree = vec![
            row("algedonic", "root", None, json!({}), json!({})),
            row(
                "engineering",
                "eng",
                Some("root"),
                json!({"p":"6"}),
                json!({"p":"2"}),
            ),
            row("build", "bld", Some("eng"), json!({"p":"3"}), json!({})),
            row("gates", "gat", Some("eng"), json!({"p":"1"}), json!({})),
        ];
        let mut draws: Vec<Draw> = Vec::new();
        let mut splits = Vec::new();
        loop {
            match plan(&[demand(&tree, Some("bld"), "p", "10", &draws, "1")]) {
                Ok(rows) => {
                    splits.push((
                        rows[0].reserved.clone().unwrap(),
                        rows[0].borrowed.clone().unwrap(),
                    ));
                    draws.push(at("build", "1"));
                }
                Err(refused) => {
                    assert_eq!(
                        refused.sentence().as_deref(),
                        Some(
                            "p exhausted: 6 allocated to engineering, 3 to build, 1 to gates, 2 \
                             floating, borrowing at most 2 from its parent; 4 held by running \
                             claims, this claim asks 1"
                        )
                    );
                    break;
                }
            }
        }
        let reserved: Vec<String> = splits.iter().map(|(r, _)| r.to_string()).collect();
        assert_eq!(
            reserved,
            ["1", "1", "1", "0", "0", "0", "0"],
            "three from build's own 3, two borrowed of engineering's 2, two of the root's"
        );
        // Gates' reservation stood idle and was never lent: it is still there.
        let gates = plan(&[demand(&tree, Some("gat"), "p", "10", &draws, "1")]).unwrap();
        assert_eq!(gates[0].reserved, Some(a("1")));
        // A claim at no station draws the root's unreserved remainder: 4,
        // less the 2 engineering borrows.
        let root = demand(&tree, None, "p", "10", &draws, "2");
        assert!(plan(&[root]).is_ok());
        let root = demand(&tree, None, "p", "10", &draws, "3");
        assert!(plan(&[root]).is_err());
    }

    /// CONSERVATION (backlog 30c1efe8): at every station and instant,
    /// sum(children's reservations) + lent + free = what it holds, and the
    /// rule admits a claim exactly when admitting it leaves every station
    /// within its borrow limit and the root borrowing nothing.
    mod conservation {
        use super::*;
        use proptest::prelude::*;

        fn limited_tree(limit: u8) -> Vec<StationSpec> {
            let limit = json!({"p": limit.to_string()});
            vec![
                row("algedonic", "root", None, json!({}), json!({})),
                row("engineering", "eng", Some("root"), json!({"p":"6"}), limit),
                row("build", "bld", Some("eng"), json!({"p":"2"}), json!({})),
                row(
                    "gates",
                    "gat",
                    Some("eng"),
                    json!({"p":"1"}),
                    json!({"p":"1"}),
                ),
                row(
                    "operations",
                    "ops",
                    Some("root"),
                    json!({"p":"1"}),
                    json!({}),
                ),
            ]
        }
        const STATIONS: [Option<&str>; 5] =
            [None, Some("eng"), Some("bld"), Some("gat"), Some("ops")];

        fn admissible(tree: &[StationSpec], draws: &[Draw]) -> bool {
            let all = books(tree, "p", &a("12"), draws).unwrap();
            all.iter().enumerate().all(|(i, level)| {
                let borrowed = level.borrowed().unwrap();
                match (i, &level.borrow_limit) {
                    (0, _) => borrowed.is_zero(),
                    (_, Some(limit)) => borrowed <= *limit,
                    (_, None) => true,
                }
            })
        }

        proptest! {
            #[test]
            fn every_station_conserves_and_admission_is_exact(
                limit in 0u8..4,
                ops in proptest::collection::vec((0usize..5, 1u8..4, any::<bool>()), 1..40),
            ) {
                let tree = limited_tree(limit);
                let mut draws: Vec<Draw> = Vec::new();
                for (station, amount, release) in ops {
                    if release && !draws.is_empty() {
                        draws.remove(usize::from(amount) % draws.len());
                    } else {
                        let id = STATIONS[station];
                        let name = id.and_then(|id| tree.iter().find(|r| r.id.as_deref() == Some(id)))
                            .map(|r| r.name.clone());
                        let amount = a(&amount.to_string());
                        let held = Amount::sum(draws.iter().map(|(_, x)| x)).unwrap();
                        let d = Demand {
                            levels: line_levels(&tree, id, "p", &a("12"), &draws).unwrap(),
                            ..Demand::flow("p", 1, amount.clone(), Capacity::Known(a("12")), held)
                        };
                        let mut with: Vec<Draw> = draws.clone();
                        with.push((name, amount.clone()));
                        match plan(&[d]) {
                            Ok(rows) => {
                                prop_assert!(admissible(&tree, &with));
                                let r = rows[0].reserved.clone().unwrap();
                                let b = rows[0].borrowed.clone().unwrap();
                                prop_assert_eq!(r.checked_add(&b).unwrap(), amount);
                                draws = with;
                            }
                            Err(refused) => {
                                prop_assert_eq!(refused.reason.as_str(), CAPACITY_EXHAUSTED);
                                prop_assert!(!admissible(&tree, &with));
                            }
                        }
                    }
                    for level in books(&tree, "p", &a("12"), &draws).unwrap() {
                        let reserved = Amount::sum(level.reserved.values()).unwrap();
                        let total = reserved
                            .checked_add(&level.lent().unwrap()).unwrap()
                            .checked_add(&level.free().unwrap()).unwrap();
                        prop_assert_eq!(total, level.holds.clone(), "{}", level.station);
                    }
                    prop_assert!(admissible(&tree, &draws));
                }
            }
        }
    }

    // THE VERSION-2 WRITER STILL PLACES A RUN'S vCPU ON ITS OWNER'S SHARE
    // (backlog 5608c1a2): readers go first (review 6fdc1391), so the claim
    // door keeps main's placement until the version-3 writer car.
    #[test]
    fn the_owners_floating_share_is_its_allocation_less_its_childrens() {
        let shares = floating_shares(&tree(), "eng").unwrap();
        assert_eq!(shares.len(), 1, "{shares:?}");
        let (pool, share) = &shares[0];
        assert_eq!(pool, "host:w-1");
        assert_eq!(share.station, "engineering");
        assert_eq!(share.allocated, a("28"));
        assert_eq!(share.reserved.get("gates"), Some(&a("12")));
        assert_eq!(share.floating().unwrap(), a("16"));
        // A station that is not the owner, or no station, floats nothing.
        assert!(floating_shares(&tree(), "nobody").unwrap().is_empty());
    }

    #[test]
    fn a_run_draws_its_vcpu_from_the_owners_host_with_the_most_room() {
        let mut tree = tree();
        let q = |s: &str| crate::resources::Quantity::parse(s).unwrap();
        tree[1].allocation.insert("host:w-2".into(), q("4"));
        let shares = floating_shares(&tree, "eng").unwrap();
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
        tree[1].allocation.insert("host:w-2".into(), q("16"));
        let shares = floating_shares(&tree, "eng").unwrap();
        let bill = resolve_host(&run_bill(Some("2.5")), &shares, |_| Ok(a("0"))).unwrap();
        assert_eq!(bill.host.unwrap().node, "w-1");
    }

    #[test]
    fn a_bill_billing_no_vcpu_or_not_from_the_run_draws_no_host() {
        let shares = floating_shares(&tree(), "eng").unwrap();
        let none = |_: &str| Ok(a("0"));
        assert_eq!(
            resolve_host(&run_bill(None), &shares, none).unwrap(),
            run_bill(None)
        );
        let fixed: ResourceBill = serde_json::from_value(json!({"vcpu":"1"})).unwrap();
        assert_eq!(resolve_host(&fixed, &shares, none).unwrap(), fixed);
    }

    #[test]
    fn an_owner_floating_no_host_is_refused_by_name() {
        let refused = resolve_host(&run_bill(Some("2.5")), &[], |_| Ok(a("0"))).unwrap_err();
        assert_eq!(refused.pool, "host");
        assert_eq!(refused.reason, "owner-floats-no-host");
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

    /// A version-2 token recorded its owner's share; it still replays and
    /// is still judged by the share it recorded.
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
    fn a_version_two_share_still_judges_its_own_token() {
        assert_eq!(plan(&[on_w1("13.5")]).unwrap().len(), 1);
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
        // A refusal on a pool with no share and no line says nothing more.
        let plain = Demand::flow("robots:opus", 1, a("1"), Capacity::Known(a("1")), a("1"));
        assert_eq!(plan(&[plain]).unwrap_err().sentence(), None);
    }

    /// SPEC (6): version 3 records the split per allocation; an older
    /// token serializes without one; and each version carries only its
    /// own decision record.
    #[test]
    fn a_version_three_claim_records_its_split_and_an_older_one_serializes_without_one() {
        let legacy = Allocation {
            pool: "robots:opus".into(),
            version: 2,
            amount: a("1"),
            reserved: None,
            borrowed: None,
        };
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            json!({"pool":"robots:opus","version":2,"amount":"1"}),
            "an older token serializes as it always did"
        );
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-19T20:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let observed = vec![demand(&tree(), Some("bld"), "robots:opus", "6", &[], "1")];
        let claim = |version: u32, observed: Vec<Demand>| Claim {
            version,
            id: uuid::Uuid::new_v4(),
            step_id: boss_core::job::StepId::new(),
            job_id: boss_core::job::JobId::new(),
            generation: 1,
            holder: "agent-claude".into(),
            workflow_kind: "agent-run".into(),
            workflow_version: 1,
            bill: serde_json::from_value(json!({"model":"opus"})).unwrap(),
            allocations: plan(&observed).unwrap(),
            observed,
            claimed_at: Some(at),
            resolved_duration_ms: Some(1000),
            lease_until: Some(at + chrono::Duration::milliseconds(2000)),
        };
        let v3 = claim(3, observed.clone());
        v3.validate().unwrap();
        assert_eq!(v3.drawn_at("robots:opus"), Some("build"));
        assert_eq!(v3.allocations[0].borrowed, Some(a("1")));
        assert!(
            claim(2, observed).validate().is_err(),
            "a line is version 3's record"
        );
        let mut shared = on_w1("0");
        shared.pool = "robots:opus".into();
        assert!(
            claim(3, vec![shared]).validate().is_err(),
            "a share is version 2's"
        );
    }
}
