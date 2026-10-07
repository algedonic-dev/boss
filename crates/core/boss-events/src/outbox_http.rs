//! The dead-letter door on boss-events-api (backlog e22b692e,
//! adversarial review H1):
//!
//! - `POST /api/events/outbox/{id}/redeliver` — put an open dead letter
//!   back on the relay's queue ([`redeliver_dead_letter`]);
//! - `POST /api/events/outbox/{id}/resolve`, body `{"reason": "..."}` —
//!   close it off the bus by decision ([`resolve_dead_letter`]).
//!
//! WHY A DOOR AND NOT A DATABASE URL. The first cut of `boss events
//! redeliver` connected to Postgres itself and signed the act with a
//! self-asserted `BOSS_ACTOR`, past any policy check — the shape retired
//! on 2026-09-27, when a tenant stamp written that way never reached the
//! live log (42da8bd2) and the cadence verbs dropped their pool for
//! reading the wrong database (a516f1f1). The act now happens where the
//! table lives: boss-events-api owns `event_outbox`, the route table
//! already sends `/api/events/*` here, and the door decides three things
//! the verb cannot be trusted with — who may act (Operator tier, and
//! nothing wider: this rewrites what the relay publishes), who is
//! credited (the signed caller's `user.id`, never a body field), and
//! what the answer says (the row and the staged fact read back from the
//! database after the act commits, never the operator's own words
//! echoed, and never the refused payload).
//!
//! Status: 200 with `{letter, row}`; 403 below Operator tier; 404 no
//! such row; 409 a row that is not an open dead letter, or over the
//! staging bound; 400 a resolution with no reason. A refusal changes
//! nothing.
//!
//! And one read, `GET /api/events/outbox/stats[?window_hours=N]`
//! (backlog 72c50b8b): what the relay still owes — the undrained count,
//! its oldest row and how far behind the newest write that row sits
//! ([`undrained`]), pending rows, open dead letters — and its delivery
//! lag p50/p95/max over the window up to the newest delivery
//! ([`relay_lag`], default 24h, 1..=720). The outbox keeps delivered
//! rows only for its retention window (a week by default, backlog
//! eec0c1f3), so a wider request answers that week: `lag.covered_hours`
//! names the span the sample actually reaches (review afdc2d5d, N2).
//! Until it, no door read
//! `event_outbox`, so how
//! long a committed write waits before audit_log holds it was
//! unmeasurable. The door is `/api/events/stats`'s own — operator or
//! auditor tier, or a role with global read — because it is the same
//! kind of fact about the same log.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_core::actor::ActorId;
use boss_policy_client::{AccessTier, CurrentUser, User};
use serde::Deserialize;
use sqlx::PgPool;

use crate::outbox::{
    DeadLetter, DeadLetterError, REDELIVERED_KIND, RESOLVED_KIND, RelayLag, Undrained,
    dead_lettered_count, pending_count, read_back, redeliver_dead_letter, relay_lag,
    resolve_dead_letter, undrained,
};

#[derive(Clone)]
struct OutboxHttpState {
    pool: Arc<PgPool>,
    role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
}

pub fn outbox_router(pool: PgPool) -> Router {
    outbox_router_with_reports(pool, None)
}

pub fn outbox_router_with_reports(
    pool: PgPool,
    role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
) -> Router {
    Router::new()
        .route("/api/events/outbox/{id}/redeliver", post(redeliver))
        .route("/api/events/outbox/{id}/resolve", post(resolve))
        .route("/api/events/outbox/stats", get(stats))
        .with_state(OutboxHttpState {
            pool: Arc::new(pool),
            role_guards,
        })
}

#[derive(Debug, Deserialize)]
pub struct ResolveBody {
    pub reason: Option<String>,
}

/// The signed caller as the act's actor, at the Operator tier only;
/// `None` is refused by [`forbidden`]. `ActorId`'s parse is infallible.
fn operator(user: &User) -> Option<ActorId> {
    matches!(user.access_tier, AccessTier::Operator).then(|| {
        user.id
            .parse::<ActorId>()
            .unwrap_or_else(|never| match never {})
    })
}

