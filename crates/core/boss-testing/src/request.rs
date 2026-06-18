//! Test request/response builders for axum routers.
//!
//! Usage:
//! ```ignore
//! let resp = TestRequest::post("/api/catalog/models")
//!     .json(&model)
//!     .send(&app)
//!     .await;
//! resp.assert_status(StatusCode::CREATED);
//! ```

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

/// A pending HTTP request that can be sent to an axum Router.
pub struct TestRequest {
    method: Method,
    uri: String,
    body: Body,
    content_type: Option<&'static str>,
    headers: Vec<(String, String)>,
}

impl TestRequest {
    pub fn new(method: Method, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            body: Body::empty(),
            content_type: None,
            headers: Vec::new(),
        }
    }

    /// Attach a header. Useful for simulating the `x-boss-user` the
    /// gateway injects when a real session hits a backend handler —
    /// tests that exercise `CurrentUser`-gated endpoints need to
    /// provide one, or they land as the anonymous/guest fallback.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Convenience: inject an `x-boss-user` header with the supplied id +
    /// role and otherwise-empty scope. Mirrors what the gateway builds
    /// in `build_user_json` for an authenticated real session.
    pub fn as_user(self, id: &str, role: &str) -> Self {
        let json = serde_json::json!({
            "id": id,
            "role": role,
            "access_tier": "user",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();
        self.header("x-boss-user", json)
    }

    /// Convenience: inject the `x-boss-user` header for the reserved
    /// `emp-smoke` fixture user (role `smoke-tester`). The role is
    /// granted Read on every projection resource by `defaults.rs`,
    /// never Write/Close/SignOff — so a smoke test that accidentally
    /// hits a mutation endpoint gets a clean Deny rather than
    /// drifting state. Use this in preference to hand-rolled
    /// `as_user("emp-X", "ceo")` calls so the policy matrix the test
    /// runs under is explicit + uniform across the suite.
    pub fn as_smoke(self) -> Self {
        let json = serde_json::json!({
            "id": "emp-smoke",
            "role": "smoke-tester",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "executive",
        })
        .to_string();
        self.header("x-boss-user", json)
    }

    pub fn get(uri: impl Into<String>) -> Self {
        Self::new(Method::GET, uri)
    }

    pub fn post(uri: impl Into<String>) -> Self {
        Self::new(Method::POST, uri)
    }

    pub fn put(uri: impl Into<String>) -> Self {
        Self::new(Method::PUT, uri)
    }

    pub fn delete(uri: impl Into<String>) -> Self {
        Self::new(Method::DELETE, uri)
    }

    /// Attach a JSON body. Content-Type is set automatically.
    pub fn json<T: Serialize>(mut self, body: &T) -> Self {
        let bytes = serde_json::to_vec(body).expect("serialize JSON body");
        self.body = Body::from(bytes);
        self.content_type = Some("application/json");
        self
    }

    /// Attach a raw body string.
    pub fn raw_body(mut self, body: impl Into<String>) -> Self {
        self.body = Body::from(body.into());
        self
    }

    /// Send the request to the router and return a TestResponse.
    pub async fn send(self, app: &Router) -> TestResponse {
        let mut req = Request::builder()
            .method(self.method.clone())
            .uri(self.uri.clone());
        if let Some(ct) = self.content_type {
            req = req.header("content-type", ct);
        }
        for (name, value) in &self.headers {
            req = req.header(name, value);
        }
        let req = req.body(self.body).expect("build request");

        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("axum Router::oneshot failed");

        TestResponse::from_response(self.method, self.uri, resp).await
    }
}

/// A captured HTTP response with assertion helpers.
///
/// Holds the body bytes pre-buffered so multiple assertions can read them.
pub struct TestResponse {
    pub method: Method,
    pub uri: String,
    pub status: StatusCode,
    pub body_bytes: Vec<u8>,
}

impl TestResponse {
    async fn from_response(method: Method, uri: String, resp: Response) -> Self {
        let status = resp.status();
        let body_bytes = resp
            .into_body()
            .collect()
            .await
            .expect("read response body")
            .to_bytes()
            .to_vec();
        Self {
            method,
            uri,
            status,
            body_bytes,
        }
    }

    /// Assert the response status code matches.
    /// Failure message includes method, URI, expected vs actual, and body.
    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        if self.status != expected {
            panic!(
                "\n  {} {}\n  expected status: {}\n  actual status:   {}\n  body: {}\n",
                self.method,
                self.uri,
                expected,
                self.status,
                self.body_text_truncated(500),
            );
        }
        self
    }

    /// Assert the response body parses as JSON of the given type.
    pub fn assert_json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body_bytes).unwrap_or_else(|e| {
            panic!(
                "\n  {} {}\n  failed to parse response as JSON: {}\n  body: {}\n",
                self.method,
                self.uri,
                e,
                self.body_text_truncated(500),
            )
        })
    }

    /// Get the response body as a string (lossy UTF-8).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body_bytes).to_string()
    }

    fn body_text_truncated(&self, max: usize) -> String {
        let s = self.body_text();
        if s.len() <= max {
            s
        } else {
            format!("{}... [{} bytes total]", &s[..max], s.len())
        }
    }
}
