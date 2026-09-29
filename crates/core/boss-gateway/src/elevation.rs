//! THE PLATFORM OWNER'S OPERATOR-TIER PASSKEY ELEVATES HIS SESSION TO
//! THE OPERATOR TIER — and nothing else elevates anything (backlog
//! 3c92c5b8).
//!
//! WHY. Every gateway session was minted `access_tier = "user"` and
//! nothing raised it, while `Session::access_tier`'s own doc promised
//! "Elevated to operator by FIDO key authentication". The jobs API's
//! operator doors read `boss_jobs::trust::can_read` (operator or
//! auditor), so `/api/credentials`, `/api/agents` and `/api/agent-runs`
//! answered 403 to the platform owner's own browser — measured
//! 2026-09-27 ~23:30Z on `/it/registry/agents`. David's decision,
//! verbatim on the packet: "the passkey-authenticated platform owner
//! session (after a WebAuthn assertion — not OIDC alone) becomes
//! operator tier at the gateway; bound to that actor; OIDC-only logins
//! stay user tier; no tier rides on break-glass."
//!
//! THE CEREMONY. `POST /api/auth/passkey/elevate/begin` mints a random
//! 32-byte challenge on the people challenge ledger as flow
//! `authenticate` (a value the ledger and its CHECK constraint already
//! admit, unused until now), bound to the session's employee, and offers
//! only his OPERATOR-TIER keys; the browser's passkey signs it with user
//! verification required; `.../elevate/finish` consumes the row
//! single-use, verifies the assertion with the SAME verifier the presence
//! ceremony uses ([`crate::passkey::verify_assertion`]) narrowed to those
//! keys, and only then asks [`elevate`], whose call to
//! `Session::into_operator` is the one write of the operator tier in this
//! crate — the pin at the foot of this file holds it to that.
//!
//! WHICH KEY. Enrolment is self-service (`register_finish`, behind any
//! session with an employee id) and stores a `user`-tier key, which signs
//! presence stamps and NEVER elevates: otherwise the owner's user-tier
//! cookie alone could enrol a software key and use it to reach operator
//! (the adversarial review of car 0bde9b99, H1). People's
//! `webauthn_credentials.access_tier` already carries the distinction;
//! a key reaches `operator` by a separate recorded act, not by anything
//! this module or the enrolment does.
//!
//! BOUND TO THE ACTOR. The owner is READ from the people roster only: the
//! first active hire holding `platform-admin`, by
//! `boss_core::platform_owner`'s rule. `BOSS_PLATFORM_OWNER` is NOT
//! honoured here — an environment variable must not decide trust (review
//! L1), whatever it decides for who a packet is filed to. The session
//! must also itself carry `platform-admin`: a session the login
//! downgraded is not raised back past that decision. The owner is asked
//! again at the grant, after the assertion, so what decides is the roster
//! at the moment trust changes, not at begin.
//!
//! WHAT IT DOES NOT DO. An OIDC or password login mints `user` and stays
//! there until this ceremony. A break-glass session carries no employee
//! and cannot begin it. The elevation is not a new session: the cookie's
//! `expiry` is kept, and THAT is the bound — the session cookie is
//! decoded statelessly, so an elevated cookie verifies until its own
//! expiry (24 h at most, `session::DEFAULT_TTL_SECONDS`), wherever a
//! copy of it is. A logout or a fresh login gives THIS browser a new
//! user-tier cookie; it does not revoke a copy of the old one. Whether to
//! shorten that bound or revoke is David's TTL decision, filed apart
//! (review M1); nothing here pretends to more.
//!
//! ON THE RECORD. A grant RECORDS `auth.session.elevated` — who, when the
//! assertion was verified, when the elevation ends, and which key (its
//! label and registration time) — before the cookie is set, and refuses
//! the grant if the event cannot be recorded (review L2: no silent
//! grant). A refusal is the structured warn line every passkey refusal
//! writes ([`crate::passkey::presence_refused`]); it changes no trust.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::platform_owner::{self, Holder, PlatformOwner, PlatformOwnerError};
use boss_core::presence::now_epoch;
use boss_core::roles::PLATFORM_ADMIN_ROLE;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::{PublicKeyCredential, Uuid};

