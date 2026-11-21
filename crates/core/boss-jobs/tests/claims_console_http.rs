//! The console is a scoped read of a materialized audit-sequence basis.
use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::{port::EventBus, publisher::DomainPublisher};
use boss_jobs::{
    InMemoryJobs,
    http::{JobsApiState, router},
};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};

/// A history whose head read never answers, so the route's outer deadline
/// is what is judged.
struct UnresponsiveHistory;
#[async_trait::async_trait]
impl boss_jobs::claims_console::ClaimsHistory for UnresponsiveHistory {
    async fn read(
        &self,
        _: Option<i64>,
        _: i64,
        _: chrono::DateTime<chrono::Utc>,
    ) -> Result<boss_jobs::claims_console::ClaimsReading, boss_jobs::claims_console::ClaimsError>
    {
        std::future::pending().await
    }
}

/// The request-owned deadline trace outlived the cuts route it was built
/// for (backlog 81f8e468, 4bec011d): the head read still names the span it
/// interrupted.
#[tokio::test]
async fn a_claims_outer_deadline_names_the_active_adapter_and_elapsed_time() {
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState {
        claims_history: Some(Arc::new(UnresponsiveHistory)),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let response = TestRequest::get("/api/claims")
        .as_user("emp-console", "operator")
        .send(&app)
        .await;
    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    let body = String::from_utf8(response.body_bytes).unwrap();
    assert!(
        body.contains("outer deadline") && body.contains("adapter") && body.contains("elapsed_ms"),
        "{body}"
    );
}

#[tokio::test]
async fn packet_and_malformed_waits_keep_the_http_read_available_and_named() {
    use boss_jobs::claims_console::{
        ClaimsHistory, HistoryBatch, InMemoryClaimsHistory, SequencedFact,
    };
    use serde_json::json;
    let now = chrono::Utc::now();
    let job = uuid::Uuid::from_u128(1800);
    let history = Arc::new(InMemoryClaimsHistory::default());
    let values = [
        ("jobs.job.created", json!({"id":job,"status":"open"})),
        (
            "jobs.step.updated",
            json!({"id":uuid::Uuid::from_u128(1801),"job_id":job,"status":"ready","metadata":{"waiting_on":{"on":"packet","id":"6bc062b1-8d49-464f-abe1-6912c7d25fa3","since":now,"why":"dependency"}}}),
        ),
        (
            "jobs.step.updated",
            json!({"id":uuid::Uuid::from_u128(1802),"job_id":job,"status":"ready","metadata":{"waiting_on":17}}),
        ),
    ];
    let facts = values
        .into_iter()
        .enumerate()
        .map(|(i, (kind, payload))| SequencedFact {
            seq: i as i64 + 1,
            recorded_at: now,
            event_id: uuid::Uuid::new_v4(),
            event_at: now,
            source: "jobs".into(),
            kind: kind.into(),
            payload: payload.to_string(),
            original_group: None,
        })
        .collect();
    history
        .materialize(&HistoryBatch {
            through: 3,
            through_at: now,
            facts,
        })
        .await
        .unwrap();
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState {
        claims_history: Some(history),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let response = TestRequest::get("/api/claims")
        .as_user("emp-console", "operator")
        .send(&app)
        .await;
    response.assert_status(StatusCode::OK);
    let body: serde_json::Value = response.assert_json();
    assert_eq!(
        body["projection"]["other_waits"],
        json!([{"kind":"packet","count":1}])
    );
    assert_eq!(
        body["projection"]["wait_attention"][0]["step_id"],
        uuid::Uuid::from_u128(1802).to_string()
    );
}

#[tokio::test]
async fn an_unwired_claim_history_is_named_unavailable_instead_of_a_missing_route() {
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState::minimal(
        Arc::new(InMemoryJobs::new()),
        bus,
        DomainPublisher::new(events, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let response = TestRequest::get("/api/claims")
        .as_user("emp-console", "operator")
        .send(&app)
        .await;
    assert_eq!(
        response.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a materialized history that was not wired is unread, never a healthy empty claims table"
    );
    assert!(
        String::from_utf8(response.body_bytes)
            .unwrap()
            .contains("materialized reader not configured")
    );
}

#[tokio::test]
async fn the_claims_route_names_its_actual_basis_and_refuses_narrow_or_future_reads() {
    use boss_jobs::claims_console::{ClaimsHistory, HistoryBatch, InMemoryClaimsHistory};
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("limited", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let history = Arc::new(InMemoryClaimsHistory::default());
    history
        .materialize(&HistoryBatch {
            through: 7,
            through_at: chrono::Utc::now(),
            facts: vec![],
        })
        .await
        .unwrap();
    let state = JobsApiState {
        claims_history: Some(history),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let app = router(state);
    let observed = TestRequest::get("/api/claims?window=24h")
        .as_user("emp-console", "operator")
        .send(&app)
        .await;
    observed.assert_status(StatusCode::OK);
    let body: serde_json::Value = observed.assert_json();
    // `as_of` stays in the ANSWER: it is the head the reading stands at,
    // not the retired selector (backlog 4bec011d).
    assert_eq!(body["as_of"], 7);
    assert_eq!(body["claims"], serde_json::json!([]));
    assert_eq!(body["window_hours"], 24);
    TestRequest::get("/api/claims?window=999h")
        .as_user("emp-console", "operator")
        .send(&app)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    TestRequest::get("/api/claims")
        .as_user("emp-console", "limited")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    TestRequest::get("/api/claims")
        .as_user("emp-console", "ungranted")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn retention_uses_the_injected_record_clock_not_a_second_wall_read() {
    use boss_jobs::claims_console::{ClaimsHistory, HistoryBatch, InMemoryClaimsHistory};
    let now = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let history = Arc::new(InMemoryClaimsHistory::default());
    history
        .materialize(&HistoryBatch {
            through: 7,
            through_at: now,
            facts: vec![],
        })
        .await
        .unwrap();
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let record_clock = Arc::new(boss_clock_client::FixedClockClient::new(
        boss_clock_client::ClockNow {
            now,
            ..boss_clock_client::ClockNow::wall()
        },
    ));
    let state = JobsApiState {
        claims_history: Some(history),
        claims_history_clock: record_clock,
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let response = TestRequest::get("/api/claims")
        .as_user("emp-console", "operator")
        .send(&router(state))
        .await;
    response.assert_status(StatusCode::OK);
    let body: serde_json::Value = response.assert_json();
    // The retention floor is the injected record clock's, 168 hours before
    // 2020-01-01 — a wall read would put it in the present.
    assert_eq!(body["retained_from"], "2019-12-25T00:00:00Z");
}

/// A materialized history at head 7, behind an operator with the full
/// job-read scope and a `limited` caller without it.
async fn head_seven_app() -> axum::Router {
    use boss_jobs::claims_console::{ClaimsHistory, HistoryBatch, InMemoryClaimsHistory};
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("limited", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let history = Arc::new(InMemoryClaimsHistory::default());
    history
        .materialize(&HistoryBatch {
            through: 7,
            through_at: chrono::Utc::now(),
            facts: vec![],
        })
        .await
        .unwrap();
    router(JobsApiState {
        claims_history: Some(history),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

/// Backlog 4bec011d: the console's time travel went with the scrubber
/// (7381cde96), and no caller passes `as_of`. Axum ignores a query key no
/// field names, so deleting the field alone would answer `?as_of=N` with
/// the HEAD and call it the past — the retired selector is refused by name
/// instead, whatever its value, and after policy like every jobs read.
#[tokio::test]
async fn the_as_of_selector_is_refused_as_retired() {
    let app = head_seven_app().await;
    for uri in [
        "/api/claims?as_of=7",
        "/api/claims?as_of=8&window=24h",
        "/api/claims?as_of=yesterday",
        "/api/claims?as_of=",
    ] {
        let refused = TestRequest::get(uri)
            .as_user("emp-console", "operator")
            .send(&app)
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{uri}");
        let body = String::from_utf8(refused.body_bytes).unwrap();
        assert!(
            body.contains("as_of") && body.contains("retired"),
            "{uri}: {body}"
        );
    }
    TestRequest::get("/api/claims?as_of=7")
        .as_user("emp-console", "limited")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

/// Backlog 4bec011d: the retained-cuts index had one reader, the review
/// screen's scrubber, removed in 7381cde96. The route is gone, not answered.
#[tokio::test]
async fn the_cuts_route_is_retired() {
    let app = head_seven_app().await;
    for uri in ["/api/claims/cuts", "/api/claims/cuts?buckets=8"] {
        let response = TestRequest::get(uri)
            .as_user("emp-console", "operator")
            .send(&app)
            .await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
    }
}

// -- the station page and the floor read their own subtree (backlog d05e29a7) --

/// A history whose head answers a fixed set of open claims, each at the
/// station it records (or none), so the route's narrowing is what is
/// judged and not the fold.
struct FixedClaims(Vec<Option<&'static str>>);
#[async_trait::async_trait]
impl boss_jobs::claims_console::ClaimsHistory for FixedClaims {
    async fn read(
        &self,
        _: Option<i64>,
        window_hours: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<boss_jobs::claims_console::ClaimsReading, boss_jobs::claims_console::ClaimsError>
    {
        use boss_jobs::claims_console::{ClaimsReading, OpenClaim, ProjectionBasis};
        Ok(ClaimsReading {
            projection: ProjectionBasis {
                generation: 0,
                through: 9,
                observed_audit_head: Some(9),
                source_observed_at: Some(now),
                sequence_distance: Some(0),
                source_unavailable_at: None,
                unknown_kinds: vec![],
                other_waits: vec![],
                wait_attention: vec![],
            },
            as_of: 9,
            as_of_at: now,
            retained_from: now,
            retention_hours: 24,
            window_hours,
            claims: self
                .0
                .iter()
                .enumerate()
                .map(|(i, station)| OpenClaim {
                    step_id: format!("s-{i}"),
                    job_id: format!("j-{i}"),
                    holder: Some("agent-claude".into()),
                    executor_kind: None,
                    executor_kind_basis: None,
                    run: None,
                    run_unavailable: None,
                    station: station.map(str::to_owned),
                    station_id: None,
                    station_resolution: None,
                    station_resolution_reason: None,
                    station_unavailable: station.is_none().then(|| "not-recorded".into()),
                    packet_opened_at: None,
                    packet_opened_at_unavailable: None,
                    resource_acquisition: None,
                    allocations: vec![],
                    since: now,
                    lease_remaining: None,
                    lease_unavailable: "none".into(),
                    identity_binding: None,
                    lease_expired: false,
                    stock_obligations: vec![],
                })
                .collect(),
            waiting_on: vec![],
            pools: vec![],
            binding: None,
        })
    }
}

/// The platform station bundle, so the subtree is the real tree's.
fn tree_rows() -> Vec<boss_jobs::StationSpec> {
    boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
        .expect("the platform station bundle parses")
}

fn claims_app(with_stations: bool) -> axum::Router {
    let registry = Arc::new(boss_jobs::InMemoryStations::new());
    for row in tree_rows() {
        registry.seed(row).expect("seed a station");
    }
    let bus = RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("limited", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    router(JobsApiState {
        claims_history: Some(Arc::new(FixedClaims(vec![
            Some("gates"),
            Some("forge-ci"),
            Some("deploy"),
            None,
            Some("engineering"),
            Some("a-station-outside-the-tree"),
        ]))),
        stations: with_stations.then_some(registry as Arc<dyn boss_jobs::StationRegistry>),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn claims_at(app: &axum::Router, uri: &str) -> serde_json::Value {
    let response = TestRequest::get(uri)
        .as_user("emp-console", "operator")
        .send(app)
        .await;
    response.assert_status(StatusCode::OK);
    response.assert_json()
}

fn stations_of(body: &serde_json::Value) -> Vec<Option<String>> {
    body["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .map(|c| c["station"].as_str().map(str::to_owned))
        .collect()
}

/// Backlog d05e29a7: the station page and the floor read `/api/claims`
/// whole on every load. `?station=<id or name>` answers the claims whose
/// recorded station stands in that station's subtree — and the claims
/// that record no station, which could be anywhere and which every
/// reader says so about rather than counting as zero. The root's subtree
/// is the whole company, so naming the root narrows nothing. The answer
/// names what it was narrowed to in `within`.
#[tokio::test]
async fn a_station_narrows_the_claims_to_its_subtree_and_says_so() {
    let app = claims_app(true);
    let whole = claims_at(&app, "/api/claims").await;
    assert_eq!(stations_of(&whole).len(), 6, "no station: every claim");
    assert_eq!(whole["within"], serde_json::Value::Null);
    assert_eq!(whole["truncated"], false);

    let engineering = claims_at(&app, "/api/claims?station=engineering").await;
    assert_eq!(
        stations_of(&engineering),
        vec![
            Some("gates".to_owned()),
            Some("forge-ci".to_owned()),
            None,
            Some("engineering".to_owned()),
        ],
        "the subtree's claims and the unplaced one, in the read's order"
    );
    assert_eq!(engineering["within"], "engineering");
    assert_eq!(engineering["truncated"], false);

    let id = tree_rows()
        .into_iter()
        .find(|s| s.name == "engineering")
        .and_then(|s| s.id)
        .expect("engineering stands in the tree");
    let by_id = claims_at(&app, &format!("/api/claims?station={id}")).await;
    assert_eq!(
        stations_of(&by_id),
        stations_of(&engineering),
        "an id or a name"
    );

    let root = tree_rows()
        .into_iter()
        .find(|s| s.id.is_some() && s.parent.is_none())
        .expect("the tree has a root");
    let company = claims_at(&app, &format!("/api/claims?station={}", root.name)).await;
    assert_eq!(stations_of(&company).len(), 6, "the root holds every claim");
    assert_eq!(company["within"], serde_json::Value::Null);

    let response = TestRequest::get("/api/claims?station=no-such-station")
        .as_user("emp-console", "operator")
        .send(&app)
        .await;
    response.assert_status(StatusCode::NOT_FOUND);

    // Policy is asked first, and the same Read: a narrower scope is
    // refused before any station is resolved.
    TestRequest::get("/api/claims?station=engineering")
        .as_user("emp-console", "limited")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // A station named with no registry wired is a named 503, never the
    // whole read passed off as the subtree's.
    let response = TestRequest::get("/api/claims?station=engineering")
        .as_user("emp-console", "operator")
        .send(&claims_app(false))
        .await;
    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
}

/// `?limit=` bounds the claim rows, and the answer says it was bounded:
/// a reader that sums a truncated list must know it is short.
#[tokio::test]
async fn a_limit_bounds_the_claims_and_the_answer_says_it_was_bounded() {
    let app = claims_app(false);
    let two = claims_at(&app, "/api/claims?limit=2").await;
    assert_eq!(stations_of(&two).len(), 2);
    assert_eq!(two["truncated"], true);
    let all = claims_at(&app, "/api/claims?limit=6").await;
    assert_eq!(stations_of(&all).len(), 6);
    assert_eq!(
        all["truncated"], false,
        "a limit the rows fit in bounds nothing"
    );

    let app = claims_app(true);
    let one = claims_at(&app, "/api/claims?station=engineering&limit=1").await;
    assert_eq!(stations_of(&one), vec![Some("gates".to_owned())]);
    assert_eq!(
        one["truncated"], true,
        "the limit applies after the subtree"
    );
}
