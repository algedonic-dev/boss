//! Postgres-backed `sim_clock` reader / writer.
//!
//! The schema's `sim_clock` table is the persistence layer for
//! the formula clock's parameters — epoch_start, warp_factor,
//! wall_anchor, paused state. Clock-api's `/now` recomputes the
//! current sim instant from these parameters on every request;
//! no `current_sim_date` column needed because time advances on
//! its own.
//!
//! - On clock-api startup in sim mode, reads `sim_clock` and
//!   primes the in-memory formula parameters. If the row is
//!   missing, falls through to `BOSS_SIM_EPOCH_START` (env) or
//!   to wall-now as last-resort cold-start.
//! - On every `/configure`, `/pause`, `/resume`,
//!   `/restart-epoch` call, persists the updated parameters so
//!   a restart picks up where it left off.
//!
//! Production (wall mode) doesn't touch Postgres at all.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use crate::types::SimClockParams;

#[derive(Debug, thiserror::Error)]
pub enum ClockStorageError {
    #[error("sim_clock query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("sim_clock row not found — apply the schema + seed before starting sim-mode clock-api")]
    NotSeeded,
}

/// Read sim_clock and project into formula parameters. Returns
/// `NotSeeded` if the row doesn't exist so the bin can fall
/// through to the env-var seed.
pub async fn read_params(pool: &PgPool) -> Result<SimClockParams, ClockStorageError> {
    let row: Option<(
        NaiveDate,
        Option<NaiveDate>,
        Option<f64>,
        Option<DateTime<Utc>>,
        bool,
        Option<DateTime<Utc>>,
        Option<f64>,
        bool,
    )> = sqlx::query_as(
        "SELECT epoch_start_date, epoch_end_date, warp_factor, \
                wall_anchor, paused, paused_at, paused_offset_seconds, \
                restart_in_progress \
         FROM sim_clock WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;

    let (
        epoch_start,
        epoch_end,
        warp_factor,
        wall_anchor,
        paused,
        paused_at,
        paused_offset_seconds,
        restart_in_progress,
    ) = row.ok_or(ClockStorageError::NotSeeded)?;

    Ok(SimClockParams {
        epoch_start,
        epoch_end,
        // Existing rows from the pre-formula migration carry NULL
        // for warp_factor + wall_anchor; default to the sim default
        // warp (1000 = 1 sim-day per ~86 wall-seconds) + anchor=now
        // so the formula still produces a sane value.
        warp_factor: warp_factor.unwrap_or(1000.0),
        wall_anchor: wall_anchor.unwrap_or_else(Utc::now),
        paused,
        paused_at,
        paused_offset_seconds: paused_offset_seconds.unwrap_or(0.0),
        restart_in_progress,
    })
}

/// Persist the formula parameters. UPSERT so a fresh DB without
/// a sim_clock row still ends up with one.
pub async fn write_params(pool: &PgPool, params: &SimClockParams) -> Result<(), ClockStorageError> {
    sqlx::query(
        "INSERT INTO sim_clock \
            (id, epoch_start_date, epoch_end_date, warp_factor, \
             wall_anchor, paused, paused_at, paused_offset_seconds, \
             restart_in_progress) \
         VALUES (1, $1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (id) DO UPDATE SET \
             epoch_start_date      = EXCLUDED.epoch_start_date, \
             epoch_end_date        = EXCLUDED.epoch_end_date, \
             warp_factor           = EXCLUDED.warp_factor, \
             wall_anchor           = EXCLUDED.wall_anchor, \
             paused                = EXCLUDED.paused, \
             paused_at             = EXCLUDED.paused_at, \
             paused_offset_seconds = EXCLUDED.paused_offset_seconds, \
             restart_in_progress   = EXCLUDED.restart_in_progress, \
             updated_at            = NOW()",
    )
    .bind(params.epoch_start)
    .bind(params.epoch_end)
    .bind(params.warp_factor)
    .bind(params.wall_anchor)
    .bind(params.paused)
    .bind(params.paused_at)
    .bind(params.paused_offset_seconds)
    .bind(params.restart_in_progress)
    .execute(pool)
    .await?;
    Ok(())
}
