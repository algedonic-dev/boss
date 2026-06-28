//! Axum HTTP handlers for `boss-clock-api`.
//!
//! Endpoints:
//! - `GET  /api/clock/health` — mode + uptime check.
//! - `GET  /api/clock/now` — the canonical "what time is it"
//!   endpoint. Every service hits this through
//!   `boss-clock-client::ClockClient::now()`.
//! - `POST /api/clock/configure` — sim-mode only. Reset epoch
//!   parameters + warp factor.
//! - `POST /api/clock/pause` / `POST /api/clock/resume` —
//!   sim-mode only. Pauses sim-time advancement.
//! - `POST /api/clock/restart-epoch` — sim-mode only. Resets
//!   wall_anchor to wall-now so sim-time starts over from
//!   epoch_start.
//!
//! ## The time-warp model
//!
//! Sim-time is a pure function of:
//!   sim_now = epoch_start + (wall_now − wall_anchor − paused_offset) × warp_factor
//!
//! Nothing else writes time. The simulator (brewery-sim, etc.)
//! READS the clock and emits events for whatever days have
//! passed since its last poll. Time advances regardless of
//! what the simulator does — like real life.

use std::sync::Arc;
use std::sync::RwLock;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::stream::Stream;
use serde::Serialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::IntervalStream;

use crate::types::{ClockMode, ClockNow, ConfigureRequest, SimClockParams};

/// Shared state for the clock API. In sim mode the `params`
/// RwLock holds the formula parameters; `/now` recomputes on
/// every read. In wall mode `params` is unused and every
/// `/now` call returns `ClockNow::wall()`.
#[derive(Clone)]
pub struct ClockApiState {
    pub mode: ClockMode,
    /// Sim-mode formula parameters. `None` in wall mode and
    /// during the cold-start window before `/configure` or
    /// `read_sim_clock` primes them.
    pub params: Arc<RwLock<Option<SimClockParams>>>,
    /// Optional pool for persisting parameter changes. Wall
    /// mode + tests leave this `None`.
    #[cfg(feature = "postgres")]
    pub pool: Option<sqlx::PgPool>,
}

pub fn router(state: ClockApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/clock/health", get(health))
        .route("/api/clock/now", get(now_handler))
        .route("/api/clock/ticks", get(ticks_stream_handler))
        .route("/api/clock/configure", post(configure_handler))
        .route("/api/clock/pause", post(pause_handler))
        .route("/api/clock/resume", post(resume_handler))
        .route("/api/clock/restart-epoch", post(restart_epoch_handler))
        .with_state(shared)
}

/// `GET /api/clock/ticks` — Server-Sent Events stream of Ticks.
///
/// Per design D3: ephemeral, stateless. Each event payload is a full
/// `ClockNow` JSON (now + simulated + epoch_start/end + paused +
/// restart_in_progress) — the clock's whole state, so a streaming
/// consumer drives its loop off this alone. Emits at a fixed wall-time
/// cadence (one tick per second by default); the consumer sees sim time
/// advancing proportional to warp_factor in sim mode.
///
/// No persistence, no replay, no cursor. On reconnect the consumer
/// gets live ticks from now() onward. Recovery from gaps is the
/// consumer's responsibility (docs/architecture-decisions.md
/// §Dispatcher — the event router: clock interaction is an
/// ephemeral stream plus one-off queries).
async fn ticks_stream_handler(
    State(state): State<Arc<ClockApiState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let interval = tokio::time::interval(Duration::from_secs(1));
    let stream = IntervalStream::new(interval).map(move |_| {
        let now = match state.mode {
            ClockMode::Wall => ClockNow::wall(),
            ClockMode::Sim => match state.params.read() {
                Ok(p) => p.as_ref().map(|p| p.now()).unwrap_or_else(ClockNow::wall),
                Err(_) => ClockNow::wall(),
            },
        };
        // Emit the full `ClockNow`, not just the timestamp, so streaming
        // consumers — the dispatcher's timing triggers and the sim daemon —
        // drive their loops off the stream alone, with no companion
        // `/api/clock/now` poll.
        Ok(Event::default().data(serde_json::to_string(&now).unwrap_or_default()))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    mode: ClockMode,
    capabilities: boss_core::startup::Capabilities,
}

async fn health(State(state): State<Arc<ClockApiState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        mode: state.mode,
        capabilities: boss_core::startup::Capabilities::new(
            "boss-clock-api",
            env!("CARGO_PKG_VERSION"),
            match state.mode {
                ClockMode::Wall => "wall",
                ClockMode::Sim => "sim",
            },
        ),
    })
}

