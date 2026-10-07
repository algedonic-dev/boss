//! The report reader joins durable restart coverage with the live half
//! (design 21946380, partial backlog b0787727); it never flips a mode.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use boss_core::audit::AuditWriter;
use boss_core::clock::FixedClock;
use boss_core::event::Event;
use boss_core::gate_evidence::Gate;
use boss_events::{PgAuditWriter, audit_tail_router};
use boss_events_api::gate_window_http::{LiveTallies, gate_window_router};
use boss_testing::TestDb;
use chrono::{Duration, Utc};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct LiveFixture(serde_json::Value);

#[async_trait::async_trait]
impl LiveTallies for LiveFixture {
    fn required_services(&self, _: Gate) -> Vec<String> {
        vec!["policy".into()]
    }
    async fn read(&self, _: Gate, _: &str) -> Result<serde_json::Value, String> {
        Ok(self.0.clone())
    }
}

fn policy_snapshot(at: chrono::DateTime<Utc>, instance: &str) -> serde_json::Value {
    json!({"switch":"policy check","file":"fixture-mode","mode":"report","mode_error":null,
        "rows":[],"overflow":0,"recording_since":at,"clean_since":at,"not_clean":[],
        "evidence":{"recorder":true,"instance":instance,"lost":0,"unstated":0,"retrying":false,"last_error":null}})
}

fn user(tier: &str, role: &str) -> String {
    json!({"id":"emp-fixture","role":role,"access_tier":tier,
        "territory_account_ids":[],"direct_report_ids":[],"department":null})
    .to_string()
}

