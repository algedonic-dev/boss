//! Guarded admission — the Jobs port that admits a delayed rule's child
//! and records its intent's decision in ONE transaction. Car 3 of the
//! finish line on backlog 84a010cb (design f1d66249, folded into
//! docs/architecture-decisions.md "Durable source-relative guarded
//! admission").
//!
//! The design: "For authoritative state wholly in Jobs, one transaction
//! fences the intent, source and named guard steps against their real
//! writers, evaluates the current guard at the due boundary, and commits
//! the complete child graph, its outbox facts and the intent's
//! terminal/evidence together, or none of them. A second independently
//! committing create call or GET/POST with before/after comparisons is
//! not that transaction."
//!
//! So this module holds what both adapters must agree on, as pure
//! functions of the rows they read under their locks:
//!
//! - [`Binding`]: what the intent's immutable `envelope` pins — the
//!   child's id, kind, pinned version and partition, the fixed due
//!   instant and the [`Guard`]. Read from the LOCKED intent row, never
//!   from the caller: a request whose child differs from the envelope is
//!   refused, so recovery cannot invent a missing binding.
//! - [`preflight`]: the intent and its `due` step judged before any
//!   guard is read. An already-decided intent answers its recorded
//!   decision and writes nothing (idempotence); work not yet due answers
//!   [`Outcome::NotDue`] and writes nothing (a delayed rule never fires
//!   early).
//! - [`judge`]: the guard evaluated against current state. An OPEN
//!   source of the pinned kind whose every named step stands in one of
//!   its allowed nonterminal statuses admits; a closed or wrong-kind
//!   source, or a named step outside its allowed set, suppresses with
//!   what was observed; a missing source or a missing or duplicate named
//!   step is HELD — absence is not a negative fact.
//! - [`decided_due`]: the `due` step completed with its `result`,
//!   `evaluated_at` and `observed` fields — the intent's terminal
//!   decision, written in the same transaction as the child graph.
//!
//! What the port does NOT do: the protocol's outcome steps
//! (`admitted`/`suppressed`) and the intent's close follow from the
//! completed `due` step through the ordinary step advance — the decision
//! is the frozen `due` row, and the advance only reads it. The port
//! does not recheck policy (the caller holds the registered dispatcher
//! actor's Create and Read, car 5's drain), record held attempts (an
//! attempt the port answers `Held` for, or a storage error, writes
//! nothing and the caller records it), or capture an intent (car 4).
//! Nothing calls it yet, and `authoring::validate` still refuses every
//! rule declaring a delay, so no delayed rule fires early.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Step, StepStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The workflow kind every intent runs
/// (`infra/platform/workflows/delayed-admission.toml`).
pub const INTENT_KIND: &str = "delayed-admission";
/// The intent's execution step, completed once with its result.
pub const DUE_STEP: &str = "due";
/// The intent's immutable job-metadata key.
pub const ENVELOPE_KEY: &str = "envelope";

/// One named step of the source whose CURRENT status the guard reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepGuard {
    /// The step's slug on the source (`spec_slug`).
    pub step: String,
    /// The statuses in which the guard holds — nonterminal only. A step
    /// the guard names must be STILL waiting: "a proven `sent`
    /// completion may establish answered", so a completed step is what
    /// suppresses, never what admits.
    pub allowed: Vec<StepStatus>,
}

/// What a delayed admission requires of current Jobs state at its due
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub source_job: JobId,
    /// The source's pinned kind: a source of another kind suppresses.
    pub source_kind: String,
    #[serde(default)]
    pub steps: Vec<StepGuard>,
}

impl Guard {
    /// Refuse a guard whose meaning a reader would have to guess.
    pub fn check(&self) -> Result<(), String> {
        if self.source_kind.is_empty() {
            return Err("the guard names no source kind".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for g in &self.steps {
            if g.step.is_empty() {
                return Err("a guard step names no step".into());
            }
            if !seen.insert(g.step.as_str()) {
                return Err(format!("the guard names step {:?} twice", g.step));
            }
            if g.allowed.is_empty() {
                return Err(format!(
                    "guard step {:?} allows no status: an explicit allowed status is required",
                    g.step
                ));
            }
            if let Some(s) = g.allowed.iter().find(|s| is_terminal(**s)) {
                return Err(format!(
                    "guard step {:?} allows terminal status {}: a guard admits only on a \
                     nonterminal status",
                    g.step,
                    status_word(*s)
                ));
            }
        }
        Ok(())
    }
}

/// What the intent's envelope pins, read from the locked intent row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub child_id: JobId,
    pub child_kind: String,
    pub child_workflow_version: i32,
    pub child_partition: String,
    pub due_at: DateTime<Utc>,
    pub guard: Guard,
}

impl Binding {
    /// Read the binding out of an intent's job metadata. Every field is
    /// required; a missing or malformed one is refused with its name,
    /// never defaulted.
    pub fn from_metadata(metadata: &Value) -> Result<Self, String> {
        let envelope = metadata
            .get(ENVELOPE_KEY)
            .ok_or_else(|| format!("the intent carries no `{ENVELOPE_KEY}`"))?;
        let text = |v: Option<&Value>, name: &str| -> Result<String, String> {
            match v.and_then(Value::as_str) {
                Some(s) if !s.is_empty() => Ok(s.to_string()),
                _ => Err(format!(
                    "the envelope's `{name}` is missing or not a string"
                )),
            }
        };
        let child_id = text(envelope.get("child_id"), "child_id")?;
        let child_id = uuid::Uuid::parse_str(&child_id)
            .map(JobId::from_uuid)
            .map_err(|e| format!("the envelope's `child_id` is not a uuid: {e}"))?;
        let child = envelope
            .get("child")
            .ok_or("the envelope carries no `child`")?;
        let child_kind = text(child.get("kind"), "child.kind")?;
        let child_workflow_version = child
            .get("workflow_version")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or("the envelope's `child.workflow_version` is missing or not an i32")?;
        let child_partition = text(child.get("partition"), "child.partition")?;
        let due_at = text(envelope.get("due_at"), "due_at")?;
        let due_at = DateTime::parse_from_rfc3339(&due_at)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| format!("the envelope's `due_at` is not RFC 3339: {e}"))?;
        let guard: Guard = serde_json::from_value(
            envelope
                .get("guard")
                .cloned()
                .ok_or("the envelope carries no `guard`")?,
        )
        .map_err(|e| format!("the envelope's `guard` is malformed: {e}"))?;
        guard.check()?;
        Ok(Self {
            child_id,
            child_kind,
            child_workflow_version,
            child_partition,
            due_at,
            guard,
        })
    }

