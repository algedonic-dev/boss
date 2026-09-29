//! ONLY AN OPERATOR-TIER PASSKEY ELEVATES (backlog 3c92c5b8, the
//! adversarial review of car 0bde9b99, finding H1).
//!
//! Enrolment is self-service: any session with an employee id can
//! register a passkey through `register_begin`/`register_finish`, with
//! no step-up. So a passkey the owner's user-tier cookie could enrol for
//! itself — a software P-256 key with attestation `none` — must never be
//! what raises that cookie to operator. The people table already carries
//! the distinction (`webauthn_credentials.access_tier`, `user` by
//! default); the elevation offers and verifies ONLY the owner's
//! `operator` rows, and a key reaches that tier by a separate recorded
//! act, never by enrolling it.
//!
//! These drive the finish with REAL assertions from a software
//! authenticator ([`SoftKey`]): a P-256 key the test holds, a stored
//! passkey written through webauthn-rs's own serde, and a signature over
//! `authenticatorData || sha256(clientDataJSON)` — so the refusals below
//! are reached PAST the signature check, not before it (the review's L3:
//! the earlier "unverified assertion" test stubbed no credentials and
//! never reached the signature at all). One positive run proves the
//! fixture is strong enough to pass; the wrong-key run proves the
//! verifier is really checking it.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, Uri, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::event::Event;
use boss_core::port::EventRecorder;
use boss_gateway::audit::AuthAudit;
use boss_gateway::elevation::{
    BEGIN_PATH, ELEVATE_FLOW, ElevationState, FINISH_PATH, RosterPlatformOwner, elevation_router,
};
use boss_gateway::passkey::PasskeyState;
use boss_gateway::session::{COOKIE_NAME, Session};
use openssl::bn::{BigNum, BigNumContext};
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::sign::Signer;
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};
use webauthn_rs_core::proto::{
    AttestationFormat, AttestationMetadata, COSEAlgorithm, COSEEC2Key, COSEKey, COSEKeyType,
    Credential, ECDSACurve, ParsedAttestation, ParsedAttestationData, RegisteredExtensions,
    UserVerificationPolicy,
};

const KEY: &[u8] = b"operator-tier-passkey-test-key!!";
const RP_ID: &str = "boss.test";
const ORIGIN: &str = "https://boss.test";
const OWNER: &str = "emp-first-hire";
const CHALLENGE: [u8; 32] = [9u8; 32];

/// A software authenticator: one P-256 key and the credential id it
/// answers to.
struct SoftKey {
    key: PKey<Private>,
    cred_id: Vec<u8>,
}

impl SoftKey {
    fn new(id: &[u8]) -> Self {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let ec = EcKey::generate(&group).unwrap();
        Self {
            key: PKey::from_ec_key(ec).unwrap(),
            cred_id: id.to_vec(),
        }
    }

    fn credential_id(&self) -> String {
        URL_SAFE_NO_PAD.encode(&self.cred_id)
    }

