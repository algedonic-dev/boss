//! PROMOTING ONE OF THE OWNER'S PASSKEYS TO THE OPERATOR TIER — the /me
//! ceremony of design 2cb6256f (David, 2026-09-28; backlog 2a228d0c,
//! car 2).
//!
//! WHY. Only an operator-tier passkey raises the platform owner's
//! session to operator (`crate::elevation`), and every enrolment stores a
//! `user`-tier key — so until a key is promoted, the elevation is inert
//! and the owner's browser reads 403 on every operator door (backlog
//! 3c92c5b8). Car 1 landed the protocol (`passkey-promotion`) and the one
//! writer (boss-people's promote door) but held the door unmounted until
//! the gateway could sign for the ceremony. This is that half.
//!
//! THE CEREMONY, three calls from /me, nothing typed:
//!
//! 1. [`REQUEST_PATH`] — "Make this my operator key" on one of the
//!    owner's user-tier keys. The gateway files ONE `passkey-promotion`
//!    packet for that key, naming it from its stored row (employee,
//!    credential id, label, enrolment time, public-key fingerprint), or
//!    answers the one already open for it (D1, D2). Filing grants
//!    nothing: the answer says whether the packet is approved yet.
//! 2. The owner approves the packet's `authorise` step with a presence
//!    stamp, on the packet's own page, over exactly those bytes.
//! 3. [`BEGIN_PATH`] / [`FINISH_PATH`] — "Finish": the gateway re-judges
//!    the packet and asks for TWO assertions: one from the key being
//!    promoted (possession, D4) and one from a break-glass hardware key
//!    (the vouch, Q1 — the one factor a copied cookie cannot reach,
//!    because those records are committed to the tree by a reviewed
//!    car). With both verified it signs a
//!    `boss_core::passkey_promotion::PromotionTicket` and asks people to
//!    flip the row, then completes the packet's `promote` step with what
//!    it spent (D7: promote, THEN the step).
//!
//! THE CHECKS AT FINISH, in the design's order, the first failure named
//! (D3): the session's employee is the roster's platform owner
//! (`RosterPlatformOwner`, no environment override) and the session
//! carries platform-admin ([`crate::elevation::judge_owner`], the
//! elevation's own rule); the key is that employee's; the packet is
//! approved by a live presence stamp of his, open, and unspent
//! (`boss_core::passkey_promotion::judge_packet`, the judge people runs
//! again under its own read); both assertions verify.
//!
//! THE TICKET NEVER LEAVES THE GATEWAY'S HANDS BUT ONE WAY: it is minted
//! in the finish, from the session key, and sent to people in the same
//! request. `ticket_has_one_minter_and_promote_one_caller` pins that
//! this file is the only place in the crate that mints one and the only
//! caller of people's promote door.
//!
//! WHAT IT REFUSES THAT WAS OPEN BEFORE: nothing (David's rule on packet
//! 62dac114). No key could reach the operator tier before this car; the
//! elevation, enrolment, presence and break-glass sign-in are unchanged.
//! It adds one road, and only the platform owner can walk it.
//!
//! Pending ceremonies live in-process (single-use, five minutes), as the
//! break-glass door's do: the break-glass half's verification state is a
//! webauthn-rs value that cannot ride the people challenge ledger, and
//! the gateway deploys single-replica (`strategy: Recreate`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::passkey_promotion::{
    AUTHORISE_STEP, PROMOTE_STEP, PROMOTION_KIND, PromotionTicket, TICKET_TTL_SECONDS, judge_packet,
};
use boss_core::platform_owner::PlatformOwner;
use boss_core::presence::{now_epoch, public_key_fingerprint};
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::{PublicKeyCredential, SecurityKeyAuthentication, Url, Uuid};

use crate::break_glass::BreakGlassState;
use crate::elevation::{judge_owner, roster_owner};
use crate::passkey::{ErrResp, PasskeyState, employee_session, err, presence_refused};
use crate::session::Session;

pub const REQUEST_PATH: &str = "/api/auth/passkey/promotion/request";
pub const BEGIN_PATH: &str = "/api/auth/passkey/promotion/begin";
pub const FINISH_PATH: &str = "/api/auth/passkey/promotion/finish";

