//! `infra/platform/workflows/delayed-admission.toml` — the durable
//! packet a delayed rule's pending work rides in, and the immutable
//! envelope that packet carries (car 2 of the finish line on backlog
//! 84a010cb, design f1d66249, folded into docs/architecture-decisions.md
//! "Durable source-relative guarded admission").
//!
//! The design: "Pending work is a generic `delayed-admission` Job running
//! a versioned platform protocol: immutable intent, due/execution step,
//! admitted, suppressed or cancelled terminals, and nonterminal held
//! attempts ... Ordinary Job/step writes must not change these
//! bindings." So the envelope is ONE job-metadata key the row declares in
//! `metadata_schema.immutable_fields`, and the protection is the adapter's
//! existing protocol declaration, judged against the packet's PINNED
//! version on every write — not a new branch in core code. These tests
//! drive each job write door of the in-memory adapter against the row as
//! shipped, and the conversion verdict against a successor that would
//! drop the declaration.
//!
//! What the row does NOT do yet: nothing files a `delayed-admission`
//! packet, so `authoring::validate` still refuses every rule declaring a
//! delay (cars 3-6 of that finish line).

use std::collections::BTreeMap;
use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobStatus, Priority, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::conditional_metadata::Command;
use boss_jobs::protocol_conversion::convertibility_for_packet;
use boss_jobs::registry::{InMemoryWorkflows, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::{InMemoryJobs, JobsRepository};
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

const KIND: &str = "delayed-admission";
const ENVELOPE: &str = "envelope";

fn bundled() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == KIND)
        .unwrap_or_else(|| panic!("{KIND} ships in the platform bundle"))
}

/// The design's shape: an intent captured, one execution step that may
/// stay open across held attempts, and exactly three ways out, each
/// selected by the result that step records.
#[test]
fn a_delayed_admission_is_captured_executed_once_and_ends_three_ways() {
    let wf = bundled();
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("{KIND} has no `{title}` step"))
    };
    assert_eq!(step("captured").kind, "trigger");
    let due = step("due");
    assert_eq!(due.ready_when, "steps.captured.done");
    for name in ["result", "evaluated_at", "observed"] {
        let f = due
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("due has no `{name}` field"));
        assert!(f.required, "{name} is required at done");
    }
    let result = due.fields.iter().find(|f| f.name == "result").unwrap();
    assert_eq!(result.field_type, "admitted|suppressed|cancelled");
    let terminals: Vec<(&str, &str)> = wf
        .steps
        .iter()
        .filter_map(|s| {
            s.terminal
                .as_ref()
                .map(|t| (t.outcome.as_str(), s.ready_when.as_str()))
        })
        .collect();
    assert_eq!(
        terminals,
        [
            (
                "admitted",
                "steps.due.done AND steps.due.metadata.result = \"admitted\""
            ),
            (
                "suppressed",
                "steps.due.done AND steps.due.metadata.result = \"suppressed\""
            ),
            (
                "cancelled",
                "steps.due.done AND steps.due.metadata.result = \"cancelled\""
            ),
        ]
    );
}

/// The envelope is declared immutable by the row itself — registry
/// data, read by the adapters' existing protocol check.
#[test]
fn the_row_declares_its_envelope_immutable() {
    assert_eq!(
        bundled().metadata_schema["immutable_fields"],
        json!([ENVELOPE])
    );
}

fn store() -> Arc<InMemoryJobs> {
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(bundled()).expect("the shipped row seeds");
    Arc::new(InMemoryJobs::new().with_workflows(registry))
}

fn packet() -> Job {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", "delayed-admission"),
        "follow up an unanswered inquiry",
        "automation:dispatcher",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
    );
    job.status = JobStatus::Open;
    job.metadata = json!({"note": "first"});
    job
}

fn envelope() -> Value {
    json!({
        "contract_version": 1,
        "intent_id": "6c1f9a0e-0000-5000-8000-000000000001",
        "child_id": "6c1f9a0e-0000-5000-8000-000000000002",
        "source_event": {"id": "e-1", "kind": "jobs.step.completed"},
        "anchor": "2026-11-06T12:00:00Z",
        "delay_seconds": 604800,
        "due_at": "2026-11-13T12:00:00Z",
        "rule": {"name": "follow-up", "version": 1},
    })
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::human("automation:dispatcher"))
}

fn bind(value: Value, expected_present: Option<Value>) -> Command {
    let expected = match expected_present {
        None => json!({ENVELOPE: {"present": false}}),
        Some(old) => json!({ENVELOPE: {"present": true, "value": old}}),
    };
    serde_json::from_value(json!({
        "contract_version": 2,
        "idempotency_key": Uuid::new_v4(),
        "expected": expected,
        "patch": {ENVELOPE: value},
        "immutable_keys": [ENVELOPE],
    }))
    .expect("a well-formed conditional command")
}

