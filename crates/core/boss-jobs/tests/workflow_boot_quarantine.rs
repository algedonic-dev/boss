//! Boot-time quarantine — blast-radius control for the 2026-08-13
//! outage.
//!
//! Before: ANY active Workflow failing the viability lint made
//! `boss-jobs-api` refuse to start. One bad registry row therefore
//! took down jobs, docs, the gateway and the human door on the next
//! routine pod roll, and recovery needed direct SQL.
//!
//! After: boot logs the problems at ERROR, retires the offending
//! row(s) through the registry's own transactional path (so the log
//! witnesses the retirement), emits one loud `jobs.kind.quarantined`
//! marker per row, and CONTINUES STARTING — unless retiring would
//! strand live work, which is the one case still worth refusing for.

use std::sync::Arc;

use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_jobs::events::{WORKFLOW_QUARANTINED, WORKFLOW_RETIRED};
use boss_jobs::registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus,
};
use boss_jobs::workflow_quarantine::quarantine_unviable_active_workflows;
use boss_jobs::{InMemoryJobs, JobsRepository};
use chrono::NaiveDate;

fn viable(kind: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        "Viable",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

/// The incident row: steps, no terminal. Arrives by direct SQL or
/// predates the publish gate — Layer 1 makes this rare, not
/// impossible.
fn unviable(kind: &str) -> WorkflowSpec {
    let mut spec = viable(kind);
    spec.steps[1].terminal = None;
    spec
}

async fn seed_open_job(jobs: &InMemoryJobs, kind: &str, version: i32, status: JobStatus) {
    let mut job = Job::new(
        kind,
        Subject::new("custom", "thing-1"),
        "Live work",
        "emp-1",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
    );
    job.workflow_version = version;
    job.status = status;
    jobs.create_job(&job).await.unwrap();
}

#[tokio::test]
async fn quarantine_retires_the_unviable_row_and_lets_boot_continue() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(viable("healthy")).unwrap();
    registry.seed(unviable("protocol-retro")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let actor = boss_core::actor::ActorId::Automation("workflow-quarantine".into());
    let now = chrono::Utc::now();

    let report =
        quarantine_unviable_active_workflows(registry.as_ref(), jobs.as_ref(), &actor, now)
            .await
            .expect("quarantine pass completes");

    assert!(
        report.stranded.is_empty(),
        "no open jobs → nothing to refuse for"
    );
    assert!(report.may_start(), "boot must continue");
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.quarantined[0].kind, "protocol-retro");
    assert!(
        report.quarantined[0]
            .problems
            .iter()
            .any(|p| p.reason.contains("no terminal")),
        "the report carries the problems the log printed"
    );

    // The row is retired through the registry's own path, so the
    // retirement is in the log like any other.
    assert!(
        registry.get_active("protocol-retro").await.is_err(),
        "the offending row must no longer be active"
    );
    assert!(
        registry.get_active("healthy").await.is_ok(),
        "a viable row is untouched"
    );
    assert!(
        registry
            .recorded_events()
            .iter()
            .any(|e| e.kind == WORKFLOW_RETIRED),
        "the registry records the retirement"
    );

    // One loud marker per quarantined workflow.
    let markers: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == WORKFLOW_QUARANTINED)
        .collect();
    assert_eq!(markers.len(), 1, "exactly one marker per quarantined row");
    assert_eq!(markers[0].payload["kind"], "protocol-retro");
    assert_eq!(markers[0].payload["version"], 1);
    assert_eq!(
        markers[0].payload["_actor"],
        "automation:workflow-quarantine"
    );
    let problems = markers[0].payload["problems"]
        .as_array()
        .expect("problems array");
    assert!(
        problems
            .iter()
            .filter_map(|p| p["message"].as_str())
            .any(|m| m.contains("no terminal")),
        "the marker names why: {problems:?}"
    );
}

#[tokio::test]
async fn quarantine_refuses_to_strand_open_jobs_pinned_to_the_row() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(unviable("protocol-retro")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    // Two open Jobs pinned to v1 of the offending Workflow, plus a
    // closed one that does not count.
    seed_open_job(&jobs, "protocol-retro", 1, JobStatus::Open).await;
    seed_open_job(&jobs, "protocol-retro", 1, JobStatus::Blocked).await;
    seed_open_job(&jobs, "protocol-retro", 1, JobStatus::Closed).await;
    let actor = boss_core::actor::ActorId::Automation("workflow-quarantine".into());
    let now = chrono::Utc::now();

    let report =
        quarantine_unviable_active_workflows(registry.as_ref(), jobs.as_ref(), &actor, now)
            .await
            .expect("quarantine pass completes");

    assert!(
        report.quarantined.is_empty(),
        "auto-retiring would strand live work"
    );
    assert_eq!(report.stranded.len(), 1);
    assert_eq!(report.stranded[0].kind, "protocol-retro");
    assert_eq!(report.stranded[0].open_jobs, 2);
    assert!(
        !report.may_start(),
        "this is the one case that still refuses to start"
    );
    let msg = report.refusal_message().expect("a refusal names the row");
    assert!(
        msg.contains("protocol-retro") && msg.contains('2'),
        "refusal must name the workflow and the open-job count: {msg}"
    );

    // Nothing was retired, nothing was marked.
    assert_eq!(
        registry
            .get_active("protocol-retro")
            .await
            .expect("still active")
            .status,
        WorkflowStatus::Active
    );
    assert!(
        jobs.recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_QUARANTINED)
    );
}

#[tokio::test]
async fn a_clean_registry_quarantines_nothing() {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(viable("healthy")).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let actor = boss_core::actor::ActorId::Automation("workflow-quarantine".into());

    let report = quarantine_unviable_active_workflows(
        registry.as_ref(),
        jobs.as_ref(),
        &actor,
        chrono::Utc::now(),
    )
    .await
    .expect("quarantine pass completes");

    assert_eq!(report.checked, 1);
    assert!(report.quarantined.is_empty() && report.stranded.is_empty());
    assert!(report.may_start());
    assert!(jobs.recorded_events().is_empty());
}
