//! A PASSKEY IS PROMOTED TO THE OPERATOR TIER ONLY WITH A BREAK-GLASS
//! VOUCH (design 2cb6256f, David 2026-09-28; backlog 2a228d0c, car 2).
//!
//! The /me ceremony, driven through the gateway's own router against a
//! stub of people and the jobs API, with REAL assertions from software
//! authenticators ([`SoftKey`], the elevation tests' fixture): a P-256
//! key, a stored credential written through webauthn-rs's own serde, and
//! a signature over `authenticatorData || sha256(clientDataJSON)`. So
//! every refusal below is reached past the signature checks, and the one
//! positive run proves the fixture strong enough to pass.
//!
//! The finding this car exists for is F3 of the adversarial review of
//! car 1d9970d1: a copied user-tier cookie can ENROL a software key and
//! sign the packet's `authorise` step with it, so an approval — even a
//! live presence stamp by the owner's id — is not enough. Here that
//! approval stands, the key being promoted asserts, and the only thing
//! missing is a break-glass hardware key: people is never asked, the
//! packet is never closed, no ticket exists.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Method, Request, StatusCode, Uri};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::passkey_promotion::{AUTHORISE_STEP, PROMOTE_STEP, PROMOTION_KIND, PromotionTicket};
use boss_core::presence::{now_epoch, public_key_fingerprint};
use boss_gateway::audit::AuthAudit;
use boss_gateway::break_glass::{BootConfig, BreakGlassState};
use boss_gateway::break_glass_alarm::AlarmFiling;
use boss_gateway::elevation::RosterPlatformOwner;
use boss_gateway::passkey::PasskeyState;
use boss_gateway::promotion::{
    BEGIN_PATH, FINISH_PATH, PromotionState, REQUEST_PATH, ceremony_router,
};
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

const KEY: &[u8] = b"passkey-promotion-test-session-k";
const RP_ID: &str = "boss.test";
const ORIGIN: &str = "https://boss.test";
const OWNER: &str = "emp-first-hire";
const PACKET: &str = "7a1d2c3b-0000-4000-8000-00000000c0de";
const PROMOTE_STEP_ID: &str = "7a1d2c3b-0000-4000-8000-0000000000a2";
const FILED: &str = "7a1d2c3b-0000-4000-8000-0000000f11ed";

/// A software authenticator: one P-256 key and the credential id it
/// answers to.
struct SoftKey {
    key: PKey<Private>,
    cred_id: Vec<u8>,
}

impl SoftKey {
    fn new(id: &[u8]) -> Self {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        Self {
            key: PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap(),
            cred_id: id.to_vec(),
        }
    }

    fn credential_id(&self) -> String {
        URL_SAFE_NO_PAD.encode(&self.cred_id)
    }

    /// b64url(serde_json({"cred": Credential})) — the stored shape of a
    /// passkey row AND of a break-glass record's `public_key`.
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

    /// A committed break-glass record for this key.
    fn break_glass_record(&self, label: &str) -> Value {
        json!({
            "credential_id": self.credential_id(),
            "public_key": self.stored_public_key(),
            "sign_count": 0,
            "aaguid": "00000000-0000-0000-0000-000000000000",
            "enrolled_at": "2026-09-28T10:00:00Z",
            "label": label,
            "rp_id": RP_ID,
        })
    }

    /// An assertion over `challenge_b64`, signed by `signer` under THIS
    /// key's credential id.
    fn assertion_signed_by(&self, signer: &SoftKey, challenge_b64: &str) -> Value {
        let client_data = json!({
            "type": "webauthn.get",
            "challenge": challenge_b64,
            "origin": ORIGIN,
            "crossOrigin": false,
        })
        .to_string();
        let mut auth_data = openssl::sha::sha256(RP_ID.as_bytes()).to_vec();
        auth_data.push(0x05); // UP | UV
        auth_data.extend_from_slice(&1u32.to_be_bytes());
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&openssl::sha::sha256(client_data.as_bytes()));
        let signature = Signer::new(MessageDigest::sha256(), &signer.key)
            .unwrap()
            .sign_oneshot_to_vec(&signed)
            .unwrap();
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

    fn assertion(&self, challenge_b64: &str) -> Value {
        self.assertion_signed_by(self, challenge_b64)
    }
}

