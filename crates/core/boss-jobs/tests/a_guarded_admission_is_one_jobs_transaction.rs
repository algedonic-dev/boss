//! Guarded admission on the in-memory adapter (backlog 84a010cb, car 3;
//! design f1d66249). The scenarios are the port's contract, shared with
//! the Postgres adapter (`guarded_admission/mod.rs`); this file adds what
//! only this adapter can inject — an unreadable guard read — and reads
//! the events it recorded.

#[path = "guarded_admission/mod.rs"]
mod scenarios;

use boss_jobs::InMemoryJobs;
use boss_jobs::events::{JOB_CREATED, STEP_CREATED, STEP_UPDATED};
use scenarios::*;

#[tokio::test]
async fn a_due_unanswered_intent_admits_its_child_and_decides() {
    let repo = InMemoryJobs::new();
    scenarios::a_due_unanswered_intent_admits_its_child_and_decides(&repo).await;
    // The child's facts and the decision's ride together.
    let events = repo.recorded_events();
    let after_plant = &events[6..];
    let kinds: Vec<&str> = after_plant.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [JOB_CREATED, STEP_CREATED, STEP_CREATED, STEP_UPDATED]
    );
}

#[tokio::test]
async fn nothing_is_written_before_the_due_instant() {
    let repo = InMemoryJobs::new();
    scenarios::nothing_is_written_before_the_due_instant(&repo).await;
    assert_eq!(repo.recorded_events().len(), 6, "the plant's events only");
}

#[tokio::test]
async fn an_answered_inquiry_suppresses_and_admits_no_child() {
    let repo = InMemoryJobs::new();
    scenarios::an_answered_inquiry_suppresses_and_admits_no_child(&repo).await;
    let events = repo.recorded_events();
    assert!(
        events
            .iter()
            .all(|e| e.kind != JOB_CREATED || e.payload["id"] != serde_json::json!(id(CHILD))),
        "no child fact"
    );
}

#[tokio::test]
async fn a_closed_source_suppresses() {
    scenarios::a_closed_source_suppresses(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn unrelated_work_does_not_move_the_anchor() {
    scenarios::unrelated_work_does_not_move_the_anchor(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn a_retry_after_the_decision_writes_nothing_more() {
    let repo = InMemoryJobs::new();
    scenarios::a_retry_after_the_decision_writes_nothing_more(&repo).await;
    let created = repo
        .recorded_events()
        .iter()
        .filter(|e| e.kind == JOB_CREATED && e.payload["id"] == serde_json::json!(id(CHILD)))
        .count();
    assert_eq!(created, 1, "one child fact across both attempts");
}

#[tokio::test]
async fn an_existing_closed_child_is_reconciled_not_admitted_twice() {
    scenarios::an_existing_closed_child_is_reconciled_not_admitted_twice(&InMemoryJobs::new())
        .await;
}

#[tokio::test]
async fn a_stranger_under_the_child_id_is_refused() {
    scenarios::a_stranger_under_the_child_id_is_refused(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn a_same_kind_packet_about_another_subject_under_the_child_id_is_refused() {
    scenarios::a_same_kind_packet_about_another_subject_under_the_child_id_is_refused(
        &InMemoryJobs::new(),
    )
    .await;
}

#[tokio::test]
async fn a_child_without_its_step_graph_is_refused_not_reconciled() {
    scenarios::a_child_without_its_step_graph_is_refused_not_reconciled(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn a_conflicting_child_is_refused_even_when_the_guard_suppresses() {
    scenarios::a_conflicting_child_is_refused_even_when_the_guard_suppresses(&InMemoryJobs::new())
        .await;
}

#[tokio::test]
async fn a_child_step_of_another_job_is_refused() {
    scenarios::a_child_step_of_another_job_is_refused(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn a_missing_guard_step_is_held() {
    scenarios::a_missing_guard_step_is_held(&InMemoryJobs::new()).await;
}

#[tokio::test]
async fn a_child_the_envelope_does_not_name_is_refused() {
    scenarios::a_child_the_envelope_does_not_name_is_refused(&InMemoryJobs::new()).await;
}

/// The guard cannot be read: the attempt errs loudly and writes nothing.
/// Never read as "no answer arrived".
#[tokio::test]
async fn an_unreadable_guard_admits_nothing_and_decides_nothing() {
    use boss_jobs::JobsRepository;
    for fault in ["job", "steps"] {
        let repo = InMemoryJobs::new();
        let w = plant(&repo).await;
        match fault {
            "job" => repo.fail_job_read(&id(SOURCE)),
            _ => repo.fail_steps_read(&id(SOURCE)),
        }
        let err = repo
            .guarded_admission_at(&request(due_at()), &due_events(due_at()))
            .await
            .expect_err("an unreadable guard is an error, never a verdict");
        assert!(err.to_string().contains("unreadable"), "{fault}: {err}");
        nothing_written(&repo, &w).await;
    }
}