use crate::audit::Elevation;
use crate::passkey::{
    ErrResp, PasskeyState, consume_challenge, employee_session, err, presence_refused,
    sign_as_gateway, verify_assertion,
};
use crate::session::{self, Session};

pub use crate::session::OPERATOR_TIER;

/// The stored `access_tier` a passkey must carry to elevate a session.
/// People's `webauthn_credentials.access_tier` is `user` (every
/// self-service enrolment) or `operator`.
pub const ELEVATING_KEY_TIER: &str = "operator";

/// The challenge-ledger flow an elevation mints and consumes. Admitted
/// by the ledger's CHECK since 10-people.sql; never used before.
pub const ELEVATE_FLOW: &str = "authenticate";

pub const BEGIN_PATH: &str = "/api/auth/passkey/elevate/begin";
pub const FINISH_PATH: &str = "/api/auth/passkey/elevate/finish";

/// What an owner with only user-tier keys reads at begin.
const NO_OPERATOR_KEY: &str = "no operator-tier passkey enrolled — a passkey enrolled here is \
     user tier and never elevates a session; promoting a key to operator is a separate recorded act";

/// Why a session is not elevated. Each names what would answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A guest, a break-glass session, an unresolved login: no employee
    /// to be the owner.
    NoEmployee,
    /// The session does not itself carry `platform-admin`.
    NotPlatformAdmin,
    /// The session's employee is not the platform owner.
    NotTheOwner,
}

impl Refusal {
    /// The text both the browser and the log read. Fixed, per the
    /// passkey module's rule for refusal texts.
    pub fn message(self) -> &'static str {
        match self {
            Self::NoEmployee => {
                "this session resolves to no employee — only the platform owner's session \
                 is elevated"
            }
            Self::NotPlatformAdmin => {
                "this session does not carry platform-admin — only the platform owner's \
                 session is elevated"
            }
            Self::NotTheOwner => "only the platform owner's session is elevated to operator",
        }
    }
}

/// THE DECISION, and the one caller of `Session::into_operator`.
/// `session` elevated for `owner` by an assertion verified at
/// `asserted_at`, or why not. Pure: the caller has verified the
/// assertion from an operator-tier key and read the owner from the
/// roster; this decides. The expiry is the session's own, never
/// extended.
pub fn elevate(session: &Session, owner: &str, asserted_at: u64) -> Result<Session, Refusal> {
    judge_owner(session, owner)?;
    Ok(session.clone().into_operator(asserted_at))
}