/// A packet filed and its envelope bound through the one door that
/// may first-record it.
async fn bound() -> (Arc<InMemoryJobs>, Job) {
    let jobs = store();
    let job = packet();
    jobs.create_job(&job).await.expect("the packet is filed");
    jobs.conditional_job_metadata_at(&job.id, &bind(envelope(), None), None, &stamp())
        .await
        .expect("the envelope is first-recorded through the conditional door");
    let stored = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata[ENVELOPE], envelope());
    (jobs, stored)
}

async fn still_bound(jobs: &InMemoryJobs, job: &Job, door: &str) {
    let stored = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata[ENVELOPE],
        envelope(),
        "{door} changed the envelope"
    );
}

/// Admission cannot carry an envelope the conditional door did not bind:
/// the create body is refused whole, so no packet exists.
#[tokio::test]
async fn a_packet_filed_with_an_unbound_envelope_is_refused() {
    let jobs = store();
    let mut job = packet();
    job.metadata[ENVELOPE] = envelope();
    let err = jobs
        .create_job(&job)
        .await
        .expect_err("an envelope enters only through its conditional first record");
    assert!(err.to_string().contains(ENVELOPE), "names the key: {err}");
    assert!(jobs.get_job(&job.id).await.unwrap().is_none());
}

/// Every job write door refuses to replace or delete the envelope once
/// bound, and leaves it exactly as recorded; a write that leaves it
/// alone still lands (the positive control).
#[tokio::test]
async fn no_job_write_door_replaces_or_deletes_a_bound_envelope() {
    let (jobs, job) = bound().await;
    let mut other = envelope();
    other["due_at"] = json!("2026-11-07T12:00:00Z");

    // The whole-row save (the job PUT): a changed envelope, and a body
    // that omits it, are both refused.
    let mut changed = job.clone();
    changed.metadata[ENVELOPE] = other.clone();
    assert!(jobs.update_job(&changed).await.is_err(), "PUT replaced it");
    still_bound(&jobs, &job, "PUT").await;
    let mut omitted = job.clone();
    omitted.metadata.as_object_mut().unwrap().remove(ENVELOPE);
    assert!(jobs.update_job(&omitted).await.is_err(), "PUT dropped it");
    still_bound(&jobs, &job, "PUT without it").await;

    // The metadata merge (PATCH /api/jobs/{id}/metadata): a new value,
    // and null (which deletes), are both refused.
    for value in [other.clone(), Value::Null] {
        let patch = json!({ENVELOPE: value}).as_object().cloned().unwrap();
        assert!(
            jobs.merge_job_metadata_at(&job.id, &patch, None, &stamp())
                .await
                .is_err(),
            "the merge wrote {value}"
        );
        still_bound(&jobs, &job, "PATCH").await;
    }

    // The close, which writes its own keys onto the row.
    let owned = json!({ENVELOPE: other.clone()})
        .as_object()
        .cloned()
        .unwrap();
    assert!(
        jobs.close_job_at(
            &job.id,
            NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
            &owned,
            &stamp(),
            &|_| vec![],
        )
        .await
        .is_err(),
        "the close replaced it"
    );
    still_bound(&jobs, &job, "close").await;

    // The conditional door itself: a second first record, and an exact
    // compare-and-set from the bound value, are both refused.
    for command in [
        bind(other.clone(), None),
        bind(other.clone(), Some(envelope())),
    ] {
        assert!(
            jobs.conditional_job_metadata_at(&job.id, &command, None, &stamp())
                .await
                .is_err(),
            "the conditional door rewrote it"
        );
        still_bound(&jobs, &job, "conditional").await;
    }

    // Positive control: a write that leaves the envelope alone lands.
    let patch = json!({"note": "second"}).as_object().cloned().unwrap();
    let after = jobs
        .merge_job_metadata_at(&job.id, &patch, None, &stamp())
        .await
        .expect("an unrelated key still merges");
    assert_eq!(after.metadata["note"], "second");
    assert_eq!(after.metadata[ENVELOPE], envelope());
}

/// The re-pin door moves a packet onto another version of its protocol.
/// A successor that no longer declares the envelope immutable would
/// make a pinned packet's binding writable, so the move is refused
/// wherever the packet stands, not left automatic.
#[test]
fn a_successor_that_drops_the_envelope_declaration_does_not_convert() {
    let from = bundled();
    let mut to = from.clone();
    to.version = from.version + 1;
    to.metadata_schema = json!({"type": "object"});
    let at: BTreeMap<String, StepStatus> = [
        ("captured".to_string(), StepStatus::Completed),
        ("due".to_string(), StepStatus::Ready),
    ]
    .into();
    let verdict = convertibility_for_packet(&from, &to, &at);
    assert!(
        verdict
            .obstacles()
            .iter()
            .any(|o| o.step.is_none() && o.reason.contains(ENVELOPE)),
        "the dropped declaration is named: {verdict:?}"
    );
    // Control: the same version again converts automatically.
    assert!(convertibility_for_packet(&from, &from, &at).is_automatic());
}