async fn now_handler(State(state): State<Arc<ClockApiState>>) -> Response {
    let now = match state.mode {
        ClockMode::Wall => ClockNow::wall(),
        ClockMode::Sim => match state.params.read() {
            Ok(guard) => match *guard {
                Some(params) => params.now(),
                None => {
                    // Cold start before /configure. Loud WARN —
                    // regen 18 surfaced 116k wallclock-stamped
                    // audit rows from exactly this fallback.
                    tracing::warn!(
                        "clock-api sim mode with no formula params — \
                         returning wallclock fallback. POST /api/clock/configure first."
                    );
                    ClockNow {
                        now: Utc::now(),
                        simulated: true,
                        epoch_start: None,
                        epoch_end: None,
                        paused: false,
                        restart_in_progress: false,
                        warp_factor: None,
                    }
                }
            },
            Err(poisoned) => poisoned
                .into_inner()
                .map(|p| p.now())
                .unwrap_or_else(ClockNow::wall),
        },
    };
    Json(now).into_response()
}

async fn configure_handler(
    State(state): State<Arc<ClockApiState>>,
    Json(req): Json<ConfigureRequest>,
) -> Response {
    if state.mode != ClockMode::Sim {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "clock is in wall mode; configure is a sim-only operation",
        )
            .into_response();
    }
    let new_params = match update_params(&state.params, |p| {
        if let Some(es) = req.epoch_start {
            p.epoch_start = es;
            // Resetting epoch_start rebases the formula — sim-time
            // starts over from the new epoch_start as of wall-now.
            p.wall_anchor = Utc::now();
            p.paused_offset_seconds = 0.0;
            p.paused_at = None;
        }
        if let Some(ee) = req.epoch_end {
            p.epoch_end = Some(ee);
        }
        if let Some(w) = req.warp_factor
            && w > 0.0
        {
            // Live-changing warp_factor: re-anchor so the running
            // sim-time doesn't teleport. New sim-time at the new
            // rate starts from the current sim-time as of wall-now.
            let current = p.now();
            p.epoch_start = current.now.date_naive();
            // Preserve sub-day position by setting wall_anchor to
            // (wall_now − (current_within_day_seconds / new_warp)).
            let within_day_secs = (current.now
                - p.epoch_start
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight valid")
                    .and_utc())
            .num_milliseconds()
            .max(0) as f64
                / 1000.0;
            let wall_offset_secs = within_day_secs / w;
            p.wall_anchor =
                Utc::now() - chrono::Duration::milliseconds((wall_offset_secs * 1000.0) as i64);
            p.warp_factor = w;
            p.paused_offset_seconds = 0.0;
            p.paused_at = None;
        }
    })
    .await
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    #[cfg(feature = "postgres")]
    persist_if_pool(&state.pool, &new_params).await;
    Json(new_params).into_response()
}

async fn pause_handler(State(state): State<Arc<ClockApiState>>) -> Response {
    if state.mode != ClockMode::Sim {
        return (StatusCode::METHOD_NOT_ALLOWED, "sim-only").into_response();
    }
    let new = match update_params(&state.params, |p| {
        if !p.paused {
            p.paused = true;
            p.paused_at = Some(Utc::now());
        }
    })
    .await
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    #[cfg(feature = "postgres")]
    persist_if_pool(&state.pool, &new).await;
    Json(new.now()).into_response()
}

async fn resume_handler(State(state): State<Arc<ClockApiState>>) -> Response {
    if state.mode != ClockMode::Sim {
        return (StatusCode::METHOD_NOT_ALLOWED, "sim-only").into_response();
    }
    let new = match update_params(&state.params, |p| {
        if p.paused {
            if let Some(paused_at) = p.paused_at {
                let elapsed = (Utc::now() - paused_at).num_milliseconds().max(0) as f64 / 1000.0;
                p.paused_offset_seconds += elapsed;
            }
            p.paused = false;
            p.paused_at = None;
        }
    })
    .await
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    #[cfg(feature = "postgres")]
    persist_if_pool(&state.pool, &new).await;
    Json(new.now()).into_response()
}

