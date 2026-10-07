//! The one writer of an operator-tier passkey (design 2cb6256f D5-D7,
//! David 2026-09-28; backlog 2a228d0c).
//!
//! An operator-tier row in `webauthn_credentials` is what lets the
//! gateway raise the platform owner's session to operator
//! (`boss-gateway` `elevation.rs`), so turning a key into one is a trust
//! act with a packet of its own — `passkey-promotion`, one per key:
//! David approves the rendered request with a presence stamp, then
//! touches the key being promoted and a break-glass hardware key on /me,
//! and only then does the gateway ask for the flip here.
//!
//! WHO MAY ASK. Only `automation:gateway`, judged by the caller's ACTOR
//! ID and not its role: the owner's own proxied browser session carries
//! `platform-admin` (the elevation needs it), so a role gate would let a
//! copy of that cookie promote a software key it had just enrolled — the
//! H1 attack of the review of car 0bde9b99. The gateway's proxy strips a
//! client's `x-boss-*` headers and stamps the session's own id.
//!
//! THE ID IS NOT ENOUGH, SO THE DOOR READS THE PACKET (adversarial review
//! of this car, findings 1 and 2, 2026-09-28). `x-boss-user` is a claim,
//! and while the machine gate is off any in-cluster pod, LAN or WireGuard
//! caller, or dev-pod agent can send `{"id":"automation:gateway"}` — so
//! an actor check alone turns transient access into a durable operator
//! key. Before anything is written, the door reads the named packet from
//! the jobs API as its own service identity and requires, naming the
//! first that fails ([`judge_packet`], `boss_core::passkey_promotion` —
//! the one judge the gateway's finish runs too): a `passkey-promotion`
//! packet, still open; its `authorise` step completed with `decision =
//! approved`; a LIVE presence stamp on that step (the one rule,
//! `Step::live_stamps`) by the key's own employee; that step naming
//! exactly this employee and this credential; and its `promote` step not
//! yet completed. A copied cookie can enrol a software key and sign
//! `authorise` with it, so these checks are necessary, not sufficient
//! (review F3 of car 1d9970d1) — which is what the ticket below is for.
//!
//! THE GATEWAY'S TICKET (car 2 of the design; the review of car
//! 293d5dc1). The break-glass vouch and the promoted key's own touch are
//! the gateway's to verify, and this door believes they happened only on
//! a `boss_core::passkey_promotion::PromotionTicket` signed with the
//! session key — which the gateway holds and a forger of `x-boss-user`
//! does not. It binds the packet, the employee, the credential, the
//! break-glass key that vouched, a nonce and a two-minute expiry; each is
//! compared with the request, `vouched_by` is READ FROM IT (the body has
//! no such field, and one naming it is refused), and the nonce is spent
//! in the flip's own transaction (`passkey_promotion_tickets`), so a
//! copied ticket replayed inside its expiry is refused.
//!
//! THE FACT RIDES WITH THE ROW (D6). The flip, the packet it spent and
//! `auth.passkey.promoted` are written in one transaction, through the
//! outbox, so the row and the record cannot disagree.
//!
//! A REPLAY IS HARMLESS, AND ONLY UNDER ITS OWN PACKET (D7). The gateway
//! promotes, then completes its step; a replay after a failed step write
//! is a FRESH ceremony (a fresh ticket) under the same packet, finds the
//! key already operator and promoted by that packet: 200, nothing written
//! but the spent nonce, no second fact. The
//! same ask under any OTHER packet is refused 409, naming the packet
//! that promoted the key — one approval never answers for another.
//!
//! THE TABLE HOLDS THE RULE TOO (review F4 of car 1d9970d1, and finding
//! 3 of this car's review). A trigger on `webauthn_credentials`
//! (`20260928170011-only-the-promotion-writes-an-operator-passkey.sql`)
//! refuses, outside a transaction carrying this module's mark, any row
//! becoming operator and any rekey of an operator row. That is a guard
//! against APPLICATION paths — a future handler, a careless UPDATE — not
//! a boundary against someone who can run SQL: anyone who can
//! `SET LOCAL` the mark, or is a database superuser, is past it.
//!
//! THE KEY IS THE KEY THE OWNER SIGNED FOR (review of car 293d5dc1). The
//! row is found by credential id, and a platform-admin can delete a key
//! and re-register the same credential id with other key material while
//! an approved packet waits. The packet's `authorise` step names the
//! key's `label`, `registered_at` and `public_key_sha256`
//! (`boss_core::presence::public_key_fingerprint` of the stored key), and
//! the flip compares all three with the row under its lock.
//!
//! MOUNTED WITH ITS TICKET. Car 1 held this door unmounted because, with
//! a free-text `vouched_by`, a copied owner cookie with LAN reach could
//! enrol a software key K, file and approve a packet naming K, and POST
//! here claiming to be the gateway (review of car 293d5dc1). The ticket
//! closes that: without the session key there is no ticket, and without
//! a break-glass assertion the gateway signs none. So the service binary
//! mounts it now, and `the_service_mounts_the_promote_door_with_its_ticket_key`
//! pins that it is mounted exactly once, with the key. Mounting it
//! refuses nothing anyone could do before (David's rule on packet
//! 62dac114): it adds the one road to an operator key.
//!
//! Demotion is removal, through the existing credential DELETE (D8).

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use boss_core::actor::{ActorId, GATEWAY_ACTOR_ID};
pub use boss_core::passkey_promotion::{
    AUTHORISE_STEP, ApprovedKey, PROMOTE_STEP, PROMOTION_KIND, PromotionTicket, Refusal,
    judge_packet,
};
use boss_core::publisher::EventStamp;
use boss_policy_client::CurrentUser;

