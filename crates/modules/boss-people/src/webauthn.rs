//! WebAuthn ceremony storage — credentials and single-use challenges.
//!
//! The presence design (docs/design/presence.md; BOSS-native call
//! accepted on packet 7218c3f1) splits the ceremony across two
//! services: the GATEWAY runs the cryptographic ceremony against the
//! browser, and this module is the storage it leans on — the
//! credential registry (who owns which authenticator) and the
//! challenge ledger (what was minted, for which step content, and
//! whether it has been spent).
//!
//! These endpoints are internal machinery: the gateway's ceremony is
//! the only legitimate caller. They cannot rely on that being true —
//! the gateway also proxies `/api/people/{*rest}` for the SPA, so any
//! session could reach these paths — and a credential row planted for
//! someone else is an account takeover. So every handler admits ONE
//! caller id, the gateway's own actor
//! ([`boss_core::actor::GATEWAY_ACTOR_ID`], which
//! `boss_gateway::passkey::sign_as_gateway` signs every ceremony
//! storage call with), and refuses every other id whatever its role —
//! the rule the promote door (`passkey_promotion.rs`) already keeps.
//! Humans reach this storage only through the gateway's
//! `/api/auth/passkey/*` ceremony, which binds the session to its
//! employee before it gets here; that includes removing a lost key
//! (`DELETE /api/auth/passkey/credentials/{id}`).
//!
//! The gate used to be the `platform-admin` ROLE, and this doc said
//! every ordinary proxied session was refused. That was false: the
//! proxy forwards the session's own role, so the owner's user-tier
//! browser cookie — and every agent, which signs as platform-admin —
//! could store, remove or rekey a passkey and mint or spend challenges
//! around the ceremony (backlog e199c02d, 2026-09-28).
//!
//! What the id check does NOT close: `strip_boss_headers` stops a
//! browser from claiming the gateway's id, but a caller on the LAN
//! machine door can still assert it, because nothing behind the
//! gateway verifies an `x-boss-user` header until the machine token is
//! enforced (backlog 2710c8fc) — the same residual the promote door
//! names. And a storage write still emits no event of its own
//! (backlog 4b97bc20).
//!
//! ONE READ IS NOT THE GATEWAY'S: the key counts by tier
//! (`boss_policy_client::coverage::TIER_COUNTS_PATH`, design 1c4e42e1),
//! which the policy service's coverage read needs and which carries no
//! key material — see `machinery_read_gate`.
//!
//! Bytes (credential ids, public keys, challenges) travel as
//! base64url-no-pad strings — the same alphabet the browser's
//! WebAuthn API speaks — and are stored as BYTEA.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::Row;

use boss_clock_client::{ClockClient, now_from};
use boss_policy_client::CurrentUser;

#[derive(Clone)]
pub struct WebauthnState {
    pub pool: Arc<PgPool>,
    pub clock: Arc<dyn ClockClient>,
    pub coverage: Option<Arc<dyn crate::coverage_guard::CoverageRead>>,
}

/// Small error value for helper Results (clippy::result_large_err —
/// Response is a big payload and refusals are cold paths); converted
/// at the handler boundary.
type ErrResp = (StatusCode, &'static str);

/// The gateway's ceremony only — judged by caller ID, never role (see
/// the module doc; backlog e199c02d).
fn gateway_gate(user: &boss_policy::User) -> Result<(), ErrResp> {
    if user.id == boss_core::actor::GATEWAY_ACTOR_ID {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "only the gateway reads or writes passkey storage, inside its \
             /api/auth/passkey ceremony — no session may, whatever its role",
        ))
    }
}

/// The one read of this storage that is NOT the gateway's: how many
/// keys each employee holds at each tier, and nothing else (design
/// 1c4e42e1, backlog 47aed706). The policy service's coverage read asks
/// it, because "a real person is an active employee with a bound
/// passkey" and "the operator tier is a platform-admin with an
/// operator-tier key" are both facts of this table — and every other
/// handler here answers only the gateway (e199c02d). No credential id,
/// public key, label or timestamp leaves: a count cannot plant, rekey
/// or replay a key. Machinery reads it, as machinery reads the other
/// registries (`boss-jobs` `trust::can_read`): a caller at the operator
/// or auditor tier. Two callers besides machinery pass, and both are
/// named so this reads true (review of this car, L4): the platform
/// owner's ELEVATED session, which the gateway's `/api/people/*` proxy
/// signs at `access_tier=operator` — harmless, since only the owner can
/// elevate and these are counts; and, until the machine token is
/// enforced (2710c8fc), any caller on the network that can reach the
/// port and simply claims operator in its `x-boss-user` header — the
/// same residual the gateway gate names for the gateway's id.
fn machinery_read_gate(user: &boss_policy::User) -> Result<(), ErrResp> {
    use boss_policy_client::AccessTier;
    if matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "the passkey tier counts are read by machinery at the operator or auditor tier",
        ))
    }
}

