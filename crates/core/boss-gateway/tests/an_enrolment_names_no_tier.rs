//! AN ENROLMENT NAMES NO TIER, AND STILL ENROLS (backlog 1d9970d1).
//!
//! People's credential POST used to take `access_tier` from its body,
//! so any platform-admin caller — the owner's user-tier browser cookie
//! through the `/api/people` proxy among them — could store an
//! operator-tier key and elevate with it. People now binds `user` for
//! every body and REFUSES a body naming a tier (422), which is pinned on
//! its side by `pg_webauthn_ceremony.rs`. That refusal would break every
//! enrolment if `register_finish` still sent the key it used to send, so
//! this drives the whole ceremony — `register_begin`, a software
//! authenticator's attestation, `register_finish` — and reads back the
//! body people was sent: exactly `credential_id`, `public_key`, `label`,
//! the body people's test proves it stores as `user`.
//!
//! The attestation is REAL: a P-256 key the test holds, attestation
//! format `none`, over the challenge `register_begin` minted — so the
//! store is reached past webauthn-rs's verification, not stubbed around
//! it.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::Json;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::event::Event;
use boss_core::port::EventRecorder;
use boss_gateway::audit::AuthAudit;
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use openssl::bn::{BigNum, BigNumContext};
use openssl::ec::{EcGroup, EcKey};
use openssl::nid::Nid;
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"an-enrolment-names-no-tier-key!!";
const RP_ID: &str = "boss.test";
const ORIGIN: &str = "https://boss.test";
const EMPLOYEE: &str = "emp-enrols";
const CRED_ID: &[u8] = b"soft-authenticator-credential";

#[derive(Default)]
struct Captured(Mutex<Vec<Event>>);

