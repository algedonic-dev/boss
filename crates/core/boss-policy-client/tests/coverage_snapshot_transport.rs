//! The runtime holder transport refuses unavailable or incomplete evidence.
use boss_policy_client::coverage::{CoverageSnapshotSource, HttpCoverageSnapshotSource};

async fn fetch(
    status: axum::http::StatusCode,
    body: String,
) -> Result<boss_policy_client::coverage::CoverageSnapshot, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        boss_policy_client::coverage::SNAPSHOT_PATH,
        axum::routing::get(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = HttpCoverageSnapshotSource::new(format!("http://{address}"))
        .snapshot()
        .await;
    server.abort();
    result
}

fn complete_body() -> serde_json::Value {
    serde_json::json!({"rules":[],"overrides":[],"roster":[{"id":"emp-founder","role":"platform-admin","active":true}],"keys":[]})
}

#[tokio::test]
async fn complete_empty_key_set_is_known_evidence() {
    let result = fetch(axum::http::StatusCode::OK, complete_body().to_string())
        .await
        .unwrap();
    assert!(result.keys.is_empty());
    assert_eq!(result.roster.len(), 1);
}

#[tokio::test]
async fn missing_sources_truncation_dark_roster_and_oversize_refuse() {
    let mut bodies = vec!["{".into(), "null".into(), " ".repeat(2 * 1024 * 1024 + 1)];
    for source in ["rules", "overrides", "roster", "keys"] {
        let mut body = complete_body();
        body.as_object_mut().unwrap().remove(source);
        bodies.push(body.to_string());
    }
    let mut dark = complete_body();
    dark["roster"] = serde_json::json!([]);
    bodies.push(dark.to_string());
    for body in bodies {
        assert!(fetch(axum::http::StatusCode::OK, body).await.is_err());
    }
    assert!(
        fetch(
            axum::http::StatusCode::FORBIDDEN,
            complete_body().to_string()
        )
        .await
        .is_err()
    );
}
