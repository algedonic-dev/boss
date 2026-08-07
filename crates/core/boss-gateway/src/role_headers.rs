//! Middleware that extracts the Boss session and injects role headers.
//!
//! After this middleware runs, downstream proxy handlers automatically
//! forward these headers to backend services:
//!   - `X-Boss-User`: JSON-encoded `boss_policy::User` (id + role + tier
//!     + scopes) — consumed by `boss_policy_client::CurrentUser`. The
//!       header must carry the full shape so policy checks downstream
//!       have the role attached; a plain username isn't enough.
//!   - `X-Boss-Role`: duplicate of the role for easier log/grep use.
//!   - `X-Boss-Employee-Id`: Boss employee ID (e.g., "emp-001").
//!   - `X-Boss-Access-Tier`: operator | user.
//!
//! ## Demo-mode persona switching
//!
//! When demo mode is on, the SPA writes a `boss-persona=<employee-id>`
//! cookie when the user picks a persona via the "View As" menu. The
//! signed-session cookie (which the gateway minted as audit-readonly)
//! stays unchanged, so policy scope remains read-only across every
//! resource. But the **id** in `x-boss-user` switches to the chosen
//! employee — that's what gates like `messages-api`'s "you can only
//! read your own inbox" need to match. Without this, the SPA renders
//! the persona's inbox URL but the backend rejects with 403 because
//! the session id is still `demo@anonymous`.
//!
//! Trust note: this is a **demo affordance only**. A persona cookie
//! cannot escalate scope beyond the underlying session — an
//! audit-readonly session that claims a CEO persona still hits
//! audit-readonly policy rules. The only effect is which employee
//! id the per-row "is this you?" checks see.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use boss_gateway::session::{self, Session};

pub async fn inject_role_headers(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    // Edge strip, before anything else: the gateway is the SOLE
    // authority for `x-boss-*` identity headers — backends trust
    // them verbatim. Injection alone only overwrites the four
    // canonical names, and only when a session exists; a
    // session-less request (or a name the injector doesn't set)
    // would otherwise carry a client-forged value straight through
    // the proxy. See SECURITY.md §Deployment trust model.
    strip_boss_headers(req.headers_mut());

    if let Some(session) = extract_session(&req, &state.session_key) {
        // The identity is the session's, full stop. There used to be a
        // demo-mode persona override here — a `boss-persona` cookie the
        // SPA wrote for "View As" — gated on the session being the
        // synthetic `audit-readonly` one. Demo mode is gone, so the
        // gate could never open and the override was dead; a
        // client-supplied cookie deciding who you are is not something
        // to leave lying around unreachable.
        let user_json = build_user_json(&session, None);
        if let Ok(val) = axum::http::HeaderValue::from_str(&user_json) {
            req.headers_mut().insert("x-boss-user", val);
        }
        if let Some(role) = &session.role
            && let Ok(val) = axum::http::HeaderValue::from_str(role)
        {
            req.headers_mut().insert("x-boss-role", val);
        }
        let effective_emp_id = session.employee_id.as_deref();
        if let Some(emp_id) = effective_emp_id
            && let Ok(val) = axum::http::HeaderValue::from_str(emp_id)
        {
            req.headers_mut().insert("x-boss-employee-id", val);
        }
        if let Ok(val) = axum::http::HeaderValue::from_str(&session.access_tier) {
            req.headers_mut().insert("x-boss-access-tier", val);
        }
    }

    next.run(req).await
}

/// Remove every inbound `x-boss-*` header. HeaderName is always
/// lowercase in the http crate, so the prefix match is total.
fn strip_boss_headers(headers: &mut axum::http::HeaderMap) {
    let inbound: Vec<axum::http::HeaderName> = headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-boss-"))
        .cloned()
        .collect();
    for name in inbound {
        headers.remove(&name);
    }
}

/// Build the JSON payload the `CurrentUser` extractor expects. Shape
/// mirrors `boss_policy::User` — hand-written rather than importing the
/// policy crate to keep the gateway's deps minimal.
///
/// Reads scope fields (territory / reports / department) off the
/// signed Session cookie — they're captured at login time
/// from `GET /api/people/{id}/scope` and baked into the cookie.
/// That keeps the per-request injection zero-cost; staleness is
/// bounded by the 8h session TTL.
fn build_user_json(session: &Session, persona_emp_id: Option<&str>) -> String {
    let access_tier_value = match session.access_tier.as_str() {
        "operator" => "operator",
        _ => "user",
    };
    // Identity precedence: (1) demo-mode persona override, (2) signed
    // employee_id from the session, (3) username fallback.
    let id = persona_emp_id
        .or(session.employee_id.as_deref())
        .unwrap_or(&session.username);
    // Default-fall-through is `audit-readonly` (Demo Mode floor) so
    // that any session reaching a backend without an explicit role
    // gets read-everywhere / write-nothing semantics — belt-and-
    // suspenders for any path that lands here with role == None.
    let role = session.role.as_deref().unwrap_or("audit-readonly");
    // serde_json for robust escaping of id/role — some usernames
    // contain characters (`.`, `-`) that are header-safe but we want
    // to be defensive.
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": access_tier_value,
        "territory_account_ids": session.territory_account_ids,
        "direct_report_ids": session.direct_report_ids,
        "department": session.department,
    })
    .to_string()
}

