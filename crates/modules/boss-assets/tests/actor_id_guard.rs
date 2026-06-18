//! Cross-service contract test for the assets `actor_id` guard.
//!
//! The guard stops `POST /api/assets/events` and
//! `POST /api/assets/events/batch` from accepting an unresolvable
//! `actor_id` (deleted, typo, made up). Without it the event lands in
//! `device_events`, the audit log captures a string nobody can
//! resolve, and downstream consumers can't tell whether the actor was
//! real.
//!
//! These tests pin the contract:
//!   - 201 when the actor is an automation (named, not FK-backed)
//!   - 201 when the actor is a human and the employee exists
//!   - 422 when the actor is a human and the employee does not exist
//!   - 503 when people is unreachable (fail-closed)
//!   - batch endpoint applies the same guard, dedup'd per actor

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use boss_assets::types::{AssetEvent, AssetEventId, AssetEventKind, AssetId, IntakeSource};
use boss_people_client::{FakePeopleClient, PeopleClient, UnreachablePeopleClient};
use boss_testing::TestRequest;
use chrono::NaiveDate;
use common::{AssetsTestApp, received_event};

fn evt_with_actor(id: &str, serial: &str, actor: Option<&str>) -> AssetEvent {
    // None → an automation actor (named, not FK-backed → guard exempts);
    // Some(emp) → Human (guard existence-checks against employees).
    let actor_id = match actor {
        Some(emp) => boss_core::actor::ActorId::human(emp),
        None => boss_core::actor::ActorId::automation("asset-intake"),
    };
    AssetEvent {
        id: AssetEventId::new(id),
        asset_id: AssetId::new(serial),
        ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        actor_id,
        kind: AssetEventKind::Received {
            sku: Some("Boss-TEST-2024".into()),
            source: IntakeSource::new("oem-new"),
            oem_serial: None,
        },
    }
}

#[tokio::test]
async fn post_event_with_known_actor_returns_201() {
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new().with_employee("emp-007"));
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let event = evt_with_actor("evt-actor-known", "SN-ACT-1", Some("emp-007"));
    TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_event_with_none_actor_is_always_accepted() {
    // Automation-generated events carry a named automation actor. Even
    // with a people client that knows zero employees, these go through.
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new());
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let event = evt_with_actor("evt-asset", "SN-ACT-2", None);
    TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_event_with_unknown_actor_returns_422() {
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new().with_employee("emp-007"));
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let event = evt_with_actor("evt-actor-unknown", "SN-ACT-3", Some("emp-99999"));
    let resp = TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["error"], "unknown actor");
    assert_eq!(body["actor_id"], "emp-99999");
}

#[tokio::test]
async fn post_event_returns_503_when_people_unreachable() {
    let people: Arc<dyn PeopleClient> = Arc::new(UnreachablePeopleClient);
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let event = evt_with_actor("evt-fail-closed", "SN-ACT-4", Some("emp-007"));
    let resp = TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["error"], "people service unreachable");
}

#[tokio::test]
async fn rejected_event_does_not_land_in_repository() {
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new());
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let event = evt_with_actor("evt-blocked", "SN-ACT-5", Some("emp-99999"));
    TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // Confirm via the public listing endpoint that no events landed.
    let list = TestRequest::get("/api/assets/ids").send(&app.router).await;
    list.assert_status(StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&list.body_bytes).unwrap();
    let data = body["data"].as_array().unwrap();
    assert!(
        data.is_empty(),
        "rejected event must not be persisted; got {data:?}"
    );
}

#[tokio::test]
async fn batch_with_one_unknown_actor_rejects_entire_batch() {
    // The batch guard is all-or-nothing: any unknown actor in the
    // batch rejects the whole call. Partial accept would leave the
    // audit log half-populated and confuse the caller.
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new().with_employee("emp-007"));
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let events = vec![
        evt_with_actor("b1", "SN-B-1", Some("emp-007")),
        evt_with_actor("b2", "SN-B-2", Some("emp-007")),
        evt_with_actor("b3", "SN-B-3", Some("emp-99999")),
    ];

    let resp = TestRequest::post("/api/assets/events/batch")
        .json(&events)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["error"], "unknown actor");
    assert_eq!(body["actor_id"], "emp-99999");

    // None of the batch's events should be in the repo.
    let list = TestRequest::get("/api/assets/ids").send(&app.router).await;
    let list_body: serde_json::Value = serde_json::from_slice(&list.body_bytes).unwrap();
    assert!(list_body["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn batch_with_all_known_actors_succeeds() {
    let people: Arc<dyn PeopleClient> = Arc::new(
        FakePeopleClient::new()
            .with_employee("emp-007")
            .with_employee("emp-042"),
    );
    let app = AssetsTestApp::with_events_and_people(vec![], people).await;

    let events = vec![
        evt_with_actor("ok1", "SN-OK-1", Some("emp-007")),
        evt_with_actor("ok2", "SN-OK-2", Some("emp-042")),
        // None is also fine even mixed in.
        evt_with_actor("ok3", "SN-OK-3", None),
    ];

    let resp = TestRequest::post("/api/assets/events/batch")
        .json(&events)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(body["inserted"], 3);
    assert_eq!(body["duplicates"], 0);
}

#[tokio::test]
async fn existing_received_event_helper_still_works() {
    // Smoke test: the convenience helper the rest of the test suite
    // uses (`received_event`) creates events with actor_id =
    // Some("emp-test"), and the default test app uses the
    // permissive AlwaysExistsPeople fake — so the existing tests
    // should still pass without modification.
    let app = AssetsTestApp::new().await;
    let event = received_event("evt-helper", "SN-HELPER");
    TestRequest::post("/api/assets/events")
        .json(&event)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);
}