use crate::events::PASSKEY_PROMOTED;

/// The tier a promotion writes, and the only place in this crate that
/// writes it (pinned below).
const OPERATOR_TIER: &str = "operator";

/// The transaction-local mark the table's trigger requires before it
/// lets a row become (or be rekeyed as) `operator`. Set to the packet id
/// the promotion spends, and spelled only here in the tree (pinned
/// below).
const PROMOTION_MARK: &str = "boss.passkey_promotion";

/// This service's own identity on the jobs API.
const PEOPLE_ACTOR: &str = "automation:people";

// ---------------------------------------------------------------------------
// The packet, read from the jobs API
// ---------------------------------------------------------------------------

/// The jobs API as this door uses it: read one packet whole, steps and
/// stamps included. A port so the door is tested against memory.
#[async_trait]
pub trait PromotionPackets: Send + Sync {
    /// `Ok(None)` when the jobs API has no such packet; `Err` when it
    /// could not be asked or did not answer sensibly.
    async fn packet(&self, job_id: Uuid) -> Result<Option<Value>, String>;
}

/// The HTTP adapter, signed as `automation:people` with the machine
/// token when the process has one. The packet's authority is the stamp
/// on it, not who fetched it.
pub struct JobsApiPromotionPackets {
    base_url: String,
    /// Stamps the machine token per request from the process's watched
    /// source, so a rotation reaches this client without a restart
    /// (design 6805c764 car 2; `no_client_bakes_the_machine_token_in`).
    http: boss_core::machine_token::Client,
    /// This service's identity, sent as `x-boss-user` on every read.
    user: String,
}

impl JobsApiPromotionPackets {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = boss_core::http_client::base(base_url);
        let user = json!({
            "id": PEOPLE_ACTOR,
            "role": boss_core::roles::PLATFORM_ADMIN_ROLE,
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string();
        Self {
            base_url,
            http,
            user,
        }
    }
}

#[async_trait]
impl PromotionPackets for JobsApiPromotionPackets {
    async fn packet(&self, job_id: Uuid) -> Result<Option<Value>, String> {
        let url = format!("{}/api/jobs/{job_id}", self.base_url);
        // Fixed text on failure: reqwest's errors name the internal URL.
        let resp = self
            .http
            .get(url)
            .header("x-boss-user", &self.user)
            .send()
            .await
            .map_err(|_| "the jobs API is unreachable — the packet cannot be read".to_string())?;
        match resp.status() {
            s if s.is_success() => resp
                .json()
                .await
                .map(Some)
                .map_err(|_| format!("packet {job_id} is malformed")),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            s => Err(format!("reading packet {job_id} answered {s}")),
        }
    }
}

// ---------------------------------------------------------------------------
// The door
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The key the gateway's ticket is verified with
// ---------------------------------------------------------------------------

/// Where the session key comes from. The gateway, the jobs API and this
/// service run in the one `boss` container and read the same read-only
/// Secret mount, named by `BOSS_SESSION_KEY` (`boss_core::presence`); no
/// new secret exists for the ticket.
pub enum TicketKey {
    /// The gateway's key file, READ AT EACH PROMOTION: promotions are
    /// rare, a rotated key is then picked up at once, and a file the
    /// gateway has not written yet (the quickstart's parallel start) is a
    /// refusal now rather than one for the life of the process.
    File(std::path::PathBuf),
    /// A key handed over whole — tests.
    Fixed(Vec<u8>),
}

impl TicketKey {
    /// The key file where the gateway itself looks for it.
    pub fn from_env() -> Self {
        Self::File(boss_core::presence::session_key_path())
    }

    /// The key, or `None` — in which case no ticket verifies (fail
    /// closed), and the log says why without naming any key material.
    async fn get(&self) -> Option<Vec<u8>> {
        match self {
            Self::Fixed(key) => Some(key.clone()),
            Self::File(path) => {
                let text = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| {
                        tracing::warn!(path = %path.display(), error = %e,
                            "passkey promotion: the gateway's key file is unreadable; no \
                             promotion ticket verifies");
                    })
                    .ok()?;
                boss_core::presence::parse_session_key(&text)
                    .map_err(|e| {
                        tracing::warn!(path = %path.display(), error = %e,
                            "passkey promotion: the gateway's key file holds no usable key; \
                             no promotion ticket verifies");
                    })
                    .ok()
            }
        }
    }
}

