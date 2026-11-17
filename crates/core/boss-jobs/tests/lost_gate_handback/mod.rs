//! The lost-gate hand-back, sent to the real jobs API admission twice
//! (backlog 7cedd72f) — shared by the in-memory and the Postgres test.
//!
//! The conductor files ONE pipeline-failure item per lost gate-run, under
//! an id derived from that run (`lost_gate::handback_item_id`, backlog
//! cedc8e69), and every pass that finds its `filing` stamp sends the SAME
//! request again. Its exactly-once rests on two answers of the jobs API it
//! never tested against this body: a re-send of the admitted body is
//! answered `already_admitted` (558396ff), and a body that differs is
//! refused 409 — which `file_handback` recovers by reading the item back
//! and accepting it only if `lost_gate::is_original_handback` holds. Each
//! scenario here sends `lost_gate::failure_item_body` itself, through the
//! router, against the platform bundle's own `backlog-item` row, so a
//! server-resolved field that moves under a re-send (the owner, a stamped
//! metadata key, the steps) would show up as a 409 or a second copy.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::lost_gate::{failure_item_body, handback_item_id, is_original_handback};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{WorkflowSpec, seedable_platform_workflows};
use boss_jobs::{JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The platform owner `owner_for_filing` answers: on the roster, and the
/// holder of every role, so an owner admission replaces resolves to him.
pub const PERSON: &str = "emp-david";
/// Who the conductor signs as (`boss-cli` identity.rs `CONDUCTOR`).
const CONDUCTOR: &str = "automation:train-conductor";
/// The lost gate-run the hand-back is for, and the run before it.
const LOST: &str = "11111111-0000-4000-8000-0000000000aa";
const FIRST: &str = "22222222-0000-4000-8000-0000000000bb";

struct OnePersonRoster;

#[async_trait]
impl RosterLookup for OnePersonRoster {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(vec![PERSON.to_string()])
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == PERSON)
    }
}

/// The platform bundle's `backlog-item` row — the protocol the hand-back
/// is admitted under in production.
pub fn backlog_item() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "backlog-item")
        .expect("the platform bundle ships backlog-item")
}

/// The jobs router over `jobs` and `kinds`, with the roster production
/// resolves owners against.
pub fn app<R: JobsRepository + 'static>(
    jobs: Arc<R>,
    kinds: Arc<dyn WorkflowRegistry>,
) -> axum::Router {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState {
        kind_registry: Some(kinds),
        roster: Some(Arc::new(OnePersonRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

/// What a backend counts as written about the packet: the events its
/// store recorded. Steps are read through the API, the way a caller sees
/// them.
#[async_trait]
pub trait Ledger: Sync {
    async fn events_about(&self, id: &str) -> usize;
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    // The conductor's header: `boss-cli` identity.rs `header`.
    let user = json!({
        "id": CONDUCTOR,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    });
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user.to_string())
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("request");
    let resp = app.clone().oneshot(request).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// The request the conductor builds for a reading, owner as
/// `owner_for_filing` answered it.
fn handback(owner: &str, why: &str) -> Value {
    failure_item_body(
        LOST,
        "fix/a-car-whose-gate-was-lost",
        "0123456789abcdef0123456789abcdef01234567",
        FIRST,
        LOST,
        why,
        owner,
    )
}

/// `GET /api/jobs/{id}` — the read `file_handback` recovers by.
async fn read(app: &axum::Router, id: &str) -> Value {
    let (status, item) = send(app, "GET", &format!("/api/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "reading {id} back: {item}");
    item
}

fn steps(item: &Value) -> usize {
    item["steps"].as_array().map_or(0, Vec::len)
}

/// THE CONTRACT the hand-back's exactly-once rests on: the request sent a
/// second time — a lost answer, a refused stamp, a restart, a second
/// conductor — is answered the item the first send admitted, with the
/// derived id and `already_admitted`, and writes nothing.
pub async fn the_handback_sent_twice_is_one_item(
    app: &axum::Router,
    ledger: &dyn Ledger,
    owner: &str,
) {
    let id = handback_item_id(LOST);
    let request = handback(owner, "the relaunch died on the same sha");

    let (status, first) = send(app, "POST", "/api/jobs", Some(&request)).await;
    assert_eq!(status, StatusCode::CREATED, "first send: {first}");
    assert_eq!(
        first["id"],
        json!(id),
        "admitted under the derived id: {first}"
    );
    let admitted = read(app, &id).await;
    let steps_once = steps(&admitted);
    assert!(
        steps_once > 0,
        "precondition: the protocol's steps: {admitted}"
    );
    let events_once = ledger.events_about(&id).await;
    assert!(events_once > 0, "precondition: the admission was recorded");

    let (status, second) = send(app, "POST", "/api/jobs", Some(&request)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the same request again is the item that exists, not a 409 the \
         conductor has to recover from (owner {owner:?}): {second}"
    );
    assert_eq!(second["id"], json!(id), "{second}");
    assert_eq!(second["already_admitted"], true, "{second}");

    let after = read(app, &id).await;
    assert_eq!(steps(&after), steps_once, "no second copy of any step");
    assert_eq!(
        ledger.events_about(&id).await,
        events_once,
        "nothing recorded by the re-send"
    );
    assert!(
        is_original_handback(&after, LOST),
        "the item reads back as THE hand-back for the lost run: {after}"
    );
}

/// The other answer `file_handback` is built for: a DIFFERENT reading
/// under the same id (a pre-fix pass, a moved `why`) is refused 409
/// naming what differs, and the read-back it then makes finds the first
/// reading and accepts it as the original — so one item, not two, and
/// not an error forever.
pub async fn a_moved_reading_is_refused_and_the_read_back_recovers_the_item(
    app: &axum::Router,
    ledger: &dyn Ledger,
) {
    let id = handback_item_id(LOST);
    let (status, first) = send(
        app,
        "POST",
        "/api/jobs",
        Some(&handback(PERSON, "the first reading")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = ledger.events_about(&id).await;

    let (status, refused) = send(
        app,
        "POST",
        "/api/jobs",
        Some(&handback(PERSON, "a reading that moved")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["id"], json!(id), "{refused}");
    assert!(
        refused["differing_fields"]
            .as_array()
            .is_some_and(|f| f.iter().any(|v| v == "metadata")),
        "the refusal names the metadata: {refused}"
    );
    assert_eq!(
        ledger.events_about(&id).await,
        events_once,
        "nothing recorded"
    );

    let item = read(app, &id).await;
    assert!(
        is_original_handback(&item, LOST),
        "the read-back accepts the first reading as the original: {item}"
    );
    assert_eq!(
        item["metadata"]["evidence"]["why"], "the first reading",
        "and it is the first reading, unmoved: {item}"
    );
}