/// The tier a promoted key holds, as people's rows spell it.
const OPERATOR_TIER: &str = "operator";

/// How long a begun ceremony may take: two touches, the second of a key
/// that may live in a drawer.
const CEREMONY_TTL: Duration = Duration::from_secs(300);

/// A begun ceremony: whose, for which packet and key, and the two
/// challenges it is waiting on.
struct Pending {
    employee_id: String,
    packet_id: Uuid,
    credential_id: String,
    /// base64url of the random challenge the promoted key must sign.
    key_challenge: String,
    /// The break-glass half's verification state.
    vouch: SecurityKeyAuthentication,
    expires: Instant,
}

/// The ceremony's state: the passkey machinery (people, jobs, the
/// verifier, the session key), the break-glass verifier, who the owner
/// is, and the ceremonies begun and not yet finished.
pub struct PromotionState {
    pub passkey: Arc<PasskeyState>,
    pub break_glass: Arc<BreakGlassState>,
    pub owner: Arc<dyn PlatformOwner>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl PromotionState {
    pub fn new(
        passkey: Arc<PasskeyState>,
        break_glass: Arc<BreakGlassState>,
        owner: Arc<dyn PlatformOwner>,
    ) -> Self {
        Self {
            passkey,
            break_glass,
            owner,
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn put(&self, id: String, p: Pending) {
        let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        map.retain(|_, p| p.expires > now);
        map.insert(id, p);
    }

    fn take(&self, id: &str) -> Result<Pending, ErrResp> {
        let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        map.retain(|_, p| p.expires > now);
        map.remove(id).ok_or_else(|| {
            err(
                StatusCode::GONE,
                "this promotion ceremony is unknown, spent or expired — press Finish again",
            )
        })
    }
}

/// The ceremony's three routes, the one list `main.rs` mounts and the
/// tests drive.
pub fn ceremony_router<S>(state: Arc<PromotionState>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            REQUEST_PATH,
            post(promotion_request).with_state(state.clone()),
        )
        .route(BEGIN_PATH, post(promotion_begin).with_state(state.clone()))
        .route(FINISH_PATH, post(promotion_finish).with_state(state))
}

// ---------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------

/// The packet the request files for `row`, one of `employee_id`'s stored
/// passkeys as people lists it. Pure. The key is named in the packet's
/// metadata, which the workflow projects onto the `authorise` step the
/// owner signs — so what he approves is exactly this row.
pub fn packet_body(employee_id: &str, row: &Value) -> Result<Value, ErrResp> {
    let text = |k: &str| row[k].as_str().unwrap_or_default().to_string();
    let (credential_id, label, registered_at) =
        (text("credential_id"), text("label"), text("registered_at"));
    let public_key = URL_SAFE_NO_PAD.decode(text("public_key")).map_err(|_| {
        err(
            StatusCode::BAD_GATEWAY,
            "credential public_key not base64url",
        )
    })?;
    if credential_id.is_empty() || label.is_empty() || registered_at.is_empty() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            "the stored key does not name its id, label and enrolment time",
        ));
    }
    Ok(json!({
        "kind": PROMOTION_KIND,
        "subject": {"subject_kind": "employee", "id": employee_id},
        "title": format!("Make {label} an operator key"),
        "owner_id": employee_id,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "employee_id": employee_id,
            "credential_id": credential_id,
            "label": label,
            "registered_at": registered_at,
            "public_key_sha256": public_key_fingerprint(&public_key),
        },
    }))
}

/// The id of the packet's step with `slug`, or a refusal naming it.
fn step_of<'a>(packet: &'a Value, slug: &str) -> Result<&'a Value, ErrResp> {
    packet["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
        .ok_or_else(|| {
            err(
                StatusCode::CONFLICT,
                format!("the promotion packet has no `{slug}` step"),
            )
        })
}

