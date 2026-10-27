//! 04fcbecd: the real bounded HTTP read says WHY the inline answer is
//! unread. One test owns the process-wide subscriber; sibling tests
//! must not race callsite interest with this capture.
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode};
use axum::routing::get;
use boss_core::machine_token::Source;
use boss_gateway::machine_client::MachineClient;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);
impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn unread_inline_flights_log_the_viewer_latency_and_cause_without_headers() {
    let log = Captured::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(log.clone())
            .with_max_level(tracing::Level::WARN)
            .finish(),
    )
    .unwrap();
    let app = Router::new()
        .route(
            "/status",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "rolling") }),
        )
        .route("/json", get(|| async { "{" }))
        .route("/shape", get(|| async { r#"{"flights":"bad"}"# }))
        .route(
            "/headers",
            get(|| async {
                std::future::pending::<()>().await;
                "unreachable"
            }),
        )
        .route(
            "/body",
            get(|| async {
                Response::new(Body::from_stream(futures::stream::pending::<
                    Result<bytes::Bytes, std::io::Error>,
                >()))
            }),
        )
        .route("/empty", get(|| async { r#"{"flights":[]}"# }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = MachineClient::build_with_source(
        reqwest::Client::builder(),
        Arc::new(Source::fixed(Some("secret-machine-value".into()))),
    )
    .unwrap();
    let viewer = HeaderValue::from_static(r#"{"id":"emp-test","unused":"secret-person-value"}"#);
    for (path, reason) in [
        ("status", "status"),
        ("json", "body"),
        ("shape", "shape"),
        ("headers", "transport"),
        ("body", "body"),
    ] {
        let before = log.text().lines().count();
        let answer = tokio::time::timeout(Duration::from_secs(5),
            boss_gateway::flights_inline::read_for(&client, format!("http://{address}/{path}"), viewer.clone()))
            .await.expect("the harness bounds observation; the production request still has its own two-second deadline");
        assert_eq!(answer, None, "{path}");
        let text = log.text();
        let rows = text
            .lines()
            .skip(before)
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "{path}: {text}");
        assert_eq!(rows[0]["fields"]["viewer"], "emp-test", "{path}: {text}");
        assert!(
            rows[0]["fields"]["elapsed_ms"].is_number(),
            "{path}: {text}"
        );
        assert_eq!(rows[0]["fields"]["reason"], reason, "{path}: {text}");
        assert_eq!(
            rows[0]["fields"]["timeout"],
            matches!(path, "headers" | "body"),
            "{path}: {text}"
        );
        assert!(
            !text.contains("secret-machine-value") && !text.contains("secret-person-value"),
            "{path}: {text}"
        );
    }
    let before = log.text();
    assert_eq!(
        boss_gateway::flights_inline::read_for(&client, format!("http://{address}/empty"), viewer)
            .await
            .as_deref(),
        Some(r#"{"flights":[]}"#)
    );
    assert_eq!(
        log.text(),
        before,
        "a valid empty answer is known OFF, not a miss"
    );
    server.abort();
}
