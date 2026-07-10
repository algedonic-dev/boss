//! Demo-mode synthetic session minter.
//!
//! When `BOSS_DEMO_MODE=1` is set, anonymous visitors (no valid
//! `boss_session` cookie) get a synthetic `audit-readonly` session
//! so the playground deployment shows live data without forcing
//! signup. Read access to every projection; no writes. A real
//! local-auth login at `/login` upgrades the session to the
//! employee's actual role.
//!
//! The mint is **idempotent** per session lifetime: once minted,
//! subsequent requests carry the cookie and skip this path entirely.
//! Browsers persist the cookie so the entire visit reuses the same
//! synthetic identity.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use boss_gateway::session::{self, Session};

/// Middleware. Pass through when a valid session cookie is already
/// present; otherwise (and only with `demo_mode=true`) mint a
/// synthetic `audit-readonly` session and inject it both into the
/// request (so downstream handlers see it) and the response (so
/// the browser persists it).
pub async fn session_minter(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if has_valid_session(req.headers(), &state.session_key) || !state.demo_mode {
        return next.run(req).await;
    }
    mint_demo_session(req, next, &state).await
}

async fn mint_demo_session(mut req: Request<Body>, next: Next, state: &AppState) -> Response {
    let mut session = Session::new("demo@anonymous", session::DEFAULT_TTL_SECONDS);
    session.role = Some("audit-readonly".to_string());
    let cookie_value = session.encode(&state.session_key);

    let injected = format!("{}={}", session::COOKIE_NAME, cookie_value);
    let merged_cookie = match req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        Some(existing) if !existing.is_empty() => format!("{existing}; {injected}"),
        _ => injected,
    };
    if let Ok(hv) = HeaderValue::from_str(&merged_cookie) {
        req.headers_mut().insert(header::COOKIE, hv);
    }

    let mut resp = next.run(req).await;

    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        session::DEFAULT_TTL_SECONDS,
        "/",
    );
    if let Ok(hv) = HeaderValue::from_str(&set_cookie) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }

    resp
}

fn has_valid_session(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(raw) = session::find_cookie(cookie_header, session::COOKIE_NAME) else {
        return false;
    };
    Session::decode(raw, key).is_ok()
}