#[async_trait::async_trait]
impl EventRecorder for Captured {
    async fn record(&self, event: &Event) -> Result<(), String> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// What the people double saw: the minted challenge row (handed back on
/// consume, as people's ledger does) and each credential body stored.
#[derive(Default)]
struct PeopleSaw {
    minted: Mutex<Option<Value>>,
    stored: Mutex<Vec<Value>>,
}

async fn people(saw: Arc<PeopleSaw>) -> String {
    let (mint, consume, store) = (saw.clone(), saw.clone(), saw);
    let app = Router::new()
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(|| async { axum::Json(json!([])) }).post(move |Json(body): Json<Value>| {
                let store = store.clone();
                async move {
                    store.stored.lock().unwrap().push(body);
                    StatusCode::CREATED
                }
            }),
        )
        .route(
            "/api/people/presence-challenges",
            post(move |Json(body): Json<Value>| {
                let mint = mint.clone();
                async move {
                    *mint.minted.lock().unwrap() = Some(body);
                    StatusCode::CREATED
                }
            }),
        )
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(move || {
                let consume = consume.clone();
                async move { axum::Json(consume.minted.lock().unwrap().clone().unwrap()) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn router(base: String) -> Router {
    let origin = Url::parse(ORIGIN).unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: boss_gateway::machine_client::MachineClient::build(reqwest::Client::builder())
            .unwrap(),
        people_base: base.clone(),
        jobs_base: base,
        webauthn: WebauthnBuilder::new(RP_ID, &origin)
            .unwrap()
            .build()
            .unwrap(),
        audit: AuthAudit::spawn(Arc::new(Captured::default())),
    }))
}

async fn post_json(router: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let mut session = Session::new("enrols@example.com", 600);
    session.employee_id = Some(EMPLOYEE.to_string());
    let req = Request::post(path)
        .header("content-type", "application/json")
        .header("cookie", format!("{COOKIE_NAME}={}", session.encode(KEY)))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

/// A CBOR byte-string header for `len` bytes (major type 2).
fn cbor_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        n if n < 24 => out.push(0x40 | n as u8),
        n if n < 256 => out.extend_from_slice(&[0x58, n as u8]),
        n => {
            out.push(0x59);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
    }
    out.extend_from_slice(bytes);
}

/// A CBOR text string (major type 3); every text here is under 24 bytes.
fn cbor_text(out: &mut Vec<u8>, text: &str) {
    out.push(0x60 | text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

/// A software authenticator's registration response over `challenge`:
/// a fresh P-256 key, attestation format `none`, user present and
/// verified — what a browser's `navigator.credentials.create` returns.
fn attestation(challenge_b64: &str) -> Value {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let ec = EcKey::generate(&group).unwrap();
    let mut ctx = BigNumContext::new().unwrap();
    let (mut x, mut y) = (BigNum::new().unwrap(), BigNum::new().unwrap());
    ec.public_key()
        .affine_coordinates(&group, &mut x, &mut y, &mut ctx)
        .unwrap();

    // COSE_Key {1: 2 (EC2), 3: -7 (ES256), -1: 1 (P-256), -2: x, -3: y}.
    let mut cose = vec![0xA5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21];
    cbor_bytes(&mut cose, &x.to_vec_padded(32).unwrap());
    cose.push(0x22);
    cbor_bytes(&mut cose, &y.to_vec_padded(32).unwrap());

    let mut auth_data = openssl::sha::sha256(RP_ID.as_bytes()).to_vec();
    auth_data.push(0x45); // UP | UV | AT
    auth_data.extend_from_slice(&0u32.to_be_bytes());
    auth_data.extend_from_slice(&[0u8; 16]); // aaguid
    auth_data.extend_from_slice(&(CRED_ID.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(CRED_ID);
    auth_data.extend_from_slice(&cose);

    // {"fmt": "none", "attStmt": {}, "authData": <auth_data>}
    let mut att = vec![0xA3];
    cbor_text(&mut att, "fmt");
    cbor_text(&mut att, "none");
    cbor_text(&mut att, "attStmt");
    att.push(0xA0);
    cbor_text(&mut att, "authData");
    cbor_bytes(&mut att, &auth_data);

    let client_data = json!({
        "type": "webauthn.create",
        "challenge": challenge_b64,
        "origin": ORIGIN,
        "crossOrigin": false,
    })
    .to_string();
    json!({
        "id": URL_SAFE_NO_PAD.encode(CRED_ID),
        "rawId": URL_SAFE_NO_PAD.encode(CRED_ID),
        "type": "public-key",
        "response": {
            "attestationObject": URL_SAFE_NO_PAD.encode(&att),
            "clientDataJSON": URL_SAFE_NO_PAD.encode(client_data.as_bytes()),
        },
        "extensions": {},
    })
}

#[tokio::test]
async fn an_enrolment_stores_a_body_that_names_no_tier() {
    let saw = Arc::new(PeopleSaw::default());
    let r = router(people(saw.clone()).await);

    let (status, begun) = post_json(&r, "/api/auth/passkey/register/begin", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{begun}");
    let challenge_id = begun["challenge_id"].as_str().unwrap().to_string();
    let challenge = begun["options"]["publicKey"]["challenge"]
        .as_str()
        .unwrap_or_else(|| panic!("the begin names a challenge: {begun}"))
        .to_string();

    let (status, finished) = post_json(
        &r,
        "/api/auth/passkey/register/finish",
        json!({
            "challenge_id": challenge_id,
            "label": "macbook",
            "credential": attestation(&challenge),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the enrolment stores: {finished}"
    );

    let stored = saw.stored.lock().unwrap().clone();
    assert_eq!(stored.len(), 1, "one store call: {stored:?}");
    let mut keys: Vec<&str> = stored[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["credential_id", "label", "public_key"],
        "people refuses a credential body naming a tier (backlog 1d9970d1), \
         so the enrolment sends none: {}",
        stored[0]
    );
    assert_eq!(stored[0]["credential_id"], URL_SAFE_NO_PAD.encode(CRED_ID));
    assert_eq!(stored[0]["label"], "macbook");
}