/// WHOSE SESSION THIS IS, judged: the platform owner's own
/// platform-admin session, or why not — in that order, the first that
/// fails. The elevation's rule, and the passkey promotion's
/// (`crate::promotion`, design 2cb6256f D3), so the two can never
/// disagree about who the owner is. Pure; `owner` is the roster's answer.
pub fn judge_owner(session: &Session, owner: &str) -> Result<(), Refusal> {
    let employee = session
        .employee_id
        .as_deref()
        .filter(|e| !e.trim().is_empty())
        .ok_or(Refusal::NoEmployee)?;
    if session.effective_role() != PLATFORM_ADMIN_ROLE {
        return Err(Refusal::NotPlatformAdmin);
    }
    if owner.trim().is_empty() || employee != owner {
        return Err(Refusal::NotTheOwner);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Who the owner is — the roster, read by the gateway itself
// ---------------------------------------------------------------------------

/// The platform owner over the people roster, for the gateway, and the
/// roster ONLY. The people client's adapter is Tier 2 and the gateway is
/// Tier 1, so the gateway reads the same path
/// ([`platform_owner::roster_path`]) and picks with the same rule
/// ([`platform_owner::first_hire`]); only the transport is its own. It
/// has no `BOSS_PLATFORM_OWNER` override on purpose: this answer decides
/// trust, and an environment variable must not (review L1). No cache: it
/// is asked twice per elevation.
pub struct RosterPlatformOwner {
    http: crate::machine_client::MachineClient,
    people_base: String,
}

impl RosterPlatformOwner {
    pub fn new(http: crate::machine_client::MachineClient, people_base: impl Into<String>) -> Self {
        Self {
            http,
            people_base: people_base.into(),
        }
    }
}

#[async_trait]
impl PlatformOwner for RosterPlatformOwner {
    async fn platform_owner(&self) -> Result<String, PlatformOwnerError> {
        let url = format!("{}{}", self.people_base, platform_owner::roster_path());
        // Signed as the gateway's own operator-tier automation, which
        // the roster admits as machinery (boss-people http.rs).
        let resp = sign_as_gateway(self.http.get(url))
            .send()
            .await
            // Fixed text: reqwest's error names the internal URL.
            .map_err(|_| PlatformOwnerError::Unreachable("people unreachable".into()))?;
        if !resp.status().is_success() {
            return Err(PlatformOwnerError::Unreachable(format!(
                "roster read: {}",
                resp.status()
            )));
        }
        let holders: Vec<Holder> = resp
            .json()
            .await
            .map_err(|_| PlatformOwnerError::Unreachable("roster malformed".into()))?;
        platform_owner::first_hire(&holders).ok_or(PlatformOwnerError::NoHolder {
            role: PLATFORM_ADMIN_ROLE,
        })
    }
}

// ---------------------------------------------------------------------------
// The ceremony
// ---------------------------------------------------------------------------

/// The ceremony's state: the passkey machinery (whose `audit` is where
/// the grant is recorded) and who the owner is.
pub struct ElevationState {
    pub passkey: Arc<PasskeyState>,
    pub owner: Arc<dyn PlatformOwner>,
}

/// The elevation's two routes, the one list `main.rs` mounts and the
/// tests drive (the lesson of backlog 3bddce66).
pub fn elevation_router<S>(state: Arc<ElevationState>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(BEGIN_PATH, post(elevate_begin).with_state(state.clone()))
        .route(FINISH_PATH, post(elevate_finish).with_state(state))
}

/// The owner, or a fixed-text refusal: a `PlatformOwnerError` can carry
/// a status line the browser has no use for, so the log keeps the
/// stage and the browser reads a sentence.
async fn owner_of(state: &ElevationState) -> Result<String, (String, ErrResp)> {
    roster_owner(state.owner.as_ref()).await
}

/// [`owner_of`] over any `PlatformOwner` — the promotion asks the same
/// roster with the same words.
pub(crate) async fn roster_owner(owner: &dyn PlatformOwner) -> Result<String, (String, ErrResp)> {
    owner.platform_owner().await.map_err(|e| {
        let reason = match e {
            PlatformOwnerError::NoHolder { .. } => "no platform owner in the roster",
            PlatformOwnerError::Unreachable(_) => "platform owner unresolvable",
        };
        (
            reason.to_string(),
            err(StatusCode::SERVICE_UNAVAILABLE, reason),
        )
    })
}

pub async fn elevate_begin(
    State(state): State<Arc<ElevationState>>,
    headers: HeaderMap,
) -> Response {
    let pk = &state.passkey;
    let (sess, employee_id) = match employee_session(&headers, &pk.session_key) {
        Ok(v) => v,
        Err(r) => return presence_refused("elevate_begin", None, "session", r),
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused("elevate_begin", Some(&employee_id), reason, r);
    // Refuse a session that could never be elevated BEFORE a passkey is
    // read or a challenge minted — the grant asks again after the
    // assertion, and that answer is the one that decides.
    let owner = match owner_of(&state).await {
        Ok(o) => o,
        Err((reason, r)) => return refused(&reason, r),
    };
    if let Err(refusal) = elevate(&sess, &owner, 0) {
        return refused(
            refusal.message(),
            err(StatusCode::FORBIDDEN, refusal.message()),
        );
    }
    // Only the operator-tier keys are offered: a self-enrolled key is
    // never asked for, so it cannot be the one that signs (review H1).
    let rows: Vec<Value> = match pk.stored_passkeys(&employee_id).await {
        Ok(v) => v
            .into_iter()
            .filter(|r| r["access_tier"] == ELEVATING_KEY_TIER)
            .collect(),
        Err(r) => return refused("stored passkeys", r),
    };
    if rows.is_empty() {
        return refused(
            "no operator-tier passkey",
            err(StatusCode::CONFLICT, NO_OPERATOR_KEY),
        );
    }

    let challenge = {
        use rand::RngExt;
        let mut buf = [0u8; 32];
        rand::rng().fill(&mut buf[..]);
        buf
    };
    let challenge_b64 = URL_SAFE_NO_PAD.encode(challenge);
    let challenge_id = Uuid::new_v4().to_string();
    let mint = pk
        .request(
            reqwest::Method::POST,
            format!("{}/api/people/presence-challenges", pk.people_base),
        )
        .json(&json!({
            "id": challenge_id,
            "employee_id": employee_id,
            "challenge": challenge_b64,
            "flow": ELEVATE_FLOW,
        }))
        .send()
        .await;
    if !matches!(&mint, Ok(r) if r.status().is_success()) {
        let reason = match &mint {
            Ok(r) => format!("challenge mint: {}", r.status()),
            Err(_) => "challenge mint: people unreachable".to_string(),
        };
        return refused(
            &reason,
            err(StatusCode::BAD_GATEWAY, "challenge mint failed"),
        );
    }

    let allow: Vec<Value> = rows
        .iter()
        .filter_map(|r| r["credential_id"].as_str())
        .map(|id| json!({"type": "public-key", "id": id}))
        .collect();
    Json(json!({
        "challenge_id": challenge_id,
        "publicKey": {
            "challenge": challenge_b64,
            "rpId": pk.webauthn.get_allowed_origins().first()
                .and_then(|o| o.host_str().map(String::from)),
            "allowCredentials": allow,
            "userVerification": "required",
            "timeout": 60_000,
        }
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ElevateFinishBody {
    challenge_id: String,
    credential: PublicKeyCredential,
}

pub async fn elevate_finish(
    State(state): State<Arc<ElevationState>>,
    headers: HeaderMap,
    Json(body): Json<ElevateFinishBody>,
) -> Response {
    let pk = &state.passkey;
    let (sess, employee_id) = match employee_session(&headers, &pk.session_key) {
        Ok(v) => v,
        Err(r) => return presence_refused("elevate_finish", None, "session", r),
    };
    let refused = |reason: &str, r: ErrResp| {
        presence_refused("elevate_finish", Some(&employee_id), reason, r)
    };
    let row = match consume_challenge(pk, &body.challenge_id).await {
        Ok(v) => v,
        Err(r) => return refused("challenge consume", r),
    };
    // A presence or enrolment challenge never elevates, and neither does
    // one minted for another employee.
    if row["flow"] != ELEVATE_FLOW || row["employee_id"] != employee_id.as_str() {
        return refused(
            "challenge minted for something else",
            err(
                StatusCode::FORBIDDEN,
                "challenge was minted for something else",
            ),
        );
    }
    let Some(challenge_b64) = row["challenge"].as_str() else {
        return refused(
            "challenge row carries no challenge",
            err(
                StatusCode::BAD_GATEWAY,
                "challenge row carries no challenge",
            ),
        );
    };
    // Verified against the operator-tier keys ONLY: an assertion from a
    // user-tier key is not in the set webauthn-rs checks (review H1).
    let key = match verify_assertion(
        pk,
        &employee_id,
        challenge_b64,
        &body.credential,
        Some(ELEVATING_KEY_TIER),
    )
    .await
    {
        Ok(key) => key,
        Err((reason, r)) => return refused(&reason, r),
    };
    grant(&state, &sess, &employee_id, now_epoch(), &key).await
}

/// After a VERIFIED assertion from the operator-tier `key`: ask the
/// roster who owns the platform, elevate if it is this session, RECORD
/// it, and only then set the cookie. Not routed and not public — its
/// only caller is [`elevate_finish`], after [`verify_assertion`]
/// returned the key.
pub(crate) async fn grant(
    state: &ElevationState,
    sess: &Session,
    employee_id: &str,
    asserted_at: u64,
    key: &Value,
) -> Response {
    let refused =
        |reason: &str, r: ErrResp| presence_refused("elevate_finish", Some(employee_id), reason, r);
    let owner = match owner_of(state).await {
        Ok(o) => o,
        Err((reason, r)) => return refused(&reason, r),
    };
    let elevated = match elevate(sess, &owner, asserted_at) {
        Ok(s) => s,
        Err(refusal) => {
            return refused(
                refusal.message(),
                err(StatusCode::FORBIDDEN, refusal.message()),
            );
        }
    };
    let recorded = state
        .passkey
        .audit
        .session_elevated(Elevation {
            email: &elevated.username,
            employee_id,
            elevated_at: asserted_at,
            expires_at: elevated.expiry,
            credential_label: key["label"].as_str().unwrap_or_default(),
            credential_registered_at: key["registered_at"].as_str().unwrap_or_default(),
        })
        .await;
    if recorded.is_err() {
        // The audit's own warn line carries why; the browser reads that
        // nothing changed.
        return refused(
            "the elevation could not be recorded",
            err(
                StatusCode::SERVICE_UNAVAILABLE,
                "the elevation could not be recorded, so it was not granted",
            ),
        );
    }
    let max_age = elevated.expiry.saturating_sub(now_epoch());
    let cookie = session::set_cookie(
        session::COOKIE_NAME,
        &elevated.encode(&state.passkey.session_key),
        max_age,
        "/",
    );
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    (
        StatusCode::OK,
        headers,
        Json(json!({
            "access_tier": elevated.access_tier(),
            "elevated_at": asserted_at,
            "expires_at": elevated.expiry,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuthAudit;
    use crate::audit::testing::Captured;
    use boss_core::platform_owner::{Fixed, Nobody};
    use webauthn_rs::prelude::{Url, WebauthnBuilder};

    const KEY: &[u8] = b"elevation-unit-test-session-key!";
    const OWNER: &str = "emp-owner";

    fn session_of(employee: Option<&str>, role: Option<&str>) -> Session {
        let mut s = Session::new("someone@example.com", 3600);
        s.employee_id = employee.map(str::to_string);
        s.role = role.map(str::to_string);
        s
    }

    fn state(owner: Arc<dyn PlatformOwner>, audit: AuthAudit) -> ElevationState {
        let origin = Url::parse("https://boss.test").unwrap();
        ElevationState {
            passkey: Arc::new(PasskeyState {
                session_key: KEY.to_vec(),
                http: crate::machine_client::MachineClient::build(reqwest::Client::builder())
                    .unwrap(),
                people_base: "http://127.0.0.1:9".into(),
                jobs_base: "http://127.0.0.1:9".into(),
                webauthn: WebauthnBuilder::new("boss.test", &origin)
                    .unwrap()
                    .build()
                    .unwrap(),
                audit,
            }),
            owner,
        }
    }

    /// The operator-tier key a verified assertion came from, as the
    /// verifier hands it back.
    fn key() -> Value {
        json!({
            "credential_id": "Y3JlZA",
            "label": "yubikey",
            "access_tier": ELEVATING_KEY_TIER,
            "registered_at": "2026-09-01T12:00:00Z",
        })
    }

    fn set_cookie_of(resp: &Response) -> Option<String> {
        resp.headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().to_string())
    }

    /// The owner's own platform-admin session is elevated — operator,
    /// the assertion's instant recorded, the expiry NOT extended. Every
    /// other shape is refused, by name.
    #[test]
    fn only_the_owners_platform_admin_session_is_elevated() {
        let owners = session_of(Some(OWNER), Some(PLATFORM_ADMIN_ROLE));
        let up = elevate(&owners, OWNER, 1_700_000_000).expect("the owner is elevated");
        assert_eq!(up.access_tier(), OPERATOR_TIER);
        assert_eq!(up.elevated_at(), Some(1_700_000_000));
        assert_eq!(up.expiry, owners.expiry, "the elevation never extends");
        assert_eq!(up.employee_id, owners.employee_id);
        assert_eq!(up.role, owners.role);

        let other = session_of(Some("emp-someone-else"), Some(PLATFORM_ADMIN_ROLE));
        assert_eq!(elevate(&other, OWNER, 1), Err(Refusal::NotTheOwner));
        // A downgraded owner session is not raised past its downgrade.
        let downgraded = session_of(Some(OWNER), Some(boss_core::roles::VISITOR_ROLE));
        assert_eq!(
            elevate(&downgraded, OWNER, 1),
            Err(Refusal::NotPlatformAdmin)
        );
        let roleless = session_of(Some(OWNER), None);
        assert_eq!(elevate(&roleless, OWNER, 1), Err(Refusal::NotPlatformAdmin));
        let guest = session_of(None, Some(boss_core::roles::AUDIT_READONLY_ROLE));
        assert_eq!(elevate(&guest, OWNER, 1), Err(Refusal::NoEmployee));
        let blank = session_of(Some("  "), Some(PLATFORM_ADMIN_ROLE));
        assert_eq!(elevate(&blank, OWNER, 1), Err(Refusal::NoEmployee));
        // No owner resolved is nobody, never a match on an empty id.
        let empty_owner = session_of(Some(""), Some(PLATFORM_ADMIN_ROLE));
        assert_eq!(elevate(&empty_owner, "", 1), Err(Refusal::NoEmployee));
        assert_eq!(elevate(&owners, "", 1), Err(Refusal::NotTheOwner));
    }

    /// No tier rides on break-glass: the emergency session carries no
    /// employee, so it can be nobody's owner.
    #[test]
    fn a_break_glass_session_is_never_elevated() {
        let (_, glass) = crate::break_glass::mint_session(KEY);
        assert_eq!(elevate(&glass, OWNER, 1), Err(Refusal::NoEmployee));
        assert_eq!(
            elevate(&glass, &glass.username, 1),
            Err(Refusal::NoEmployee)
        );
    }

    /// A fresh login is user tier with no elevation; an elevated
    /// session survives the cookie round trip with both fields.
    #[test]
    fn the_elevation_rides_the_signed_cookie() {
        let fresh = session_of(Some(OWNER), Some(PLATFORM_ADMIN_ROLE));
        assert_eq!(fresh.access_tier(), "user");
        assert_eq!(fresh.elevated_at(), None);
        assert!(
            !serde_json::to_string(&fresh).unwrap().contains("\"ea\""),
            "an unelevated cookie carries no elevation field"
        );
        let up = elevate(&fresh, OWNER, 42).unwrap();
        let back = Session::decode(&up.encode(KEY), KEY).unwrap();
        assert_eq!(back.access_tier(), OPERATOR_TIER);
        assert_eq!(back.elevated_at(), Some(42));
    }

    /// The grant elevates the owner, RECORDS `auth.session.elevated`
    /// naming the key, and sets the cookie for the rest of the session
    /// only.
    #[tokio::test]
    async fn the_grant_elevates_the_owner_and_puts_it_on_the_record() {
        let cap = Arc::new(Captured::default());
        let st = state(Arc::new(Fixed(OWNER.into())), AuthAudit::spawn(cap.clone()));
        let sess = session_of(Some(OWNER), Some(PLATFORM_ADMIN_ROLE));
        let now = now_epoch();
        let resp = grant(&st, &sess, OWNER, now, &key()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = set_cookie_of(&resp).expect("the elevated cookie is set");
        let max_age: u64 = cookie
            .split("Max-Age=")
            .nth(1)
            .and_then(|s| s.split(';').next())
            .and_then(|s| s.parse().ok())
            .expect("a Max-Age");
        assert!(max_age <= 3600, "the cookie outlives its session: {cookie}");
        let value = cookie
            .split(';')
            .next()
            .and_then(|kv| kv.split_once('='))
            .map(|(_, v)| v)
            .unwrap();
        let back = Session::decode(value, KEY).unwrap();
        assert_eq!(back.access_tier(), OPERATOR_TIER);
        assert_eq!(back.elevated_at(), Some(now));
        assert_eq!(back.expiry, sess.expiry);

        // Recorded BEFORE the response, not staged beside it: no drain
        // wait is needed to see it.
        let events = cap.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, "auth.session.elevated");
        assert_eq!(e.source, "gateway");
        assert_eq!(e.payload["employee_id"], OWNER);
        assert_eq!(e.payload["email"], "someone@example.com");
        assert_eq!(e.payload["method"], "passkey");
        assert_eq!(e.payload["access_tier"], OPERATOR_TIER);
        assert_eq!(e.payload["credential_label"], "yubikey");
        assert_eq!(
            e.payload["credential_registered_at"],
            "2026-09-01T12:00:00Z"
        );
        assert!(e.payload["elevated_at"].as_str().is_some(), "{e:?}");
        assert!(e.payload["expires_at"].as_str().is_some(), "{e:?}");
        let mut keys: Vec<&str> = e
            .payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "access_tier",
                "credential_label",
                "credential_registered_at",
                "elevated_at",
                "email",
                "employee_id",
                "expires_at",
                "method"
            ]
        );
    }

    /// A verified assertion from anyone but the owner changes nothing:
    /// no cookie, no event. Nor does one when no owner resolves.
    #[tokio::test]
    async fn the_grant_refuses_everyone_else_and_records_nothing() {
        let cap = Arc::new(Captured::default());
        let st = state(Arc::new(Fixed(OWNER.into())), AuthAudit::spawn(cap.clone()));
        let other = session_of(Some("emp-someone-else"), Some(PLATFORM_ADMIN_ROLE));
        let resp = grant(&st, &other, "emp-someone-else", now_epoch(), &key()).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(set_cookie_of(&resp).is_none());

        let st = state(Arc::new(Nobody), AuthAudit::spawn(cap.clone()));
        let owner = session_of(Some(OWNER), Some(PLATFORM_ADMIN_ROLE));
        let resp = grant(&st, &owner, OWNER, now_epoch(), &key()).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(set_cookie_of(&resp).is_none());

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            cap.0.lock().unwrap().is_empty(),
            "a refusal recorded an event"
        );
    }

    /// Review L2 — no silent grant: when the elevation cannot be put on
    /// the record (no audit staging configured, or the insert fails),
    /// the owner's session is NOT elevated.
    #[tokio::test]
    async fn an_elevation_that_cannot_be_recorded_is_not_granted() {
        let st = state(Arc::new(Fixed(OWNER.into())), AuthAudit::disabled());
        let owner = session_of(Some(OWNER), Some(PLATFORM_ADMIN_ROLE));
        let resp = grant(&st, &owner, OWNER, now_epoch(), &key()).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(set_cookie_of(&resp).is_none(), "an unrecorded grant");

        let st = state(
            Arc::new(Fixed(OWNER.into())),
            AuthAudit::spawn(Arc::new(Refusing)),
        );
        let resp = grant(&st, &owner, OWNER, now_epoch(), &key()).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(set_cookie_of(&resp).is_none(), "an unrecorded grant");
    }

    /// A recorder whose insert always fails.
    struct Refusing;

    #[async_trait]
    impl boss_core::port::EventRecorder for Refusing {
        async fn record(&self, _event: &boss_core::event::Event) -> Result<(), String> {
            Err("the outbox refused".into())
        }
    }

    /// THE ONE WRITER (review M2). A session's tier is private to
    /// `session.rs`, whose one writer of the operator tier is
    /// `Session::into_operator`; this scans the crate's source for every
    /// call of it and allows exactly one, here in `elevate`. An OIDC,
    /// password, guest or break-glass mint cannot raise a tier, and the
    /// next caller has to come through this test. Tests included.
    #[test]
    fn into_operator_has_one_caller_and_it_is_elevate() {
        // Spelled in two halves so this test's own text is not a match.
        let call = concat!("into_", "operator(");
        let definition = concat!("fn into_", "operator(");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut calls = Vec::new();
        let mut dirs = vec![src];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if code.contains(call) && !code.contains(definition) {
                        calls.push(format!("{name}:{}: {}", n + 1, line.trim()));
                    }
                }
            }
        }
        assert_eq!(
            calls.len(),
            1,
            "Session::into_operator must have exactly one caller, elevation::elevate:\n  {}",
            calls.join("\n  ")
        );
        assert!(
            calls[0].starts_with("elevation.rs:")
                && calls[0].contains(&format!("session.clone().{call}asserted_at)")),
            "{calls:?}"
        );
    }
}