fn forbidden(user: &User) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "acting on a dead-lettered event is operator machinery",
            "actor_id": user.id,
            "why": "redelivery and resolution change what the relay publishes to every \
                    subscriber, so the door admits the Operator tier and nothing wider",
        })),
    )
        .into_response()
}

fn refusal(e: DeadLetterError) -> Response {
    let status = match &e {
        DeadLetterError::NotFound(_) => StatusCode::NOT_FOUND,
        DeadLetterError::Refused(_) => StatusCode::CONFLICT,
        DeadLetterError::Invalid(_) => StatusCode::BAD_REQUEST,
        DeadLetterError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

/// The act committed; answer with what the database now holds.
async fn answer(pool: &PgPool, letter: DeadLetter, act_kind: &str) -> Response {
    match read_back(pool, letter.outbox_id, act_kind).await {
        Ok(row) => Json(serde_json::json!({ "letter": letter, "row": row })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!(
                    "the act on outbox row {} COMMITTED, and reading it back failed: {e}",
                    letter.outbox_id
                ),
                "letter": letter,
            })),
        )
            .into_response(),
    }
}

async fn redeliver(
    State(state): State<OutboxHttpState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
) -> Response {
    let pool = &state.pool;
    let Some(actor) = operator(&user) else {
        return forbidden(&user);
    };
    match redeliver_dead_letter(pool, id, actor).await {
        Ok(letter) => answer(pool, letter, REDELIVERED_KIND).await,
        Err(e) => refusal(e),
    }
}

async fn resolve(
    State(state): State<OutboxHttpState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<ResolveBody>,
) -> Response {
    let pool = &state.pool;
    let Some(actor) = operator(&user) else {
        return forbidden(&user);
    };
    let reason = body.reason.unwrap_or_default();
    match resolve_dead_letter(pool, id, &reason, actor).await {
        Ok(letter) => answer(pool, letter, RESOLVED_KIND).await,
        Err(e) => refusal(e),
    }
}

/// The lag window when the caller names none, and the widest it may name.
const DEFAULT_WINDOW_HOURS: i32 = 24;
const MAX_WINDOW_HOURS: i32 = 720;

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    pub window_hours: Option<i32>,
}

/// What `GET /api/events/outbox/stats` answers.
#[derive(Debug, serde::Serialize)]
pub struct OutboxStats {
    /// Committed writes audit_log does not hold yet.
    pub undrained: Undrained,
    /// Rows the relay still owes a move. `pending - undrained.count` is
    /// the rows already in audit_log but not yet published and stamped.
    pub pending: i64,
    /// Rows set aside and not yet resolved — not lag; the relay is done.
    pub dead_lettered_open: i64,
    /// Delivery lag over the requested window; `lag.covered_hours` is
    /// the window the retained rows actually span.
    pub lag: RelayLag,
}

async fn stats(
    State(state): State<OutboxHttpState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<StatsQuery>,
) -> Response {
    let pool = &state.pool;
    if !crate::tail_http::observed_audit_read(
        &user,
        state.role_guards.as_deref(),
        "outbox-stats-read",
    ) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }
    let window_hours = q.window_hours.unwrap_or(DEFAULT_WINDOW_HOURS);
    if !(1..=MAX_WINDOW_HOURS).contains(&window_hours) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "window_hours must be 1..={MAX_WINDOW_HOURS}, got {window_hours}"
                ),
            })),
        )
            .into_response();
    }
    let read = async {
        Ok::<_, String>(OutboxStats {
            undrained: undrained(pool.as_ref()).await?,
            pending: pending_count(pool).await?,
            dead_lettered_open: dead_lettered_count(pool).await?,
            lag: relay_lag(pool, window_hours).await?,
        })
    };
    match read.await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}