/// A `passkey-promotion` packet for `key`, approved by the owner with a
/// live presence stamp — the state the finish needs — unless `approver`
/// says someone else signed it.
fn packet(key: &SoftKey, approver: &str) -> Value {
    let row = key.row("user", "yubikey-5c");
    let fingerprint = public_key_fingerprint(
        &URL_SAFE_NO_PAD
            .decode(row["public_key"].as_str().unwrap())
            .unwrap(),
    );
    let mut p = json!({
        "id": PACKET,
        "kind": PROMOTION_KIND,
        "status": "open",
        "steps": [
            {
                "id": "7a1d2c3b-0000-4000-8000-0000000000a1",
                "job_id": PACKET,
                "spec_slug": AUTHORISE_STEP,
                "kind": "sign-off",
                "title": "Authorise making yubikey-5c an operator key",
                "status": "completed",
                "metadata": {
                    "employee_id": OWNER,
                    "credential_id": key.credential_id(),
                    "label": "yubikey-5c",
                    "registered_at": "2026-09-01T12:00:00Z",
                    "public_key_sha256": fingerprint,
                    "decision": "approved",
                },
                "sign_offs": [],
            },
            {
                "id": PROMOTE_STEP_ID,
                "job_id": PACKET,
                "spec_slug": PROMOTE_STEP,
                "kind": "task",
                "title": "Finish on /me",
                "status": "ready",
                "metadata": {"procedure": "Completed by the gateway."},
            },
        ],
    });
    let step = &mut p["steps"][0];
    let hash = boss_core::job::step_shape_hash(step["title"].as_str().unwrap(), &step["metadata"]);
    step["sign_offs"] = json!([{
        "authority_id": approver,
        "role": "platform-admin",
        "stamped_at": "2026-09-28T10:00:01Z",
        "shape_hash": hash,
        "assurance": "presence",
        "presence_nonce": "n0nce",
    }]);
    p
}

/// Every request the stub was asked: method, path, body.
type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