async fn restart_epoch_handler(State(state): State<Arc<ClockApiState>>) -> Response {
    if state.mode != ClockMode::Sim {
        return (StatusCode::METHOD_NOT_ALLOWED, "sim-only").into_response();
    }
    let new = match update_params(&state.params, |p| {
        p.wall_anchor = Utc::now();
        p.paused_offset_seconds = 0.0;
        p.paused_at = None;
        p.paused = false;
    })
    .await
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    #[cfg(feature = "postgres")]
    persist_if_pool(&state.pool, &new).await;
    Json(new.now()).into_response()
}

async fn update_params<F>(
    state: &Arc<RwLock<Option<SimClockParams>>>,
    mutator: F,
) -> Result<SimClockParams, &'static str>
where
    F: FnOnce(&mut SimClockParams),
{
    let mut guard = state.write().map_err(|_| "params lock poisoned")?;
    let mut params = guard.unwrap_or(SimClockParams {
        epoch_start: chrono::Utc::now().date_naive(),
        epoch_end: None,
        warp_factor: 8640.0,
        wall_anchor: Utc::now(),
        paused: false,
        paused_at: None,
        paused_offset_seconds: 0.0,
        restart_in_progress: false,
    });
    mutator(&mut params);
    *guard = Some(params);
    Ok(params)
}

#[cfg(feature = "postgres")]
async fn persist_if_pool(pool: &Option<sqlx::PgPool>, params: &SimClockParams) {
    if let Some(pool) = pool
        && let Err(e) = crate::postgres::write_params(pool, params).await
    {
        tracing::warn!(error = %e, "failed to persist sim_clock params");
    }
}

