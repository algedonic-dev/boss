//! The live pools publish door (backlog f399458f, David 2026-11-08: "we
//! should be able to make these registry changes via a different
//! protocol than the code changes").
//!
//! Until this door a pool row reached the registry only through the
//! platform seed on a converge, so raising `robots:opus` from 4 to 6
//! (item 89b17d21) rode a code car, a gate and a train. The door has the
//! shape of the stations and cadence doors: `GET /api/pools`,
//! `GET /api/pools/{name}/versions`, `POST /api/pools/_validate`,
//! `POST /api/pools/{name}/publish` and `POST /api/pools/{name}/retire`,
//! append-only and versioned, every write one fact, gated on `pool`.
//!
//! What is asserted here, against the real router over the in-memory
//! adapters:
//!
//!  1. A publish lands the DECLARED version over the active row, so the
//!     tree file and the live row can agree; one `jobs.pool.published`
//!     fact signed by the caller.
//!  2. A version not above the lineage's newest is 409 and writes
//!     nothing; a body that disagrees with its path, declares a key this
//!     build does not know, or is not an active row is 400.
//!  3. `_validate` and publish run the same lints: a station no active
//!     row declares is a problem in the dry run and a 422 at publish,
//!     with the same `{ok, problems}` body.
//!  4. A retire answers the row it retired and one `jobs.pool.retired`
//!     fact; a second retire is 404, never a silent success.
//!  5. Every door asks policy on `pool`: a caller who may read jobs and
//!     publish workflows still may not publish or retire a pool.
//!  6. A publish that names the live version it was signed over
//!     (`?over=<version>`) is held to it INSIDE the write: a lineage
//!     that moved between the signer's read and this write is 409 and
//!     writes nothing (review R1 of car 4019ba69).

use boss_policy_client::types::{AccessTier, User};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::pools::{InMemoryPools, PoolRegistry, PoolSpec};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// A row in the bundle file's shape, as `boss pool publish` sends it.
fn row(version: i32, amount: &str) -> Value {
    json!({
        "name": "robots:door", "version": version, "status": "active",
        "unit": "slot", "kind": "flow",
        "capacity": {"state": "configured", "amount": amount},
        "capacity_from": "the pools door test"
    })
}