/// What the `promote` step is completed with: every key it already
/// holds (the step PUT refuses a body that drops one) plus what this
/// ceremony spent. `promoted_at` is the ceremony's instant, by the wall
/// clock the ticket's expiry is minted against (an auth act, like the
/// elevation's `asserted_at`, never the simulation clock).
pub fn promote_step_metadata(
    existing: &Value,
    credential_id: &str,
    vouched_by: &str,
    promoted_at_epoch: u64,
) -> Value {
    let promoted_at = chrono::DateTime::from_timestamp(promoted_at_epoch as i64, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();
    let mut m = existing.as_object().cloned().unwrap_or_default();
    m.insert("credential_id".into(), json!(credential_id));
    m.insert("promoted_at".into(), json!(promoted_at));
    m.insert("vouched_by".into(), json!(vouched_by));
    Value::Object(m)
}

// ---------------------------------------------------------------------------
// The jobs and people calls, each signed as the gateway
// ---------------------------------------------------------------------------

async fn read_packet(pk: &PasskeyState, id: Uuid) -> Result<Value, ErrResp> {
    let url = format!("{}/api/jobs/{id}", pk.jobs_base);
    // Fixed text: reqwest's errors name the internal URL.
    let resp = pk
        .request(reqwest::Method::GET, url)
        .send()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "jobs unreachable"))?;
    match resp.status() {
        s if s.is_success() => resp
            .json()
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "promotion packet malformed")),
        reqwest::StatusCode::NOT_FOUND => Err(err(
            StatusCode::NOT_FOUND,
            format!("no packet {id} — press \"Make this my operator key\" to file one"),
        )),
        s => Err(err(
            StatusCode::BAD_GATEWAY,
            format!("reading the promotion packet answered {s}"),
        )),
    }
}

/// The OPEN `passkey-promotion` packets naming this key, whole list or a
/// refusal — a truncated page would read as "none" and file a second.
async fn open_packets_for(
    pk: &PasskeyState,
    employee_id: &str,
    credential_id: &str,
) -> Result<Vec<Uuid>, ErrResp> {
    let names = json!({"employee_id": employee_id, "credential_id": credential_id}).to_string();
    // Encoded by the URL parser: the metadata filter is a JSON object.
    let url = Url::parse_with_params(
        &format!("{}/api/jobs", pk.jobs_base),
        &[
            ("kind", PROMOTION_KIND),
            ("status", "open"),
            ("metadata", names.as_str()),
            ("limit", "50"),
        ],
    )
    .map_err(|_| err(StatusCode::BAD_GATEWAY, "the jobs API address is malformed"))?;
    let resp = pk
        .request(reqwest::Method::GET, url.to_string())
        .send()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "jobs unreachable"))?;
    if !resp.status().is_success() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            format!("listing promotion packets answered {}", resp.status()),
        ));
    }
    let page: Value = resp
        .json()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "promotion packet list malformed"))?;
    let rows = page["data"].as_array().cloned().unwrap_or_default();
    if page["total"].as_u64() != Some(rows.len() as u64) {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            "the open promotion packets for this key could not be read whole",
        ));
    }
    Ok(rows
        .iter()
        .filter_map(|r| r["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()))
        .collect())
}

async fn file_packet(pk: &PasskeyState, body: &Value) -> Result<Uuid, ErrResp> {
    let url = format!("{}/api/jobs", pk.jobs_base);
    let resp = pk
        .request(reqwest::Method::POST, url)
        .json(body)
        .send()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "jobs unreachable"))?;
    let status = resp.status();
    let created: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(err(
            StatusCode::BAD_GATEWAY,
            format!("filing the promotion packet answered {status}"),
        ));
    }
    created
        .get("id")
        .or_else(|| created.pointer("/data/id"))
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            err(
                StatusCode::BAD_GATEWAY,
                "the jobs API filed the promotion packet without an id",
            )
        })
}

/// PEOPLE'S PROMOTE DOOR — the one call of it in this crate (pinned).
/// `Ok(promoted)` on a 200; otherwise people's own status and sentence.
async fn ask_people_to_promote(
    pk: &PasskeyState,
    employee_id: &str,
    credential_id: &str,
    packet_id: Uuid,
    ticket: &str,
) -> Result<bool, ErrResp> {
    let url = format!(
        "{}/api/people/{employee_id}/webauthn-credentials/{credential_id}/promote",
        pk.people_base
    );
    let resp = pk
        .request(reqwest::Method::POST, url)
        .json(&json!({ "packet_id": packet_id, "ticket": ticket }))
        .send()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "people unreachable"))?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if status.is_success() {
        let out: Value = resp.json().await.unwrap_or(Value::Null);
        return Ok(out["promoted"].as_bool().unwrap_or(false));
    }
    // People's refusals are fixed sentences about the packet and the
    // key; they are the owner's to read.
    Err((status, resp.text().await.unwrap_or_default()))
}

