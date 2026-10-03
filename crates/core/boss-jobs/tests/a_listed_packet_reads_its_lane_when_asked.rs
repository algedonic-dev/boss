//! `GET /api/jobs?…&lane=true` — each listed row carries the lane the
//! SERVER read off it: `{"lane": <label>, "basis": "recorded" |
//! "unclassified"}`, from `boss_jobs::channels::lane_of`.
//!
//! WHY THIS FILE EXISTS (backlog 1eea4554, decided 2026-09-24). The
//! receiving board classified every packet itself, in a six-lane
//! vocabulary of its own, from `metadata.channel` — a key no filer
//! writes — and three kind lists; so 0 of 1,626 arrivals read as
//! recorded while 492 of them carried `input_channel`, the key `boss job
//! file --channel` and the machine filers write. The decision moved the
//! rule to the server, where the vocabulary already lives, and the board
//! now draws what this read says.
//!
//! WHY A FLAG ON THE LIST AND NOT THE REGIONS PAYLOAD. The board draws a
//! lane for every arrival of its window, open or closed — 2,069 in the
//! week measured 2026-09-26 — against a 15 KB regions payload the map
//! polls. The board already lists those rows; classifying them where
//! they are listed costs one field per row and no second read. The
//! receiving REGION reads the same `lane_of` for its KPI.
//!
//! Opt-in, so no other reader of the list meets a field it never asked
//! for — a lane on a gate-run is not a reading anyone takes.
//!
//! `origin=true` is the second reading of the same shape, held below:
//! where the item came from and its area (backlog 9b473d4a).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const SAID: &str = "00000000-0000-0000-0000-0000000a1e01";
const OLD_KEY: &str = "00000000-0000-0000-0000-0000000a1e02";
const SILENT: &str = "00000000-0000-0000-0000-0000000a1e03";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn item(id: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).expect("uuid")),
        status: JobStatus::Open,
        metadata,
        ..Job::new(
            "backlog-item",
            Subject::new("asset", "BOSSNET"),
            format!("item {id}"),
            "emp-david",
            Priority::Standard,
            day(2026, 9, 26),
        )
    }
}

async fn seed() -> Router {
    seed_with(vec![
        (
            SAID,
            serde_json::json!({ "input_channel": "review-finding" }),
        ),
        // The key the board used to read — no filer writes it.
        (OLD_KEY, serde_json::json!({ "channel": "monitoring" })),
        (SILENT, serde_json::json!({})),
    ])
    .await
}

async fn seed_with(items: Vec<(&str, serde_json::Value)>) -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day(2026, 9, 26)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    for (id, md) in items {
        jobs.create_job(&item(id, md)).await.expect("job");
    }
    let state = JobsApiState::minimal(jobs, bus, publisher, policy, clock);
    router(state)
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

fn row<'a>(body: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{id} listed: {body}"))
}

#[tokio::test]
async fn a_listed_packet_carries_the_lane_the_server_read_when_asked() {
    let app = seed().await;
    let body = list(&app, "kind=backlog-item&lane=true").await;
    assert_eq!(
        row(&body, SAID)["lane"],
        serde_json::json!({ "lane": "review-finding", "basis": "recorded" })
    );
    for id in [OLD_KEY, SILENT] {
        assert_eq!(
            row(&body, id)["lane"],
            serde_json::json!({ "lane": "unclassified", "basis": "unclassified" }),
            "{id}: a filer who recorded no lane reads unclassified, never guessed"
        );
    }
    // The steps contract rides unchanged beside it.
    assert!(row(&body, SAID)["steps"].is_array(), "{body}");
}

#[tokio::test]
async fn a_list_that_does_not_ask_carries_no_lane() {
    let app = seed().await;
    for query in ["kind=backlog-item", "kind=backlog-item&lane=false"] {
        let body = list(&app, query).await;
        for id in [SAID, OLD_KEY, SILENT] {
            assert!(
                row(&body, id).get("lane").is_none(),
                "{query}: {id} carries a lane nobody asked for: {body}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// `origin=true` — the same shape of reading, for where the item came
// from: `{source: {basis, kind, key, id, branch}, area}`, from
// `boss_jobs::origin::origin_of` (design 3036296f mechanism D, backlog
// 9b473d4a car 2). The receiving board groups its untriaged items by the
// `key` the server read, and draws every unrecorded basis as one visible
// "no recorded source" group; the receiving region tallies the same rule.
// ---------------------------------------------------------------------

const REVIEWED: &str = "00000000-0000-0000-0000-0000000a1e11";
const BY_RUN: &str = "00000000-0000-0000-0000-0000000a1e12";
const AD_HOC: &str = "00000000-0000-0000-0000-0000000a1e13";
const PROSE: &str = "00000000-0000-0000-0000-0000000a1e14";

async fn seed_origins() -> Router {
    seed_with(vec![
        (
            REVIEWED,
            serde_json::json!({ "area": "jobs",
                "source": { "kind": "review", "branch": "fix/x",
                            "id": "ed84acc8-90ac-4403-9dc9-2cff0e537474" } }),
        ),
        (
            BY_RUN,
            serde_json::json!({ "source": { "kind": "agent-run", "id": "665c7419" } }),
        ),
        (
            AD_HOC,
            serde_json::json!({ "source_car": "fix/x", "area": "jobs" }),
        ),
        (
            PROSE,
            serde_json::json!({ "source": "review of car fix/x (42da8bd2)" }),
        ),
        (SILENT, serde_json::json!({})),
    ])
    .await
}

#[tokio::test]
async fn a_listed_packet_carries_the_origin_the_server_read_when_asked() {
    let app = seed_origins().await;
    let body = list(&app, "kind=backlog-item&origin=true").await;
    assert_eq!(
        row(&body, REVIEWED)["origin"],
        serde_json::json!({
            "source": { "basis": "recorded", "kind": "review", "key": "review:fix/x",
                        "id": "ed84acc8-90ac-4403-9dc9-2cff0e537474", "branch": "fix/x" },
            "area": "jobs",
        })
    );
    assert_eq!(
        row(&body, BY_RUN)["origin"]["source"]["key"],
        "agent-run:665c7419"
    );
    assert_eq!(
        row(&body, BY_RUN)["origin"]["area"],
        serde_json::Value::Null
    );
    // Every unrecorded shape is listed, named — never dropped, never a
    // source guessed out of a sentence.
    for (id, basis) in [(AD_HOC, "ad-hoc"), (PROSE, "prose"), (SILENT, "missing")] {
        let s = &row(&body, id)["origin"]["source"];
        assert_eq!(s["basis"], basis, "{id}: {body}");
        assert_eq!(s["key"], serde_json::Value::Null, "{id}: {body}");
    }
    assert_eq!(row(&body, AD_HOC)["origin"]["area"], "jobs");
    // It rides beside the lane when both are asked, and the steps stay.
    let body = list(&app, "kind=backlog-item&origin=true&lane=true").await;
    assert!(row(&body, REVIEWED)["lane"].is_object(), "{body}");
    assert!(row(&body, REVIEWED)["steps"].is_array(), "{body}");
}

#[tokio::test]
async fn a_list_that_does_not_ask_carries_no_origin() {
    let app = seed_origins().await;
    for query in ["kind=backlog-item", "kind=backlog-item&origin=false"] {
        let body = list(&app, query).await;
        for id in [REVIEWED, BY_RUN, AD_HOC, PROSE, SILENT] {
            assert!(
                row(&body, id).get("origin").is_none(),
                "{query}: {id} carries an origin nobody asked for: {body}"
            );
        }
    }
}