/// Spawn a background task that re-reads `sim_clock` from the DB
/// into the in-memory `params` cache on a fixed wall cadence.
///
/// Why: the cache is mutated in-process by `/configure`, `/pause`,
/// `/resume`, and `/restart-epoch`, but other processes also write
/// `sim_clock` directly — most importantly the brewery demo-loop
/// Reset (`boss-jobs::run_restart_epoch_background`), which rewinds
/// `wall_anchor`/`epoch_start` with a plain SQL `UPDATE`. Without
/// this refresher, those external writes never reach the cache and
/// `/now` keeps serving the pre-Reset time until the process is
/// restarted. The DB row is the source of truth; this task makes the
/// cache converge to it within one `period`.
///
/// Convergence, not coordination: the refresher unconditionally
/// overwrites the cache with the authoritative DB row. In-process
/// writers persist immediately after mutating the cache (see
/// `persist_if_pool`), and every persisted field is absolute
/// (`wall_anchor`, `paused_offset_seconds`, …) rather than a delta,
/// so a refresh that races a just-applied API write either reads the
/// already-persisted new value or the not-yet-persisted old one —
/// and the following refresh re-converges either way. No write is
/// lost because the DB, not the cache, holds it.
///
/// The lock is held only for the duration of the overwrite (no
/// `.await` under the guard), so this never deadlocks or thrashes
/// against a concurrent API write.
#[cfg(feature = "postgres")]
pub fn spawn_db_refresher(
    pool: sqlx::PgPool,
    params: Arc<RwLock<Option<SimClockParams>>>,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        // The first tick fires immediately; skip it — startup already
        // primed the cache from the DB. Refresh from the *next* tick on.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match crate::postgres::read_params(&pool).await {
                Ok(fresh) => {
                    if let Ok(mut guard) = params.write() {
                        *guard = Some(fresh);
                    }
                    // A poisoned lock means a writer panicked mid-update;
                    // skip this round rather than risk reasoning about
                    // half-written state. The next tick retries.
                }
                Err(crate::postgres::ClockStorageError::NotSeeded) => {
                    // Row not present yet (cold start before the first
                    // /configure or seed). Leave the cache untouched so
                    // we don't clobber a just-configured in-memory value
                    // with None.
                }
                Err(e) => {
                    tracing::warn!(error = %e, "sim_clock refresh read failed; keeping cached params");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn wall_state() -> ClockApiState {
        ClockApiState {
            mode: ClockMode::Wall,
            params: Arc::new(RwLock::new(None)),
            #[cfg(feature = "postgres")]
            pool: None,
        }
    }

    fn sim_state() -> ClockApiState {
        ClockApiState {
            mode: ClockMode::Sim,
            params: Arc::new(RwLock::new(None)),
            #[cfg(feature = "postgres")]
            pool: None,
        }
    }

    async fn get_json(app: Router, path: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    async fn post_json(
        app: Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn wall_now_is_not_simulated() {
        let app = router(wall_state());
        let (status, body) = get_json(app, "/api/clock/now").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["simulated"], false);
    }

    #[tokio::test]
    async fn wall_mode_refuses_configure() {
        let app = router(wall_state());
        let (status, _) = post_json(
            app,
            "/api/clock/configure",
            serde_json::json!({"warp_factor": 2.0}),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn sim_configure_sets_epoch_start_and_now_is_close() {
        let app = router(sim_state());
        let (status, _) = post_json(
            app.clone(),
            "/api/clock/configure",
            serde_json::json!({"epoch_start": "2025-04-01", "warp_factor": 1.0}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = get_json(app, "/api/clock/now").await;
        assert_eq!(body["simulated"], true);
        let now_str = body["now"].as_str().unwrap();
        // warp_factor=1 + just-now anchor → sim-time ≈ epoch_start
        assert!(now_str.starts_with("2025-04-01"));
    }

    #[tokio::test]
    async fn pause_freezes_time() {
        let app = router(sim_state());
        post_json(
            app.clone(),
            "/api/clock/configure",
            serde_json::json!({"epoch_start": "2025-04-01", "warp_factor": 1000.0}),
        )
        .await;
        let (_, before) = post_json(app.clone(), "/api/clock/pause", serde_json::json!({})).await;
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (_, after) = get_json(app, "/api/clock/now").await;
        // With warp=1000 and 100ms wall, 100 sim-seconds would
        // pass. Pause means they don't.
        assert_eq!(before["now"], after["now"]);
    }

    /// The reset↔clock-api desync fix: `/now` must read the shared
    /// `params` cell live, so an out-of-band overwrite (what the DB
    /// refresher does after the demo-loop Reset rewinds `sim_clock`)
    /// is reflected immediately — no process restart. We drive the
    /// clock forward, then overwrite the cache with a rewound-anchor
    /// params (epoch_start as of wall-now, the post-Reset state) and
    /// assert `/now` snaps back to epoch_start.
    #[tokio::test]
    async fn external_params_overwrite_is_reflected_by_now() {
        let state = sim_state();
        // Start at 2025-04-01, anchored ~5s in the past at warp=1000
        // so sim-time has already advanced well past epoch_start.
        {
            let mut guard = state.params.write().unwrap();
            *guard = Some(SimClockParams {
                epoch_start: chrono::NaiveDate::from_ymd_opt(2025, 4, 1).unwrap(),
                epoch_end: None,
                warp_factor: 1000.0,
                wall_anchor: Utc::now() - chrono::Duration::seconds(5),
                paused: false,
                paused_at: None,
                paused_offset_seconds: 0.0,
                restart_in_progress: false,
            });
        }
        let shared = Arc::clone(&state.params);
        let app = router(state);
        let (_, advanced) = get_json(app.clone(), "/api/clock/now").await;
        // 5s wall × warp 1000 = ~5000 sim-seconds past midnight.
        assert!(
            !advanced["now"].as_str().unwrap().contains("00:00:0"),
            "precondition: sim-time should have advanced past midnight, got {advanced:?}"
        );

        // Simulate the refresher loading a freshly-rewound DB row:
        // wall_anchor = NOW(), paused_offset = 0 — sim-time snaps back
        // to epoch_start. This is exactly what boss-jobs writes and
        // what spawn_db_refresher reads back into the cache.
        {
            let mut guard = shared.write().unwrap();
            *guard = Some(SimClockParams {
                epoch_start: chrono::NaiveDate::from_ymd_opt(2025, 4, 1).unwrap(),
                epoch_end: None,
                warp_factor: 1000.0,
                wall_anchor: Utc::now(),
                paused: false,
                paused_at: None,
                paused_offset_seconds: 0.0,
                restart_in_progress: false,
            });
        }
        let (_, rewound) = get_json(app, "/api/clock/now").await;
        assert!(
            rewound["now"].as_str().unwrap().starts_with("2025-04-01"),
            "after external rewind, /now must report epoch_start, got {rewound:?}"
        );
    }
}