pub fn webauthn_router(pool: PgPool, clock: Arc<dyn ClockClient>) -> Router {
    webauthn_router_with_coverage(pool, clock, None)
}

pub fn webauthn_router_with_coverage(
    pool: PgPool,
    clock: Arc<dyn ClockClient>,
    coverage: Option<Arc<dyn crate::coverage_guard::CoverageRead>>,
) -> Router {
    let state = WebauthnState {
        pool: Arc::new(pool),
        clock,
        coverage,
    };
    Router::new()
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(list_credentials).post(register_credential),
        )
        .route(
            "/api/people/{id}/webauthn-credentials/{credential_id}",
            delete(remove_credential),
        )
        .route(
            "/api/people/webauthn-credentials/used",
            post(record_credential_use),
        )
        // A literal, because the read pin reads paths from this source;
        // it is `boss_policy_client::coverage::TIER_COUNTS_PATH`, the
        // spelling the policy service reads, and the ceremony test asks
        // it by that const — a drift answers 404 there.
        .route("/api/people/webauthn-credentials/tiers", get(tier_counts))
        .route("/api/people/presence-challenges", post(mint_challenge))
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(consume_challenge),
        )
        .with_state(state)
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(field: &str, s: &str) -> Result<Vec<u8>, (StatusCode, String)> {
    URL_SAFE_NO_PAD.decode(s).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("{field} is not base64url-no-pad"),
        )
    })
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// The body names no tier, and a body that tries is refused (422).
/// It used to carry `access_tier`, and the handler stored whatever
/// `operator|user` it was sent — while its only gate was the
/// platform-admin ROLE, which the owner's user-tier browser session
/// carries through the gateway's `/api/people` proxy. So that cookie
/// could store an operator-tier key of its own and elevate with it
/// (backlog 1d9970d1: H1 of car 0bde9b99's review, one layer below the
/// ceremony). Every credential this door stores is `user`; an operator
/// key is a separate, recorded promotion, never a field here.
/// `deny_unknown_fields` makes the refusal loud, so a caller that still
/// believes it can choose finds out rather than being quietly ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterCredentialBody {
    credential_id: String,
    public_key: String,
    #[serde(default = "default_label")]
    label: String,
}

fn default_label() -> String {
    "default".into()
}

