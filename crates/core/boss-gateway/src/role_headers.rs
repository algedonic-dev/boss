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
//! ## Identity comes from the session, and only from the session
//!
//! There is one source: the signed `boss_session` cookie. Client-
//! supplied `x-boss-*` headers are stripped at the edge before
//! anything reads them, so a caller cannot assert who they are.
//!
//! The gateway used to accept a second source — a `boss-persona=
//! <employee-id>` cookie the SPA wrote for its "View As" menu, which
//! replaced the **id** in `x-boss-user` while leaving policy scope on
//! the underlying session. It was scoped to demo mode, and it went
//! when demo mode did.
//!
//! The dev-server still reads that cookie for `bun run dev`
//! (`apps/web/src/dev-server.ts`), where there is no gateway and no
//! real session to speak of. That is a local affordance and stops at
//! the gateway's edge. (The live smoke suite that also relied on it was
//! deleted 2026-09-18, design 0e07ce64; the nightly playground crawl
//! that replaced it signs in as the gateway's own guest session.)

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
        let user_json = session.user_json();
        if let Ok(val) = axum::http::HeaderValue::from_str(&user_json) {
            req.headers_mut().insert("x-boss-user", val);
        }
        // The effective role, the one `x-boss-user` above carries — not
        // `session.role` raw, which sent a blank role as an empty header
        // and a roleless session as none, disagreeing with the header
        // beside it (backlog 8f45e0b4 item 4).
        if let Ok(val) = axum::http::HeaderValue::from_str(session.effective_role()) {
            req.headers_mut().insert("x-boss-role", val);
        }
        let effective_emp_id = session.employee_id.as_deref();
        if let Some(emp_id) = effective_emp_id
            && let Ok(val) = axum::http::HeaderValue::from_str(emp_id)
        {
            req.headers_mut().insert("x-boss-employee-id", val);
        }
        if let Ok(val) = axum::http::HeaderValue::from_str(session.access_tier()) {
            req.headers_mut().insert("x-boss-access-tier", val);
        }
        // Presence ticket swap (docs/design/presence.md): a verified
        // assertion travels as `x-presence-ticket` — a name outside
        // the x-boss-* prefix so the edge strip above doesn't eat it.
        // A valid ticket (HMAC over the session key, unexpired) is
        // forwarded, STILL SIGNED, as `x-boss-presence`; an invalid one
        // is dropped here — the sign-off then fails 422 as a Session
        // stamp would. The strip is NOT what makes the forwarded header
        // unforgeable: the jobs API's machine door is reachable without
        // this gateway, so boss-jobs verifies the same signature itself
        // before it grants presence (backlog 72fe3640, 2026-09-24).
        let ticket = req
            .headers()
            .get(boss_gateway::passkey::TICKET_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if let Some(ticket) = ticket {
            req.headers_mut()
                .remove(boss_gateway::passkey::TICKET_HEADER);
            if let Some(hdr) =
                boss_gateway::passkey::presence_header_from_ticket(&ticket, &state.session_key)
                && let Ok(val) = axum::http::HeaderValue::from_str(&hdr)
            {
                req.headers_mut()
                    .insert(boss_gateway::passkey::PRESENCE_HEADER, val);
            }
        }
    }

    // Machine token (design 6805c764 choice 6, car 2): stamped on EVERY
    // request the gateway forwards, session or not. It used to ride the
    // session branch only (7fcd78fa phase 1), on the reasoning that on
    // sessionless traffic it would be a blanket pass; once reads join the
    // gate that would refuse every guest and public read the gateway
    // proxies. What the token asserts on this hop is "this passed the
    // edge strip above", so the identity headers beside it are always
    // the gateway's own resolution — the session's, or none — and a
    // guest's authority stays where it is decided today, in policy. The
    // strip already removed any client-forged copy (x-boss-*).
    //
    // Never on the gate's own routes (review of car 1, a159e1ee): there
    // the token would let any signed-in user read `/misses`, every
    // caller's address, with the gateway's credential.
    //
    // This stamp is a MARK, not the value that leaves: no upstream is
    // chosen yet, so the host rule cannot be asked here.
    // [`restamp_for_upstream`] asks it once the forward knows where it is
    // sending (backlog 6162aff2).
    if !boss_core::machine_gate::is_gate_route(req.uri().path())
        && let Some(token) = state.machine_token.current()
        && let Ok(val) = axum::http::HeaderValue::from_str(&token)
    {
        req.headers_mut()
            .insert(boss_core::machine_token::HEADER, val);
    }

    next.run(req).await
}

