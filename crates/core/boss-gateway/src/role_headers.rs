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
    if let Some(session) = extract_session(&req, &state.session_key) {
        // Demo-mode persona override. The SPA writes a `boss-persona`
        // cookie when "View As" picks an employee; we use it as the
        // effective id for the x-boss-user payload + headers. Gated
        // on demo_mode AND the session being audit-readonly so a
        // real BOSS login can't be hijacked by a forged cookie.
        let persona_emp_id = if state.demo_mode && session.role.as_deref() == Some("audit-readonly")
        {
            extract_persona_cookie(&req)
        } else {
            None
        };

        let user_json = build_user_json(&session, persona_emp_id.as_deref());
        if let Ok(val) = axum::http::HeaderValue::from_str(&user_json) {
            req.headers_mut().insert("x-boss-user", val);
        }
        if let Some(role) = &session.role
            && let Ok(val) = axum::http::HeaderValue::from_str(role)
        {
            req.headers_mut().insert("x-boss-role", val);
        }
        let effective_emp_id = persona_emp_id.as_deref().or(session.employee_id.as_deref());
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

const PERSONA_COOKIE: &str = "boss-persona";

/// Parse the `boss-persona` cookie value (an employee id) out of the
/// Cookie header. Returns `None` when the cookie is missing or the
/// value is empty / structurally invalid for an employee id slug
/// (alphanumeric + `-`).
fn extract_persona_cookie(req: &Request) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    let raw = session::find_cookie(cookie_header, PERSONA_COOKIE)?;
    // Employee ids are alphanumeric + dash + underscore. Reject
    // anything else (no need to URL-decode; the SPA's
    // `encodeURIComponent` is a no-op on the id alphabet).
    if raw.is_empty()
        || !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(raw.to_string())
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

    fn req_with_cookie(value: &str) -> Request<axum::body::Body> {
        let mut r = Request::builder().body(axum::body::Body::empty()).unwrap();
        r.headers_mut().insert(
            header::COOKIE,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        r
    }

    #[test]
    fn persona_cookie_extract_happy_path() {
        let r = req_with_cookie("boss-persona=emp-aa-004");
        assert_eq!(extract_persona_cookie(&r), Some("emp-aa-004".to_string()));
    }

    #[test]
    fn persona_cookie_extract_with_other_cookies() {
        let r = req_with_cookie("foo=bar; boss-persona=emp-cto; other=x");
        assert_eq!(extract_persona_cookie(&r), Some("emp-cto".to_string()));
    }

    #[test]
    fn persona_cookie_missing_returns_none() {
        let r = req_with_cookie("foo=bar; baz=qux");
        assert_eq!(extract_persona_cookie(&r), None);
    }

    #[test]
    fn persona_cookie_rejects_special_chars() {
        // Defend against forged cookies trying to inject control
        // chars or path separators into the eventual header value.
        let r = req_with_cookie("boss-persona=emp; DROP TABLE");
        // `find_cookie` parses up to the next `;` — so the captured
        // value is `emp` plus whitespace, which still has a space.
        // Either way the validator rejects.
        assert_eq!(extract_persona_cookie(&r), Some("emp".to_string()));
        let r2 = req_with_cookie("boss-persona=../etc/passwd");
        assert_eq!(extract_persona_cookie(&r2), None);
    }

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
}