    /// The row's `public_key`: b64url(serde_json(Passkey)), the shape
    /// `register_finish` stores — built through webauthn-rs's own types.
    fn stored_public_key(&self) -> String {
        let ec = self.key.ec_key().unwrap();
        let mut ctx = BigNumContext::new().unwrap();
        let (mut x, mut y) = (BigNum::new().unwrap(), BigNum::new().unwrap());
        ec.public_key()
            .affine_coordinates(ec.group(), &mut x, &mut y, &mut ctx)
            .unwrap();
        let cred = Credential {
            cred_id: self.cred_id.clone().into(),
            cred: COSEKey {
                type_: COSEAlgorithm::ES256,
                key: COSEKeyType::EC_EC2(COSEEC2Key {
                    curve: ECDSACurve::SECP256R1,
                    x: x.to_vec_padded(32).unwrap().into(),
                    y: y.to_vec_padded(32).unwrap().into(),
                }),
            },
            counter: 0,
            transports: None,
            user_verified: true,
            backup_eligible: false,
            backup_state: false,
            registration_policy: UserVerificationPolicy::Required,
            extensions: RegisteredExtensions::none(),
            attestation: ParsedAttestation {
                data: ParsedAttestationData::None,
                metadata: AttestationMetadata::None,
            },
            attestation_format: AttestationFormat::None,
        };
        // A stored Passkey serialises as `{"cred": <Credential>}` — the
        // shape `passkey_jsons` reads back. Written through serde rather
        // than `Passkey::from`, which webauthn-rs keeps behind its
        // `danger-credential-internals` feature.
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({ "cred": cred })).unwrap())
    }

    /// A people row for this key at `tier`.
    fn row(&self, tier: &str, label: &str) -> Value {
        json!({
            "credential_id": self.credential_id(),
            "public_key": self.stored_public_key(),
            "sign_count": 0,
            "label": label,
            "access_tier": tier,
            "registered_at": "2026-09-01T12:00:00Z",
            "last_used_at": null,
        })
    }

    /// A user-verified assertion over `challenge` for this origin,
    /// signed by `signer` (this key, or another for the wrong-key run)
    /// under THIS key's credential id.
    fn assertion_signed_by(&self, signer: &SoftKey, challenge: &[u8]) -> Value {
        let client_data = json!({
            "type": "webauthn.get",
            "challenge": URL_SAFE_NO_PAD.encode(challenge),
            "origin": ORIGIN,
            "crossOrigin": false,
        })
        .to_string();
        let mut auth_data = openssl::sha::sha256(RP_ID.as_bytes()).to_vec();
        auth_data.push(0x05); // UP | UV
        auth_data.extend_from_slice(&1u32.to_be_bytes());
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&openssl::sha::sha256(client_data.as_bytes()));
        let mut signer_ctx = Signer::new(MessageDigest::sha256(), &signer.key).unwrap();
        let signature = signer_ctx.sign_oneshot_to_vec(&signed).unwrap();
        json!({
            "id": self.credential_id(),
            "rawId": self.credential_id(),
            "type": "public-key",
            "response": {
                "authenticatorData": URL_SAFE_NO_PAD.encode(&auth_data),
                "clientDataJSON": URL_SAFE_NO_PAD.encode(client_data.as_bytes()),
                "signature": URL_SAFE_NO_PAD.encode(&signature),
                "userHandle": null,
            },
            "extensions": {},
        })
    }

    fn assertion(&self, challenge: &[u8]) -> Value {
        self.assertion_signed_by(self, challenge)
    }
}

/// The event recorder, in memory — the port's test adapter.
#[derive(Default)]
struct Captured(Mutex<Vec<Event>>);