#[derive(Serialize)]
struct CredentialOut {
    credential_id: String,
    public_key: String,
    sign_count: i32,
    label: String,
    access_tier: String,
    registered_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

/// One employee's key counts by tier.
#[derive(Serialize)]
struct TierCounts {
    employee_id: String,
    user: i64,
    operator: i64,
}

/// Every employee holding at least one key, with its keys counted by
/// tier — `{data, total}`, in employee order.
async fn tier_counts(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if let Err(r) = machinery_read_gate(&user) {
        return r.into_response();
    }
    let rows = sqlx::query(
        "SELECT employee_id,
                count(*) FILTER (WHERE access_tier = 'user') AS user_keys,
                count(*) FILTER (WHERE access_tier = 'operator') AS operator_keys
           FROM webauthn_credentials
          GROUP BY employee_id
          ORDER BY employee_id",
    )
    .fetch_all(state.pool.as_ref())
    .await;
    match rows {
        Ok(rows) => {
            let out: Vec<TierCounts> = rows
                .iter()
                .map(|r| TierCounts {
                    employee_id: r.get("employee_id"),
                    user: r.get("user_keys"),
                    operator: r.get("operator_keys"),
                })
                .collect();
            let total = out.len();
            Json(serde_json::json!({ "data": out, "total": total })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_credentials(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    let rows = sqlx::query(
        "SELECT credential_id, public_key, sign_count, label, access_tier,
                registered_at, last_used_at
           FROM webauthn_credentials
          WHERE employee_id = $1
          ORDER BY registered_at",
    )
    .bind(&employee_id)
    .fetch_all(state.pool.as_ref())
    .await;
    match rows {
        Ok(rows) => {
            let out: Vec<CredentialOut> = rows
                .iter()
                .map(|r| CredentialOut {
                    credential_id: b64(&r.get::<Vec<u8>, _>("credential_id")),
                    public_key: b64(&r.get::<Vec<u8>, _>("public_key")),
                    sign_count: r.get("sign_count"),
                    label: r.get("label"),
                    access_tier: r.get("access_tier"),
                    registered_at: r.get("registered_at"),
                    last_used_at: r.get("last_used_at"),
                })
                .collect();
            Json(out).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn register_credential(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
    Json(body): Json<RegisterCredentialBody>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    let cred_id = match unb64("credential_id", &body.credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let pub_key = match unb64("public_key", &body.public_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Err(e) = crate::coverage_guard_pg::lock(&mut tx).await {
        return coverage_error(e);
    }
    // The tier is a literal in the statement, not a bind: nothing a
    // caller sends can reach the column (backlog 1d9970d1).
    let res = sqlx::query(
        "INSERT INTO webauthn_credentials
           (employee_id, credential_id, public_key, label, access_tier, registered_at)
         VALUES ($1, $2, $3, $4, 'user', $5)",
    )
    .bind(&employee_id)
    .bind(&cred_id)
    .bind(&pub_key)
    .bind(&body.label)
    .bind(now)
    .execute(&mut *tx)
    .await;
    match res {
        Ok(_) => match tx.commit().await {
            Ok(()) => StatusCode::CREATED.into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => (
            StatusCode::CONFLICT,
            "credential_id already registered — a credential binds to one authenticator forever",
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct CredentialUsedBody {
    credential_id: String,
    sign_count: i32,
}

/// `DELETE /api/people/{id}/webauthn-credentials/{credential_id}` —
/// a lost or retired authenticator can go; the LAST one stays.
///
/// David, feedback 16414d99: "Important so users can add a backup key,
/// but don't let them delete all their keys." A person with no passkey
/// cannot pass a presence-gated step, so the surface that let them
/// remove the last one would be the one that locked them out. The
/// refusal is a 409 in the user's terms — add a backup, then remove —
/// and a credential that is not this employee's is a 404, never a
/// silent 204 that reads as "removed". One statement does the count
/// and the delete together, so two removals racing cannot both see
/// "two keys" and leave none.
async fn remove_credential(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path((employee_id, credential_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    let cred = match unb64("credential_id", &credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    // The basis is read before the transaction, and only when removing
    // this key can take a holder away: the last key of a real person, or
    // their operator-tier standing (`coverage_guard::can_orphan`). One of
    // several user keys reads nothing (review c3b96c09 F1, 2026-10-06).
    let standing = if let Some(source) = &state.coverage {
        let local = match crate::coverage_guard_pg::read_unlocked(state.pool.as_ref()).await {
            Ok(local) => local,
            Err(e) => return coverage_error(e),
        };
        let tier = match stored_tier(state.pool.as_ref(), &employee_id, &cred).await {
            Ok(tier) => tier,
            Err(e) => return coverage_error(e),
        };
        // No such key, or one the forecast cannot place: the transaction
        // below answers, on the rows it reads under the lock.
        match tier
            .and_then(|tier| crate::coverage_guard::keys_without(&local.keys, &employee_id, &tier))
        {
            Some(after) => {
                match crate::coverage_guard::standing_for(
                    source.as_ref(),
                    &local.roster,
                    &local.keys,
                    &local.roster,
                    &after,
                )
                .await
                {
                    Ok(standing) => standing,
                    Err(e) => return coverage_error(e),
                }
            }
            None => None,
        }
    } else {
        None
    };
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let local = match crate::coverage_guard_pg::read_locked(&mut tx).await {
        Ok(local) => local,
        Err(e) => return coverage_error(e),
    };
    if state.coverage.is_some() {
        let tier = match stored_tier(&mut *tx, &employee_id, &cred).await {
            Ok(tier) => tier,
            Err(e) => return coverage_error(e),
        };
        let Some(tier) = tier else {
            return (StatusCode::NOT_FOUND, "no such passkey on this account").into_response();
        };
        let Some(after) = crate::coverage_guard::keys_without(&local.keys, &employee_id, &tier)
        else {
            return coverage_error(crate::port::PeopleError::Unavailable(
                "the selected key is absent from the complete key snapshot".into(),
            ));
        };
        if let Err(e) = crate::coverage_guard::judge_at_commit(
            standing.as_ref(),
            &local.roster,
            &local.keys,
            &local.roster,
            &after,
        ) {
            return coverage_error(e);
        }
    }
    let deleted = sqlx::query(
        "DELETE FROM webauthn_credentials
          WHERE employee_id = $1 AND credential_id = $2
            AND (SELECT count(*) FROM webauthn_credentials WHERE employee_id = $1) > 1",
    )
    .bind(&employee_id)
    .bind(&cred)
    .execute(&mut *tx)
    .await;
    match deleted {
        Ok(r) if r.rows_affected() == 1 => match tx.commit().await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Ok(_) => {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM webauthn_credentials
                                WHERE employee_id = $1 AND credential_id = $2)",
            )
            .bind(&employee_id)
            .bind(&cred)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(false);
            if exists {
                (
                    StatusCode::CONFLICT,
                    "this is the last passkey on the account and it stays — add a backup \
                     key first, then remove this one",
                )
                    .into_response()
            } else {
                (StatusCode::NOT_FOUND, "no such passkey on this account").into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The guard's answer in the People door's own terms — one mapping,
/// where a 503 is also logged with its reason (`http.rs`).
fn coverage_error(error: crate::port::PeopleError) -> Response {
    crate::http::people_error_response(error)
}

/// The tier one stored key carries, or `None` when the account holds no
/// such key. A tier the guard cannot read is not judged as some tier.
async fn stored_tier<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    employee_id: &str,
    credential: &[u8],
) -> Result<Option<boss_policy_client::AccessTier>, crate::port::PeopleError> {
    use crate::port::PeopleError;
    let tier: Option<String> = sqlx::query_scalar(
        "SELECT access_tier FROM webauthn_credentials WHERE employee_id = $1 AND credential_id = $2",
    )
    .bind(employee_id)
    .bind(credential)
    .fetch_optional(executor)
    .await
    .map_err(|e| PeopleError::Storage(e.to_string()))?;
    tier.map(|tier| {
        serde_json::from_value(serde_json::Value::String(tier))
            .map_err(|_| PeopleError::Unavailable("stored key tier cannot be judged".into()))
    })
    .transpose()
}

async fn record_credential_use(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CredentialUsedBody>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    let cred_id = match unb64("credential_id", &body.credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let res = sqlx::query(
        "UPDATE webauthn_credentials
            SET sign_count = $2, last_used_at = $3
          WHERE credential_id = $1",
    )
    .bind(&cred_id)
    .bind(body.sign_count)
    .bind(now)
    .execute(state.pool.as_ref())
    .await;
    match res {
        Ok(r) if r.rows_affected() == 0 => {
            (StatusCode::NOT_FOUND, "unknown credential").into_response()
        }
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Challenges
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MintChallengeBody {
    id: String,
    employee_id: String,
    challenge: String,
    flow: String,
    #[serde(default)]
    step_id: Option<String>,
    #[serde(default)]
    shape_hash: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    /// Seconds until expiry; default 300 (the table's original 5 min).
    #[serde(default)]
    ttl_seconds: Option<i64>,
}

#[derive(Serialize)]
struct ChallengeOut {
    id: String,
    employee_id: String,
    challenge: String,
    flow: String,
    step_id: Option<String>,
    shape_hash: Option<String>,
    nonce: Option<String>,
}

async fn mint_challenge(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<MintChallengeBody>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    if !["register", "authenticate", "presence"].contains(&body.flow.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            "flow must be register|authenticate|presence",
        )
            .into_response();
    }
    let challenge = match unb64("challenge", &body.challenge) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let expires = now + chrono::Duration::seconds(body.ttl_seconds.unwrap_or(300));
    let res = sqlx::query(
        "INSERT INTO webauthn_challenges
           (id, employee_id, challenge, flow, step_id, shape_hash, nonce,
            created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&body.id)
    .bind(&body.employee_id)
    .bind(&challenge)
    .bind(&body.flow)
    .bind(&body.step_id)
    .bind(&body.shape_hash)
    .bind(&body.nonce)
    .bind(now)
    .bind(expires)
    .execute(state.pool.as_ref())
    .await;
    match res {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            (StatusCode::CONFLICT, "challenge id already minted").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Atomic single-use consumption: the UPDATE claims the row only if it
/// is unspent and unexpired, so two racing verifications cannot both
/// succeed. A spent or expired row answers 410 (it existed; it is no
/// longer redeemable), an unknown id 404 — the caller can tell "replay
/// or too slow" from "never minted".
async fn consume_challenge(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = gateway_gate(&user) {
        return r.into_response();
    }
    let now = now_from(&state.clock).await;
    let row = sqlx::query(
        "UPDATE webauthn_challenges
            SET used_at = $2
          WHERE id = $1 AND used_at IS NULL AND expires_at > $2
      RETURNING employee_id, challenge, flow, step_id, shape_hash, nonce",
    )
    .bind(&id)
    .bind(now)
    .fetch_optional(state.pool.as_ref())
    .await;
    match row {
        Ok(Some(r)) => Json(ChallengeOut {
            id,
            employee_id: r
                .get::<Option<String>, _>("employee_id")
                .unwrap_or_default(),
            challenge: b64(&r.get::<Vec<u8>, _>("challenge")),
            flow: r.get("flow"),
            step_id: r.get("step_id"),
            shape_hash: r.get("shape_hash"),
            nonce: r.get("nonce"),
        })
        .into_response(),
        Ok(None) => {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM webauthn_challenges WHERE id = $1",
            )
            .bind(&id)
            .fetch_one(state.pool.as_ref())
            .await
            .unwrap_or(0);
            if exists > 0 {
                (StatusCode::GONE, "challenge spent or expired").into_response()
            } else {
                (StatusCode::NOT_FOUND, "unknown challenge").into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