/// Re-decide the edge's stamp for the upstream a forward is about to send
/// to (backlog 6162aff2). [`inject_role_headers`] stamps the INBOUND
/// request before any route has chosen an upstream, so it cannot ask the
/// host rule every other stamp asks; its header says only that this
/// request passed the edge strip and is not one of the gate's own routes.
/// The upstream is operator-set (`BOSS_<NAME>_UPSTREAM`) and can name any
/// host, so the forward calls this: a marked request carries
/// `machine_token::token_for`'s value for `upstream_url` — none for a host
/// that is not loopback and not in `BOSS_MACHINE_TOKEN_HOSTS`, said once
/// per process — and an unmarked one carries none. Until 6162aff2 the
/// mark was forwarded verbatim, the one stamp in the estate that skipped
/// that rule.
pub(crate) fn restamp_for_upstream(
    headers: &mut axum::http::HeaderMap,
    source: &boss_core::machine_token::Source,
    hosts: &boss_core::machine_token::Hosts,
    upstream_url: &str,
) {
    if headers.remove(boss_core::machine_token::HEADER).is_some()
        && let Some(value) = boss_core::machine_token::token_for(source, hosts, upstream_url)
    {
        headers.insert(boss_core::machine_token::HEADER, value);
    }
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
    fn identity_is_the_signed_employee_id() {
        let mut session = Session::new("real@example.com", 3600);
        session.employee_id = Some("emp-001".to_string());
        let json = session.user_json();
        assert!(json.contains("\"id\":\"emp-001\""), "got: {json}");
    }

    /// A guest carries no employee_id, and must identify as itself
    /// rather than as an empty or defaulted employee — whatever it
    /// touches gets attributed to a name someone can look up.
    #[test]
    fn a_session_without_an_employee_identifies_by_username() {
        let mut session = Session::new("guest@algedonic.dev", 3600);
        session.role = Some("audit-readonly".to_string());
        let json = session.user_json();
        assert!(
            json.contains("\"id\":\"guest@algedonic.dev\""),
            "got: {json}"
        );
        assert!(json.contains("\"role\":\"audit-readonly\""), "got: {json}");
    }

    /// A session with no role tells every service it is a `visitor` —
    /// the least access — not `audit-readonly`, the widest read (design
    /// 2830b6b7). The fallback is `Session::effective_role`, the same
    /// one the proxy's read-only refusal asks (07e797b4), so a roleless
    /// session is read-only downstream AND at the edge.
    #[test]
    fn a_session_without_a_role_is_announced_as_a_visitor() {
        let session = Session::new("nobody@example.com", 3600);
        let json = session.user_json();
        let role = boss_core::roles::VISITOR_ROLE;
        assert!(
            json.contains(&format!("\"role\":\"{role}\"")),
            "got: {json}"
        );
        assert!(boss_core::roles::is_read_only_floor(role));
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
            machine_token: Default::default(),
            machine: boss_gateway::machine_client::MachineClient::unstamped(
                reqwest::Client::builder(),
            )
            .unwrap(),
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

    /// Backlog 8f45e0b4 item (4), 2026-09-27: `x-boss-role` forwarded
    /// `session.role` raw while `x-boss-user` beside it carried
    /// `effective_role()`, so a blank role went downstream as an empty
    /// `x-boss-role` and a roleless session as none at all — two headers
    /// on one request disagreeing about who sent it. Both say the
    /// effective role now: the visitor, for either.
    #[tokio::test]
    async fn the_role_header_is_the_effective_role_the_user_header_carries() {
        for role in [None, Some(""), Some("   ")] {
            let mut session = Session::new("real@example.com", 3600);
            session.employee_id = Some("emp-001".to_string());
            session.role = role.map(str::to_string);
            let cookie = format!("{}={}", session::COOKIE_NAME, session.encode(TEST_KEY));
            let req = Request::builder()
                .uri("/probe")
                .header(header::COOKIE, cookie)
                .body(axum::body::Body::empty())
                .unwrap();
            let seen = probe_response(probe_app(), req).await;
            let visitor = boss_core::roles::VISITOR_ROLE;
            assert!(
                seen.contains(&format!("x-boss-role={visitor};")),
                "{role:?}: {seen}"
            );
            assert!(
                seen.contains(&format!("\"role\":\"{visitor}\"")),
                "{role:?}: {seen}"
            );
        }
        // Control: a real role is forwarded as itself on both headers.
        let mut session = Session::new("real@example.com", 3600);
        session.role = Some("team-lead".to_string());
        let cookie = format!("{}={}", session::COOKIE_NAME, session.encode(TEST_KEY));
        let req = Request::builder()
            .uri("/probe")
            .header(header::COOKIE, cookie)
            .body(axum::body::Body::empty())
            .unwrap();
        let seen = probe_response(probe_app(), req).await;
        assert!(seen.contains("x-boss-role=team-lead;"), "{seen}");
    }

    /// Backlog 3c92c5b8, 2026-09-27: the IT registries answered 403 to
    /// the platform owner's browser because no session was ever operator
    /// tier. What a service BELIEVES about a session is what its own
    /// extractor reads off `x-boss-user` — `boss_policy_client::
    /// CurrentUser`, the one boss-jobs' `trust::can_read` is asked of — so
    /// that is what this reads, through the middleware. The owner's
    /// passkey-elevated session arrives as `Operator` (which `can_read`
    /// and `is_trusted` admit); the same owner's session straight from an
    /// OIDC login, and another platform-admin's, arrive as `User` (which
    /// they refuse, the 403 already pinned in boss-jobs' trust tests).
    #[tokio::test]
    async fn only_the_owners_passkey_elevated_session_reaches_a_service_as_operator() {
        use axum::extract::FromRequestParts;
        use boss_policy_client::{AccessTier, CurrentUser};
        async fn tier(req: Request<axum::body::Body>) -> String {
            let (mut parts, _) = req.into_parts();
            match CurrentUser::from_request_parts(&mut parts, &()).await {
                Ok(CurrentUser(user)) => format!("{:?}", user.access_tier),
                Err(_) => "refused".to_string(),
            }
        }
        let state = Arc::new(crate::AppState {
            session_key: TEST_KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(crate::perf::PerfCollector::new()),
            machine_token: Default::default(),
            machine: boss_gateway::machine_client::MachineClient::unstamped(
                reqwest::Client::builder(),
            )
            .unwrap(),
        });
        let app = axum::Router::new()
            .route("/whoami", axum::routing::get(tier))
            .layer(axum::middleware::from_fn_with_state(
                state,
                inject_role_headers,
            ));
        let seen_as = |session: Session| {
            let app = app.clone();
            async move {
                let cookie = format!("{}={}", session::COOKIE_NAME, session.encode(TEST_KEY));
                let req = Request::builder()
                    .uri("/whoami")
                    .header(header::COOKIE, cookie)
                    .body(axum::body::Body::empty())
                    .unwrap();
                probe_response(app, req).await
            }
        };
        let owner = "emp-owner";
        let login = |employee: &str| {
            let mut s = Session::new(format!("{employee}@example.com"), 3600);
            s.employee_id = Some(employee.to_string());
            s.role = Some(boss_core::roles::PLATFORM_ADMIN_ROLE.to_string());
            s
        };
        let elevated = boss_gateway::elevation::elevate(&login(owner), owner, 1)
            .expect("the owner's session is elevated");
        assert_eq!(
            seen_as(elevated).await,
            format!("{:?}", AccessTier::Operator)
        );
        assert_eq!(
            seen_as(login(owner)).await,
            format!("{:?}", AccessTier::User),
            "an OIDC-only login is user tier, owner or not"
        );
        assert!(
            boss_gateway::elevation::elevate(&login("emp-other-admin"), owner, 1).is_err(),
            "another platform-admin's passkey elevates nothing"
        );
        assert_eq!(
            seen_as(login("emp-other-admin")).await,
            format!("{:?}", AccessTier::User)
        );
    }

    // --- The machine door behind the gateway (design 6805c764, car 2;
    // backlog 2710c8fc). A service port, gated in `enforce` with the
    // estate token in its `current` slot, reached two ways: straight at
    // its port (the LAN machine door) and through this middleware (the
    // gateway). The in-process "proxy" below forwards method, path and
    // headers exactly as proxy.rs does, minus the network. ---

    use boss_core::machine_gate::{self, MachineGate, Mode, Reading, Slots};
    use boss_core::machine_token::{HEADER as TOKEN_HEADER, Source};
    use tower::ServiceExt;

    const ESTATE_TOKEN: &str = "estate-token-under-test";
    const FORGED_ADMIN: &str = r#"{"id":"attacker","role":"platform-admin"}"#;

    /// A service port in `enforce`, answering the `x-boss-user` it
    /// received — so a test reads who the service believed it served.
    fn service_port() -> axum::Router {
        let gate = Arc::new(MachineGate::new(
            "things",
            &["/api/things/health"],
            Reading::new(
                Mode::Enforce,
                Slots::new(Some(ESTATE_TOKEN.into()), None, None),
            ),
        ));
        let whoami = |headers: axum::http::HeaderMap| async move {
            headers
                .get("x-boss-user")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string()
        };
        machine_gate::gated(
            axum::Router::new().route("/api/things/{id}", axum::routing::put(whoami)),
            gate,
        )
    }

    /// The gateway in front of that port, holding `token` as the
    /// mounted `current` slot it stamps from.
    fn gateway_before(token: Option<&str>) -> axum::Router {
        let state = Arc::new(crate::AppState {
            session_key: TEST_KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(crate::perf::PerfCollector::new()),
            machine_token: Arc::new(Source::fixed(token.map(String::from))),
            machine: boss_gateway::machine_client::MachineClient::unstamped(
                reqwest::Client::builder(),
            )
            .unwrap(),
        });
        let forward = |req: Request<axum::body::Body>| async move {
            let (parts, body) = req.into_parts();
            let upstream = Request::from_parts(parts, body);
            service_port().oneshot(upstream).await.unwrap()
        };
        axum::Router::new()
            .fallback(forward)
            .layer(axum::middleware::from_fn_with_state(
                state,
                inject_role_headers,
            ))
    }

    fn put(path: &str, headers: &[(&str, &str)]) -> Request<axum::body::Body> {
        let mut req = Request::builder().method("PUT").uri(path);
        for (n, v) in headers {
            req = req.header(*n, *v);
        }
        req.body(axum::body::Body::empty()).unwrap()
    }

    fn session_cookie() -> String {
        let mut session = Session::new("real@example.com", 3600);
        session.employee_id = Some("emp-001".to_string());
        session.role = Some("team-lead".to_string());
        format!("{}={}", session::COOKIE_NAME, session.encode(TEST_KEY))
    }

    async fn send(app: axum::Router, req: Request<axum::body::Body>) -> (u16, String) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    /// The attack the item names: platform-admin asserted straight at a
    /// service port, with no token or a guessed one, is refused once the
    /// port enforces. And the control: a machine caller that attaches
    /// the mounted token is admitted, its asserted identity intact.
    #[tokio::test]
    async fn a_forged_identity_at_a_service_port_is_refused_and_a_token_holder_admitted() {
        for forged in [
            vec![("x-boss-user", FORGED_ADMIN)],
            vec![("x-boss-user", FORGED_ADMIN), (TOKEN_HEADER, "a-guess")],
        ] {
            let (status, body) = send(service_port(), put("/api/things/1", &forged)).await;
            assert_eq!(status, 401, "forged {forged:?} was admitted: {body}");
            assert!(!body.contains("attacker"), "{body}");
        }
        let (status, body) = send(
            service_port(),
            put(
                "/api/things/1",
                &[
                    ("x-boss-user", r#"{"id":"agent-seeder"}"#),
                    (TOKEN_HEADER, ESTATE_TOKEN),
                ],
            ),
        )
        .await;
        assert_eq!(status, 200, "the token holder was refused: {body}");
        assert!(body.contains("agent-seeder"), "{body}");
    }

    /// Design choice 6: the gateway stamps the token on EVERY request it
    /// forwards, sessionless or not, after its edge strip — so a forged
    /// identity or a forged token sent through it arrives as neither,
    /// and a session arrives as the session. Before car 2 a sessionless
    /// request went unstamped and every guest read would be refused the
    /// day reads joined the gate.
    #[tokio::test]
    async fn the_gateway_stamps_every_request_it_forwards_after_the_edge_strip() {
        let forged = [("x-boss-user", FORGED_ADMIN), (TOKEN_HEADER, "a-guess")];
        let (status, body) = send(
            gateway_before(Some(ESTATE_TOKEN)),
            put("/api/things/1", &forged),
        )
        .await;
        assert_eq!(status, 200, "a sessionless forward was refused: {body}");
        assert_eq!(body, "-", "a forged identity survived the edge: {body}");

        let cookie = session_cookie();
        let with_session = [(header::COOKIE.as_str(), cookie.as_str()), forged[0]];
        let (status, body) = send(
            gateway_before(Some(ESTATE_TOKEN)),
            put("/api/things/1", &with_session),
        )
        .await;
        assert_eq!(status, 200, "a session forward was refused: {body}");
        assert!(
            body.contains("\"id\":\"emp-001\"") && !body.contains("attacker"),
            "{body}"
        );

        // Nothing mounted (every pod until car 4): nothing stamped, so an
        // enforcing port refuses — the gateway is inert without its file.
        let (status, _) = send(gateway_before(None), put("/api/things/1", &with_session)).await;
        assert_eq!(status, 401);
    }

    /// The review of car 1 (a159e1ee): stamped, a signed-in user's read of
    /// `/api/machine-gate/misses` would carry the gateway's credential and
    /// answer every caller's address. The gate's own routes are never
    /// stamped, however the path is spelled.
    #[tokio::test]
    async fn the_gateway_never_stamps_the_gate_routes() {
        let cookie = session_cookie();
        for path in [
            "/api/machine-gate/misses",
            "/api/machine%2Dgate/misses",
            "//api/machine-gate/accepts",
        ] {
            let req = Request::builder()
                .uri(path)
                .header(header::COOKIE, cookie.as_str())
                .body(axum::body::Body::empty())
                .unwrap();
            let seen = probe_response(stamping_probe_app(), req).await;
            assert!(
                !seen.contains(TOKEN_HEADER),
                "{path} was stamped with the machine token: {seen}"
            );
        }
        // Control: the same session on an ordinary path IS stamped.
        let req = Request::builder()
            .uri("/probe")
            .header(header::COOKIE, cookie.as_str())
            .body(axum::body::Body::empty())
            .unwrap();
        let seen = probe_response(stamping_probe_app(), req).await;
        assert!(seen.contains(TOKEN_HEADER), "{seen}");
    }

    /// The gateway in front of the service port, forwarding as proxy.rs
    /// does: the edge's mark re-decided for `upstream_url` under `hosts`
    /// before the request leaves. The in-process port stands in for
    /// whatever host the URL names; what is under test is the header.
    fn gateway_forwarding_to(upstream_url: &'static str, hosts: &[&str]) -> axum::Router {
        let source = Arc::new(Source::fixed(Some(ESTATE_TOKEN.into())));
        let hosts = Arc::new(boss_core::machine_token::Hosts::listed(hosts));
        let state = Arc::new(crate::AppState {
            session_key: TEST_KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(crate::perf::PerfCollector::new()),
            machine_token: source.clone(),
            machine: boss_gateway::machine_client::MachineClient::unstamped(
                reqwest::Client::builder(),
            )
            .unwrap(),
        });
        let forward = move |req: Request<axum::body::Body>| {
            let (source, hosts) = (source.clone(), hosts.clone());
            async move {
                let mut req = req;
                restamp_for_upstream(req.headers_mut(), &source, &hosts, upstream_url);
                service_port().oneshot(req).await.unwrap()
            }
        };
        axum::Router::new()
            .fallback(forward)
            .layer(axum::middleware::from_fn_with_state(
                state,
                inject_role_headers,
            ))
    }

    /// Backlog 6162aff2: the forward stamp asks the shared host rule
    /// (`machine_token::token_for`) of the upstream it sends to, as every
    /// other stamp in the estate does. Loopback and a listed host are
    /// stamped and admitted; an upstream an operator pointed anywhere else
    /// arrives unstamped, so an enforcing port refuses it rather than the
    /// estate token reaching that host.
    #[tokio::test]
    async fn the_forward_stamp_asks_the_host_rule_of_its_upstream() {
        let cookie = session_cookie();
        let req = || {
            put(
                "/api/things/1",
                &[(header::COOKIE.as_str(), cookie.as_str())],
            )
        };
        for (upstream, listed, admitted) in [
            ("http://127.0.0.1:7900/api/things/1", &[][..], true),
            (
                "http://boss-things.boss.svc.cluster.local:7900/api/things/1",
                &[".boss.svc.cluster.local"][..],
                true,
            ),
            (
                "http://upstream.example.net:7900/api/things/1",
                &[".boss.svc.cluster.local"][..],
                false,
            ),
        ] {
            let (status, body) = send(gateway_forwarding_to(upstream, listed), req()).await;
            let want = if admitted { 200 } else { 401 };
            assert_eq!(
                status, want,
                "a forward to {upstream} under {listed:?} answered {status}: {body}"
            );
        }
    }

    #[test]
    fn a_restamp_carries_only_the_rules_value_and_only_for_a_marked_request() {
        let source = Source::fixed(Some(ESTATE_TOKEN.into()));
        let hosts = boss_core::machine_token::Hosts::listed([".boss.svc.cluster.local"]);
        let after = |marked: bool, url: &str| {
            let mut h = axum::http::HeaderMap::new();
            if marked {
                h.insert(
                    TOKEN_HEADER,
                    axum::http::HeaderValue::from_static("the-edges-mark"),
                );
            }
            restamp_for_upstream(&mut h, &source, &hosts, url);
            h.get(TOKEN_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        assert_eq!(
            after(true, "http://127.0.0.1:7900/x").as_deref(),
            Some(ESTATE_TOKEN)
        );
        assert_eq!(after(true, "http://upstream.example.net:7900/x"), None);
        // A gate route is never marked, so never stamped, even to loopback.
        assert_eq!(after(false, "http://127.0.0.1:7900/x"), None);
    }

    /// The header probe, behind a gateway that holds the token, on every
    /// path.
    fn stamping_probe_app() -> axum::Router {
        let state = Arc::new(crate::AppState {
            session_key: TEST_KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(crate::perf::PerfCollector::new()),
            machine_token: Arc::new(Source::fixed(Some(ESTATE_TOKEN.into()))),
            machine: boss_gateway::machine_client::MachineClient::unstamped(
                reqwest::Client::builder(),
            )
            .unwrap(),
        });
        axum::Router::new()
            .fallback(axum::routing::get(probe))
            .layer(axum::middleware::from_fn_with_state(
                state,
                inject_role_headers,
            ))
    }
}
