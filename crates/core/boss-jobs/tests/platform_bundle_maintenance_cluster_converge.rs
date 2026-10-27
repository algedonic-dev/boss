//! `maintenance-cluster-converge` records which trigger started the run
//! (backlog e4d51e12).
//!
//! The protocol had one trigger, `scheduled` (`trigger_name =
//! "systemd-timer"`), so every converge packet read as a timer run —
//! including the merge-started ones converge-on-merge asks for, median
//! 36 s after the merge (measured on run 73832267 across 191 trains;
//! again 2026-10-20: packet 57fa9ab8, opened 3 s after its ops-request
//! a7b45bdf, read `systemd-timer`). A second trigger, `requested`, is
//! the alternative the wrap names when an ops-request asked for the run
//! (`metadata.trigger_name = "ops-request"`, stamped by
//! infra/boss-maintenance-wrap.sh from the converge's request inbox,
//! pinned in boss-testing's a_requested_converge_names_its_request.rs);
//! admission fires that one and skips the timer, and the run waits on
//! either.

use boss_core::job::{JobId, StepId, StepStatus, Subject};
use boss_jobs::registry::{WorkflowSpec, materialize_steps_at, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;

const KIND: &str = "maintenance-cluster-converge";

fn converge() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == KIND)
        .expect("the bundle carries maintenance-cluster-converge")
}

/// The steps a packet is born with, given the metadata the wrap POSTs —
/// through the same materialisation the admission handler runs, trigger
/// resolution included.
fn born(job_metadata: serde_json::Value) -> Vec<(String, StepStatus)> {
    let spec = converge();
    let reg = StepRegistry::v1();
    materialize_steps_at(
        &spec,
        &Subject::new("custom", format!("infra/{KIND}")),
        JobId::new(),
        &job_metadata,
        StepId::new,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 20),
        Some(&reg),
    )
    .into_iter()
    .map(|s| (s.spec_slug.unwrap_or_default(), s.status))
    .collect()
}

fn status(steps: &[(String, StepStatus)], slug: &str) -> StepStatus {
    steps
        .iter()
        .find(|(s, _)| s == slug)
        .unwrap_or_else(|| panic!("`{slug}` materialised: {steps:?}"))
        .1
}

#[test]
fn a_merge_started_run_never_reads_as_a_timer_run() {
    let steps = born(serde_json::json!({
        "chore": KIND,
        "host": "forge",
        "trigger_name": "ops-request",
        "ops_requests": ["a7b45bdf-57be-4453-a434-587dc8b2d0b6"],
    }));
    assert_eq!(status(&steps, "requested"), StepStatus::Completed);
    assert_eq!(
        status(&steps, "scheduled"),
        StepStatus::Skipped,
        "the timer did not fire this run"
    );
    assert_eq!(
        status(&steps, "run"),
        StepStatus::Ready,
        "the run waits on either trigger"
    );
}

#[test]
fn a_timer_run_reads_as_a_timer_run() {
    let steps = born(serde_json::json!({ "chore": KIND, "host": "forge" }));
    assert_eq!(status(&steps, "scheduled"), StepStatus::Completed);
    assert_eq!(status(&steps, "requested"), StepStatus::Skipped);
    assert_eq!(status(&steps, "run"), StepStatus::Ready);
}

/// The timer trigger stays FIRST: a packet that names no trigger — every
/// timer run, a hand start, a wrap from before this car — fires the
/// first trigger, and that must remain the timer.
#[test]
fn the_timer_is_the_trigger_named_by_no_one() {
    let spec = converge();
    let triggers: Vec<(&str, Option<&str>)> = spec
        .steps
        .iter()
        .filter(|s| s.kind == "trigger")
        .map(|s| {
            (
                s.title.as_str(),
                s.metadata_defaults
                    .get("trigger_name")
                    .and_then(|v| v.as_str()),
            )
        })
        .collect();
    assert_eq!(
        triggers,
        vec![
            ("scheduled", Some("systemd-timer")),
            ("requested", Some("ops-request")),
        ]
    );
}