    /// The source job an envelope names, read leniently so an adapter
    /// can take the source's row lock beside the intent's before any
    /// step lock (job rows precede step rows). `None` when it cannot be
    /// read; [`preflight`] then refuses the envelope by name.
    pub fn source_of(metadata: &Value) -> Option<JobId> {
        metadata
            .get(ENVELOPE_KEY)?
            .get("guard")?
            .get("source_job")?
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(JobId::from_uuid)
    }

    /// The child an envelope names, read leniently for the same reason.
    pub fn child_of(metadata: &Value) -> Option<JobId> {
        metadata
            .get(ENVELOPE_KEY)?
            .get("child_id")?
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(JobId::from_uuid)
    }
}

/// One guarded admission attempt.
#[derive(Debug, Clone)]
pub struct Request {
    /// The `delayed-admission` packet.
    pub intent: JobId,
    /// The child graph, materialized by the caller from the pinned
    /// workflow version. Held to the envelope: its id, kind, version and
    /// partition must be the ones the envelope names, and every step's
    /// job id must be the child's.
    pub child: Job,
    pub child_steps: Vec<Step>,
    pub child_job_events: Vec<boss_core::event::Event>,
    /// Index-aligned with `child_steps`, one STEP_CREATED each.
    pub child_step_events: Vec<boss_core::event::Event>,
    pub now: DateTime<Utc>,
    pub actor: ActorId,
}

/// What an attempt did. Only `Admitted` and `Suppressed` wrote anything.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The child exists under the envelope's id and the `due` step
    /// recorded `admitted`. `reconciled` is true when the exact requested
    /// child already existed (any status, CLOSED included; [`reconciles`])
    /// and no second graph was written.
    Admitted {
        child: JobId,
        reconciled: bool,
        due: Step,
    },
    /// The guard was false; no child, and the `due` step recorded
    /// `suppressed` with the reason and what was observed.
    Suppressed { reason: String, due: Step },
    /// The guard could not be judged from what Jobs holds (a missing
    /// source, a missing or duplicate named step, a due step not yet
    /// ready). Nothing written; the intent stays open.
    Held { reason: String, observed: Value },
    /// The fixed due instant has not arrived. Nothing written.
    NotDue { due_at: DateTime<Utc> },
    /// The `due` step was already decided: its recorded row, nothing
    /// written. A retry after a lost answer lands here.
    AlreadyDecided { due: Step },
    /// The request cannot be executed as made — not an intent, an
    /// envelope missing or malformed, a child that is not the
    /// envelope's, a child id naming a different packet. Nothing
    /// written, and a retry cannot fix it.
    Refused { reason: String },
}

/// What the guard said, before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub enum Judgement {
    Admit { observed: Value },
    Suppress { reason: String, observed: Value },
    Held { reason: String, observed: Value },
}

pub fn is_terminal(status: StepStatus) -> bool {
    matches!(status, StepStatus::Completed | StepStatus::Skipped)
}

