//! Pin the actual founding-hire request, without the session's token or login.

use std::{sync::Arc, time::Duration};

const TEST: &str = "the_first_operator_hire_post_carries_the_exact_actor";
const CHILD_BASE: &str = "BOSS_TEST_OPERATOR_HIRE_BASE";

#[tokio::test(flavor = "multi_thread")]
async fn the_first_operator_hire_post_carries_the_exact_actor() {
    if let Ok(base) = std::env::var(CHILD_BASE) {
        let dir = boss_testing::scratch_dir("operator-hire-wire");
        let path = dir.join("hires.toml");
        boss_testing::write_file(
            &path,
            "[[hire]]\nid = 'emp-wire-first'\nemail = 'wire@example.test'\nskills = []\ncertifications = []\n",
        );
        let summary = tokio::task::spawn_blocking(move || {
            let client = boss_core::machine_token::BlockingClient::build_with_source(
                reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)),
                Arc::new(boss_core::machine_token::Source::fixed(None)),
            )
            .unwrap();
            boss_people::operator_baseline::seed(&client, &base, &path, None)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!((summary.inserted, summary.skipped), (1, 0));
        return;
    }

    let (send, mut receive) = tokio::sync::mpsc::channel(1);
    let app = axum::Router::new().route(
        "/api/people",
        axum::routing::post(
            move |headers: axum::http::HeaderMap,
                  axum::Json(body): axum::Json<serde_json::Value>| {
                let send = send.clone();
                async move {
                    send.send((headers, body)).await.unwrap();
                    axum::http::StatusCode::CREATED
                }
            },
        )
        .get(|| async {
            axum::Json(serde_json::json!([{
                "id": "emp-wire-first", "email": "wire@example.test",
                "skills": [], "certifications": []
            }]))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    // Give the seed its bootstrap email through a child environment, never a
    // process-global write or fallback login file read.
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD_BASE, base)
        .env("BOSS_BOOTSTRAP_ADMIN_EMAIL", "wire@example.test")
        .output()
        .await
        .unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (headers, body) = receive.recv().await.expect("the seed sent its hire POST");
    let actual: serde_json::Value = serde_json::from_str(
        headers
            .get("x-boss-user")
            .expect("the hire POST must carry its actor identity")
            .to_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        actual,
        serde_json::json!({
            "id": "automation:operator-baseline",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "it",
        })
    );
    assert_eq!(body["id"], "emp-wire-first");
    assert_eq!(body["email"], "wire@example.test");
    assert!(headers.get("x-boss-machine-token").is_none());
    assert!(headers.get("x-sim-origin").is_none());
}