fn extract_session(req: &Request, key: &[u8]) -> Option<Session> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    let raw = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(raw, key).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn build_user_json_uses_persona_when_provided() {
        let session = Session::new("demo@anonymous", 3600);
        let json = build_user_json(&session, Some("emp-aa-004"));
        assert!(json.contains("\"id\":\"emp-aa-004\""), "got: {json}");
    }

    #[test]
    fn build_user_json_falls_back_to_session_when_no_persona() {
        let mut session = Session::new("real@example.com", 3600);
        session.employee_id = Some("emp-001".to_string());
        let json = build_user_json(&session, None);
        assert!(json.contains("\"id\":\"emp-001\""), "got: {json}");
    }

    #[test]
    fn strip_boss_headers_removes_every_x_boss_name_only() {
        let mut headers = axum::http::HeaderMap::new();
        for (n, v) in [
            ("x-boss-user", "{\"id\":\"attacker\"}"),
            ("x-boss-role", "platform-admin"),
            ("x-boss-not-yet-invented", "1"),
            ("content-type", "application/json"),
            ("cookie", "a=b"),
        ] {
            headers.insert(n, axum::http::HeaderValue::from_static(v));
        }
        strip_boss_headers(&mut headers);
        assert!(
            !headers.keys().any(|k| k.as_str().starts_with("x-boss-")),
            "x-boss-* survived: {headers:?}"
        );
        assert!(headers.contains_key("content-type"));
        assert!(headers.contains_key("cookie"));
    }

    // --- Middleware-level: the strip-then-inject ordering is the
    // security property, so pin it through a probe router. ---

    const TEST_KEY: &[u8] = b"role-headers-test-key-0123456789";

    async fn probe(headers: axum::http::HeaderMap) -> String {
        let mut seen: Vec<String> = headers
            .iter()
            .filter(|(n, _)| n.as_str().starts_with("x-boss-"))
            .map(|(n, v)| format!("{}={}", n, v.to_str().unwrap_or("?")))
            .collect();
        seen.sort();
        if seen.is_empty() {
            "none".to_string()
        } else {
            seen.join(";")
        }
    }

    fn probe_app() -> axum::Router {
        let state = Arc::new(crate::AppState {
            session_key: TEST_KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(crate::perf::PerfCollector::new()),
        });
        axum::Router::new()
            .route("/probe", axum::routing::get(probe))
            .layer(axum::middleware::from_fn_with_state(
                state,
                inject_role_headers,
            ))
    }

    async fn probe_response(app: axum::Router, req: Request<axum::body::Body>) -> String {
        use tower::ServiceExt;
        let resp = app.oneshot(req).await.expect("probe request");
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("probe body");
        String::from_utf8(body.to_vec()).expect("utf8 body")
    }

    #[tokio::test]
    async fn forged_identity_headers_do_not_survive_the_edge() {
        let req = Request::builder()
            .uri("/probe")
            .header(
                "x-boss-user",
                r#"{"id":"attacker","role":"platform-admin"}"#,
            )
            .header("x-boss-role", "platform-admin")
            .header("x-boss-access-tier", "operator")
            .header("x-boss-not-yet-invented", "1")
            .body(axum::body::Body::empty())
            .unwrap();
        let seen = probe_response(probe_app(), req).await;
        assert_eq!(seen, "none", "forged headers reached the backend: {seen}");
    }

    #[tokio::test]
    async fn session_identity_wins_over_forged_headers() {
        let mut session = Session::new("real@example.com", 3600);
        session.employee_id = Some("emp-001".to_string());
        session.role = Some("brewmaster".to_string());
        let cookie = format!("{}={}", session::COOKIE_NAME, session.encode(TEST_KEY));

        let req = Request::builder()
            .uri("/probe")
            .header(header::COOKIE, cookie)
            .header(
                "x-boss-user",
                r#"{"id":"attacker","role":"platform-admin"}"#,
            )
            .header("x-boss-employee-id", "emp-attacker")
            .body(axum::body::Body::empty())
            .unwrap();
        let seen = probe_response(probe_app(), req).await;
        assert!(
            seen.contains("\"id\":\"emp-001\"") && !seen.contains("attacker"),
            "session identity must replace the forged headers, got: {seen}"
        );
    }
}