#[derive(Clone)]
pub struct PromotionState {
    pub pool: Arc<PgPool>,
    pub packets: Arc<dyn PromotionPackets>,
    pub key: Arc<TicketKey>,
}

/// `POST /api/people/{id}/webauthn-credentials/{credential_id}/promote`,
/// mounted by the service binary with the jobs API as `packets` and the
/// gateway's key file as `key`.
pub fn promotion_router(
    pool: PgPool,
    packets: Arc<dyn PromotionPackets>,
    key: Arc<TicketKey>,
) -> Router {
    Router::new()
        .route(
            "/api/people/{id}/webauthn-credentials/{credential_id}/promote",
            post(promote_credential),
        )
        .with_state(PromotionState {
            pool: Arc::new(pool),
            packets,
            key,
        })
}

/// Exactly these two keys: a body that also names a tier, a voucher, or
/// anything else, is refused rather than ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromoteBody {
    /// The `passkey-promotion` packet whose approved authorisation this
    /// promotion spends; the door reads it, and the fact names it.
    packet_id: String,
    /// The gateway-signed [`PromotionTicket`], encoded. Everything the
    /// door believes about the ceremony's two assertions — including
    /// which break-glass key vouched — is read from it.
    ticket: String,
}

/// The ticket, verified against `key` and held to this request: signed,
/// unexpired, and naming exactly this packet, employee and credential.
/// Pure, so each refusal is pinned by a test. 403 for every failure —
/// a forger learns nothing about which part was wrong but the sentence.
pub fn verify_ticket(
    raw: &str,
    key: &[u8],
    now_epoch: u64,
    packet_id: &str,
    employee_id: &str,
    credential_id: &str,
) -> Result<PromotionTicket, Refusal> {
    let forbidden = |m: &str| (StatusCode::FORBIDDEN, m.to_string());
    let t = PromotionTicket::decode(raw.trim(), key, now_epoch).ok_or_else(|| {
        forbidden(
            "no valid promotion ticket — a promotion is asked for only by the gateway, with the \
             ticket it signs after the promoted key AND a break-glass key have asserted on /me; \
             an unsigned, forged or expired ticket promotes nothing",
        )
    })?;
    if t.p != packet_id || t.i != employee_id || t.c != credential_id {
        return Err(forbidden(
            "the promotion ticket was signed for another packet, employee or key than this \
             request names",
        ));
    }
    if t.v.trim().is_empty() || t.n.trim().is_empty() {
        return Err(forbidden(
            "the promotion ticket names no vouching break-glass key, or no nonce",
        ));
    }
    Ok(t)
}

async fn promote_credential(
    State(state): State<PromotionState>,
    CurrentUser(user): CurrentUser,
    Path((employee_id, credential_id)): Path<(String, String)>,
    Json(body): Json<PromoteBody>,
) -> Response {
    match promote_request(&state, &user.id, &employee_id, &credential_id, &body).await {
        Ok(promoted) => Json(json!({
            "credential_id": credential_id,
            "access_tier": OPERATOR_TIER,
            "promoted": promoted,
        }))
        .into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// Every refusal before the write, then the write. `Ok(true)` when this
/// call promoted the key, `Ok(false)` for the harmless replay.
async fn promote_request(
    state: &PromotionState,
    caller: &str,
    employee_id: &str,
    credential_id: &str,
    body: &PromoteBody,
) -> Result<bool, Refusal> {
    if caller != GATEWAY_ACTOR_ID {
        return Err((
            StatusCode::FORBIDDEN,
            "only the gateway promotes a passkey, at the end of a passkey-promotion ceremony \
             on /me — no session may, whatever its role"
                .into(),
        ));
    }
    let Ok(packet_id) = Uuid::parse_str(body.packet_id.trim()) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "a promotion names its passkey-promotion packet by id (packet_id)".into(),
        ));
    };
    if body.ticket.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "a promotion carries the gateway's promotion ticket (ticket)".into(),
        ));
    }
    let Ok(cred) = URL_SAFE_NO_PAD.decode(credential_id) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "credential_id is not base64url-no-pad".into(),
        ));
    };
    // The ticket BEFORE the packet read: a caller without one learns
    // nothing about any packet.
    let key = state.key.get().await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "the gateway's key cannot be read here, so no promotion ticket can be verified"
                .to_string(),
        )
    })?;
    let ticket = verify_ticket(
        &body.ticket,
        &key,
        boss_core::presence::now_epoch(),
        &packet_id.to_string(),
        employee_id,
        credential_id,
    )?;
    let packet = state
        .packets
        .packet(packet_id)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                format!("no packet {packet_id} — a promotion spends an approved packet"),
            )
        })?;
    let approved = judge_packet(&packet, employee_id, credential_id)?;
    flip(
        state,
        employee_id,
        &cred,
        credential_id,
        &approved,
        &packet_id.to_string(),
        &ticket,
    )
    .await
}

