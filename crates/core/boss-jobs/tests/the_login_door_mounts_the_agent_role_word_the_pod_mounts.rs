//! The function the binary calls, run: `LoginDoor::
//! judging_rows_by_the_mounted_word()` mounts the `agent-role` word at
//! the file the pod mounts, and judges by what that file says now.
//!
//! Backlog 4e51bf23, delta review 012ccafa D2. The first fold moved the
//! binary's mount and closure into the door "where a test runs them" —
//! and the test it wrote built its own `ModeSwitch` and called the
//! function one level down. Nothing ran THIS one, and two mutants of it
//! survived every test the car had:
//!
//!   A2  its body replaced by a closure that always answers `enforce`;
//!   A1  its default path pointed at `/etc/boss/machine-gate/
//!       policy-check`, a sibling word's file that reads `enforce` on
//!       the live cluster today.
//!
//! Either one, landed, judges `agent-claude` by a row that holds no
//! policy rule at the next boot: the coordinating session locked out of
//! the system of record by a car whose whole claim is that it changes
//! nothing.
//!
//! **This file holds exactly one test, and that is the point**, twice
//! over. The function reads a process environment variable, which no
//! test may set beside siblings that read the environment; and what it
//! mounted is known only from its mount line, which needs a subscriber
//! no sibling can race (the reason `station_boot_log.rs` gives). So the
//! process is this test's own.
//!
//! What is NOT read here: that the pod mounts the ConfigMap at that
//! directory. boss-testing's `the_agent_role_word_is_not_enforce_until_
//! the_rows_are_read_back.rs` holds the door's constant to the
//! manifest's mount path and key.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::any;
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agents::{
    AgentInput, AgentsRegistry, InMemoryAgents, LoginDoor, ROW_ROLE_MODE_FILE,
    ROW_ROLE_MODE_FILE_ENV, resolve_login,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::Captured;

/// Spelled here on purpose, and nowhere derived: the file the jobs pod
/// holds the word in, and the variable that moves it.
const THE_FILE_THE_POD_MOUNTS: &str = "/etc/boss/machine-gate/agent-role";
const THE_OVERRIDE: &str = "BOSS_AGENT_ROLE_MODE_FILE";

async fn echo(headers: HeaderMap) -> axum::Json<serde_json::Value> {
    let seen: serde_json::Value = headers
        .get("x-boss-user")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
    axum::Json(serde_json::json!({ "seen": seen }))
}

/// `agent-claude` holding `engineering-agent`, as the live registry did
/// on 2026-10-08, behind a door wired the way the binary wires it.
async fn the_binarys_door() -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let registry = InMemoryAgents::new();
    let stamp = publisher.stamp_with_actor(publisher.default_actor()).await;
    registry
        .publish(
            &[AgentInput {
                id: "agent-claude".to_string(),
                display_name: "agent-claude".to_string(),
                default_model: "opus-5".to_string(),
                aliases: vec!["claude@algedonic.dev".to_string()],
                role: Some("engineering-agent".to_string()),
                department: None,
                hourly_budget_usd_micros: None,
                max_concurrent_runs: None,
            }],
            PublishMode::InsertIfAbsent,
            &stamp,
        )
        .await
        .expect("declare the agent");
    let door = Arc::new(
        LoginDoor::new(Arc::new(registry) as Arc<dyn AgentsRegistry>, publisher)
            .judging_rows_by_the_mounted_word(),
    );
    axum::Router::new()
        .route("/echo", any(echo))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login))
}

/// One write signed with the alias, asserting `platform-admin` at
/// operator tier: the role and tier the handler saw.
async fn judged_as(app: &axum::Router) -> (String, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/echo")
                .header(
                    "x-boss-user",
                    r#"{"id":"claude@algedonic.dev","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}"#,
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["seen"]["id"], "agent-claude", "{body}");
    let text = |field: &str| body["seen"][field].as_str().unwrap_or("").to_string();
    (text("role"), text("access_tier"))
}

fn as_asserted() -> (String, String) {
    ("platform-admin".to_string(), "operator".to_string())
}

fn by_the_row() -> (String, String) {
    ("engineering-agent".to_string(), "user".to_string())
}

#[tokio::test]
async fn the_binarys_wiring_mounts_the_pods_file_and_judges_by_what_it_says() {
    assert_eq!(ROW_ROLE_MODE_FILE, THE_FILE_THE_POD_MOUNTS);
    assert_eq!(ROW_ROLE_MODE_FILE_ENV, THE_OVERRIDE);

    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");
    // The one mount line the log gained since `from`.
    let mount_line = |from: usize| {
        let said = log.text()[from..].to_string();
        let lines: Vec<&str> = said
            .lines()
            .filter(|l| l.contains("agent role mounted"))
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "the wiring mounts the word once and says so: {said:?}"
        );
        lines[0].to_string()
    };

    // 1. NO OVERRIDE, as in the pod: the file mounted is the pod's. The
    //    mount line is the operator's first read after landing, and here
    //    it is the only witness of the default path (mutant A1).
    // SAFETY: this binary holds one test; no other thread reads or
    // writes the environment while this runs.
    unsafe { std::env::remove_var(THE_OVERRIDE) };
    let mark = log.text().len();
    let app = the_binarys_door().await;
    let line = mount_line(mark);
    assert!(
        line.contains(&format!("file=\"{THE_FILE_THE_POD_MOUNTS}\"")),
        "with no override the word is mounted at the pod's file: {line}"
    );
    if !std::path::Path::new(THE_FILE_THE_POD_MOUNTS).exists() {
        // Every box but a jobs pod: no file, which is `off` (mutant A2).
        assert!(line.contains("mode=\"off\""), "{line}");
        assert_eq!(judged_as(&app).await, as_asserted(), "no file is off");
    }

    // 2. THE OVERRIDE NAMES THE FILE, and an absent one is `off`.
    let dir = tempfile::tempdir().expect("a temp directory");
    let file = dir.path().join("agent-role");
    // SAFETY: as above.
    unsafe { std::env::set_var(THE_OVERRIDE, &file) };
    let mark = log.text().len();
    let app = the_binarys_door().await;
    let line = mount_line(mark);
    assert!(
        line.contains(&format!("file=\"{}\"", file.display())),
        "the override names the file: {line}"
    );
    assert!(
        line.contains("mode=\"off\""),
        "an absent file is off: {line}"
    );
    assert_eq!(judged_as(&app).await, as_asserted(), "absent: off");

    // 3. THE WORD WRITTEN LATER MOVES THE DOOR, with no restart: the
    //    mount's own re-read picks it up (five seconds), and the next
    //    request is judged by the row.
    std::fs::write(&file, "enforce\n").expect("write the word");
    let started = Instant::now();
    let mut seen = judged_as(&app).await;
    while seen != by_the_row() && started.elapsed() < Duration::from_secs(40) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        seen = judged_as(&app).await;
    }
    assert_eq!(
        seen,
        by_the_row(),
        "`enforce` written to the mounted file judges the next request by its row"
    );

    // 4. ...and a door built while the file says `enforce` judges from
    //    its first request, and one built on `report` does not.
    let mark = log.text().len();
    let app = the_binarys_door().await;
    assert!(mount_line(mark).contains("mode=\"enforce\""));
    assert_eq!(judged_as(&app).await, by_the_row(), "mounted at enforce");
    std::fs::write(&file, "report\n").expect("write the word");
    let app = the_binarys_door().await;
    assert_eq!(judged_as(&app).await, as_asserted(), "mounted at report");
}
