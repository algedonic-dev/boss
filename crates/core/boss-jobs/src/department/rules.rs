//! The dispatcher rule registry, read for the readiness read.
//!
//! The rules a deployment enforces live in the dispatcher — loaded from
//! `dispatcher_rules` at its boot and served at
//! `GET /api/dispatcher/rules`, with each rule's `source` (`product` or
//! `tenant:<id>`). The readiness read asks that surface rather than the
//! table: the surface is what is FIRING, and reading a sibling service's
//! table from here would be a second reader of its schema. Port + a
//! reqwest adapter + an in-memory fake, the `owner_resolution` shape.
//!
//! The read is made AS THE VIEWER (backlog 493cebf3). The dispatcher
//! scopes its rule reads now — a caller whose scope reads no packets is
//! told they are withheld — and this adapter sent no `x-boss-user`, so
//! it arrived as the identity-less caller that scope refuses: every
//! department's rules part would have read "withheld" for the operator.
//! It signs the way the schedule read beside it already did
//! (`dispatcher_schedule`), so the dispatcher judges the person asking.

use async_trait::async_trait;
use boss_policy_client::User;
use serde_json::Value;

#[async_trait]
pub trait DispatcherRules: Send + Sync {
    /// Every enforced rule, as the dispatcher's read surface serves it
    /// (`name`, `version`, `source`, `do`, …), read AS `viewer`. `Err` is
    /// the surface not answering, or withholding the rules from this
    /// viewer — a different fact from "no rules", and answered as one.
    async fn enforced_rules(&self, viewer: &User) -> Result<Vec<Value>, String>;
}

/// `GET {base}/api/dispatcher/rules`, the viewer in `x-boss-user` → its
/// `rules` array.
pub struct ReqwestDispatcherRules {
    base_url: String,
    http: boss_core::machine_token::Client,
}

impl ReqwestDispatcherRules {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = boss_core::http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl DispatcherRules for ReqwestDispatcherRules {
    async fn enforced_rules(&self, viewer: &User) -> Result<Vec<Value>, String> {
        let url = format!("{}/api/dispatcher/rules", self.base_url);
        let who = serde_json::to_string(viewer).map_err(|e| format!("GET {url}: {e}"))?;
        let who = reqwest::header::HeaderValue::from_str(&who)
            .map_err(|e| format!("GET {url}: the viewer cannot ride a header ({e})"))?;
        let resp = self
            .http
            .get(&url)
            .header("x-boss-user", who)
            .send()
            .await
            .map_err(|e| format!("GET {url}: the dispatcher did not answer ({e})"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url}: HTTP {status}"));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("GET {url}: the body is not JSON ({e})"))?;
        // The surface answers 200 with `error` set when its own load
        // failed, or when it withheld the rules from this viewer; neither
        // is an empty registry.
        if let Some(err) = body.get("error").and_then(Value::as_str) {
            return Err(format!("GET {url}: the dispatcher reported: {err}"));
        }
        Ok(body
            .get("rules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

/// A fixed rule list, for tests and the in-memory path.
pub struct FakeDispatcherRules(pub Vec<Value>);

#[async_trait]
impl DispatcherRules for FakeDispatcherRules {
    async fn enforced_rules(&self, _viewer: &User) -> Result<Vec<Value>, String> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn viewer() -> User {
        serde_json::from_value(serde_json::json!({
            "id": "emp-viewer", "role": "platform-admin", "access_tier": "operator"
        }))
        .expect("a viewer")
    }

    /// A stand-in dispatcher that answers the rules only to a signed
    /// caller, and withholds them from the identity-less one — the shape
    /// the real surface takes since backlog 493cebf3.
    async fn dispatcher() -> String {
        let app = axum::Router::new().route(
            "/api/dispatcher/rules",
            axum::routing::get(|headers: HeaderMap| async move {
                let signed = headers
                    .get("x-boss-user")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("emp-viewer"));
                axum::Json(if signed {
                    serde_json::json!({"rules": [{"name": "auto-park-on-gate-green"}]})
                } else {
                    serde_json::json!({"error": "withheld from this caller", "rules": []})
                })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// The readiness read signs as its viewer, so the dispatcher judges
    /// the operator asking — not an identity-less caller it withholds
    /// the rules from.
    #[tokio::test]
    async fn the_rules_read_is_signed_as_its_viewer() {
        let rules = ReqwestDispatcherRules::new(dispatcher().await);
        let read = rules.enforced_rules(&viewer()).await;
        assert_eq!(
            read,
            Ok(vec![serde_json::json!({"name": "auto-park-on-gate-green"})])
        );
    }
}