/// The flip, the packet it spent and its fact, in one transaction.
async fn flip(
    state: &PromotionState,
    employee_id: &str,
    cred: &[u8],
    credential_id: &str,
    approved: &ApprovedKey,
    packet_id: &str,
    ticket: &PromotionTicket,
) -> Result<bool, Refusal> {
    let vouched_by = ticket.v.trim();
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| internal(e.to_string()))?;
    crate::coverage_guard_pg::lock(&mut tx)
        .await
        .map_err(|e| internal(e.to_string()))?;
    // FOR UPDATE: two racing promotes of one key serialise here, so the
    // second reads `operator` and records nothing.
    let row = sqlx::query(
        "SELECT label, access_tier, registered_at, promoted_by_packet, public_key
           FROM webauthn_credentials
          WHERE employee_id = $1 AND credential_id = $2
          FOR UPDATE",
    )
    .bind(employee_id)
    .bind(cred)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| internal(e.to_string()))?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "no such passkey on this account".to_string(),
        )
    })?;
    // KEY SUBSTITUTION (review of car 293d5dc1). The row is matched by
    // credential id, and a platform-admin can delete a key and register
    // the same credential id with other key material while an approved
    // packet waits. Under the lock, the row must still be the key the
    // owner signed for: its label, its enrolment time and the
    // fingerprint of its public key. A mismatch writes nothing.
    let row_key = ApprovedKey {
        label: row.get("label"),
        registered_at: row.get("registered_at"),
        public_key_sha256: boss_core::presence::public_key_fingerprint(
            &row.get::<Vec<u8>, _>("public_key"),
        ),
    };
    if &row_key != approved {
        let differs: Vec<&str> = [
            ("label", row_key.label != approved.label),
            (
                "registered_at",
                row_key.registered_at != approved.registered_at,
            ),
            (
                "public_key_sha256",
                row_key.public_key_sha256 != approved.public_key_sha256,
            ),
        ]
        .into_iter()
        .filter_map(|(name, differs)| differs.then_some(name))
        .collect();
        return Err((
            StatusCode::CONFLICT,
            format!(
                "the stored key is not the key packet {packet_id} approved: its {} differ — a \
                 key re-registered under the same credential id promotes nothing",
                differs.join(", ")
            ),
        ));
    }
    // THE TICKET IS SPENT ONCE, whatever it then answers — the harmless
    // replay included, which is why this comes before the tier check and
    // that path commits. A copy of a ticket inside its expiry finds its
    // nonce spent. The gateway's own re-ask (D7) is a fresh ceremony, so
    // a fresh nonce.
    let spent = sqlx::query(
        "INSERT INTO passkey_promotion_tickets (nonce, packet_id, employee_id, credential_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (nonce) DO NOTHING",
    )
    .bind(&ticket.n)
    .bind(packet_id)
    .bind(employee_id)
    .bind(credential_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| internal(e.to_string()))?;
    if spent.rows_affected() == 0 {
        return Err((
            StatusCode::CONFLICT,
            "this promotion ticket is already spent — finish the ceremony on /me again for a \
             fresh one"
                .to_string(),
        ));
    }
    if row.get::<String, _>("access_tier") == OPERATOR_TIER {
        let by: Option<String> = row.get("promoted_by_packet");
        return match by {
            Some(by) if by == packet_id => {
                tx.commit().await.map_err(|e| internal(e.to_string()))?;
                Ok(false)
            }
            Some(by) => Err((
                StatusCode::CONFLICT,
                format!(
                    "this key is already operator, promoted by packet {by}; packet {packet_id} \
                     answers for nothing here"
                ),
            )),
            None => Err((
                StatusCode::CONFLICT,
                format!(
                    "this key is already operator with no promotion packet on record; packet \
                     {packet_id} answers for nothing here"
                ),
            )),
        };
    }
    // `true`: local to this transaction, so the mark dies with it.
    sqlx::query("SELECT set_config($1, $2, true)")
        .bind(PROMOTION_MARK)
        .bind(packet_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| internal(e.to_string()))?;
    sqlx::query(
        "UPDATE webauthn_credentials SET access_tier = $3, promoted_by_packet = $4
          WHERE employee_id = $1 AND credential_id = $2",
    )
    .bind(employee_id)
    .bind(cred)
    .bind(OPERATOR_TIER)
    .bind(packet_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| internal(e.to_string()))?;
    let registered_at: DateTime<Utc> = row.get("registered_at");
    // The caller was admitted as exactly this id, so the fact is its act.
    let Ok(gateway) = GATEWAY_ACTOR_ID.parse::<ActorId>();
    let event = EventStamp::new("people", gateway).event(
        PASSKEY_PROMOTED,
        json!({
            "employee_id": employee_id,
            "credential_label": row.get::<String, _>("label"),
            "credential_id": credential_id,
            "registered_at": registered_at,
            "packet_id": packet_id,
            "vouched_by": vouched_by,
        }),
    );
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(|e| internal(e.to_string()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::presence::now_epoch;

    #[tokio::test]
    async fn removing_the_last_operator_key_keeps_its_row_when_user_keys_remain() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ('emp-owner', 'platform-admin', 'active', '2024-01-15')")
            .execute(&db.pool).await.unwrap();
        // The isolated fixture lands the historical operator row under
        // the trigger's actual promotion mark; no production door or
        // permission is weakened to manufacture the after-state.
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(PROMOTION_MARK)
            .bind(PACKET)
            .execute(&mut *tx)
            .await
            .unwrap();
        for (credential, tier) in [
            (b"operator-key".as_slice(), "operator"),
            (b"user-key".as_slice(), "user"),
        ] {
            sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ('emp-owner', $1, $2, $3)")
                .bind(credential).bind(b"fixture-public-key".as_slice()).bind(tier)
                .execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        struct Fixed;
        #[async_trait]
        impl crate::coverage_guard::CoverageRead for Fixed {
            async fn standing(
                &self,
                ids: &[String],
            ) -> Result<crate::coverage_guard::Standing, crate::port::PeopleError> {
                Ok(crate::coverage_guard::Standing {
                    controls: vec![
                        boss_policy_client::coverage::Control::PlatformOwner,
                        boss_policy_client::coverage::Control::OperatorTier,
                    ],
                    rules: vec![],
                    overrides: vec![],
                    employee_ids: ids.iter().cloned().collect(),
                })
            }
        }
        let router = crate::webauthn::webauthn_router_with_coverage(
            db.pool.clone(),
            Arc::new(boss_clock_client::WallClockClient),
            Some(Arc::new(Fixed)),
        );
        let credential = URL_SAFE_NO_PAD.encode(b"operator-key");
        let response = boss_testing::TestRequest::delete(format!(
            "/api/people/emp-owner/webauthn-credentials/{credential}"
        ))
        .as_user(GATEWAY_ACTOR_ID, "platform-admin")
        .send(&router)
        .await;
        response.assert_status(StatusCode::CONFLICT);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE employee_id = 'emp-owner' AND access_tier = 'operator'")
            .fetch_one(&db.pool).await.unwrap();
        assert_eq!(count, 1);
    }

    /// Removing a key reads the coverage basis only when it can take a
    /// holder away (review c3b96c09 F1): one of two user keys beside an
    /// operator key leaves the owner real and operator-tier, so it reads
    /// nothing and is removed; the operator key is judged and refused.
    #[tokio::test]
    async fn only_a_key_removal_that_can_take_a_holder_away_reads_the_basis() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let db = boss_testing::TestDb::new().await;
        sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ('emp-owner', 'platform-admin', 'active', '2024-01-15')")
            .execute(&db.pool).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(PROMOTION_MARK)
            .bind(PACKET)
            .execute(&mut *tx)
            .await
            .unwrap();
        for (credential, tier) in [
            (b"operator-key".as_slice(), "operator"),
            (b"user-key-one".as_slice(), "user"),
            (b"user-key-two".as_slice(), "user"),
        ] {
            sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ('emp-owner', $1, $2, $3)")
                .bind(credential).bind(b"fixture-public-key".as_slice()).bind(tier)
                .execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        struct Counted(Arc<AtomicUsize>);
        #[async_trait]
        impl crate::coverage_guard::CoverageRead for Counted {
            async fn standing(
                &self,
                ids: &[String],
            ) -> Result<crate::coverage_guard::Standing, crate::PeopleError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                G2Fixed.standing(ids).await
            }
        }
        let reads = Arc::new(AtomicUsize::new(0));
        let router = crate::webauthn::webauthn_router_with_coverage(
            db.pool.clone(),
            Arc::new(boss_clock_client::WallClockClient),
            Some(Arc::new(Counted(reads.clone()))),
        );
        let remove = |credential: &'static [u8]| {
            boss_testing::TestRequest::delete(format!(
                "/api/people/emp-owner/webauthn-credentials/{}",
                URL_SAFE_NO_PAD.encode(credential)
            ))
            .as_user(GATEWAY_ACTOR_ID, "platform-admin")
        };
        remove(b"user-key-one")
            .send(&router)
            .await
            .assert_status(StatusCode::NO_CONTENT);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        remove(b"no-such-key")
            .send(&router)
            .await
            .assert_status(StatusCode::NOT_FOUND);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        remove(b"operator-key")
            .send(&router)
            .await
            .assert_status(StatusCode::CONFLICT);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM webauthn_credentials WHERE employee_id = 'emp-owner'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(left, 2);
    }

    use crate::{PeopleError, PeopleRepository};
    use boss_core::{actor::ActorId, publisher::EventStamp};
    // Historical G2 fixtures belong beside the private promotion mark.
    // They do not expose a second operator writer or a live ceremony.
    struct G2Fixed;
    #[async_trait]
    impl crate::coverage_guard::CoverageRead for G2Fixed {
        async fn standing(
            &self,
            ids: &[String],
        ) -> Result<crate::coverage_guard::Standing, crate::PeopleError> {
            Ok(crate::coverage_guard::Standing {
                controls: vec![
                    boss_policy_client::coverage::Control::PlatformOwner,
                    boss_policy_client::coverage::Control::OperatorTier,
                ],
                rules: vec![],
                overrides: vec![],
                employee_ids: ids.iter().cloned().collect(),
            })
        }
    }
    #[tokio::test]
    async fn employee_retirement_and_operator_key_removal_share_one_serialized_basis() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use base64::Engine;
        use tower::ServiceExt;
        struct Concurrent(tokio::sync::Barrier);
        #[async_trait::async_trait]
        impl crate::coverage_guard::CoverageRead for Concurrent {
            async fn standing(
                &self,
                ids: &[String],
            ) -> Result<crate::coverage_guard::Standing, PeopleError> {
                self.0.wait().await;
                G2Fixed.standing(ids).await
            }
        }
        let db = boss_testing::TestDb::new().await;
        // Historical operator rows in this isolated fixture use the existing
        // promotion trigger's mark; no live ceremony or permission is changed.
        let mut fixture = db.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(PROMOTION_MARK)
            .bind(PACKET)
            .execute(&mut *fixture)
            .await
            .unwrap();
        for id in ["emp-retire", "emp-key-removal"] {
            sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ($1, 'platform-admin', 'active', '2024-01-15')")
            .bind(id).execute(&mut *fixture).await.unwrap();
            sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ($1, $2, $3, 'operator')")
            .bind(id).bind(id.as_bytes()).bind(b"fixture-public-key".as_slice()).execute(&mut *fixture).await.unwrap();
        }
        fixture.commit().await.unwrap();
        sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ('emp-key-removal', $1, $2, 'user')")
        .bind(b"remaining-user-key".as_slice()).bind(b"fixture-public-key".as_slice()).execute(&db.pool).await.unwrap();
        let source = std::sync::Arc::new(Concurrent(tokio::sync::Barrier::new(2)));
        let repo = crate::PgPeople::new(db.pool.clone()).with_coverage(source.clone());
        let app = crate::webauthn::webauthn_router_with_coverage(
            db.pool.clone(),
            std::sync::Arc::new(boss_clock_client::WallClockClient),
            Some(source),
        );
        let mut person = repo.employee_by_id("emp-retire").await.unwrap().unwrap();
        person.status = Some("terminated".into());
        let credential =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"emp-key-removal");
        let user = serde_json::to_string(&boss_policy_client::User::service("gateway")).unwrap();
        let request = Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/people/emp-key-removal/webauthn-credentials/{credential}"
            ))
            .header("x-boss-user", user)
            .body(Body::empty())
            .unwrap();
        let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
        let (retirement, deletion) =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(
                    repo.update_employee_at(&person.id, &person, chrono::Utc::now(), &stamp),
                    app.oneshot(request)
                )
            })
            .await
            .expect("the two local mutation kinds must terminate");
        let deletion = deletion.unwrap().status();
        assert_eq!(
            usize::from(retirement.is_ok()) + usize::from(deletion == StatusCode::NO_CONTENT),
            1
        );
        assert!(
            matches!(retirement, Err(PeopleError::Conflict(_))) || deletion == StatusCode::CONFLICT
        );
        let holders: i64 = sqlx::query_scalar("SELECT count(*) FROM employees e JOIN webauthn_credentials k ON e.id = k.employee_id WHERE e.status = 'active' AND k.access_tier = 'operator'")
        .fetch_one(&db.pool).await.unwrap();
        assert_eq!(holders, 1);
        let facts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE kind = 'people.employee.updated'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(facts, i64::from(retirement.is_ok()));
    }

    #[tokio::test]
    async fn postgres_enrollment_and_later_revocation_of_a_nonholder_preserve_owner_and_key() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ('emp-existing-owner', 'platform-admin', 'active', '2024-01-15')")
        .execute(&db.pool).await.unwrap();
        let mut fixture = db.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(PROMOTION_MARK)
            .bind(PACKET)
            .execute(&mut *fixture)
            .await
            .unwrap();
        sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ('emp-existing-owner', $1, $2, 'operator')")
        .bind(b"existing-owner-key".as_slice()).bind(b"existing-owner-public".as_slice()).execute(&mut *fixture).await.unwrap();
        fixture.commit().await.unwrap();
        let repo =
            crate::PgPeople::new(db.pool.clone()).with_coverage(std::sync::Arc::new(G2Fixed));
        let before = repo
            .employee_by_id("emp-existing-owner")
            .await
            .unwrap()
            .unwrap();
        let newcomer = crate::Employee {
            id: "emp-native-nonholder".into(),
            name: Some("Test Employee emp-native-nonholder".into()),
            email: Some("emp-native-nonholder@boss.io".into()),
            role: Some("service-tech".into()),
            department: Some("service".into()),
            skill_level: Some(3),
            skills: vec![],
            hire_date: chrono::NaiveDate::from_ymd_opt(2024, 1, 15),
            location: None,
            manager_id: None,
            employment_type: Some("full-time".into()),
            status: Some("active".into()),
            certifications: vec![],
            annual_salary_cents: None,
        };
        let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
        let now = chrono::Utc::now();
        repo.create_employee_at(&newcomer, now, &stamp)
            .await
            .unwrap();
        // Enroll a real, ordinary user key through the existing native key door.
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use base64::Engine;
        use tower::ServiceExt;
        let app = crate::webauthn::webauthn_router_with_coverage(
            db.pool.clone(),
            std::sync::Arc::new(boss_clock_client::WallClockClient),
            Some(std::sync::Arc::new(G2Fixed)),
        );
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let body = serde_json::json!({"credential_id":b64.encode(b"newcomer-key"), "public_key":b64.encode(b"newcomer-public")});
        let user = serde_json::to_string(&boss_policy_client::User::service("gateway")).unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/people/{}/webauthn-credentials", newcomer.id))
                    .header("content-type", "application/json")
                    .header("x-boss-user", user)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let ordinary: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webauthn_credentials WHERE employee_id = $1 AND access_tier = 'user'",
    )
    .bind(&newcomer.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
        assert_eq!(ordinary, 1);
        repo.delete_employee_at(&newcomer.id, now, &stamp)
            .await
            .unwrap();
        assert!(repo.employee_by_id(&newcomer.id).await.unwrap().is_none());
        assert_eq!(repo.employee_by_id(&before.id).await.unwrap(), Some(before));
        let original: Vec<(Vec<u8>, Vec<u8>, String)> = sqlx::query_as("SELECT credential_id, public_key, access_tier FROM webauthn_credentials WHERE employee_id = 'emp-existing-owner'")
        .fetch_all(&db.pool).await.unwrap();
        assert_eq!(
            original,
            vec![(
                b"existing-owner-key".to_vec(),
                b"existing-owner-public".to_vec(),
                "operator".into()
            )]
        );
        let facts: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM event_outbox WHERE kind LIKE 'people.employee.%' ORDER BY id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            facts,
            vec!["people.employee.created", "people.employee.deleted"]
        );
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE employee_id = $1")
                .bind(&newcomer.id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);
    }

    // The judge's own tests moved with it to boss_core::passkey_promotion.

    const PACKET: &str = "7a1d2c3b-0000-4000-8000-00000000c0de";
    const OWNER: &str = "emp-owner";
    const CRED: &str = "cHJvbW90ZS1jcmVkZW50aWFsLWlk";
    const KEY: &[u8] = b"people-promotion-ticket-test-key";

    fn ticket() -> PromotionTicket {
        PromotionTicket {
            p: PACKET.into(),
            i: OWNER.into(),
            c: CRED.into(),
            v: "primary".into(),
            n: "n0nce".into(),
            e: now_epoch() + 60,
        }
    }

    fn verify(raw: &str) -> Result<PromotionTicket, Refusal> {
        verify_ticket(raw, KEY, now_epoch(), PACKET, OWNER, CRED)
    }

    #[test]
    fn a_ticket_the_gateway_signed_for_this_request_verifies() {
        let t = ticket();
        assert_eq!(verify(&t.encode(KEY).unwrap()).unwrap(), t);
    }

    /// Everything a forger of `x-boss-user` could send instead, and a
    /// real ticket held to the wrong request: each is 403, none verifies.
    #[test]
    fn no_other_ticket_verifies() {
        let mut cases: Vec<(&str, String)> = vec![
            ("nothing", String::new()),
            ("free text", "primary".into()),
            (
                "the fields as json",
                serde_json::to_string(&ticket()).unwrap(),
            ),
            (
                "signed with another key",
                ticket().encode(b"not-the-session-key").unwrap(),
            ),
        ];
        let expired = PromotionTicket {
            e: now_epoch() - 1,
            ..ticket()
        };
        cases.push(("expired", expired.encode(KEY).unwrap()));
        for (what, t) in [
            (
                "another packet",
                PromotionTicket {
                    p: "7a1d2c3b-0000-4000-8000-0000000c0de2".into(),
                    ..ticket()
                },
            ),
            (
                "another employee",
                PromotionTicket {
                    i: "emp-other".into(),
                    ..ticket()
                },
            ),
            (
                "another key",
                PromotionTicket {
                    c: "b3RoZXIta2V5".into(),
                    ..ticket()
                },
            ),
            (
                "no voucher",
                PromotionTicket {
                    v: " ".into(),
                    ..ticket()
                },
            ),
            (
                "no nonce",
                PromotionTicket {
                    n: String::new(),
                    ..ticket()
                },
            ),
        ] {
            cases.push((what, t.encode(KEY).unwrap()));
        }
        for (what, raw) in cases {
            let (status, _) = verify(&raw).expect_err(what);
            assert_eq!(status, StatusCode::FORBIDDEN, "{what}");
        }
    }

    /// MOUNTED WITH ITS TICKET (car 2 of design 2cb6256f; this replaces
    /// car 1's `nothing_mounts_the_promote_door_until_the_ticket_lands`).
    /// The service binary mounts the door exactly once, reading the
    /// packet from the jobs API and the ticket key from the gateway's key
    /// file — and nothing else in the tree outside this crate's tests
    /// mounts it, so there is one door and it is the ticketed one.
    #[test]
    fn the_service_mounts_the_promote_door_with_its_ticket_key() {
        let root = boss_testing::repo_root();
        let callers: Vec<String> = ["crates", "infra", "apps", "examples", "libs"]
            .into_iter()
            .flat_map(|dir| {
                code_lines_containing(&root.join(dir), concat!("promotion", "_router("))
            })
            .filter(|l| {
                !l.contains("crates/modules/boss-people/tests/")
                    && !l.contains("pub fn promotion_router(")
            })
            .collect();
        assert!(
            callers.len() == 1
                && callers[0].contains("crates/modules/boss-people/src/bin/boss_people_api.rs:"),
            "the people service mounts the promote door, once, and nothing else does: \
             {callers:#?}"
        );
        let bin = std::fs::read_to_string(
            root.join("crates/modules/boss-people/src/bin/boss_people_api.rs"),
        )
        .unwrap();
        assert!(
            bin.contains("TicketKey::from_env()") && bin.contains("JobsApiPromotionPackets::new("),
            "the mounted door verifies tickets with the gateway's key and reads the live packet"
        );
    }

    /// THE ONE WRITER (design 2cb6256f D5). Every statement in this crate
    /// that SETs a credential's `access_tier` lives in this file, so the
    /// next writer of the operator tier has to come through this test.
    /// Tests included; comments excluded.
    #[test]
    fn only_the_promotion_sets_a_credential_tier() {
        // Spelled in two halves so this test's own text is not a match.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let writers = code_lines_containing(&src, concat!("SET access", "_tier"));
        assert!(
            writers.iter().all(|w| w.contains("/passkey_promotion.rs:")),
            "a credential's tier is set only by the gateway-only promote \
             (design 2cb6256f D5); found: {writers:#?}"
        );
        assert_eq!(
            writers.len(),
            1,
            "the promotion's own UPDATE is the one writer: {writers:#?}"
        );
    }

    /// The table's trigger lets a row become `operator` only inside a
    /// transaction carrying the promotion mark, so the WHOLE TREE spells
    /// that mark twice: the const this file binds, and the trigger that
    /// reads it (review F4 of car 1d9970d1, widened by finding 4 of this
    /// car's review). A third spelling anywhere — a script, a seed, another
    /// crate — would be a second writer of the operator tier.
    #[test]
    fn only_the_promotion_and_its_trigger_spell_the_mark() {
        let root = boss_testing::repo_root();
        let mut spellings = Vec::new();
        for dir in ["crates", "infra", "apps", "examples", "libs"] {
            spellings.extend(code_lines_containing(
                &root.join(dir),
                concat!("boss.passkey", "_promotion"),
            ));
        }
        let (rust, sql): (Vec<&String>, Vec<&String>) =
            spellings.iter().partition(|s| s.contains(".rs:"));
        assert!(
            rust.len() == 1
                && rust[0].contains("crates/modules/boss-people/src/passkey_promotion.rs:")
                && rust[0].contains("const PROMOTION_MARK"),
            "the mark is the one const in passkey_promotion.rs: {spellings:#?}"
        );
        assert!(
            sql.len() == 1
                && sql[0].contains(
                    "infra/postgres/schema/20260928170011-only-the-promotion-writes-an-operator-passkey.sql:"
                ),
            "the only other spelling is the trigger that reads it: {spellings:#?}"
        );
    }

    /// `path:line: text` for every line under `dir` whose code (comments
    /// stripped for .rs, .sql and shell) contains `needle`,
    /// case-insensitively. Skips build output and installed packages.
    fn code_lines_containing(dir: &std::path::Path, needle: &str) -> Vec<String> {
        let needle = needle.to_ascii_uppercase();
        let mut found = Vec::new();
        let mut dirs = vec![dir.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir() {
                    if !matches!(name, "target" | "node_modules" | ".git" | "dist") {
                        dirs.push(path);
                    }
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let comment = match ext {
                    "rs" | "ts" | "js" | "svelte" => "//",
                    "sql" => "--",
                    "sh" | "toml" | "yaml" | "yml" | "py" => "#",
                    _ => continue,
                };
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (n, line) in text.lines().enumerate() {
                    let code = line.split(comment).next().unwrap_or("");
                    if code.to_ascii_uppercase().contains(&needle) {
                        found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        found
    }
}