async fn ask(
    app: axum::Router,
    uri: &str,
    identity: Option<&str>,
    method: &str,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(identity) = identity {
        request = request.header("x-boss-user", identity);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn fixture_app(now: chrono::DateTime<Utc>, body: serde_json::Value) -> axum::Router {
    use boss_core::gate_evidence::InMemoryGateEvidence;
    let log = InMemoryGateEvidence::new(vec![Event {
        id: Uuid::new_v4(),
        timestamp: now - Duration::hours(74),
        source: "policy".into(),
        kind: "policy.check.recording_began".into(),
        payload: json!({"service":"policy","mode":"report","since":now-Duration::hours(74),"instance":"current"}),
    }]);
    gate_window_router(
        Arc::new(log),
        Arc::new(LiveFixture(body)),
        Arc::new(FixedClock::new(now)),
    )
}

#[tokio::test]
async fn the_reader_keeps_the_existing_audit_read_door_and_has_no_write_route() {
    let db = TestDb::new().await;
    let now = Utc::now();
    let app = fixture_app(now, policy_snapshot(now - Duration::hours(74), "current"));
    for (tier, role) in [
        ("operator", "service-tech"),
        ("auditor", "service-tech"),
        ("user", "cto"),
        ("user", "platform-admin"),
        ("user", "service-tech"),
        ("user", "unknown-role"),
        ("user", "break-glass"),
    ] {
        let identity = user(tier, role);
        // Role privileges are registry data, not historical names.
        // Compare the actual existing door on the same identity; a dark
        // class reader must give both readers the same refusal.
        let existing = ask(
            audit_tail_router(db.pool.clone()),
            "/api/events/tail?limit=1",
            Some(&identity),
            "GET",
        )
        .await
        .status();
        if tier == "operator" || tier == "auditor" {
            assert_eq!(existing, StatusCode::OK);
        }
        assert_eq!(
            ask(
                app.clone(),
                "/api/events/gate-window?gate=policy-check",
                Some(&identity),
                "GET"
            )
            .await
            .status(),
            existing,
            "{tier}/{role}"
        );
    }
    assert_eq!(
        ask(
            app.clone(),
            "/api/events/gate-window?gate=policy-check",
            None,
            "GET"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let identity = user("operator", "service-tech");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        assert_eq!(
            ask(
                app.clone(),
                "/api/events/gate-window?gate=policy-check",
                Some(&identity),
                method
            )
            .await
            .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}

#[tokio::test]
async fn query_bounds_cannot_select_less_coverage_or_another_upstream() {
    let now = Utc::now();
    let app = fixture_app(now, policy_snapshot(now - Duration::hours(74), "current"));
    let identity = user("operator", "service-tech");
    for query in [
        "gate=sideways",
        "gate=policy-check&hours=0",
        "gate=policy-check&hours=169",
        "gate=policy-check&hours=-1",
        "gate=policy-check&hours=4294967296",
        "gate=policy-check&service=jobs",
        "gate=policy-check&url=http://fixture",
        "gate=policy-check&hours=72&hours=1",
        "",
    ] {
        let uri = format!("/api/events/gate-window?{query}");
        assert_eq!(
            ask(app.clone(), &uri, Some(&identity), "GET")
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    for hours in [1, 72, 168] {
        let uri = format!("/api/events/gate-window?gate=policy-check&hours={hours}");
        assert_eq!(
            ask(app.clone(), &uri, Some(&identity), "GET")
                .await
                .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn missing_log_or_dirty_live_half_is_reported_with_all_available_evidence() {
    let now = Utc::now();
    let at = now - Duration::hours(74);
    let mut body = policy_snapshot(at, "current");
    body["not_clean"] = json!(["one caller keeps missing"]);
    body["clean_since"] = serde_json::Value::Null;
    let app = fixture_app(now, body.clone());
    let identity = user("auditor", "service-tech");
    let response = ask(
        app,
        "/api/events/gate-window?gate=policy-check",
        Some(&identity),
        "GET",
    )
    .await;
    let result: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(result["covers_requested_window"], false);
    assert_eq!(result["live"][0]["snapshot"], body);
    assert_eq!(
        result["live"][0]["not_clean"][0],
        "one caller keeps missing"
    );
    assert!(result["clean_since"].is_null());
}

#[tokio::test]
async fn the_real_policy_producer_and_path_still_fit_the_reader_contract() {
    use boss_core::gate_evidence::{Evidence, InMemoryGateEvidence};
    use boss_core::gate_window::{LiveRead, join_window};
    use boss_core::machine_gate::Mode;
    use boss_policy::check_mode::{CheckMode, REFUSALS_PATH};
    assert_eq!(
        boss_events_api::gate_window_http::POLICY_REFUSALS_PATH,
        REFUSALS_PATH
    );
    let (evidence, mut pending) = Evidence::channel(Gate::PolicyCheck, "policy");
    let mode = Arc::try_unwrap(CheckMode::fixed(Mode::Report))
        .ok()
        .unwrap()
        .with_evidence(evidence);
    let began = pending.recv().await.unwrap();
    let body = serde_json::to_value(mode.refusals()).unwrap();
    let now = Utc::now();
    let log = InMemoryGateEvidence::new(vec![began]);
    use boss_core::gate_evidence::GateEvidenceLog;
    let result = join_window(
        Gate::PolicyCheck,
        &["policy".into()],
        now - Duration::hours(72),
        now,
        log.facts(Gate::PolicyCheck, now - Duration::hours(72))
            .await,
        vec![LiveRead {
            service: "policy".into(),
            answer: Ok(body.clone()),
        }],
    );
    assert!(result.not_clean.is_empty(), "{result:?}");
    assert!(
        !result.covers_requested_window,
        "a fresh actual producer has not watched72h"
    );
    assert_eq!(result.live[0].snapshot.as_ref(), Some(&body));
}

#[derive(Clone)]
struct Upstream {
    body: String,
    status: StatusCode,
    redirect: Option<String>,
    delay: std::time::Duration,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

struct PrivateServer {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for PrivateServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(upstream: Upstream) -> PrivateServer {
    use axum::extract::State;
    use axum::response::IntoResponse;
    async fn answer(
        State(up): State<Upstream>,
        headers: axum::http::HeaderMap,
    ) -> axum::response::Response {
        up.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            headers.get("x-boss-machine-token").unwrap(),
            "private-fixture-token"
        );
        let actor: serde_json::Value =
            serde_json::from_str(headers.get("x-boss-user").unwrap().to_str().unwrap()).unwrap();
        assert_eq!(actor["id"], "automation:events");
        tokio::time::sleep(up.delay).await;
        let mut response = (up.status, up.body).into_response();
        if let Some(to) = up.redirect {
            response
                .headers_mut()
                .insert("location", to.parse().unwrap());
        }
        response
    }
    let app = axum::Router::new()
        .route(
            boss_core::machine_gate::MISSES_PATH,
            axum::routing::get(answer),
        )
        .route(
            boss_events_api::gate_window_http::POLICY_REFUSALS_PATH,
            axum::routing::get(answer),
        )
        .with_state(upstream);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    PrivateServer { base, task }
}

fn fixture_client(timeout: std::time::Duration) -> boss_core::machine_token::Client {
    // Explicit source: this test never reads an installed credential.
    boss_core::machine_token::Client::build_with_source(
        reqwest::Client::builder().timeout(timeout),
        Arc::new(boss_core::machine_token::Source::fixed(Some(
            "private-fixture-token".into(),
        ))),
    )
    .unwrap()
}

#[tokio::test]
async fn the_actual_transport_requires_one_json_document_and_preserves_refusals() {
    use boss_events_api::gate_window_http::LocalTallies;
    let now = Utc::now();
    let good = policy_snapshot(now, "fixture-process");
    for (body, status, valid) in [
        (good.to_string(), StatusCode::OK, true),
        (String::new(), StatusCode::OK, false),
        ("{}\n{}".into(), StatusCode::OK, false),
        ("{".into(), StatusCode::OK, false),
        (
            "reader grant was deliberately refused".into(),
            StatusCode::FORBIDDEN,
            false,
        ),
    ] {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = serve(Upstream {
            body,
            status,
            redirect: None,
            delay: std::time::Duration::ZERO,
            reads: reads.clone(),
        })
        .await;
        let reader = LocalTallies::new(
            fixture_client(std::time::Duration::from_secs(1)),
            vec![("policy".into(), server.base.clone())],
        );
        let answer = reader.read(Gate::PolicyCheck, "policy").await;
        assert_eq!(answer.is_ok(), valid, "{answer:?}");
        if status == StatusCode::FORBIDDEN {
            assert!(
                answer
                    .unwrap_err()
                    .contains("reader grant was deliberately refused")
            );
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn the_actual_transport_follows_no_redirect_and_times_out_as_a_named_gap() {
    use boss_events_api::gate_window_http::LocalTallies;
    let target_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let target = serve(Upstream {
        body: "{}".into(),
        status: StatusCode::OK,
        redirect: None,
        delay: std::time::Duration::ZERO,
        reads: target_reads.clone(),
    })
    .await;
    let redirect = serve(Upstream {
        body: "upstream moved".into(),
        status: StatusCode::FOUND,
        redirect: Some(format!(
            "{}{}",
            target.base,
            boss_events_api::gate_window_http::POLICY_REFUSALS_PATH
        )),
        delay: std::time::Duration::ZERO,
        reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    })
    .await;
    let reader = LocalTallies::new(
        fixture_client(std::time::Duration::from_millis(50)),
        vec![("policy".into(), redirect.base.clone())],
    );
    assert!(
        reader
            .read(Gate::PolicyCheck, "policy")
            .await
            .unwrap_err()
            .contains("302")
    );
    assert_eq!(target_reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    let slow = serve(Upstream {
        body: "{}".into(),
        status: StatusCode::OK,
        redirect: None,
        delay: std::time::Duration::from_secs(1),
        reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    })
    .await;
    let reader = LocalTallies::new(
        fixture_client(std::time::Duration::from_millis(30)),
        vec![("policy".into(), slow.base.clone())],
    );
    assert!(
        reader
            .read(Gate::PolicyCheck, "policy")
            .await
            .unwrap_err()
            .contains("GET")
    );
}

#[tokio::test]
async fn the_production_roster_comes_from_all_declared_gated_ports() {
    use boss_events_api::gate_window_http::LocalTallies;
    // This tests the production constructor without sending a request.
    // A fixed private client tests the equivalent explicit input safely.
    let services: Vec<_> = boss_ports::all()
        .filter(|s| boss_core::machine_gate::is_gated(s.name))
        .map(|s| (s.name.to_string(), boss_ports::url(s.name)))
        .collect();
    assert!(services.len() > 20);
    let reader = LocalTallies::at_declared_ports(fixture_client(std::time::Duration::from_secs(1)));
    assert_eq!(
        reader.required_services(Gate::MachineGate),
        services.into_iter().map(|(s, _)| s).collect::<Vec<_>>()
    );
    assert_eq!(reader.required_services(Gate::PolicyCheck), vec!["policy"]);
    // A roster with a missing upstream returns a gap before HTTP, and
    // duplicate declaration never chooses one by list order.
    let missing = LocalTallies::new(fixture_client(std::time::Duration::from_secs(1)), vec![]);
    assert!(
        missing
            .read(Gate::PolicyCheck, "policy")
            .await
            .unwrap_err()
            .contains("0 declared")
    );
    let duplicate = LocalTallies::new(
        fixture_client(std::time::Duration::from_secs(1)),
        vec![("policy".into(), "http://unreachable.invalid".into()); 2],
    );
    assert!(
        duplicate
            .read(Gate::PolicyCheck, "policy")
            .await
            .unwrap_err()
            .contains("2 declared")
    );
}

#[tokio::test]
async fn the_joined_http_reader_keeps_every_required_private_port_and_its_identity() {
    use boss_core::gate_evidence::InMemoryGateEvidence;
    use boss_events_api::gate_window_http::LocalTallies;
    let now = Utc::now();
    let since = now - Duration::hours(74);
    let machine = |service: &str, instance: &str| {
        json!({"service":service,"mode":"report","recording_since":since,
        "rows":[],"overflow":0,"source_overflow":[],"not_clean":[],
        "evidence":{"recorder":true,"instance":instance,"lost":0,"unstated":0,"retrying":false,"last_error":null}})
    };
    let events: Vec<_> = [("jobs", "jobs-process"), ("people", "people-process")]
        .into_iter()
        .map(|(service, instance)| Event {
            id: Uuid::new_v4(),
            timestamp: since,
            source: service.into(),
            kind: "machine_gate.recording_began".into(),
            payload: json!({"service":service,"mode":"report","since":since,"instance":instance}),
        })
        .collect();
    for problem in [
        "valid",
        "wrong-service",
        "wrong-instance",
        "missing-health",
        "dirty",
        "overflow",
        "dark",
        "multiple-json",
    ] {
        let mut people = machine("people", "people-process");
        match problem {
            "wrong-service" => people["service"] = json!("jobs"),
            "wrong-instance" => people["evidence"]["instance"] = json!("older-process"),
            "missing-health" => {
                people.as_object_mut().unwrap().remove("evidence");
            }
            "dirty" => people["not_clean"] = json!(["one caller still misses"]),
            "overflow" => people["overflow"] = json!(1),
            _ => {}
        }
        let jobs = serve(Upstream {
            body: machine("jobs", "jobs-process").to_string(),
            status: StatusCode::OK,
            redirect: None,
            delay: std::time::Duration::ZERO,
            reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .await;
        let people = serve(Upstream {
            body: if problem == "multiple-json" {
                format!("{people}\n{people}")
            } else {
                people.to_string()
            },
            status: if problem == "dark" {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            },
            redirect: None,
            delay: std::time::Duration::ZERO,
            reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .await;
        let live = LocalTallies::new(
            fixture_client(std::time::Duration::from_secs(1)),
            vec![
                ("jobs".into(), jobs.base.clone()),
                ("people".into(), people.base.clone()),
            ],
        );
        let app = gate_window_router(
            Arc::new(InMemoryGateEvidence::new(events.clone())),
            Arc::new(live),
            Arc::new(FixedClock::new(now)),
        );
        let response = ask(
            app,
            "/api/events/gate-window?gate=machine-gate&hours=72",
            Some(&user("auditor", "service-tech")),
            "GET",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let result: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(result["required_services"], json!(["jobs", "people"]));
        assert_eq!(result["live"].as_array().unwrap().len(), 2);
        assert_eq!(
            result["live"][0]["snapshot"]["evidence"]["instance"],
            "jobs-process"
        );
        assert_eq!(
            result["covers_requested_window"],
            problem == "valid",
            "{problem}: {result}"
        );
        if problem != "valid" {
            assert!(
                !result["not_clean"].as_array().unwrap().is_empty(),
                "{result}"
            );
        }
    }
}

#[tokio::test]
async fn a_clean_restart_is_read_as_one_durable_window() {
    let db = TestDb::new().await;
    let now = Utc::now();
    let before = now - Duration::hours(74);
    let ended = now - Duration::hours(1);
    let after = ended + Duration::minutes(1);
    let writer = PgAuditWriter::new(db.pool.clone());
    for (kind, at, payload) in [
        (
            "policy.check.recording_began",
            before,
            json!({"service":"policy","mode":"report","since":before,"instance":"old"}),
        ),
        (
            "policy.check.recording_ended",
            ended,
            json!({"service":"policy","instance":"old","clean":true,"lost":0,"unstated":0}),
        ),
        (
            "policy.check.recording_began",
            after,
            json!({"service":"policy","mode":"report","since":after,"instance":"current"}),
        ),
    ] {
        writer
            .write(&Event {
                id: Uuid::new_v4(),
                timestamp: at,
                source: "policy".into(),
                kind: kind.into(),
                payload,
            })
            .await
            .unwrap();
    }
    let user = json!({"id":"emp-reviewer","role":"service-tech","access_tier":"operator",
        "territory_account_ids":[],"direct_report_ids":[],"department":null});
    let live = json!({"switch":"policy check","file":"fixture-mode","mode":"report","mode_error":null,
        "rows":[],"overflow":0,"recording_since":after,"clean_since":after,"not_clean":[],
        "evidence":{"recorder":true,"instance":"current","lost":0,"unstated":0,"retrying":false,"last_error":null}});
    let app = audit_tail_router(db.pool.clone()).merge(gate_window_router(
        Arc::new(boss_events::PgGateEvidence::new(db.pool.clone())),
        Arc::new(LiveFixture(live)),
        Arc::new(FixedClock::new(now)),
    ));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/events/gate-window?gate=policy-check&hours=72")
                .header("x-boss-user", user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the joined reader must exist"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["covers_requested_window"], true, "{result}");
    assert_eq!(
        result["log"]["coverage"][0]["recording_since"],
        json!(before)
    );
    assert_eq!(
        result["live"][0]["snapshot"]["evidence"]["instance"],
        "current"
    );
}

#[tokio::test]
async fn a_malformed_postgres_start_expires_only_after_a_complete_later_watch() {
    for service in [None, Some(json!("people"))] {
        for (age, covers) in [(Duration::minutes(1), false), (Duration::hours(100), true)] {
            let db = TestDb::new().await;
            let now = Utc::now();
            let before = now - Duration::hours(74);
            let writer = PgAuditWriter::new(db.pool.clone());
            writer.write(&Event {
            id: Uuid::new_v4(), timestamp: before, source: "policy".into(),
            kind: "policy.check.recording_began".into(),
            payload: json!({"service":"policy","mode":"report","since":before,"instance":"current"}),
        }).await.unwrap();
            let newer = now - age;
            let mut payload = json!({"mode":"report","since":newer,"instance":"newer"});
            if let Some(service) = service.clone() {
                payload["service"] = service;
            }
            let identity = Uuid::new_v4();
            writer
                .write(&Event {
                    id: identity,
                    timestamp: newer,
                    source: "policy".into(),
                    kind: "policy.check.recording_began".into(),
                    payload,
                })
                .await
                .unwrap();
            let live = policy_snapshot(before, "current");
            let facts = boss_core::gate_evidence::GateEvidenceLog::facts(
                &boss_events::PgGateEvidence::new(db.pool.clone()),
                Gate::PolicyCheck,
                now - Duration::hours(72),
            )
            .await
            .unwrap();
            assert!(facts.iter().any(|event| event.id == identity));
            let app = gate_window_router(
                Arc::new(boss_events::PgGateEvidence::new(db.pool.clone())),
                Arc::new(LiveFixture(live.clone())),
                Arc::new(FixedClock::new(now)),
            );
            let response = ask(
                app,
                "/api/events/gate-window?gate=policy-check&hours=72",
                Some(&user("operator", "service-tech")),
                "GET",
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let report: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(report["covers_requested_window"], covers, "{report}");
            if covers {
                assert_eq!(report["clean_since"], json!(now - Duration::hours(72)));
                assert_eq!(report["not_clean"], json!([]));
            } else {
                assert!(report["clean_since"].is_null(), "{report}");
                assert!(
                    report["not_clean"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|reason| reason.as_str().unwrap().contains(&identity.to_string())),
                    "{report}"
                );
            }
            assert_eq!(report["live"][0]["snapshot"], live);
            assert!(report["log"].is_object());
        }
    }
}

/// Backlog 93e0814a (2026-10-06). The reader requires what the launcher
/// recorded it started, and the record can only excuse a port that
/// shows no process: a record that names a running service as skipped,
/// a record that is missing or cut short, and a reader given no record
/// path at all each leave the window not clean, saying why.
#[tokio::test]
async fn the_required_services_are_what_the_launcher_started_and_no_record_hides_a_running_one() {
    use boss_core::gate_evidence::InMemoryGateEvidence;
    use boss_events_api::gate_window_http::LocalTallies;
    let now = Utc::now();
    let since = now - Duration::hours(74);
    let machine = |service: &str| {
        json!({"service":service,"mode":"report","recording_since":since,
        "rows":[],"overflow":0,"source_overflow":[],"not_clean":[],
        "evidence":{"recorder":true,"instance":format!("{service}-process"),"lost":0,"unstated":0,"retrying":false,"last_error":null}})
    };
    let events: Vec<_> = ["jobs", "people"]
        .into_iter()
        .map(|service| Event {
            id: Uuid::new_v4(),
            timestamp: since,
            source: service.into(),
            kind: "machine_gate.recording_began".into(),
            payload: json!({"service":service,"mode":"report","since":since,"instance":format!("{service}-process")}),
        })
        .collect();
    let upstream = |body: String, status: StatusCode| Upstream {
        body,
        status,
        redirect: None,
        delay: std::time::Duration::ZERO,
        reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let jobs = serve(upstream(machine("jobs").to_string(), StatusCode::OK)).await;
    let people = serve(upstream(machine("people").to_string(), StatusCode::OK)).await;
    // A port nothing listens on: bound once so the number is real, then
    // released.
    let closed = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    // A process the record does not account for. It refuses the tally
    // read, as a service that is up and unhealthy would; the port
    // answering is what gives it away.
    let stray = serve(upstream(
        "not ready".into(),
        StatusCode::SERVICE_UNAVAILABLE,
    ))
    .await;
    let dir = boss_testing::scratch_dir("gate-window-launch-record");
    let skip = "skip boss-assets-api module equipment is not on in the tenant manifest";
    let whole = format!(
        "boss-launch-record v1\nstart boss-jobs-api\nstart boss-people-api\n{skip}\nend 3\n"
    );
    let write = |name: &str, body: &str| {
        let path = dir.join(name);
        boss_testing::write_file(&path, body);
        path
    };
    let record = write("whole", &whole);
    let cut = write("cut", &whole.replace("end 3\n", ""));
    let all_started = write("all-started", &whole.replace(skip, "start boss-assets-api"));
    struct Case {
        name: &'static str,
        assets: String,
        record: Result<std::path::PathBuf, String>,
        clean: bool,
        required: serde_json::Value,
    }
    let two = json!(["jobs", "people"]);
    let three = json!(["assets", "jobs", "people"]);
    let cases = [
        Case {
            name: "skipped and silent",
            assets: closed.clone(),
            record: Ok(record.clone()),
            clean: true,
            required: two.clone(),
        },
        Case {
            name: "recorded as skipped while a process answers on its port",
            assets: stray.base.clone(),
            record: Ok(record.clone()),
            clean: false,
            required: two.clone(),
        },
        Case {
            name: "started and down",
            assets: closed.clone(),
            record: Ok(all_started),
            clean: false,
            required: three.clone(),
        },
        Case {
            name: "the record is missing",
            assets: closed.clone(),
            record: Ok(dir.join("absent")),
            clean: false,
            required: three.clone(),
        },
        Case {
            name: "the record is cut short",
            assets: closed.clone(),
            record: Ok(cut),
            clean: false,
            required: three.clone(),
        },
        Case {
            name: "no record path was handed to this process",
            assets: closed.clone(),
            record: Err("BOSS_LAUNCH_RECORD is unset".into()),
            clean: false,
            required: three.clone(),
        },
    ];
    for case in cases {
        let live = LocalTallies::new(
            fixture_client(std::time::Duration::from_secs(1)),
            vec![
                ("jobs".into(), jobs.base.clone()),
                ("people".into(), people.base.clone()),
                ("assets".into(), case.assets.clone()),
            ],
        )
        .with_launch_record(case.record.clone());
        let app = gate_window_router(
            Arc::new(InMemoryGateEvidence::new(events.clone())),
            Arc::new(live),
            Arc::new(FixedClock::new(now)),
        );
        let response = ask(
            app,
            "/api/events/gate-window?gate=machine-gate&hours=72",
            Some(&user("auditor", "service-tech")),
            "GET",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let result: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let name = case.name;
        assert_eq!(
            result["required_services"], case.required,
            "{name}: {result}"
        );
        assert_eq!(
            result["covers_requested_window"], case.clean,
            "{name}: {result}"
        );
        let why = result["not_clean"].to_string();
        match name {
            "skipped and silent" => {
                assert_eq!(result["not_launched"][0]["service"], "assets");
                assert_eq!(result["not_launched"][0]["listening"], false);
                // The excused port's read stays in the observation.
                assert_eq!(result["observation"]["reads"].as_array().unwrap().len(), 3);
                assert_eq!(result["observation"]["not_launched"], json!(["assets"]));
            }
            "recorded as skipped while a process answers on its port" => {
                assert!(
                    why.contains("assets: recorded as not launched")
                        && why.contains("its port accepts a connection"),
                    "{why}"
                );
            }
            "started and down" => assert!(why.contains("assets: GET"), "{why}"),
            _ => {
                assert!(why.contains("launch record: "), "{name}: {why}");
                assert_eq!(result["roster_errors"].as_array().unwrap().len(), 1);
                assert!(result["not_launched"].as_array().unwrap().is_empty());
            }
        }
    }
}
