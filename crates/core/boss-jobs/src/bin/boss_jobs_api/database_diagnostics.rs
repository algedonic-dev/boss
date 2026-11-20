//! Observe the serving pool without joining its queue (d9b39685).
//! SQLx exposes size and idle atomics, but no public waiter counter.
//! These separately sampled values cannot establish why a request waited.

use axum::{Json, Router, routing::get};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;

pub(super) fn mount_with_login(
    app: Router,
    pool: PgPool,
    door: Arc<boss_jobs::agents::LoginDoor>,
) -> Router {
    // Ordinary requests still resolve their actor through the registry. This
    // fixed process-only observation has no actor-derived privilege and cannot
    // acquire that registry's pool; the binary's outer machine gate covers both.
    app.layer(axum::middleware::from_fn_with_state(
        door,
        boss_jobs::agents::resolve_login,
    ))
    .merge(router(pool))
}

#[derive(Serialize)]
struct Observation {
    observed_at: DateTime<Utc>,
    process_id: u32,
    build_commit: Option<String>,
    target: DatabaseTarget,
    pool: PoolObservation,
}

#[derive(Serialize)]
struct DatabaseTarget {
    host: String,
    port: u16,
    database: Option<String>,
}

#[derive(Serialize)]
struct PoolObservation {
    size: u32,
    idle: usize,
    max_connections: u32,
    closed: bool,
    waiters: Option<u32>,
    waiters_unavailable: &'static str,
    consistency: &'static str,
}

pub(super) fn router(pool: PgPool) -> Router {
    Router::new().route(
        "/api/jobs/database-observation",
        get(move || {
            let pool = pool.clone();
            async move {
                // Read the serving pool's parsed options, never its URL or
                // credentials. Missing database identity stays unknown.
                let options = pool.connect_options();
                Json(Observation {
                    observed_at: boss_clock_client::wall_now(),
                    process_id: std::process::id(),
                    build_commit: std::env::var("BOSS_BUILD_COMMIT").ok().or_else(|| {
                        option_env!("BOSS_BUILD_COMMIT").map(str::to_owned)
                    }),
                    target: DatabaseTarget {
                        host: options.get_host().to_owned(),
                        port: options.get_port(),
                        database: options.get_database().map(str::to_owned),
                    },
                    pool: PoolObservation {
                        size: pool.size(),
                        idle: pool.num_idle(),
                        max_connections: pool.options().get_max_connections(),
                        closed: pool.is_closed(),
                        waiters: None,
                        waiters_unavailable: "SQLx has no public waiter or acquisition latency snapshot",
                        consistency: "size and idle are separately sampled atomics",
                    },
                })
            }
        }),
    )
}