#[async_trait::async_trait]
impl EventRecorder for Captured {
    async fn record(&self, event: &Event) -> Result<(), String> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// The people service: the owner first in the roster, `creds` as his
/// stored passkeys, and a consumed `authenticate` challenge minted for
/// him over [`CHALLENGE`].
async fn people(creds: Value) -> String {
    let consumed = json!({
        "id": "c1", "employee_id": OWNER, "flow": ELEVATE_FLOW,
        "challenge": URL_SAFE_NO_PAD.encode(CHALLENGE),
    });
    let app = Router::new().fallback(move |_m: Method, uri: Uri| {
        let creds = creds.clone();
        let consumed = consumed.clone();
        async move {
            let path = uri.path();
            let reply = if path == "/api/people" {
                json!([{"id": OWNER, "hire_date": "2026-01-05"}])
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
    format!("http://{addr}")
}

fn router(base: String, events: Arc<Captured>) -> Router {
    let origin = Url::parse(ORIGIN).unwrap();
    let audit = AuthAudit::spawn(events);
    let passkey = Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: boss_gateway::machine_client::MachineClient::build(reqwest::Client::builder())
            .unwrap(),
        people_base: base.clone(),
        jobs_base: base.clone(),
        webauthn: WebauthnBuilder::new(RP_ID, &origin)
            .unwrap()
            .build()
            .unwrap(),
        audit,
    });
    let owner = RosterPlatformOwner::new(
        boss_gateway::machine_client::MachineClient::build(reqwest::Client::builder()).unwrap(),
        base,
    );
    elevation_router(Arc::new(ElevationState {
        passkey,
        owner: Arc::new(owner),
    }))
}

fn owners_session() -> Session {
    let mut s = Session::new("owner@example.com", 600);
    s.employee_id = Some(OWNER.to_string());
    s.role = Some("platform-admin".to_string());
    s
}

async fn post(router: &Router, path: &str, body: Value) -> (StatusCode, Option<String>, String) {
    let req = Request::post(path)
        .header("content-type", "application/json")
        .header(
            "cookie",
            format!("{COOKIE_NAME}={}", owners_session().encode(KEY)),
        )
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

async fn finish(router: &Router, assertion: Value) -> (StatusCode, Option<String>, String) {
    post(
        router,
        FINISH_PATH,
        json!({ "challenge_id": "c1", "credential": assertion }),
    )
    .await
}

fn elevated_events(events: &Captured) -> Vec<Event> {
    events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.kind == "auth.session.elevated")
        .cloned()
        .collect()
}

/// The positive run: the owner's operator-tier key, a real signature,
/// and the session comes back operator — on the record, naming the key.
#[tokio::test]
async fn the_owners_operator_tier_key_elevates_his_session() {
    let operator_key = SoftKey::new(b"operator-key");
    let events = Arc::new(Captured::default());
    let r = router(
        people(json!([operator_key.row("operator", "yubikey")])).await,
        events.clone(),
    );
    let (status, cookie, body) = finish(&r, operator_key.assertion(&CHALLENGE)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cookie = cookie.expect("the elevated cookie is set");
    let value = cookie
        .split(';')
        .next()
        .and_then(|kv| kv.split_once('='))
        .map(|(_, v)| v)
        .unwrap();
    let back = Session::decode(value, KEY).unwrap();
    assert_eq!(back.access_tier(), "operator");
    assert!(back.elevated_at().is_some());
    let recorded = elevated_events(&events);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].payload["credential_label"], "yubikey");
    assert_eq!(
        recorded[0].payload["credential_registered_at"],
        "2026-09-01T12:00:00Z"
    );
}

/// H1: a key the owner could enrol for himself from a user-tier cookie
/// — a valid assertion, signature and all — never elevates, even with
/// an operator-tier key enrolled beside it.
#[tokio::test]
async fn a_valid_assertion_from_a_user_tier_passkey_is_refused() {
    let self_enrolled = SoftKey::new(b"self-enrolled-key");
    let operator_key = SoftKey::new(b"operator-key");
    let events = Arc::new(Captured::default());
    let r = router(
        people(json!([
            self_enrolled.row("user", "software key"),
            operator_key.row("operator", "yubikey"),
        ]))
        .await,
        events.clone(),
    );
    let (status, cookie, body) = finish(&r, self_enrolled.assertion(&CHALLENGE)).await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "a user-tier passkey elevated the session: {status} {body}"
    );
    assert!(cookie.is_none(), "a user-tier passkey set a cookie");
    assert!(elevated_events(&events).is_empty());
}

/// The verifier is really checking the signature: the operator key's
/// credential id, signed by a different private key, is refused.
#[tokio::test]
async fn an_assertion_signed_by_the_wrong_key_is_refused() {
    let operator_key = SoftKey::new(b"operator-key");
    let impostor = SoftKey::new(b"operator-key");
    let events = Arc::new(Captured::default());
    let r = router(
        people(json!([operator_key.row("operator", "yubikey")])).await,
        events.clone(),
    );
    let (status, cookie, body) =
        finish(&r, operator_key.assertion_signed_by(&impostor, &CHALLENGE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(cookie.is_none());
    assert!(elevated_events(&events).is_empty());
}

/// An owner with only user-tier keys is told so at begin, before any
/// challenge is minted: 409, naming why.
#[tokio::test]
async fn an_owner_with_no_operator_tier_key_cannot_begin() {
    let self_enrolled = SoftKey::new(b"self-enrolled-key");
    let r = router(
        people(json!([self_enrolled.row("user", "software key")])).await,
        Arc::new(Captured::default()),
    );
    let (status, _, body) = post(&r, BEGIN_PATH, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("operator-tier"), "{body}");
}
