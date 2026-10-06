//! THE PLATFORM OWNER'S PASSKEY ELEVATES HIS SESSION, AND NOTHING ELSE
//! DOES (backlog 3c92c5b8, decided by David 2026-09-27).
//!
//! The elevation ceremony over HTTP, against a stub people service that
//! answers the roster (so the owner is READ — the first active
//! platform-admin hire — never written here), the stored passkeys, and
//! the single-use challenge ledger. What these pin is every way the
//! ceremony refuses; the grant itself, after a verified assertion, is
//! pinned beside it in `elevation.rs`, and the header it produces, read
//! by the extractor boss-jobs uses, in `role_headers.rs`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, Uri, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_gateway::audit::AuthAudit;
use boss_gateway::elevation::{
    BEGIN_PATH, ELEVATE_FLOW, ElevationState, FINISH_PATH, RosterPlatformOwner, elevation_router,
};
use boss_gateway::passkey::PasskeyState;
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"elevation-http-test-session-key!";
const OWNER: &str = "emp-first-hire";
const OTHER_ADMIN: &str = "emp-later-hire";
const CREDENTIAL_ID: &str = "Y3JlZGVudGlhbC0x";

/// Every (method, path) the stub was asked, and every body POSTed.
type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

/// The people service: a roster with two active platform-admins (the
/// owner hired first), a stored-passkey list for each employee (`creds`),
/// and a challenge ledger whose consume answers `consumed`.
async fn people(creds: Value, consumed: Value) -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = Router::new().fallback(move |method: Method, uri: Uri, body: String| {
        let log = log.clone();
        let creds = creds.clone();
        let consumed = consumed.clone();
        async move {
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            log.lock()
                .unwrap()
                .push((method.to_string(), uri.to_string(), parsed));
            let path = uri.path();
            let reply = if path == "/api/people" {
                json!([
                    {"id": OTHER_ADMIN, "hire_date": "2026-09-20"},
                    {"id": OWNER, "hire_date": "2026-01-05"},
                ])
            } else if path.ends_with("/webauthn-credentials") {
                creds
            } else if path.ends_with("/consume") {
                consumed
            } else {
                json!({})
            };
            axum::Json(reply)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn router(base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    let passkey = Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: boss_gateway::machine_client::MachineClient::unstamped(reqwest::Client::builder())
            .unwrap(),
        people_base: base.clone(),
        jobs_base: base.clone(),
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
        audit: AuthAudit::disabled(),
    });
    let owner = RosterPlatformOwner::new(
        boss_gateway::machine_client::MachineClient::unstamped(reqwest::Client::builder()).unwrap(),
        base,
    );
    elevation_router(Arc::new(ElevationState {
        passkey,
        owner: Arc::new(owner),
    }))
}

fn platform_admin(employee: &str) -> Session {
    let mut s = Session::new(format!("{employee}@example.com"), 600);
    s.employee_id = Some(employee.to_string());
    s.role = Some("platform-admin".to_string());
    s
}

async fn post(
    router: &Router,
    path: &str,
    session: &Session,
    body: Value,
) -> (StatusCode, Option<String>, String) {
    let req = Request::post(path)
        .header("content-type", "application/json")
        .header("cookie", format!("{COOKIE_NAME}={}", session.encode(KEY)))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, cookie, String::from_utf8_lossy(&bytes).into_owned())
}

/// One operator-tier key and one self-enrolled user-tier key: only the
/// first is ever offered.
fn one_passkey() -> Value {
    json!([
        { "credential_id": CREDENTIAL_ID, "public_key": "", "access_tier": "operator" },
        { "credential_id": "c2VsZi1lbnJvbGxlZA", "public_key": "", "access_tier": "user" },
    ])
}

fn assertion() -> Value {
    json!({
        "id": CREDENTIAL_ID, "rawId": CREDENTIAL_ID, "type": "public-key",
        "response": { "authenticatorData": "AAAA", "clientDataJSON": "e30",
                      "signature": "AAAA", "userHandle": null },
        "extensions": {},
    })
}

fn minted(seen: &Seen) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(m, p, _)| m == "POST" && p == "/api/people/presence-challenges")
        .map(|(_, _, b)| b.clone())
        .collect()
}

