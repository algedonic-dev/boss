//! `gate=actor-role` on the one gate-window reader (backlog e0bdba74):
//! the enforce arm of design abf9eeae reads
//! `/api/events/gate-window?gate=actor-role&hours=72` exactly as row C
//! reads the machine gate's — the same door, the same answer shape, the
//! same join. No database here: the log and the live reads are the
//! ports' in-memory doubles.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use boss_core::clock::FixedClock;
use boss_core::event::Event;
use boss_core::gate_evidence::{Gate, InMemoryGateEvidence};
use boss_events_api::gate_window_http::{LiveTallies, LocalTallies, gate_window_router};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn fact(service: &str, kind: &str, when: DateTime<Utc>, mut payload: Value) -> Event {
    payload["service"] = json!(service);
    Event {
        id: Uuid::new_v4(),
        timestamp: when,
        source: service.into(),
        kind: format!("actor_role.{kind}"),
        payload,
    }
}

fn began(service: &str, instance: &str, when: DateTime<Utc>) -> Event {
    fact(
        service,
        "recording_began",
        when,
        json!({"mode": "report", "since": when, "instance": instance}),
    )
}

fn report(service: &str, instance: &str, since: DateTime<Utc>, rows: Value, why: Value) -> Value {
    json!({"service": service, "mode": "report", "snapshot": {"state": "ready"},
        "report": {"mode": "report", "recording_since": since, "rows": rows, "overflow": 0,
            "not_clean": why, "durable_window": true,
            "evidence": {"recorder": true, "instance": instance, "lost": 0, "unstated": 0,
                         "retrying": false, "last_error": null}}})
}

/// Two services, each answering its own report.
struct Estate(Vec<(String, Value)>);

#[async_trait::async_trait]
impl LiveTallies for Estate {
    fn required_services(&self, _: Gate) -> Vec<String> {
        self.0.iter().map(|(service, _)| service.clone()).collect()
    }
    async fn read(&self, gate: Gate, service: &str) -> Result<Value, String> {
        assert_eq!(gate, Gate::ActorRole);
        self.0
            .iter()
            .find(|(s, _)| s == service)
            .map(|(_, body)| body.clone())
            .ok_or_else(|| format!("{service} is not in this fixture"))
    }
}

async fn read_window(
    log: Vec<Event>,
    live: Estate,
    now: DateTime<Utc>,
    uri: &str,
) -> (StatusCode, Value) {
    let app = gate_window_router(
        Arc::new(InMemoryGateEvidence::new(log)),
        Arc::new(live),
        Arc::new(FixedClock::new(now)),
    );
    let operator = json!({"id": "emp-fixture", "role": "platform-admin", "access_tier": "operator",
        "territory_account_ids": [], "direct_report_ids": [], "department": null})
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-boss-user", operator)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

const URI: &str = "/api/events/gate-window?gate=actor-role&hours=72";

#[tokio::test]
async fn the_window_is_asked_and_answered_as_the_other_gates_are() {
    let now = Utc::now();
    let start = now - Duration::hours(80);
    let restart = now - Duration::hours(2);
    let log = vec![
        began("jobs", "old", start),
        fact(
            "jobs",
            "recording_ended",
            restart - Duration::minutes(1),
            json!({"instance": "old", "clean": true, "lost": 0, "unstated": 0}),
        ),
        began("jobs", "current", restart),
        began("people", "only", start),
    ];
    let estate = |people_rows: Value, why: Value| {
        Estate(vec![
            (
                "jobs".into(),
                report("jobs", "current", restart, json!([]), json!([])),
            ),
            (
                "people".into(),
                report("people", "only", start, people_rows, why),
            ),
        ])
    };
    let (status, clean) = read_window(log.clone(), estate(json!([]), json!([])), now, URI).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clean["gate"], "actor-role");
    assert_eq!(clean["required_services"], json!(["jobs", "people"]));
    assert_eq!(clean["covers_requested_window"], true, "{clean:#}");
    assert_eq!(clean["not_clean"], json!([]));
    assert_eq!(
        clean["log"]["coverage"][0]["recording_since"],
        json!(start),
        "jobs restarted two hours ago and its watch is eighty hours old"
    );

    // The same log, and `people` — one process, never restarted — holds
    // a shape it first saw before the window opened.
    let held = json!([{
        "observation": {"actor": "agent-claude", "asserted_role": "platform-admin",
            "recorded_actor": "agent-claude", "recorded_role": "engineering-agent",
            "action": "update", "resource": "job", "lookup_status": "registered",
            "asserted_allowed": true, "recorded_allowed": false,
            "would_deny": true, "would_change_scope": false},
        "count": 40, "first_seen": start + Duration::hours(1), "last_seen": now - Duration::hours(1),
        "would_refuse": "would-deny"}]);
    let why = json!(["1 shape(s), 40 observation(s), that `enforce` answers differently"]);
    let (_, dirty) = read_window(log, estate(held, why), now, URI).await;
    assert_eq!(dirty["covers_requested_window"], false);
    assert_eq!(dirty["clean_since"], Value::Null);
    assert!(
        dirty["not_clean"]
            .as_array()
            .unwrap()
            .iter()
            .any(|why| why.as_str().unwrap().starts_with("people: ")),
        "{dirty:#}"
    );
    assert_eq!(
        dirty["log"]["dirty"],
        json!([]),
        "the log half alone is clean"
    );
}

#[tokio::test]
async fn the_production_roster_is_every_service_that_mounts_a_report() {
    let client = boss_core::machine_token::Client::build_with_source(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(1)),
        Arc::new(boss_core::machine_token::Source::fixed(None)),
    )
    .unwrap();
    let reader = LocalTallies::at_declared_ports(client);
    let all: Vec<String> = boss_ports::all().map(|s| s.name.to_string()).collect();
    assert!(all.len() >= 28, "{all:?}");
    assert_eq!(reader.required_services(Gate::ActorRole), all);
    // The other gates' rosters are what they were.
    assert_eq!(reader.required_services(Gate::PolicyCheck), vec!["policy"]);
    assert_eq!(
        reader.required_services(Gate::MachineGate),
        boss_ports::all()
            .filter(|s| boss_core::machine_gate::is_gated(s.name))
            .map(|s| s.name.to_string())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn the_transport_asks_each_service_at_its_own_report_path() {
    use axum::extract::State;
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    async fn answer(
        State(asked): State<Arc<std::sync::Mutex<Vec<String>>>>,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
    ) -> axum::Json<Value> {
        asked.lock().unwrap().push(uri.path().to_string());
        let actor: Value =
            serde_json::from_str(headers.get("x-boss-user").unwrap().to_str().unwrap()).unwrap();
        assert_eq!(actor["id"], "automation:events");
        axum::Json(json!({"service": "fixture"}))
    }
    let app = axum::Router::new()
        .fallback(axum::routing::get(answer))
        .with_state(asked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = boss_core::machine_token::Client::build_with_source(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)),
        Arc::new(boss_core::machine_token::Source::fixed(None)),
    )
    .unwrap();
    let services = ["jobs", "subject-kinds", "simulator", "sim-control"];
    let reader = LocalTallies::new(
        client,
        services
            .iter()
            .map(|s| (s.to_string(), base.clone()))
            .collect(),
    );
    for service in services {
        reader.read(Gate::ActorRole, service).await.unwrap();
    }
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "/api/jobs/actor-role-reports",
            "/api/subject-kinds/actor-role-reports",
            "/simulator/api/actor-role-reports",
            "/actor-role-reports"
        ]
    );
    server.abort();
}