/// People and the jobs API in one stub: the owner first in the roster,
/// `creds` his stored keys, `open` the open packets for any key, and
/// `packet` the one packet there is.
async fn services(creds: Value, open: Vec<Value>, packet: Value, seen: Seen) -> String {
    let app = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let (creds, open, packet, seen) =
            (creds.clone(), open.clone(), packet.clone(), seen.clone());
        async move {
            let path = uri.path().to_string();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            seen.lock()
                .unwrap()
                .push((method.to_string(), path.clone(), body));
            let reply = match (method.as_str(), path.as_str()) {
                ("GET", "/api/people") => json!([{"id": OWNER, "hire_date": "2026-01-05"}]),
                ("GET", p) if p.ends_with("/webauthn-credentials") => creds,
                ("POST", p) if p.ends_with("/promote") => {
                    json!({"access_tier": "operator", "promoted": true})
                }
                ("GET", "/api/jobs") => json!({"data": open, "total": open.len()}),
                ("POST", "/api/jobs") => json!({"id": FILED}),
                ("GET", p) if p == format!("/api/jobs/{PACKET}") => packet,
                _ => json!({}),
            };
            axum::Json(reply)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// The boot alarm has nothing to say here: the store is bound.
struct NoAlarms;

#[async_trait::async_trait]
impl AlarmFiling for NoAlarms {
    async fn open_finding_keys(&self) -> Result<std::collections::BTreeSet<String>, String> {
        Ok(Default::default())
    }
    async fn file(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
}

/// The gateway's promotion routes over the stub at `base`, with `glass`
/// as the one committed break-glass record (label `primary`).
fn router(base: &str, glass: &SoftKey, store: &Path) -> Router {
    std::fs::write(
        store.join("primary.json"),
        glass.break_glass_record("primary").to_string(),
    )
    .unwrap();
    let origin = Url::parse(ORIGIN).unwrap();
    let http =
        || boss_gateway::machine_client::MachineClient::build(reqwest::Client::builder()).unwrap();
    let passkey = Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: http(),
        people_base: base.to_string(),
        jobs_base: base.to_string(),
        webauthn: WebauthnBuilder::new(RP_ID, &origin)
            .unwrap()
            .build()
            .unwrap(),
        audit: AuthAudit::disabled(),
    });
    let (break_glass, _alarm) = BreakGlassState::boot(
        BootConfig {
            public_url: ORIGIN.into(),
            dir: store.to_path_buf(),
            authorisers: vec![],
            jobs_base: base.to_string(),
        },
        KEY.to_vec(),
        AuthAudit::disabled(),
        Arc::new(NoAlarms),
    )
    .unwrap();
    let owner = RosterPlatformOwner::new(http(), base.to_string());
    ceremony_router(Arc::new(PromotionState::new(
        passkey,
        Arc::new(break_glass),
        Arc::new(owner),
    )))
}

fn session_of(employee: &str) -> Session {
    let mut s = Session::new("owner@example.com", 600);
    s.employee_id = Some(employee.to_string());
    s.role = Some("platform-admin".to_string());
    s
}

async fn post_as(router: &Router, who: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::post(path)
        .header("content-type", "application/json")
        .header(
            "cookie",
            format!("{COOKIE_NAME}={}", session_of(who).encode(KEY)),
        )
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

async fn post(router: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    post_as(router, OWNER, path, body).await
}

/// Begin, and the two challenges it asks to be signed.
async fn begin(router: &Router) -> (String, String, String) {
    let (status, b) = post(router, BEGIN_PATH, json!({ "packet_id": PACKET })).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    (
        b["ceremony_id"].as_str().unwrap().to_string(),
        b["key"]["publicKey"]["challenge"]
            .as_str()
            .unwrap()
            .to_string(),
        b["vouch"]["publicKey"]["challenge"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

fn writes(seen: &Seen) -> Vec<(String, String, Value)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(m, p, _)| {
            (m == "POST" && p.ends_with("/promote")) || (m == "PUT" && p.contains("/steps/"))
        })
        .cloned()
        .collect()
}

struct Fixture {
    router: Router,
    seen: Seen,
    promoted: SoftKey,
    glass: SoftKey,
}

async fn fixture(approver: &str, extra_keys: &[&SoftKey]) -> Fixture {
    let promoted = SoftKey::new(b"the-key-being-promoted");
    let glass = SoftKey::new(b"break-glass-primary");
    let mut creds = vec![promoted.row("user", "yubikey-5c")];
    creds.extend(extra_keys.iter().map(|k| k.row("user", "another key")));
    let seen: Seen = Arc::default();
    let base = services(
        Value::Array(creds),
        vec![],
        packet(&promoted, approver),
        seen.clone(),
    )
    .await;
    let store = boss_testing::scratch_dir("passkey-promotion-break-glass");
    let router = router(&base, &glass, &store);
    Fixture {
        router,
        seen,
        promoted,
        glass,
    }
}

/// THE ROAD. The owner's approved packet, the key being promoted
/// asserts, a committed break-glass key vouches: people is asked ONCE,
/// with a ticket signed by the session key naming the packet, the owner,
/// the key and the break-glass label; THEN the packet's `promote` step is
/// completed with what was spent, its own keys kept.
#[tokio::test]
async fn the_owner_promotes_his_key_with_a_break_glass_vouch() {
    let f = fixture(OWNER, &[]).await;
    let (ceremony, key_challenge, vouch_challenge) = begin(&f.router).await;
    let (status, out) = post(
        &f.router,
        FINISH_PATH,
        json!({
            "ceremony_id": ceremony,
            "key": f.promoted.assertion(&key_challenge),
            "vouch": f.glass.assertion(&vouch_challenge),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["access_tier"], "operator");
    assert_eq!(out["vouched_by"], "primary");

    let w = writes(&f.seen);
    assert_eq!(w.len(), 2, "one promote, then one step write: {w:?}");
    let (_, promote_path, promote_body) = &w[0];
    assert_eq!(
        promote_path,
        &format!(
            "/api/people/{OWNER}/webauthn-credentials/{}/promote",
            f.promoted.credential_id()
        )
    );
    assert!(promote_body.get("vouched_by").is_none(), "{promote_body}");
    let ticket =
        PromotionTicket::decode(promote_body["ticket"].as_str().unwrap(), KEY, now_epoch())
            .expect("the ticket is signed with the session key");
    assert_eq!(ticket.p, PACKET);
    assert_eq!(ticket.i, OWNER);
    assert_eq!(ticket.c, f.promoted.credential_id());
    assert_eq!(ticket.v, "primary");
    let (_, step_path, step_body) = &w[1];
    assert_eq!(
        step_path,
        &format!("/api/jobs/{PACKET}/steps/{PROMOTE_STEP_ID}")
    );
    assert_eq!(step_body["status"], "completed");
    let m = &step_body["metadata"];
    assert_eq!(m["credential_id"], f.promoted.credential_id());
    assert_eq!(m["vouched_by"], "primary");
    assert!(m["promoted_at"].is_string());
    assert_eq!(
        m["procedure"], "Completed by the gateway.",
        "the step keeps its keys"
    );
}

/// F3 (review of car 1d9970d1). The approval is live and the key being
/// promoted asserts — but what "vouches" is a freshly enrolled software
/// key, or the promoted key itself, never a committed break-glass record.
/// Nothing is promoted: no ticket, no call to people, no step closed.
#[tokio::test]
async fn an_approval_signed_by_a_fresh_key_with_no_break_glass_assertion_promotes_nothing() {
    let fresh = SoftKey::new(b"freshly-enrolled-software-key");
    let f = fixture(OWNER, &[&fresh]).await;
    for (why, voucher) in [
        ("a freshly enrolled key", &fresh),
        ("the promoted key itself", &f.promoted),
    ] {
        let (ceremony, key_challenge, vouch_challenge) = begin(&f.router).await;
        let (status, out) = post(
            &f.router,
            FINISH_PATH,
            json!({
                "ceremony_id": ceremony,
                "key": f.promoted.assertion(&key_challenge),
                "vouch": voucher.assertion(&vouch_challenge),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}: {out}");
    }
    // Nor a break-glass record's id signed by any other private key.
    let (ceremony, key_challenge, vouch_challenge) = begin(&f.router).await;
    let (status, _) = post(
        &f.router,
        FINISH_PATH,
        json!({
            "ceremony_id": ceremony,
            "key": f.promoted.assertion(&key_challenge),
            "vouch": f.glass.assertion_signed_by(&fresh, &vouch_challenge),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(writes(&f.seen).is_empty(), "{:?}", writes(&f.seen));
}

/// D4. Approval shows consent, possession shows it is his key: another
/// of the owner's keys answering for the promoted one promotes nothing.
#[tokio::test]
async fn another_of_the_owners_keys_cannot_answer_for_the_promoted_one() {
    let other = SoftKey::new(b"another-of-his-keys");
    let f = fixture(OWNER, &[&other]).await;
    let (ceremony, key_challenge, vouch_challenge) = begin(&f.router).await;
    let (status, out) = post(
        &f.router,
        FINISH_PATH,
        json!({
            "ceremony_id": ceremony,
            "key": other.assertion(&key_challenge),
            "vouch": f.glass.assertion(&vouch_challenge),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    assert!(writes(&f.seen).is_empty());
}

/// D3. A packet signed by anyone but the key's owner is refused at
/// begin, before any challenge; and nobody but the roster's owner may
/// begin, request or finish.
#[tokio::test]
async fn only_the_owners_own_approval_and_session_reach_a_challenge() {
    let f = fixture("emp-someone-else", &[]).await;
    let (status, out) = post(&f.router, BEGIN_PATH, json!({ "packet_id": PACKET })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    assert!(
        out.to_string()
            .contains("presence-signed by emp-someone-else"),
        "{out}"
    );

    let f = fixture(OWNER, &[]).await;
    for path in [BEGIN_PATH, REQUEST_PATH] {
        let (status, out) = post_as(
            &f.router,
            "emp-not-the-owner",
            path,
            json!({ "packet_id": PACKET, "credential_id": f.promoted.credential_id() }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {out}");
    }
    assert!(writes(&f.seen).is_empty());
}

/// A begun ceremony is spent by its finish, whatever the finish answers.
#[tokio::test]
async fn a_ceremony_is_finished_once() {
    let f = fixture(OWNER, &[]).await;
    let (ceremony, key_challenge, _) = begin(&f.router).await;
    let body = json!({
        "ceremony_id": ceremony,
        "key": f.promoted.assertion(&key_challenge),
        "vouch": f.promoted.assertion(&key_challenge),
    });
    let (first, _) = post(&f.router, FINISH_PATH, body.clone()).await;
    assert_eq!(first, StatusCode::UNAUTHORIZED);
    let (second, _) = post(&f.router, FINISH_PATH, body).await;
    assert_eq!(second, StatusCode::GONE);
}

/// "Make this my operator key" files ONE packet for a user-tier key,
/// naming it as its row does, and grants nothing; an operator key with
/// nothing left to finish is refused, naming it.
#[tokio::test]
async fn the_request_files_a_packet_and_grants_nothing() {
    let f = fixture(OWNER, &[]).await;
    let (status, out) = post(
        &f.router,
        REQUEST_PATH,
        json!({ "credential_id": f.promoted.credential_id() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["packet_id"], FILED);
    assert_eq!(out["approved"], false);
    let filed: Vec<Value> = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, p, _)| m == "POST" && p == "/api/jobs")
        .map(|(_, _, b)| b.clone())
        .collect();
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0]["kind"], PROMOTION_KIND);
    assert_eq!(
        filed[0]["metadata"]["credential_id"],
        f.promoted.credential_id()
    );
    assert_eq!(filed[0]["metadata"]["label"], "yubikey-5c");
    assert!(writes(&f.seen).is_empty(), "filing promotes nothing");

    let operator = SoftKey::new(b"already-operator");
    let seen: Seen = Arc::default();
    let base = services(
        json!([operator.row("operator", "yubikey-old")]),
        vec![],
        packet(&operator, OWNER),
        seen.clone(),
    )
    .await;
    let r = router(
        &base,
        &f.glass,
        &boss_testing::scratch_dir("passkey-promotion-operator"),
    );
    let (status, out) = post(
        &r,
        REQUEST_PATH,
        json!({ "credential_id": operator.credential_id() }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert!(
        out.to_string()
            .contains("yubikey-old is already an operator key"),
        "{out}"
    );
}