fn app() -> (axum::Router, Arc<InMemoryPools>) {
    let pools = Arc::new(InMemoryPools::default());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::pool(), Scope::All)
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::pool(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Retire,
                Resource::pool(),
                Scope::All,
            )
            // Reads every packet and publishes every protocol, and still
            // holds nothing on `pool`.
            .allow("author", Action::Read, Resource::job(), Scope::All)
            .allow("author", Action::Publish, Resource::workflow(), Scope::All)
            .allow("author", Action::Retire, Resource::workflow(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        pools: Some(pools.clone() as Arc<dyn PoolRegistry>),
        stations: Some(Arc::new(boss_jobs::InMemoryStations::new())),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), pools)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    role: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-boss-user", user_header("emp-david", role))
        .body(match body {
            Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

fn kinds(pools: &InMemoryPools) -> Vec<String> {
    pools
        .recorded_events()
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn a_publish_lands_the_declared_version_and_bumps_the_lineage() {
    let (app, pools) = app();
    let admin = "platform-admin";
    let (st, body) = call(
        &app,
        "POST",
        "/api/pools/robots:door/publish",
        admin,
        Some(row(1, "4")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        (body["version"].as_i64(), body["status"].as_str()),
        (Some(1), Some("active"))
    );
    let (st, body) = call(
        &app,
        "POST",
        "/api/pools/robots:door/publish",
        admin,
        Some(row(2, "6")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["capacity"]["amount"], "6", "{body}");

    let (st, lineage) = call(&app, "GET", "/api/pools/robots:door/versions", admin, None).await;
    assert_eq!(st, StatusCode::OK, "{lineage}");
    let lineage: Vec<(i64, String)> = lineage
        .as_array()
        .expect("a list")
        .iter()
        .map(|r| {
            (
                r["version"].as_i64().unwrap(),
                r["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(lineage, vec![(1, "retired".into()), (2, "active".into())]);

    let (st, active) = call(&app, "GET", "/api/pools", admin, None).await;
    assert_eq!(st, StatusCode::OK, "{active}");
    let served: Vec<PoolSpec> = serde_json::from_value(active).expect("pool rows");
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].version, 2, "the live registry serves v2");

    let events = pools.recorded_events().expect("events");
    assert_eq!(events.len(), 2, "one fact per write");
    assert!(events.iter().all(|e| e.kind == boss_jobs::pools::PUBLISHED));
    assert_eq!(
        events[1].payload["_actor"], "emp-david",
        "signed as the caller"
    );
}

#[tokio::test]
async fn a_version_not_above_the_newest_is_409_and_writes_nothing() {
    let (app, pools) = app();
    let admin = "platform-admin";
    let path = "/api/pools/robots:door/publish";
    assert_eq!(
        call(&app, "POST", path, admin, Some(row(2, "4"))).await.0,
        StatusCode::OK
    );
    for v in [1, 2] {
        let (st, body) = call(&app, "POST", path, admin, Some(row(v, "9"))).await;
        assert_eq!(st, StatusCode::CONFLICT, "v{v}: {body}");
        assert!(
            body.to_string().contains("not above"),
            "names the bound: {body}"
        );
    }
    assert_eq!(kinds(&pools).len(), 1, "a refusal writes nothing");
}

#[tokio::test]
async fn a_body_that_disagrees_with_its_path_or_names_an_unknown_key_is_400() {
    let (app, pools) = app();
    let admin = "platform-admin";
    let (st, body) = call(
        &app,
        "POST",
        "/api/pools/robots:other/publish",
        admin,
        Some(row(1, "4")),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("robots:other"), "{body}");

    let mut typo = row(1, "4");
    typo["reclaimr"] = json!("infra/forge/x.sh");
    let (st, body) = call(
        &app,
        "POST",
        "/api/pools/robots:door/publish",
        admin,
        Some(typo),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("reclaimr"),
        "names the key: {body}"
    );

    let mut retired = row(1, "4");
    retired["status"] = json!("retired");
    let (st, body) = call(
        &app,
        "POST",
        "/api/pools/robots:door/publish",
        admin,
        Some(retired),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("retire"),
        "points at the retire door: {body}"
    );
    assert!(kinds(&pools).is_empty(), "a refusal writes nothing");
}

#[tokio::test]
async fn validate_and_publish_run_the_same_lint() {
    let (app, pools) = app();
    let admin = "platform-admin";
    let (st, clean) = call(
        &app,
        "POST",
        "/api/pools/_validate",
        admin,
        Some(row(1, "4")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{clean}");
    assert_eq!(clean["ok"], true, "{clean}");

    let mut stationed = row(1, "4");
    stationed["station"] = json!("no-such-station");
    let (st, dry) = call(
        &app,
        "POST",
        "/api/pools/_validate",
        admin,
        Some(stationed.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "lint failures are data: {dry}");
    assert_eq!(dry["ok"], false, "{dry}");
    assert!(dry.to_string().contains("no-such-station"), "{dry}");

    let (st, refused) = call(
        &app,
        "POST",
        "/api/pools/robots:door/publish",
        admin,
        Some(stationed),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused, dry, "the refusal is the dry run's body");
    assert!(kinds(&pools).is_empty(), "a refusal writes nothing");

    let mut typo = row(1, "4");
    typo["progam"] = json!("x");
    let (st, dry) = call(&app, "POST", "/api/pools/_validate", admin, Some(typo)).await;
    assert_eq!(st, StatusCode::OK, "{dry}");
    assert_eq!(dry["ok"], false, "{dry}");
    assert!(dry.to_string().contains("progam"), "{dry}");
}

#[tokio::test]
async fn a_retire_answers_the_retired_row_then_404_and_records_once() {
    let (app, pools) = app();
    let admin = "platform-admin";
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/pools/robots:door/publish",
            admin,
            Some(row(1, "4"))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (st, body) = call(&app, "POST", "/api/pools/robots:door/retire", admin, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        (body["version"].as_i64(), body["status"].as_str()),
        (Some(1), Some("retired"))
    );
    let (_, active) = call(&app, "GET", "/api/pools", admin, None).await;
    assert_eq!(active, json!([]), "a retired pool is not served");

    let (st, body) = call(&app, "POST", "/api/pools/robots:door/retire", admin, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        kinds(&pools),
        [boss_jobs::pools::PUBLISHED, boss_jobs::pools::RETIRED],
        "one fact per write; the 404 writes none"
    );
}

#[tokio::test]
async fn every_pool_door_asks_policy_on_pool() {
    let (app, pools) = app();
    for (method, path, body) in [
        ("GET", "/api/pools", None),
        ("GET", "/api/pools/robots:door/versions", None),
        ("POST", "/api/pools/_validate", Some(row(1, "4"))),
        ("POST", "/api/pools/robots:door/publish", Some(row(1, "4"))),
        ("POST", "/api/pools/robots:door/retire", None),
    ] {
        let (st, body) = call(&app, method, path, "author", body).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    assert!(kinds(&pools).is_empty(), "a refusal writes nothing");
}

/// The race review R1 of car 4019ba69 measured (2026-11-08): a signed
/// approval is a diff against live v1; its publisher reads the lineage
/// (v1), another publisher lands v2, and the signed v3 then POSTs. A door
/// that checks only "above the newest" takes v3 over a v2 nobody
/// approved, and the client's own read cannot close the window. So the
/// signed baseline rides the write as `?over=<version>` and is judged
/// inside the publish's own transaction, against the newest version the
/// lineage holds under the write's lock: not equal, 409, nothing written.
#[tokio::test]
async fn a_signed_baseline_that_moved_between_the_read_and_the_write_is_409_inside_the_write() {
    let (app, pools) = app();
    let admin = "platform-admin";
    let path = "/api/pools/robots:door/publish";
    assert_eq!(
        call(&app, "POST", path, admin, Some(row(1, "4"))).await.0,
        StatusCode::OK
    );

    // The signer's read: the lineage is at v1, and v3 is approved over it.
    let (st, before) = call(&app, "GET", "/api/pools/robots:door/versions", admin, None).await;
    assert_eq!(st, StatusCode::OK, "{before}");
    assert_eq!(before.as_array().map(Vec::len), Some(1), "{before}");

    // Another publisher lands v2 between that read and the signed write.
    assert_eq!(
        call(&app, "POST", path, admin, Some(row(2, "5"))).await.0,
        StatusCode::OK
    );

    // The signed write, held to the baseline it was approved over.
    let (st, body) = call(
        &app,
        "POST",
        &format!("{path}?over=1"),
        admin,
        Some(row(3, "6")),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    let text = body.to_string();
    assert!(
        text.contains("v2") && text.contains("v1"),
        "names the live version and the signed one: {body}"
    );

    let (st, lineage) = call(&app, "GET", "/api/pools/robots:door/versions", admin, None).await;
    assert_eq!(st, StatusCode::OK, "{lineage}");
    let lineage: Vec<(i64, String)> = lineage
        .as_array()
        .expect("a list")
        .iter()
        .map(|r| {
            (
                r["version"].as_i64().unwrap(),
                r["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        lineage,
        vec![(1, "retired".into()), (2, "active".into())],
        "v2 stays active and no v3 exists"
    );
    assert_eq!(kinds(&pools).len(), 2, "the refusal wrote no fact");

    // The same row, signed over what IS live, lands.
    let (st, body) = call(
        &app,
        "POST",
        &format!("{path}?over=2"),
        admin,
        Some(row(3, "6")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["version"].as_i64(), Some(3), "{body}");

    // A baseline that is not a version is a refusal, not a publish.
    let (st, body) = call(
        &app,
        "POST",
        &format!("{path}?over=latest"),
        admin,
        Some(row(4, "7")),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        kinds(&pools).len(),
        3,
        "nothing written on a malformed baseline"
    );
}
