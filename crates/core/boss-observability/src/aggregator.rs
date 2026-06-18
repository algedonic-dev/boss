//! Per-VM HTTP snapshot aggregator.
//!
//! Fans a single observability request out to every configured VM's
//! Cybernetics HTTP API, then returns the combined response. Each VM is
//! queried concurrently. Individual failures do not fail the whole request —
//! the failing VM gets an `error` entry so the dashboard can show partial
//! state.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use reqwest::Client;
use serde::Serialize;
use tracing::warn;

use crate::config::VmEntry;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Known Cybernetics introspection endpoints. See `boss-cybernetics/src/http.rs`.
#[derive(Debug, Clone, Copy)]
pub enum Endpoint {
    Health,
    Agents,
    Queues,
    Runs,
    Costs,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Endpoint::Health => "/health",
            Endpoint::Agents => "/agents",
            Endpoint::Queues => "/queues",
            Endpoint::Runs => "/runs",
            Endpoint::Costs => "/costs",
        }
    }
}

/// Result of proxying a single VM. On failure `body` is `null` and `error`
/// carries the reason. `status` is the upstream HTTP status or 0 on transport
/// error.
#[derive(Debug, Clone, Serialize)]
pub struct VmResult {
    pub vm_id: String,
    pub status: u16,
    pub body: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Aggregator {
    http: Client,
    vms: Arc<[VmEntry]>,
}

impl Aggregator {
    pub fn new(vms: Vec<VmEntry>) -> Self {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("building reqwest client");
        Self {
            http,
            vms: vms.into(),
        }
    }

    pub fn vms(&self) -> &[VmEntry] {
        &self.vms
    }

    /// Fetch a single endpoint from every configured VM, concurrently.
    pub async fn fetch_all(&self, endpoint: Endpoint) -> Vec<VmResult> {
        let tasks = self.vms.iter().map(|vm| {
            let http = self.http.clone();
            let url = format!("{}{}", vm.http_url.trim_end_matches('/'), endpoint.path());
            let vm_id = vm.id.clone();
            async move { fetch_one(&http, &vm_id, &url).await }
        });
        join_all(tasks).await
    }

    /// Fetch a single endpoint from one named VM. None if no such VM.
    pub async fn fetch_one_by_id(&self, vm_id: &str, endpoint: Endpoint) -> Option<VmResult> {
        let vm = self.vms.iter().find(|v| v.id == vm_id)?;
        let url = format!("{}{}", vm.http_url.trim_end_matches('/'), endpoint.path());
        Some(fetch_one(&self.http, vm_id, &url).await)
    }
}

async fn fetch_one(http: &Client, vm_id: &str, url: &str) -> VmResult {
    match http.get(url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.json::<serde_json::Value>().await {
                Ok(body) => VmResult {
                    vm_id: vm_id.to_string(),
                    status,
                    body,
                    error: None,
                },
                Err(e) => {
                    warn!(vm = %vm_id, url = %url, error = %e, "decoding upstream body");
                    VmResult {
                        vm_id: vm_id.to_string(),
                        status,
                        body: serde_json::Value::Null,
                        error: Some(format!("decode: {e}")),
                    }
                }
            }
        }
        Err(e) => {
            warn!(vm = %vm_id, url = %url, error = %e, "fetching from VM");
            VmResult {
                vm_id: vm_id.to_string(),
                status: 0,
                body: serde_json::Value::Null,
                error: Some(e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use serde_json::json;
    use tokio::net::TcpListener;

    /// Spin up a tiny axum server that mimics Cybernetics HTTP on a random
    /// port, and return its base URL plus a cancellation handle.
    async fn mock_vm(agents: serde_json::Value) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route(
                "/health",
                get(|| async { axum::Json(json!({"status": "ok"})) }),
            )
            .route(
                "/agents",
                get(move || {
                    let agents = agents.clone();
                    async move { axum::Json(agents) }
                }),
            )
            .route("/queues", get(|| async { axum::Json(json!([])) }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn fetch_all_returns_body_per_vm() {
        let (url_a, _h1) = mock_vm(json!([{"id": "develop"}])).await;
        let (url_b, _h2) = mock_vm(json!([{"id": "deploy"}])).await;

        let agg = Aggregator::new(vec![
            VmEntry {
                id: "a".into(),
                http_url: url_a,
            },
            VmEntry {
                id: "b".into(),
                http_url: url_b,
            },
        ]);

        let results = agg.fetch_all(Endpoint::Agents).await;
        assert_eq!(results.len(), 2);
        let a = results.iter().find(|r| r.vm_id == "a").unwrap();
        assert_eq!(a.status, 200);
        assert_eq!(a.body[0]["id"], "develop");
        assert!(a.error.is_none());
    }

    #[tokio::test]
    async fn fetch_all_records_transport_errors_per_vm() {
        let (good_url, _h) = mock_vm(json!([])).await;
        let agg = Aggregator::new(vec![
            VmEntry {
                id: "good".into(),
                http_url: good_url,
            },
            // unreachable port
            VmEntry {
                id: "bad".into(),
                http_url: "http://127.0.0.1:1".into(),
            },
        ]);

        let results = agg.fetch_all(Endpoint::Health).await;
        let good = results.iter().find(|r| r.vm_id == "good").unwrap();
        let bad = results.iter().find(|r| r.vm_id == "bad").unwrap();
        assert_eq!(good.status, 200);
        assert!(good.error.is_none());
        assert_eq!(bad.status, 0);
        assert!(bad.error.is_some());
    }

    #[tokio::test]
    async fn fetch_one_by_id_returns_none_for_unknown() {
        let agg = Aggregator::new(vec![]);
        assert!(
            agg.fetch_one_by_id("ghost", Endpoint::Health)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn fetch_one_by_id_returns_result_for_known() {
        let (url, _h) = mock_vm(json!([])).await;
        let agg = Aggregator::new(vec![VmEntry {
            id: "v1".into(),
            http_url: url,
        }]);
        let result = agg.fetch_one_by_id("v1", Endpoint::Health).await.unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body["status"], "ok");
    }

    #[test]
    fn trailing_slash_in_http_url_is_handled() {
        // Pure URL construction check.
        let url = format!(
            "{}{}",
            "http://host:7700/".trim_end_matches('/'),
            Endpoint::Health.path()
        );
        assert_eq!(url, "http://host:7700/health");
    }
}