fn status_word(status: StepStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn job_status_word(status: JobStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Judge the intent and its `due` step, before any guard is read.
/// `due_steps` are the intent's steps whose slug is [`DUE_STEP`]. The
/// answer that stops the attempt is boxed: it carries a whole step row.
pub fn preflight(
    intent: Option<&Job>,
    due_steps: &[Step],
    request: &Request,
) -> Result<(Binding, Step), Box<Outcome>> {
    let refused = |reason: String| Box::new(Outcome::Refused { reason });
    let Some(intent) = intent else {
        return Err(refused(format!("no packet {}", request.intent)));
    };
    if intent.kind != INTENT_KIND {
        return Err(refused(format!(
            "packet {} is a {:?}, not a {INTENT_KIND:?} intent",
            intent.id, intent.kind
        )));
    }
    let due = match due_steps {
        [due] => due,
        [] => {
            return Err(refused(format!(
                "intent {} has no `{DUE_STEP}` step",
                intent.id
            )));
        }
        _ => {
            return Err(refused(format!(
                "intent {} has {} `{DUE_STEP}` steps",
                intent.id,
                due_steps.len()
            )));
        }
    };
    // Decided first: a retry after the decision committed reads it back,
    // whatever has happened to the packet since (its close included).
    if is_terminal(due.status) {
        return Err(Box::new(Outcome::AlreadyDecided { due: due.clone() }));
    }
    if intent.status != JobStatus::Open {
        return Err(refused(format!(
            "intent {} is {}: only an open intent admits",
            intent.id,
            job_status_word(intent.status)
        )));
    }
    if due.status == StepStatus::Pending {
        return Err(Box::new(Outcome::Held {
            reason: format!("the `{DUE_STEP}` step is pending: the intent is not captured yet"),
            observed: json!({"due_step": due.id, "status": "pending"}),
        }));
    }
    // A claimed step's resources are released by the claim door's
    // completion; this port does not own that release, so it refuses
    // rather than complete around it.
    if due.metadata.get(crate::flow_resources::TOKEN_KEY).is_some() {
        return Err(refused(format!(
            "the `{DUE_STEP}` step holds a resource claim: it completes through the claim door"
        )));
    }
    let binding = Binding::from_metadata(&intent.metadata).map_err(refused)?;
    let child = &request.child;
    if child.id != binding.child_id
        || child.kind != binding.child_kind
        || child.workflow_version != binding.child_workflow_version
        || child.partition.as_str() != binding.child_partition
    {
        return Err(refused(format!(
            "the child requested ({} {:?} v{} {}) is not the one the envelope names ({} {:?} v{} {})",
            child.id,
            child.kind,
            child.workflow_version,
            child.partition.as_str(),
            binding.child_id,
            binding.child_kind,
            binding.child_workflow_version,
            binding.child_partition
        )));
    }
    // Every child step is the child's. Both adapters insert a step under
    // whatever job id it carries, so a step naming another job — the
    // guard's own source, say — would be written into that job in this
    // transaction (review d6774dfd of car 2b7ff3ca, B2). The port writes
    // the child graph and the intent's decision, nothing else.
    if let Some(stray) = request.child_steps.iter().find(|s| s.job_id != child.id) {
        return Err(refused(format!(
            "child step {} belongs to job {}, not the child {}",
            stray.id, stray.job_id, child.id
        )));
    }
    if !is_due(binding.due_at, request.now) {
        return Err(Box::new(Outcome::NotDue {
            due_at: binding.due_at,
        }));
    }
    Ok((binding, due.clone()))
}

/// At or after the bound, never before it (the dispatcher's
/// `delayed::is_due`, restated here because Jobs cannot depend on the
/// dispatcher; pinned equal by the boundary test below).
pub fn is_due(due_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now >= due_at
}

/// Judge the guard against the source as it stands under the lock.
/// `source_steps` are the source's steps whose slugs the guard names.
pub fn judge(guard: &Guard, source: Option<&Job>, source_steps: &[Step]) -> Judgement {
    let steps_seen: Vec<Value> = source_steps
        .iter()
        .map(|s| json!({"step": s.spec_slug, "id": s.id, "status": status_word(s.status)}))
        .collect();
    let observed = |source: Value| {
        json!({
            "source": source,
            "steps": steps_seen,
            "guard": guard,
        })
    };
    let Some(source) = source else {
        return Judgement::Held {
            reason: format!("source {} is not in Jobs", guard.source_job),
            observed: observed(Value::Null),
        };
    };
    let seen_source = json!({
        "id": source.id,
        "kind": source.kind,
        "status": job_status_word(source.status),
        "workflow_version": source.workflow_version,
    });
    if source.kind != guard.source_kind {
        return Judgement::Suppress {
            reason: format!(
                "source {} is a {:?}, not the pinned {:?}",
                source.id, source.kind, guard.source_kind
            ),
            observed: observed(seen_source),
        };
    }
    match source.status {
        JobStatus::Open => {}
        JobStatus::Closed | JobStatus::Cancelled => {
            return Judgement::Suppress {
                reason: format!("source {} is {}", source.id, job_status_word(source.status)),
                observed: observed(seen_source),
            };
        }
        JobStatus::Draft => {
            return Judgement::Held {
                reason: format!("source {} is a draft, not yet open", source.id),
                observed: observed(seen_source),
            };
        }
    }
    for g in &guard.steps {
        let named: Vec<&Step> = source_steps
            .iter()
            .filter(|s| s.job_id == source.id && s.spec_slug.as_deref() == Some(g.step.as_str()))
            .collect();
        let step = match named.as_slice() {
            [step] => step,
            [] => {
                return Judgement::Held {
                    reason: format!("source {} has no `{}` step", source.id, g.step),
                    observed: observed(seen_source),
                };
            }
            _ => {
                return Judgement::Held {
                    reason: format!(
                        "source {} has {} `{}` steps",
                        source.id,
                        named.len(),
                        g.step
                    ),
                    observed: observed(seen_source),
                };
            }
        };
        if !g.allowed.contains(&step.status) {
            return Judgement::Suppress {
                reason: format!(
                    "source step `{}` is {}, outside the guard's allowed {:?}",
                    g.step,
                    status_word(step.status),
                    g.allowed
                        .iter()
                        .map(|s| status_word(*s))
                        .collect::<Vec<_>>()
                ),
                observed: observed(seen_source),
            };
        }
    }
    Judgement::Admit {
        observed: observed(seen_source),
    }
}

/// The `due` step completed with its decision. `extra` carries the
/// result's own keys (`child_id` and `reconciled`, or `reason`).
pub fn decided_due(
    due: &Step,
    result: &str,
    observed: &Value,
    binding: &Binding,
    extra: Map<String, Value>,
    now: DateTime<Utc>,
    actor: &ActorId,
) -> Step {
    let mut step = due.clone();
    let mut metadata = match &due.metadata {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    metadata.insert("result".into(), json!(result));
    metadata.insert(
        "evaluated_at".into(),
        json!(now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
    );
    // `observed` is a string field on the protocol row; the observation
    // rides whole, as its JSON text, so nothing is reduced to fit it.
    metadata.insert("observed".into(), json!(observed.to_string()));
    metadata.insert(
        "due_at".into(),
        json!(
            binding
                .due_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
    );
    metadata.extend(extra);
    step.metadata = Value::Object(metadata);
    step.status = StepStatus::Completed;
    step.completed_on = Some(now.date_naive());
    step.completed_at = Some(now);
    step.completed_by = Some(actor.clone());
    step
}

/// An existing packet under the child id is reconciled only when it is
/// EXACTLY the requested child — the envelope's kind, pinned version and
/// partition, and `child` (the request's, held to the envelope by
/// [`preflight`]) on every field its caller decides at admission. Any
/// other packet under that id is a conflict.
///
/// Admission and the decision commit together, so a retry after this
/// port admitted lands in [`Outcome::AlreadyDecided`] and never here:
/// the only packet that reaches this check is one ANOTHER door wrote,
/// and the child id is a deterministic id in plain text on the intent.
/// Comparing kind, version and partition alone answered a same-kind
/// packet planted days early about another subject as admitted by this
/// intent, lending the delayed rule's provenance to a packet it never
/// wrote (review d6774dfd of car 2b7ff3ca, B1). The design: "Conflicting
/// bytes under an identity are refused", and a retry reconciles "an
/// EXACT existing child".
///
/// The comparison is the jobs API's for a re-sent create body,
/// [`crate::port::admission_differences`], not a second copy: status
/// (a closed child reconciles) and the metadata keys the server stamps
/// beside the caller's are left out there for the reasons it gives. The
/// owner and the opened date are compared, because this port writes the
/// child as its caller materialized it: nothing resolves the owner or
/// lets the clock supply the date.
///
/// The packet is its job row AND its step graph: `existing_steps` are
/// every step under the child id, read under the same locks. A job row
/// that matched while its graph did not — none of its steps, say — was
/// reconciled, completing the intent `admitted` for a child that never
/// got the graph this transaction promises and consuming the intent for
/// good (review b12f08eb of car 2b7ff3ca, P1). So the graph must equal
/// `request.child_steps` in shape ([`graph_differences`]).
pub fn reconciles(
    existing: &Job,
    existing_steps: &[Step],
    binding: &Binding,
    request: &Request,
) -> Result<(), String> {
    let child = &request.child;
    if existing.kind != binding.child_kind
        || existing.workflow_version != binding.child_workflow_version
        || existing.partition.as_str() != binding.child_partition
    {
        return Err(format!(
            "child id {} already names a {:?} v{} {} packet, not the envelope's {:?} v{} {}",
            existing.id,
            existing.kind,
            existing.workflow_version,
            existing.partition.as_str(),
            binding.child_kind,
            binding.child_workflow_version,
            binding.child_partition
        ));
    }
    let differing = crate::port::admission_differences(child, true, true, existing);
    if !differing.is_empty() {
        return Err(format!(
            "child id {} already names a packet that differs from the requested child in {}",
            existing.id,
            differing.join(", ")
        ));
    }
    match graph_differences(existing_steps, &request.child_steps) {
        None => Ok(()),
        Some(difference) => Err(format!(
            "child id {} already names the requested child, but not its step graph: {difference}",
            existing.id
        )),
    }
}

/// How two child step graphs differ in SHAPE, or `None` when they do
/// not: every step's slug, kind, title, order, required sign-offs and
/// assurance, and its `blocked_by` edges named by the steps they point
/// at. Step ids are minted afresh at each materialization, so an edge is
/// compared by what it names, never by id; an edge naming no step of its
/// own graph is named by its id, so it never matches. What the child's
/// work has moved since admission — status, completion, assignee,
/// metadata, notes, sign-off stamps — is not shape: a closed child, its
/// steps completed, still reconciles.
pub fn graph_differences(existing: &[Step], requested: &[Step]) -> Option<String> {
    fn shape(steps: &[Step]) -> Vec<String> {
        let name = |id: &boss_core::job::StepId| {
            steps
                .iter()
                .find(|s| &s.id == id)
                .map(|s| s.spec_slug.clone().unwrap_or_else(|| s.title.clone()))
                .unwrap_or_else(|| format!("outside the graph: {id}"))
        };
        let mut shape: Vec<String> = steps
            .iter()
            .map(|s| {
                let mut edges: Vec<String> = s.blocked_by.iter().map(name).collect();
                edges.sort();
                json!({
                    "order": s.sort_order,
                    "slug": s.spec_slug,
                    "kind": s.kind,
                    "title": s.title,
                    "sign_offs_required": s.sign_offs_required,
                    "assurance_required": s.assurance_required,
                    "blocked_by": edges,
                })
                .to_string()
            })
            .collect();
        shape.sort();
        shape
    }
    let (have, want) = (shape(existing), shape(requested));
    if have == want {
        return None;
    }
    let missing: Vec<&String> = want.iter().filter(|s| !have.contains(s)).collect();
    let extra: Vec<&String> = have.iter().filter(|s| !want.contains(s)).collect();
    Some(format!(
        "{} step(s) stand where {} are requested; requested but not standing: [{}]; standing \
         but not requested: [{}]",
        existing.len(),
        requested.len(),
        missing
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        extra
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A packet already standing under the envelope's child id, read under
/// the attempt's locks: its job row and every step under it.
#[derive(Debug, Clone, Copy)]
pub struct Existing<'a> {
    pub job: &'a Job,
    pub steps: &'a [Step],
}

/// The decision for a judged attempt: the due step to write and the
/// outcome to answer. `existing` is the packet already under the child
/// id, read under the same locks.
///
/// An occupied child identity is judged BEFORE the guard's verdict is
/// acted on (review b12f08eb of car 2b7ff3ca, P2): the check once ran in
/// the admit arm only, so a packet of another kind under the child id,
/// with the guard saying suppress, was recorded `suppressed` beside it.
/// A packet that is not exactly the requested child is refused whatever
/// the guard says. The exact child standing while the guard SUPPRESSES
/// is refused too: `suppressed` records that no child was admitted, and
/// one stands under the id, so neither terminal is true and the intent
/// stays open for its caller to record the refusal — loud, never a
/// guess.
pub fn decide(
    judgement: Judgement,
    due: &Step,
    binding: &Binding,
    existing: Option<Existing<'_>>,
    request: &Request,
) -> Decision {
    if let Some(e) = existing
        && let Err(reason) = reconciles(e.job, e.steps, binding, request)
    {
        return Decision::Answer(Outcome::Refused { reason });
    }
    match judgement {
        Judgement::Held { reason, observed } => {
            Decision::Answer(Outcome::Held { reason, observed })
        }
        Judgement::Suppress { reason, .. } if existing.is_some() => {
            Decision::Answer(Outcome::Refused {
                reason: format!(
                    "the guard suppresses ({reason}), but child {} already stands under the \
                     envelope's id: neither `suppressed` nor `admitted` would be true",
                    binding.child_id
                ),
            })
        }
        Judgement::Suppress { reason, observed } => {
            let mut extra = Map::new();
            extra.insert("reason".into(), json!(reason));
            let due = decided_due(
                due,
                "suppressed",
                &observed,
                binding,
                extra,
                request.now,
                &request.actor,
            );
            Decision::Suppress { reason, due }
        }
        Judgement::Admit { observed } => {
            let reconciled = existing.is_some();
            let mut extra = Map::new();
            extra.insert("child_id".into(), json!(binding.child_id));
            extra.insert("reconciled".into(), json!(reconciled));
            let due = decided_due(
                due,
                "admitted",
                &observed,
                binding,
                extra,
                request.now,
                &request.actor,
            );
            Decision::Admit { reconciled, due }
        }
    }
}

/// What an adapter writes for a judged attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Write nothing; answer this.
    Answer(Outcome),
    /// Write the completed `due` step and its events.
    Suppress { reason: String, due: Step },
    /// Write the child graph (unless `reconciled`) and the completed
    /// `due` step, with their events.
    Admit { reconciled: bool, due: Step },
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Priority, Subject};
    use chrono::{NaiveDate, TimeDelta, TimeZone};

    /// The answer `preflight` stops the attempt with, unboxed so the
    /// assertions compare outcomes by value; `None` when it proceeds.
    fn stopped(intent: Option<&Job>, due_steps: &[Step], request: &Request) -> Option<Outcome> {
        preflight(intent, due_steps, request)
            .err()
            .map(|outcome| *outcome)
    }

    fn uuid(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    fn source() -> Job {
        Job {
            id: JobId::from_uuid(uuid(0xa1)),
            status: JobStatus::Open,
            ..Job::new(
                "receive-an-inquiry",
                Subject::new("custom", "inquiry"),
                "an inquiry",
                "emp-1",
                Priority::Standard,
                NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            )
        }
    }

    fn step_of(job: &Job, slug: &str, status: StepStatus) -> Step {
        let mut s = Step::new(job.id, "task", slug, 0);
        s.spec_slug = Some(slug.into());
        s.status = status;
        s
    }

    fn guard() -> Guard {
        Guard {
            source_job: JobId::from_uuid(uuid(0xa1)),
            source_kind: "receive-an-inquiry".into(),
            steps: vec![StepGuard {
                step: "sent".into(),
                allowed: vec![StepStatus::Pending, StepStatus::Ready, StepStatus::Active],
            }],
        }
    }

    fn due_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap()
    }

    fn envelope() -> Value {
        json!({
            "child_id": uuid(0xc1).to_string(),
            "child": {"kind": "follow-up-a-prospect", "workflow_version": 3, "partition": "real"},
            "due_at": "2026-11-15T12:00:00Z",
            "guard": guard(),
        })
    }

    fn intent() -> Job {
        Job {
            id: JobId::from_uuid(uuid(0xb1)),
            status: JobStatus::Open,
            metadata: json!({ "envelope": envelope() }),
            ..Job::new(
                INTENT_KIND,
                Subject::new("custom", "delayed-admission"),
                "follow up",
                "automation:dispatcher",
                Priority::Standard,
                NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            )
        }
    }

    fn child() -> Job {
        let mut j = Job::new(
            "follow-up-a-prospect",
            Subject::new("custom", "inquiry"),
            "follow up a prospect",
            "automation:dispatcher",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        j.id = JobId::from_uuid(uuid(0xc1));
        j.workflow_version = 3;
        j.partition = "real".parse().unwrap();
        j.status = JobStatus::Open;
        j
    }

    fn request(now: DateTime<Utc>) -> Request {
        Request {
            intent: JobId::from_uuid(uuid(0xb1)),
            child: child(),
            child_steps: vec![],
            child_job_events: vec![],
            child_step_events: vec![],
            now,
            actor: ActorId::Automation("dispatcher".into()),
        }
    }

    #[test]
    fn the_envelope_binding_reads_every_field_and_refuses_a_missing_one() {
        let b = Binding::from_metadata(&intent().metadata).expect("binding");
        assert_eq!(b.child_id, JobId::from_uuid(uuid(0xc1)));
        assert_eq!(b.child_kind, "follow-up-a-prospect");
        assert_eq!(b.child_workflow_version, 3);
        assert_eq!(b.child_partition, "real");
        assert_eq!(b.due_at, due_at());
        assert_eq!(b.guard, guard());
        assert_eq!(
            Binding::source_of(&intent().metadata),
            Some(guard().source_job)
        );
        for path in [
            "/child_id",
            "/child/kind",
            "/child/workflow_version",
            "/child/partition",
            "/due_at",
            "/guard",
        ] {
            let mut m = intent().metadata;
            let (parent, leaf) = path.rsplit_once('/').unwrap();
            m["envelope"]
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
                .unwrap()
                .remove(leaf);
            let err = Binding::from_metadata(&m).expect_err(path);
            assert!(err.contains(leaf), "{path}: {err}");
        }
        assert!(Binding::from_metadata(&json!({})).is_err());
    }

    #[test]
    fn a_guard_that_admits_on_a_terminal_or_no_status_is_refused() {
        let mut g = guard();
        g.steps[0].allowed = vec![StepStatus::Completed];
        assert!(g.check().unwrap_err().contains("terminal"));
        g.steps[0].allowed = vec![];
        assert!(g.check().unwrap_err().contains("allows no status"));
        let mut g = guard();
        g.steps.push(g.steps[0].clone());
        assert!(g.check().unwrap_err().contains("twice"));
        let mut g = guard();
        g.source_kind.clear();
        assert!(g.check().is_err());
        assert_eq!(guard().check(), Ok(()));
    }

    /// Before / exact / after the fixed due instant.
    #[test]
    fn nothing_is_judged_before_the_due_instant_and_the_exact_instant_is_due() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let early = request(due_at() - TimeDelta::seconds(1));
        assert_eq!(
            stopped(Some(&intent), std::slice::from_ref(&due), &early),
            Some(Outcome::NotDue { due_at: due_at() })
        );
        for now in [due_at(), due_at() + TimeDelta::days(3)] {
            let (b, d) =
                preflight(Some(&intent), std::slice::from_ref(&due), &request(now)).expect("due");
            assert_eq!(b.due_at, due_at(), "lateness never moves the anchor");
            assert_eq!(d.id, due.id);
        }
    }

    #[test]
    fn a_decided_intent_answers_its_decision_even_after_it_closed() {
        let mut intent = intent();
        intent.status = JobStatus::Closed;
        let due = step_of(&intent, DUE_STEP, StepStatus::Completed);
        assert_eq!(
            stopped(
                Some(&intent),
                std::slice::from_ref(&due),
                &request(due_at())
            ),
            Some(Outcome::AlreadyDecided { due })
        );
    }

    #[test]
    fn a_child_other_than_the_envelopes_is_refused() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let mutations: [fn(&mut Job); 4] = [
            |j| j.id = JobId::from_uuid(uuid(0xc2)),
            |j| j.kind = "another".into(),
            |j| j.workflow_version = 4,
            |j| j.partition = "simulated".parse().unwrap(),
        ];
        for m in mutations {
            let mut r = request(due_at());
            m(&mut r.child);
            assert!(matches!(
                stopped(Some(&intent), std::slice::from_ref(&due), &r),
                Some(Outcome::Refused { .. })
            ));
        }
    }

    /// Every child step belongs to the child: a step carrying another
    /// job's id would be written into that job — the guard's source, say
    /// — in the same transaction (review d6774dfd, B2).
    #[test]
    fn a_child_step_of_another_job_is_refused() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let mut r = request(due_at());
        r.child_steps
            .push(step_of(&r.child, "write", StepStatus::Ready));
        assert!(stopped(Some(&intent), std::slice::from_ref(&due), &r).is_none());
        let stray = step_of(&source(), "sent", StepStatus::Completed);
        r.child_steps.push(stray.clone());
        let Some(Outcome::Refused { reason }) =
            stopped(Some(&intent), std::slice::from_ref(&due), &r)
        else {
            panic!("refused");
        };
        assert!(reason.contains(&stray.id.to_string()), "{reason}");
        assert!(reason.contains(&source().id.to_string()), "{reason}");
    }

    #[test]
    fn a_wrong_kind_closed_or_missing_intent_is_refused_and_a_pending_due_is_held() {
        let due_of = |j: &Job, s| vec![step_of(j, DUE_STEP, s)];
        let r = request(due_at());
        assert!(matches!(
            stopped(None, &[], &r),
            Some(Outcome::Refused { .. })
        ));
        let mut other = intent();
        other.kind = "backlog-item".into();
        assert!(matches!(
            stopped(Some(&other), &due_of(&other, StepStatus::Ready), &r),
            Some(Outcome::Refused { .. })
        ));
        let mut cancelled = intent();
        cancelled.status = JobStatus::Cancelled;
        assert!(matches!(
            stopped(Some(&cancelled), &due_of(&cancelled, StepStatus::Ready), &r),
            Some(Outcome::Refused { .. })
        ));
        let i = intent();
        assert!(matches!(
            stopped(Some(&i), &due_of(&i, StepStatus::Pending), &r),
            Some(Outcome::Held { .. })
        ));
        assert!(matches!(
            stopped(Some(&i), &[], &r),
            Some(Outcome::Refused { .. })
        ));
        let two = [due_of(&i, StepStatus::Ready), due_of(&i, StepStatus::Ready)].concat();
        assert!(matches!(
            stopped(Some(&i), &two, &r),
            Some(Outcome::Refused { .. })
        ));
        let mut claimed = due_of(&i, StepStatus::Active);
        claimed[0].metadata = json!({crate::flow_resources::TOKEN_KEY: "t"});
        assert!(matches!(
            stopped(Some(&i), &claimed, &r),
            Some(Outcome::Refused { .. })
        ));
    }

    #[test]
    fn an_open_source_with_its_step_still_waiting_admits() {
        let s = source();
        for status in [StepStatus::Pending, StepStatus::Ready, StepStatus::Active] {
            let steps = [step_of(&s, "sent", status)];
            assert!(matches!(
                judge(&guard(), Some(&s), &steps),
                Judgement::Admit { .. }
            ));
        }
    }

    /// The reply arrived (the step the guard names completed), the
    /// source closed or was cancelled, or it is another kind: suppressed
    /// with the exact observed state.
    #[test]
    fn a_false_guard_suppresses_with_what_it_observed() {
        let s = source();
        let answered = [step_of(&s, "sent", StepStatus::Completed)];
        let Judgement::Suppress { reason, observed } = judge(&guard(), Some(&s), &answered) else {
            panic!("an answered inquiry suppresses");
        };
        assert!(reason.contains("completed"), "{reason}");
        assert_eq!(observed["steps"][0]["status"], "completed");
        assert_eq!(observed["source"]["status"], "open");
        let waiting = [step_of(&s, "sent", StepStatus::Ready)];
        for status in [JobStatus::Closed, JobStatus::Cancelled] {
            let mut closed = source();
            closed.status = status;
            assert!(matches!(
                judge(&guard(), Some(&closed), &waiting),
                Judgement::Suppress { .. }
            ));
        }
        let mut other = source();
        other.kind = "backlog-item".into();
        assert!(matches!(
            judge(&guard(), Some(&other), &waiting),
            Judgement::Suppress { .. }
        ));
    }

    /// Absence is not a negative fact: a source Jobs does not hold, a
    /// named step that is missing or doubled, a draft source — held.
    #[test]
    fn missing_or_ambiguous_evidence_is_held_never_guessed() {
        let s = source();
        assert!(matches!(judge(&guard(), None, &[]), Judgement::Held { .. }));
        assert!(matches!(
            judge(&guard(), Some(&s), &[]),
            Judgement::Held { .. }
        ));
        let two = [
            step_of(&s, "sent", StepStatus::Ready),
            step_of(&s, "sent", StepStatus::Ready),
        ];
        assert!(matches!(
            judge(&guard(), Some(&s), &two),
            Judgement::Held { .. }
        ));
        let mut draft = source();
        draft.status = JobStatus::Draft;
        assert!(matches!(
            judge(
                &guard(),
                Some(&draft),
                &[step_of(&draft, "sent", StepStatus::Ready)]
            ),
            Judgement::Held { .. }
        ));
        // A step of the right slug on ANOTHER packet is not the source's.
        let elsewhere = Job {
            id: JobId::from_uuid(uuid(0xa2)),
            ..source()
        };
        assert!(matches!(
            judge(
                &guard(),
                Some(&s),
                &[step_of(&elsewhere, "sent", StepStatus::Ready)]
            ),
            Judgement::Held { .. }
        ));
    }

    #[test]
    fn the_decided_due_step_carries_its_protocol_fields_and_its_actor() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let b = Binding::from_metadata(&intent.metadata).unwrap();
        let now = due_at() + TimeDelta::hours(5);
        let r = request(now);
        let s = source();
        let j = judge(
            &guard(),
            Some(&s),
            &[step_of(&s, "sent", StepStatus::Ready)],
        );
        let Decision::Admit { reconciled, due } = decide(j, &due, &b, None, &r) else {
            panic!("admit");
        };
        assert!(!reconciled);
        assert_eq!(due.status, StepStatus::Completed);
        assert_eq!(due.completed_at, Some(now));
        assert_eq!(due.completed_by, Some(r.actor.clone()));
        assert_eq!(due.metadata["result"], "admitted");
        assert_eq!(due.metadata["child_id"], json!(b.child_id));
        assert_eq!(due.metadata["due_at"], "2026-11-15T12:00:00Z");
        assert_eq!(due.metadata["evaluated_at"], "2026-11-15T17:00:00.000000Z");
        let observed: Value =
            serde_json::from_str(due.metadata["observed"].as_str().unwrap()).unwrap();
        assert_eq!(observed["source"]["id"], json!(s.id));
    }

    /// A retry after the child closed reconciles the exact child; any
    /// other packet under the id is refused, never overwritten.
    #[test]
    fn an_existing_child_is_reconciled_only_when_it_is_the_envelopes() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let b = Binding::from_metadata(&intent.metadata).unwrap();
        let r = request(due_at());
        let s = source();
        let admit = || {
            judge(
                &guard(),
                Some(&s),
                &[step_of(&s, "sent", StepStatus::Ready)],
            )
        };
        let mut closed = child();
        closed.status = JobStatus::Closed;
        let Decision::Admit { reconciled, due: d } = decide(admit(), &due, &b, alone(&closed), &r)
        else {
            panic!("reconciled");
        };
        assert!(reconciled);
        assert_eq!(d.metadata["reconciled"], true);
        let mut stranger = child();
        stranger.kind = "backlog-item".into();
        assert!(matches!(
            decide(admit(), &due, &b, alone(&stranger), &r),
            Decision::Answer(Outcome::Refused { .. })
        ));
        // Same kind, version and partition, but not the request's child
        // on a field the caller decides (review d6774dfd, B1): refused,
        // naming the field.
        let mutations: [(&str, fn(&mut Job)); 5] = [
            ("subject", |j| j.subject = Subject::new("custom", "other")),
            ("title", |j| j.title = "planted".into()),
            ("owner_id", |j| j.owner_id = "emp-mallory".into()),
            ("tags", |j| j.tags = vec!["planted".into()]),
            ("metadata", |j| j.metadata = json!({"x": 1})),
        ];
        for (field, m) in mutations {
            let mut r = request(due_at());
            if field == "metadata" {
                r.child.metadata = json!({"x": 2});
            }
            let mut planted = child();
            m(&mut planted);
            let decided = decide(admit(), &due, &b, alone(&planted), &r);
            assert!(
                matches!(&decided, Decision::Answer(Outcome::Refused { reason }) if reason.contains(field)),
                "{field}: {decided:?}"
            );
        }
        // Keys the server stamps beside the caller's are not the caller's.
        let mut stamped = child();
        stamped.metadata = json!({"opened_by": "automation:dispatcher"});
        assert!(matches!(
            decide(admit(), &due, &b, alone(&stamped), &r),
            Decision::Admit {
                reconciled: true,
                ..
            }
        ));
    }

    /// An existing packet with no steps, for a request that asks for none.
    fn alone(job: &Job) -> Option<Existing<'_>> {
        Some(Existing { job, steps: &[] })
    }

    /// The child graph `write` then `send`, `send` blocked by `write`,
    /// minted afresh on each call as a materialization mints it.
    fn graph(child: &Job) -> Vec<Step> {
        let write = step_of(child, "write", StepStatus::Ready);
        let mut send = step_of(child, "send", StepStatus::Pending);
        send.sort_order = 1;
        send.blocked_by = vec![write.id];
        vec![write, send]
    }

    /// The exact child reconciles only when its step graph is the
    /// requested one in shape (review b12f08eb, P1): fresh step ids and
    /// work done since admission do not matter; a missing, extra or
    /// reshaped step, or an edge to another step, does.
    #[test]
    fn an_existing_child_reconciles_only_with_the_requested_step_graph() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let b = Binding::from_metadata(&intent.metadata).unwrap();
        let s = source();
        let admit = || {
            judge(
                &guard(),
                Some(&s),
                &[step_of(&s, "sent", StepStatus::Ready)],
            )
        };
        let mut r = request(due_at());
        r.child_steps = graph(&r.child);
        let mut closed = child();
        closed.status = JobStatus::Closed;
        let mut worked = graph(&closed);
        for st in &mut worked {
            st.status = StepStatus::Completed;
            st.assignee_id = Some("emp-sales".into());
        }
        let decided = decide(
            admit(),
            &due,
            &b,
            Some(Existing {
                job: &closed,
                steps: &worked,
            }),
            &r,
        );
        assert!(
            matches!(
                decided,
                Decision::Admit {
                    reconciled: true,
                    ..
                }
            ),
            "{decided:?}"
        );
        let reshapes: [(&str, fn(&mut Vec<Step>)); 6] = [
            ("none of its steps", |g| g.clear()),
            ("one missing", |g| {
                g.pop();
            }),
            ("one extra", |g| {
                let extra = step_of(&child(), "extra", StepStatus::Pending);
                g.push(extra);
            }),
            ("a retitled step", |g| g[0].title = "planted".into()),
            ("another kind", |g| g[1].kind = "sign-off".into()),
            ("an edge to itself", |g| g[1].blocked_by = vec![g[1].id]),
        ];
        for (what, reshape) in reshapes {
            let mut planted = graph(&closed);
            reshape(&mut planted);
            let decided = decide(
                admit(),
                &due,
                &b,
                Some(Existing {
                    job: &closed,
                    steps: &planted,
                }),
                &r,
            );
            assert!(
                matches!(&decided, Decision::Answer(Outcome::Refused { reason }) if reason.contains("step graph")),
                "{what}: {decided:?}"
            );
        }
        assert_eq!(graph_differences(&graph(&closed), &graph(&closed)), None);
    }

    /// An occupied child identity is judged before the guard's verdict
    /// (review b12f08eb, P2): a stranger is refused whether the guard
    /// admits, suppresses or holds, and even the exact child is refused
    /// when the guard suppresses, because neither terminal would be true.
    #[test]
    fn an_occupied_child_identity_is_judged_before_the_guard() {
        let intent = intent();
        let due = step_of(&intent, DUE_STEP, StepStatus::Ready);
        let b = Binding::from_metadata(&intent.metadata).unwrap();
        let r = request(due_at());
        let s = source();
        let suppress = || {
            judge(
                &guard(),
                Some(&s),
                &[step_of(&s, "sent", StepStatus::Completed)],
            )
        };
        let held = || judge(&guard(), None, &[]);
        assert!(matches!(suppress(), Judgement::Suppress { .. }));
        assert!(matches!(held(), Judgement::Held { .. }));
        let mut stranger = child();
        stranger.kind = "backlog-item".into();
        for judged in [suppress(), held()] {
            let decided = decide(judged, &due, &b, alone(&stranger), &r);
            assert!(
                matches!(&decided, Decision::Answer(Outcome::Refused { reason }) if reason.contains("backlog-item")),
                "{decided:?}"
            );
        }
        let decided = decide(suppress(), &due, &b, alone(&child()), &r);
        assert!(
            matches!(&decided, Decision::Answer(Outcome::Refused { reason }) if reason.contains("already stands")),
            "{decided:?}"
        );
        assert!(matches!(
            decide(suppress(), &due, &b, None, &r),
            Decision::Suppress { .. }
        ));
    }
}