async fn complete_promote_step(
    pk: &PasskeyState,
    packet_id: Uuid,
    step: &Value,
    metadata: &Value,
) -> Result<(), ErrResp> {
    let step_id = step["id"].as_str().unwrap_or_default();
    let step_id = Uuid::parse_str(step_id)
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "the promote step has no id"))?;
    let url = format!("{}/api/jobs/{packet_id}/steps/{step_id}", pk.jobs_base);
    let resp = pk
        .request(reqwest::Method::PUT, url)
        .json(&json!({ "status": "completed", "metadata": metadata }))
        .send()
        .await
        .map_err(|_| err(StatusCode::BAD_GATEWAY, "jobs unreachable"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_GATEWAY,
            format!("completing the promote step answered {}", resp.status()),
        ))
    }
}

// ---------------------------------------------------------------------------
// The handlers
// ---------------------------------------------------------------------------

/// D3's first two checks, for every call: a session with an employee,
/// who is the roster's platform owner, in a session carrying
/// platform-admin. `ceremony` names the call in the log.
async fn owners_session(
    state: &PromotionState,
    headers: &HeaderMap,
    ceremony: &'static str,
) -> Result<(Session, String), Response> {
    let (sess, employee_id) = employee_session(headers, &state.passkey.session_key)
        .map_err(|r| presence_refused(ceremony, None, "session", r))?;
    let owner = roster_owner(state.owner.as_ref())
        .await
        .map_err(|(reason, r)| presence_refused(ceremony, Some(&employee_id), &reason, r))?;
    judge_owner(&sess, &owner).map_err(|refusal| {
        let words = format!(
            "promoting a passkey to operator is the platform owner's own act: {}",
            refusal.message()
        );
        presence_refused(
            ceremony,
            Some(&employee_id),
            refusal.message(),
            err(StatusCode::FORBIDDEN, words),
        )
    })?;
    Ok((sess, employee_id))
}

/// The session's own stored key `credential_id`, or 403 — D3's third
/// check: a key on anyone else's account is not his to promote.
async fn own_key(
    pk: &PasskeyState,
    employee_id: &str,
    credential_id: &str,
) -> Result<Value, ErrResp> {
    pk.stored_passkeys(employee_id)
        .await?
        .into_iter()
        .find(|r| r["credential_id"] == credential_id)
        .ok_or_else(|| {
            err(
                StatusCode::FORBIDDEN,
                "that passkey is not one of yours — only your own key can be promoted",
            )
        })
}

#[derive(Deserialize)]
pub struct RequestBody {
    credential_id: String,
}