/// The owner — the FIRST hire among the roster's platform-admins, not
/// the first row — begins with a fresh 32-byte challenge minted as the
/// `authenticate` flow for himself, user verification required.
#[tokio::test]
async fn the_owner_begins_with_a_fresh_authenticate_challenge() {
    let (base, seen) = people(one_passkey(), Value::Null).await;
    let (status, cookie, body) =
        post(&router(base), BEGIN_PATH, &platform_admin(OWNER), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(cookie.is_none(), "a begin changes no session");
    let body: Value = serde_json::from_str(&body).unwrap();
    let mints = minted(&seen);
    assert_eq!(mints.len(), 1, "{mints:?}");
    let mint = &mints[0];
    assert_eq!(mint["flow"], ELEVATE_FLOW);
    assert_eq!(mint["employee_id"], OWNER);
    assert!(mint.get("step_id").is_none(), "an elevation binds no step");
    let challenge = mint["challenge"].as_str().unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(challenge).unwrap().len(), 32);
    assert_eq!(body["publicKey"]["challenge"], challenge);
    assert_eq!(body["challenge_id"], mint["id"]);
    assert_eq!(body["publicKey"]["userVerification"], "required");
    assert_eq!(
        body["publicKey"]["allowCredentials"],
        json!([{ "type": "public-key", "id": CREDENTIAL_ID }])
    );
}

/// Another platform-admin holds a passkey too, and is refused before a
/// passkey is read or a challenge minted.
#[tokio::test]
async fn another_platform_admin_is_refused_before_any_mint() {
    let (base, seen) = people(one_passkey(), Value::Null).await;
    let (status, cookie, body) = post(
        &router(base),
        BEGIN_PATH,
        &platform_admin(OTHER_ADMIN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("only the platform owner"), "{body}");
    assert!(cookie.is_none());
    assert!(
        minted(&seen).is_empty(),
        "a refused begin minted a challenge"
    );
}

/// No tier rides on break-glass: the emergency session names no
/// employee, so it is refused before the people service is asked
/// anything at all.
#[tokio::test]
async fn a_break_glass_session_cannot_begin() {
    let (base, seen) = people(one_passkey(), Value::Null).await;
    let (_, glass) = boss_gateway::break_glass::mint_session(KEY);
    let (status, cookie, _) = post(&router(base), BEGIN_PATH, &glass, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(cookie.is_none());
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
}

/// A presence challenge — one minted to sign a step — never elevates,
/// even for the owner.
#[tokio::test]
async fn a_presence_challenge_never_elevates() {
    let consumed = json!({
        "id": "c1", "employee_id": OWNER, "flow": "presence",
        "challenge": URL_SAFE_NO_PAD.encode([7u8; 32]),
        "step_id": "s", "shape_hash": "h", "nonce": "n",
    });
    let (base, _) = people(json!([]), consumed).await;
    let (status, cookie, body) = post(
        &router(base),
        FINISH_PATH,
        &platform_admin(OWNER),
        json!({ "challenge_id": "c1", "credential": assertion() }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(cookie.is_none(), "a presence challenge set a cookie");
}

/// A challenge minted for another employee never elevates this one.
#[tokio::test]
async fn a_challenge_minted_for_someone_else_never_elevates() {
    let consumed = json!({
        "id": "c1", "employee_id": OTHER_ADMIN, "flow": ELEVATE_FLOW,
        "challenge": URL_SAFE_NO_PAD.encode([7u8; 32]),
    });
    let (base, _) = people(json!([]), consumed).await;
    let (status, cookie, _) = post(
        &router(base),
        FINISH_PATH,
        &platform_admin(OWNER),
        json!({ "challenge_id": "c1", "credential": assertion() }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(cookie.is_none());
}

/// The owner's own authenticate challenge, but he holds only a
/// self-enrolled user-tier key: refused before any signature is checked,
/// no cookie — the tier is never granted on the challenge alone, and
/// never through a user-tier key. (The signature-checked refusals — a
/// user-tier key's VALID assertion, and a wrong key — are driven with a
/// real software authenticator in only_an_operator_tier_passkey_elevates.rs.)
#[tokio::test]
async fn a_finish_with_no_operator_tier_key_elevates_nothing() {
    let consumed = json!({
        "id": "c1", "employee_id": OWNER, "flow": ELEVATE_FLOW,
        "challenge": URL_SAFE_NO_PAD.encode([7u8; 32]),
    });
    let user_tier_only = json!([
        { "credential_id": CREDENTIAL_ID, "public_key": "", "access_tier": "user" },
    ]);
    let (base, seen) = people(user_tier_only, consumed).await;
    let (status, cookie, body) = post(
        &router(base),
        FINISH_PATH,
        &platform_admin(OWNER),
        json!({ "challenge_id": "c1", "credential": assertion() }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(cookie.is_none(), "a user-tier key set a cookie");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, p, _)| p.starts_with("/api/people?")),
        "the owner is asked only after the assertion verifies"
    );
}
