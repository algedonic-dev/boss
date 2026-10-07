//! The production HTTP reader consumes the actual agents, automations
//! and people response contracts, with no anonymous read or guessed role.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, routing::get};
use boss_core::machine_token::Source;
use boss_core::role_of_record::RoleOfRecord;
use boss_policy_client::User;
use boss_policy_client::role_reader::HttpRoleReader;

async fn registry(
    agents: serde_json::Value,
    automations: serde_json::Value,
    people: serde_json::Value,
) -> (String, Arc<Mutex<Vec<(String, Option<String>)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut app = Router::new();
    for (path, body) in [
        ("/api/agents", agents),
        ("/api/agents/automations", automations),
        ("/api/people", people),
    ] {
        let recorded = seen.clone();
        app = app.route(
            path,
            get(move |headers: axum::http::HeaderMap| {
                let body = body.clone();
                let recorded = recorded.clone();
                async move {
                    recorded.lock().unwrap().push((
                        path.into(),
                        headers
                            .get("x-boss-user")
                            .and_then(|h| h.to_str().ok())
                            .map(String::from),
                    ));
                    Json(body)
                }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

fn empty() -> serde_json::Value {
    serde_json::json!({"data":[], "total":0})
}

#[tokio::test]
async fn a_complete_authenticated_fetch_is_reused_for_many_request_time_lookups() {
    let (base, seen) = registry(
        serde_json::json!({"data":[{"id":"agent-example","aliases":["alias"],"role":"tenant-role"}],"total":1}),
        empty(),
        serde_json::json!([]),
    )
    .await;
    let reader = HttpRoleReader::with_source(
        base.clone(),
        base,
        User::service("people"),
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    )
    .unwrap();
    let roles = reader.fetch_snapshot().await.unwrap();
    for _ in 0..8 {
        assert_eq!(
            roles.role_for("alias").await.unwrap().unwrap().actor_id,
            "agent-example"
        );
        assert_eq!(roles.role_for("missing").await.unwrap(), None);
    }
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        3,
        "lookups must not issue any additional registry reads"
    );
    for (_, header) in seen.iter() {
        let user: User = serde_json::from_str(header.as_deref().unwrap()).unwrap();
        assert_eq!(user.id, "automation:people");
    }
}

#[tokio::test]
async fn unavailable_malformed_oversized_and_timed_out_reads_are_unknown_not_absence() {
    use axum::response::IntoResponse;
    for failure in ["status", "json", "oversized", "timeout", "redirect"] {
        let app = Router::new()
            .route(
                "/api/agents",
                get(move || async move {
                    match failure {
                        "status" => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        "json" => "not JSON".into_response(),
                        "oversized" => "x".repeat(4 * 1024 * 1024 + 1).into_response(),
                        "redirect" => {
                            axum::response::Redirect::temporary("/pretend").into_response()
                        }
                        _ => {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            Json(empty()).into_response()
                        }
                    }
                }),
            )
            .route("/pretend", get(|| async { Json(empty()) }))
            .route("/api/agents/automations", get(|| async { Json(empty()) }))
            .route("/api/people", get(|| async { Json(serde_json::json!([])) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let reader = HttpRoleReader::with_source(
            base.clone(),
            base,
            User::service("classes"),
            Duration::from_millis(100),
            Arc::new(Source::fixed(None)),
        )
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), reader.role_for("actor"))
            .await
            .unwrap();
        assert!(
            result.is_err(),
            "{failure} must not become an unregistered actor: {result:?}"
        );
        task.abort();
    }
}

#[tokio::test]
async fn complete_absence_inactive_people_and_cross_registry_ambiguity_are_distinct() {
    for (agents, automations, people, ambiguous) in [
        (empty(), empty(), serde_json::json!([]), false),
        (
            empty(),
            empty(),
            serde_json::json!([{"id":"actor","role":"tenant-role","status":"inactive"}]),
            false,
        ),
        (
            serde_json::json!({"data":[{"id":"actor","role":"a","aliases":[]}],"total":1}),
            serde_json::json!({"data":[{"id":"actor","role":"b"}],"total":1}),
            serde_json::json!([]),
            true,
        ),
    ] {
        let (base, _) = registry(agents, automations, people).await;
        let reader = HttpRoleReader::with_source(
            base.clone(),
            base,
            User::service("classes"),
            Duration::from_secs(2),
            Arc::new(Source::fixed(None)),
        )
        .unwrap();
        let result = reader.role_for("actor").await;
        if ambiguous {
            assert!(matches!(
                result,
                Err(boss_core::role_of_record::RoleLookupError::Ambiguous(_))
            ));
        } else {
            assert!(result.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn a_people_row_without_status_is_unknown_not_unregistered() {
    let (base, _) = registry(
        empty(),
        empty(),
        serde_json::json!([{"id":"actor", "role":"tenant-person"}]),
    )
    .await;
    let reader = HttpRoleReader::with_source(
        base.clone(),
        base,
        User::service("classes"),
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    )
    .unwrap();
    assert!(reader.role_for("actor").await.is_err());
}

#[test]
fn the_report_mode_key_is_mounted_and_enforce_is_still_report_only() {
    use boss_core::machine_gate::{Mode, ModeSwitch};
    use boss_policy_client::role_reader::{MountedReportMode, ROLE_MODE_FILE};
    use boss_policy_client::role_reporting::{ReportMode, ReportModeSource};
    let root = boss_testing::repo_root();
    let manifest = std::fs::read_to_string(root.join("infra/cluster/manifests/boss.yaml")).unwrap();
    let key = std::path::Path::new(ROLE_MODE_FILE)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        manifest
            .lines()
            .any(|line| line == format!("  {key}: report"))
    );
    for (mode, want) in [
        (Mode::Off, ReportMode::Off),
        (Mode::Report, ReportMode::Report),
        (Mode::Enforce, ReportMode::Report),
    ] {
        let reader =
            MountedReportMode::over(Arc::new(ModeSwitch::new("test", "unused", (mode, None))));
        assert_eq!(reader.mode(), want);
    }
}

#[tokio::test]
async fn canonical_agent_alias_automation_and_person_are_read_without_inference() {
    let (base, seen) = registry(
        serde_json::json!({"data":[{"id":"agent-example","role":"tenant-agent","aliases":["agent@example.test"]}], "total":1}),
        serde_json::json!({"data":[{"id":"automation:example","role":"tenant-automation"}], "total":1}),
        serde_json::json!([{"id":"emp-example","role":"tenant-person","status":"active","email":"person@example.test"}]),
    ).await;
    let reader = HttpRoleReader::with_source(
        base.clone(),
        base,
        User::service("classes"),
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    )
    .unwrap();
    for (id, canonical, role) in [
        ("agent-example", "agent-example", "tenant-agent"),
        ("agent@example.test", "agent-example", "tenant-agent"),
        (
            "automation:example",
            "automation:example",
            "tenant-automation",
        ),
        ("emp-example", "emp-example", "tenant-person"),
    ] {
        let row = reader.role_for(id).await.unwrap().unwrap();
        assert_eq!(row.actor_id, canonical);
        assert_eq!(row.role.as_deref(), Some(role));
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        12,
        "exactly three finite GETs per lookup"
    );
    for (_, header) in seen.lock().unwrap().iter() {
        let actor: User = serde_json::from_str(header.as_deref().unwrap()).unwrap();
        assert_eq!(actor.id, "automation:classes");
    }
}

#[tokio::test]
async fn null_is_registered_but_missing_role_truncated_and_ambiguous_answers_are_errors() {
    let variants = [
        (
            serde_json::json!({"data":[{"id":"actor","role":null,"aliases":[]}],"total":1}),
            true,
        ),
        (
            serde_json::json!({"data":[{"id":"actor","aliases":[]}],"total":1}),
            false,
        ),
        (
            serde_json::json!({"data":[{"id":"actor","role":"tenant-role","aliases":[]}],"total":2}),
            false,
        ),
        (
            serde_json::json!({"data":[{"id":"actor","role":"a","aliases":[]},{"id":"actor","role":"b","aliases":[]}],"total":2}),
            false,
        ),
    ];
    for (agents, valid) in variants {
        let (base, _) = registry(agents, empty(), serde_json::json!([])).await;
        let reader = HttpRoleReader::with_source(
            base.clone(),
            base,
            User::service("classes"),
            Duration::from_secs(2),
            Arc::new(Source::fixed(None)),
        )
        .unwrap();
        let result = reader.role_for("actor").await;
        if valid {
            assert_eq!(result.unwrap().unwrap().role, None);
        } else {
            assert!(
                result.is_err(),
                "an untrustworthy answer must not be absence: {result:?}"
            );
        }
    }
}