/// `POST /api/auth/passkey/promotion/request` — "Make this my operator
/// key". Files the key's packet, or finds the one already open, and says
/// whether it is approved. Grants nothing.
pub async fn promotion_request(
    State(state): State<Arc<PromotionState>>,
    headers: HeaderMap,
    Json(body): Json<RequestBody>,
) -> Response {
    const CEREMONY: &str = "promotion_request";
    let (_sess, employee_id) = match owners_session(&state, &headers, CEREMONY).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused(CEREMONY, Some(&employee_id), reason, r);
    let pk = &state.passkey;
    let row = match own_key(pk, &employee_id, &body.credential_id).await {
        Ok(r) => r,
        Err(r) => return refused("not the session's key", r),
    };
    let label = row["label"].as_str().unwrap_or("passkey").to_string();
    let open = match open_packets_for(pk, &employee_id, &body.credential_id).await {
        Ok(v) => v,
        Err(r) => return refused("open packets unreadable", r),
    };
    // An open packet for this key is the answer, never a second one; the
    // approved one first, so a finish interrupted after the flip (D7) is
    // finished rather than refiled.
    let mut found: Option<(Uuid, bool)> = None;
    for id in open {
        let packet = match read_packet(pk, id).await {
            Ok(p) => p,
            Err(r) => return refused("open packet unreadable", r),
        };
        let approved = judge_packet(&packet, &employee_id, &body.credential_id).is_ok();
        if found.is_none() || approved {
            found = Some((id, approved));
        }
        if approved {
            break;
        }
    }
    if row["access_tier"] == OPERATOR_TIER && !found.is_some_and(|(_, approved)| approved) {
        return refused(
            "already operator",
            err(
                StatusCode::CONFLICT,
                format!("{label} is already an operator key — there is nothing to promote"),
            ),
        );
    }
    let (packet_id, approved) = match found {
        Some(f) => f,
        None => {
            let filed = match packet_body(&employee_id, &row) {
                Ok(b) => file_packet(pk, &b).await,
                Err(r) => Err(r),
            };
            match filed {
                Ok(id) => (id, false),
                Err(r) => return refused("packet not filed", r),
            }
        }
    };
    Json(json!({
        "packet_id": packet_id,
        "label": label,
        "approved": approved,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct BeginBody {
    packet_id: String,
}

/// `POST /api/auth/passkey/promotion/begin` — "Finish", first half: the
/// packet judged, then the two challenges.
pub async fn promotion_begin(
    State(state): State<Arc<PromotionState>>,
    headers: HeaderMap,
    Json(body): Json<BeginBody>,
) -> Response {
    const CEREMONY: &str = "promotion_begin";
    let (_sess, employee_id) = match owners_session(&state, &headers, CEREMONY).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused(CEREMONY, Some(&employee_id), reason, r);
    let pk = &state.passkey;
    let Ok(packet_id) = Uuid::parse_str(body.packet_id.trim()) else {
        return refused(
            "malformed packet id",
            err(StatusCode::BAD_REQUEST, "malformed packet id"),
        );
    };
    let packet = match read_packet(pk, packet_id).await {
        Ok(p) => p,
        Err(r) => return refused("packet unreadable", r),
    };
    let credential_id = step_of(&packet, AUTHORISE_STEP)
        .map(|s| {
            s["metadata"]["credential_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default();
    let row = match own_key(pk, &employee_id, &credential_id).await {
        Ok(r) => r,
        Err(r) => return refused("not the session's key", r),
    };
    if let Err(r) = judge_packet(&packet, &employee_id, &credential_id) {
        return refused("packet does not authorise", r);
    }
    let (vouch_options, vouch) = match state.break_glass.begin_assertion() {
        Ok(v) => v,
        Err(r) => return refused("no break-glass key can vouch", r),
    };
    let key_challenge = {
        use rand::RngExt;
        let mut buf = [0u8; 32];
        rand::rng().fill(&mut buf[..]);
        URL_SAFE_NO_PAD.encode(buf)
    };
    let ceremony_id = Uuid::new_v4().to_string();
    state.put(
        ceremony_id.clone(),
        Pending {
            employee_id: employee_id.clone(),
            packet_id,
            credential_id: credential_id.clone(),
            key_challenge: key_challenge.clone(),
            vouch,
            expires: Instant::now() + CEREMONY_TTL,
        },
    );
    Json(json!({
        "ceremony_id": ceremony_id,
        "label": row["label"],
        // The key being promoted: only it is offered.
        "key": {
            "publicKey": {
                "challenge": key_challenge,
                "rpId": pk.webauthn.get_allowed_origins().first()
                    .and_then(|o| o.host_str().map(String::from)),
                "allowCredentials": [{"type": "public-key", "id": credential_id}],
                "userVerification": "required",
                "timeout": 60_000,
            }
        },
        // A break-glass hardware key: the committed records bound here.
        "vouch": vouch_options,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct FinishBody {
    ceremony_id: String,
    key: PublicKeyCredential,
    vouch: PublicKeyCredential,
}

/// `POST /api/auth/passkey/promotion/finish` — every check again, then
/// the ticket, the flip, and the step.
pub async fn promotion_finish(
    State(state): State<Arc<PromotionState>>,
    headers: HeaderMap,
    Json(body): Json<FinishBody>,
) -> Response {
    const CEREMONY: &str = "promotion_finish";
    // Taken first and single-use: a finish that fails anywhere below
    // needs a fresh begin, so no challenge is ever answered twice.
    let pending = match state.take(&body.ceremony_id) {
        Ok(p) => p,
        Err(r) => return presence_refused(CEREMONY, None, "ceremony unknown", r),
    };
    // D3.1, D3.2 — the roster's owner, in a platform-admin session…
    let (_sess, employee_id) = match owners_session(&state, &headers, CEREMONY).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused(CEREMONY, Some(&employee_id), reason, r);
    // …the same one that began it.
    if pending.employee_id != employee_id {
        return refused(
            "ceremony begun by another session",
            err(
                StatusCode::FORBIDDEN,
                "this ceremony was begun by someone else",
            ),
        );
    }
    let pk = &state.passkey;
    // D3.3 — the key is his.
    if let Err(r) = own_key(pk, &employee_id, &pending.credential_id).await {
        return refused("not the session's key", r);
    }
    // D3.4, D3.5 — approved by his live presence stamp, open, unspent.
    let packet = match read_packet(pk, pending.packet_id).await {
        Ok(p) => p,
        Err(r) => return refused("packet unreadable", r),
    };
    if let Err(r) = judge_packet(&packet, &employee_id, &pending.credential_id) {
        return refused("packet does not authorise", r);
    }
    let promote_step = match step_of(&packet, PROMOTE_STEP) {
        Ok(s) => s.clone(),
        Err(r) => return refused("no promote step", r),
    };
    // D3.6 / D4 — the key being promoted asserted (possession)…
    let asserted = match crate::passkey::verify_assertion(
        pk,
        &employee_id,
        &pending.key_challenge,
        &body.key,
        None,
    )
    .await
    {
        Ok(row) => row,
        Err((reason, r)) => return refused(&reason, r),
    };
    if asserted["credential_id"] != pending.credential_id.as_str() {
        return refused(
            "another key asserted",
            err(
                StatusCode::FORBIDDEN,
                "the key that answered is not the key being promoted — touch that key",
            ),
        );
    }
    // …and a break-glass hardware key vouched (Q1). Without this no
    // ticket exists, and without a ticket people flips nothing: an
    // approval signed by a freshly enrolled key promotes nothing (F3).
    let vouched_by = match state
        .break_glass
        .finish_assertion(&body.vouch, &pending.vouch)
        .ok()
        .and_then(|cred| state.break_glass.label_of(&cred))
    {
        Some(label) => label.as_str().to_string(),
        None => {
            return refused(
                "break-glass vouch refused",
                err(
                    StatusCode::UNAUTHORIZED,
                    "the break-glass key's assertion did not verify — a promotion is vouched \
                     for by one of the committed break-glass hardware keys",
                ),
            );
        }
    };
    let Some(ticket) = (PromotionTicket {
        p: pending.packet_id.to_string(),
        i: employee_id.clone(),
        c: pending.credential_id.clone(),
        v: vouched_by.clone(),
        n: Uuid::new_v4().to_string(),
        e: now_epoch() + TICKET_TTL_SECONDS,
    })
    .encode(&pk.session_key) else {
        let reason = "the promotion ticket could not be signed";
        return refused(reason, err(StatusCode::INTERNAL_SERVER_ERROR, reason));
    };
    // D7: promote, THEN the step. A failure between the two is repaired
    // by pressing Finish again: people answers the same packet's key
    // "already operator" with 200 and records nothing twice.
    let promoted = match ask_people_to_promote(
        pk,
        &employee_id,
        &pending.credential_id,
        pending.packet_id,
        &ticket,
    )
    .await
    {
        Ok(p) => p,
        Err(r) => return refused("people refused the promotion", r),
    };
    let metadata = promote_step_metadata(
        &promote_step["metadata"],
        &pending.credential_id,
        &vouched_by,
        now_epoch(),
    );
    if let Err((_, why)) =
        complete_promote_step(pk, pending.packet_id, &promote_step, &metadata).await
    {
        return refused(
            &why,
            err(
                StatusCode::BAD_GATEWAY,
                "the key is now an operator key, but its packet could not be closed — press \
                 Finish again to close it",
            ),
        );
    }
    Json(json!({
        "packet_id": pending.packet_id,
        "credential_id": pending.credential_id,
        "access_tier": OPERATOR_TIER,
        "vouched_by": vouched_by,
        "promoted": promoted,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Value {
        json!({
            "credential_id": "Y3JlZA",
            "label": "yubikey-5c",
            "registered_at": "2026-09-28T09:00:00.123456Z",
            "public_key": URL_SAFE_NO_PAD.encode(b"stored key bytes"),
            "access_tier": "user",
        })
    }

    /// The filed packet names the key exactly as its row does, and its
    /// fingerprint is of the bytes people stores — what people compares
    /// under the flip's lock.
    #[test]
    fn the_filed_packet_names_the_key_as_its_row_does() {
        let b = packet_body("emp-owner", &row()).unwrap();
        assert_eq!(b["kind"], PROMOTION_KIND);
        assert_eq!(
            b["subject"],
            json!({"subject_kind": "employee", "id": "emp-owner"})
        );
        assert_eq!(b["owner_id"], "emp-owner");
        let m = &b["metadata"];
        assert_eq!(m["employee_id"], "emp-owner");
        assert_eq!(m["credential_id"], "Y3JlZA");
        assert_eq!(m["label"], "yubikey-5c");
        assert_eq!(m["registered_at"], "2026-09-28T09:00:00.123456Z");
        assert_eq!(
            m["public_key_sha256"],
            public_key_fingerprint(b"stored key bytes")
        );
        assert!(
            m.get("public_key").is_none(),
            "never key material on a packet"
        );
    }

    #[test]
    fn a_row_that_does_not_name_its_key_files_nothing() {
        for k in ["credential_id", "label", "registered_at"] {
            let mut r = row();
            r[k] = json!("");
            assert!(packet_body("emp-owner", &r).is_err(), "{k}");
        }
        let mut r = row();
        r["public_key"] = json!("not base64url!");
        assert!(packet_body("emp-owner", &r).is_err());
    }

    /// The step PUT refuses a body that drops a key the step holds, so
    /// the completion carries the step's own keys plus the spend.
    #[test]
    fn the_promote_step_keeps_its_keys_and_adds_the_spend() {
        let m = promote_step_metadata(
            &json!({"procedure": "p"}),
            "Y3JlZA",
            "primary",
            1_790_000_000,
        );
        assert_eq!(m["procedure"], "p");
        assert_eq!(m["credential_id"], "Y3JlZA");
        assert_eq!(m["vouched_by"], "primary");
        assert_eq!(m["promoted_at"], "2026-09-21T14:13:20+00:00");
    }

    /// ONE MINTER, ONE CALLER (design 2cb6256f D5's gateway half). A
    /// promotion ticket is signed in exactly one place in this crate, and
    /// people's promote door is called from exactly one — both here — so
    /// the next road to an operator key has to come through this test.
    /// Tests excluded; comments excluded.
    #[test]
    fn ticket_has_one_minter_and_promote_one_caller() {
        let mint = concat!("PromotionTicket", " {");
        let door = concat!("/promote", "\"");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let (mut mints, mut doors) = (Vec::new(), Vec::new());
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
                // Only the code before a file's test module.
                let code = text.split("#[cfg(test)]").next().unwrap_or("");
                for (n, line) in code.lines().enumerate() {
                    let line = line.split("//").next().unwrap_or("");
                    if line.contains(mint) {
                        mints.push(format!("{name}:{}", n + 1));
                    }
                    if line.contains(door) {
                        doors.push(format!("{name}:{}", n + 1));
                    }
                }
            }
        }
        assert!(
            mints.len() == 1 && mints[0].starts_with("promotion.rs:"),
            "a promotion ticket is minted only by the finish: {mints:?}"
        );
        assert!(
            doors.len() == 1 && doors[0].starts_with("promotion.rs:"),
            "people's promote door has one caller, the finish: {doors:?}"
        );
    }
}
