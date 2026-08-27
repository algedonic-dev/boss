//! Boot-time quarantine of unviable ACTIVE Workflows.
//!
//! Every active Workflow must pass the viability lint
//! ([`crate::workflow_lint`]) — a spec whose graph can't reach an
//! outcome describes work no Job can finish, and a previously-valid
//! spec can go bad when an upstream StepType's enum domain changes.
//!
//! **What this pass used to do:** refuse to start. One bad registry
//! row therefore held the whole service hostage — and on 2026-08-13
//! it did: a `protocol-retro` row with no terminal published cleanly,
//! lay latent until a routine deploy rolled the pod, and then took
//! jobs, docs, the gateway, and the human door down for ~11 minutes.
//! Recovery needed direct SQL.
//!
//! **What it does now:** log the problems at ERROR, retire the
//! offending row through the registry's own transactional path (so
//! `jobs.kind.retired` witnesses it like any other retirement), emit
//! one loud `jobs.kind.quarantined` marker carrying the problems, and
//! let the service start. The blast radius of a bad row is now that
//! one Workflow instead of the entire API.
//!
//! **The one case still worth refusing for:** open Jobs pinned to the
//! offending row. Retiring it would strand live work, which is worse
//! than a loud outage — so that case keeps the old behaviour, with a
//! message naming the Workflow and the open-Job count.
//!
//! The publish gate ([`crate::workflow_lint::gate_active`], Layer 1)
//! makes this pass rare BY CONSTRUCTION: no API path can set an
//! unviable row active any more. Quarantine exists for rows that
//! predate the gate, rows written by direct SQL, and specs that went
//! bad under a StepType change — not for anything publish can still
//! let through.

use chrono::{DateTime, Utc};

use crate::port::JobsRepository;
use crate::registry::{WorkflowRegistry, WorkflowSpec};
use crate::step_registry::StepRegistry;
use crate::workflow_lint::{WorkflowLintError, validate_workflow};

/// The actor every quarantine write is stamped with — a named
/// platform automation, so the log says who retired the row.
pub const QUARANTINE_ACTOR: &str = "workflow-quarantine";

/// A Workflow row this pass retired.
#[derive(Debug, Clone)]
pub struct Quarantined {
    pub kind: String,
    pub version: i32,
    pub problems: Vec<WorkflowLintError>,
}

/// An unviable Workflow row this pass REFUSED to retire, because
/// open Jobs are pinned to it.
#[derive(Debug, Clone)]
pub struct Stranded {
    pub kind: String,
    pub version: i32,
    /// Non-terminal Jobs pinned to this exact `(kind, version)`.
    pub open_jobs: i64,
    pub problems: Vec<WorkflowLintError>,
}

/// What one quarantine pass did.
#[derive(Debug, Clone, Default)]
pub struct QuarantineReport {
    /// Active Workflows examined.
    pub checked: usize,
    pub quarantined: Vec<Quarantined>,
    pub stranded: Vec<Stranded>,
}

impl QuarantineReport {
    /// May the service start? Yes unless auto-retiring would have
    /// stranded live work.
    pub fn may_start(&self) -> bool {
        self.stranded.is_empty()
    }

    /// The operator-facing reason to refuse, naming every Workflow
    /// that blocked the start and how much live work sits on it.
    /// `None` when the service may start.
    pub fn refusal_message(&self) -> Option<String> {
        if self.stranded.is_empty() {
            return None;
        }
        let detail = self
            .stranded
            .iter()
            .map(|s| {
                format!(
                    "`{}` v{} ({} open job(s) pinned; {})",
                    s.kind,
                    s.version,
                    s.open_jobs,
                    s.problems
                        .iter()
                        .map(|p| p.reason.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "refusing to start: {} unviable active Workflow(s) have open Jobs pinned to them, \
             so quarantine would strand live work: {detail}. Fix the spec (publish a viable \
             version) or close/migrate the open Jobs, then restart.",
            self.stranded.len()
        ))
    }
}

/// Run one quarantine pass over every active Workflow.
///
/// Retires each unviable row that has no open Jobs pinned to it and
/// records a `jobs.kind.quarantined` marker for it; collects the rest
/// as [`Stranded`] for the caller to refuse on. `Err` is reserved for
/// the pass itself failing (registry unreachable, retire write
/// failed) — the caller cannot tell whether the registry is clean, so
/// it must not start.
pub async fn quarantine_unviable_active_workflows<R: JobsRepository + ?Sized>(
    registry: &dyn WorkflowRegistry,
    jobs: &R,
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
) -> Result<QuarantineReport, String> {
    let active = registry
        .list_active(None)
        .await
        .map_err(|e| format!("could not list active Workflows: {e}"))?;

    let step_types = StepRegistry::v1();
    let mut report = QuarantineReport {
        checked: active.len(),
        ..Default::default()
    };

    for spec in &active {
        let problems = validate_workflow(spec, &step_types);
        if problems.is_empty() {
            continue;
        }
        // Log exactly as the refuse-to-start path always did — the
        // problems are the operator's first clue either way.
        for p in &problems {
            tracing::error!("boot viability check: {p}");
        }

        let open_jobs = jobs
            .count_open_jobs_for_workflow(&spec.kind, spec.version)
            .await
            .map_err(|e| {
                format!(
                    "could not count open Jobs pinned to `{}` v{}: {e}",
                    spec.kind, spec.version
                )
            })?;

        if open_jobs > 0 {
            tracing::error!(
                kind = %spec.kind,
                version = spec.version,
                open_jobs,
                "unviable active Workflow has open Jobs pinned to it — refusing to quarantine it \
                 (auto-retiring would strand live work)"
            );
            report.stranded.push(Stranded {
                kind: spec.kind.clone(),
                version: spec.version,
                open_jobs,
                problems,
            });
            continue;
        }

        retire_and_mark(registry, jobs, spec, &problems, actor, now).await?;
        report.quarantined.push(Quarantined {
            kind: spec.kind.clone(),
            version: spec.version,
            problems,
        });
    }

    if report.quarantined.is_empty() && report.stranded.is_empty() {
        tracing::info!(active = report.checked, "boot viability check passed");
    }
    Ok(report)
}

/// Retire the row through the registry (recording
/// `jobs.kind.retired` in the same transaction as the flip) and
/// record the loud `jobs.kind.quarantined` marker.
async fn retire_and_mark<R: JobsRepository + ?Sized>(
    registry: &dyn WorkflowRegistry,
    jobs: &R,
    spec: &WorkflowSpec,
    problems: &[WorkflowLintError],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
) -> Result<(), String> {
    registry
        .retire(&spec.kind, actor, now)
        .await
        .map_err(|e| format!("could not quarantine `{}`: {e}", spec.kind))?;

    let event = crate::events::workflow_quarantined_event(actor, spec, problems);
    jobs.record_events(std::slice::from_ref(&event))
        .await
        .map_err(|e| {
            format!(
                "quarantined `{}` but could not record the marker event: {e}",
                spec.kind
            )
        })?;

    tracing::error!(
        kind = %spec.kind,
        version = spec.version,
        problems = problems.len(),
        "QUARANTINED unviable active Workflow — retired it and continued starting"
    );
    Ok(())
}
