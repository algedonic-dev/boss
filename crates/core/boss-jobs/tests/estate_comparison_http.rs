//! `POST /api/estate/comparison` — the compare handler's one write.
//!
//! The estate comparison (59ef456a) is COMPUTED in the dispatcher's
//! `estate.compare` handler over the jobs API's own read surfaces —
//! handlers own no database — and lands here as one
//! `jobs.estate.compared` event per observation. Same dumb-door
//! posture as the observation door above it and the census door before
//! that: validate shape and trust, record verbatim, never recompute.
//!
//! Three properties pinned:
//! - an operator-tier caller's comparison lands as ONE actor-stamped
//!   event whose payload is the comparison;
//! - a non-operator caller is refused and nothing is recorded;
//! - a body without a `scope` string and a `findings` object is a 400
//!   — a comparison that cannot say what it compared or what it found
//!   is not a comparison.

use axum::http::StatusCode;
use boss_jobs::InMemoryJobs;
use std::sync::Arc;
#[path = "estate_comparison_http/common.rs"]
mod common;
use common::{
    app_with, comparison_body, exercise_observation_identity_boundary, post_comparison, user_header,
};

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    (app_with(jobs.clone()), jobs)
}

#[tokio::test]
async fn replay_of_one_observation_records_one_comparison() {
    let (app, jobs) = app();
    let user = user_header("rule:estate-compare-on-observation", "operator");
    let mut body: serde_json::Value = serde_json::from_str(&comparison_body()).unwrap();
    body["observation_id"] = serde_json::json!("a56cb17b-00ba-4f61-9dc3-01c48871968a");
    let body = body.to_string();
    let (a, b) = tokio::join!(
        post_comparison(&app, Some(&user), &body),
        post_comparison(&app, Some(&user), &body)
    );
    assert_eq!((a, b), (StatusCode::ACCEPTED, StatusCode::ACCEPTED));
    assert_eq!(
        jobs.recorded_events()
            .iter()
            .filter(|e| e.kind == "jobs.estate.compared")
            .count(),
        1,
        "a redelivered observation must not manufacture persistence"
    );
}

#[tokio::test]
async fn canonical_identity_deduplicates_one_fact_but_preserves_distinct_facts_in_memory() {
    let (app, jobs) = app();
    exercise_observation_identity_boundary(&app).await;
    let events = jobs.recorded_events();
    let compared: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.estate.compared")
        .collect();
    assert_eq!(compared.len(), 2);
    assert_eq!(
        compared[0].payload["observation_id"],
        "a56cb17b-00ba-4f61-9dc3-01c48871968a"
    );
    assert_eq!(
        compared[1].payload["observation_id"],
        "a56cb17b-00ba-4f61-9dc3-01c48871968b"
    );
}

#[tokio::test]
async fn an_operator_comparison_lands_as_one_actor_stamped_event() {
    let (app, jobs) = app();
    let status = post_comparison(
        &app,
        Some(&user_header(
            "rule:estate-compare-on-observation",
            "operator",
        )),
        &comparison_body(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let events = jobs.recorded_events();
    let compared: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "jobs.estate.compared")
        .collect();
    assert_eq!(compared.len(), 1, "exactly one event per comparison");
    let e = compared[0];
    assert_eq!(e.source, "jobs");
    assert_eq!(e.payload["scope"], "kubernetes-nodes");
    assert_eq!(e.payload["counts"]["observed_not_declared"], 1);
    assert_eq!(
        e.payload["findings"]["observed_not_declared"][0]["id"],
        "w-1"
    );
    assert_eq!(
        e.payload["_actor"],
        "automation:rule:estate-compare-on-observation"
    );
}

#[tokio::test]
async fn a_non_operator_caller_is_refused_and_nothing_is_recorded() {
    let (app, jobs) = app();
    let status = post_comparison(
        &app,
        Some(&user_header("emp-1", "user")),
        &comparison_body(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        jobs.recorded_events().is_empty(),
        "a refused comparison must record nothing"
    );
}

#[tokio::test]
async fn a_comparison_without_scope_or_findings_is_a_400() {
    let (app, jobs) = app();
    for body in [
        r#"{"findings": {}}"#,
        r#"{"scope": "kubernetes-nodes"}"#,
        r#"{"scope": 7, "findings": {}}"#,
        r#"{"scope": "kubernetes-nodes", "findings": []}"#,
        r#"[1,2,3]"#,
    ] {
        let status = post_comparison(
            &app,
            Some(&user_header(
                "rule:estate-compare-on-observation",
                "operator",
            )),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    }
    assert!(jobs.recorded_events().is_empty());
}
