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
//! Both modes record their original mode/epoch before listening. The
//! schema trigger captures parameter changes from every database writer
//! in that writer's transaction, including direct Jobs reset paths.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgPool};

use crate::types::{BaselineProvenance, ClockMode, ClockNow, SimClockParams};

#[derive(Debug, thiserror::Error)]
pub enum ClockStorageError {
    #[error("sim_clock query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("sim_clock row not found — apply the schema + seed before starting sim-mode clock-api")]
    NotSeeded,
    #[error("original clock mapping unavailable: {0}")]
    Mapping(String),
}

/// Read the seed baseline's provenance out of `sim_clock`.
///
/// All three columns are nullable: an install seeded before they
/// existed, or one seeded on a host without git, reports what it
/// knows and `None` for the rest. Absent provenance must read as
/// "unknown" — never as "current" — because the whole point is to
/// make a stale pin legible.
pub async fn read_baseline(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<BaselineProvenance, ClockStorageError> {
    let row: Option<(Option<i64>, Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT epoch_baseline_audit_id, baseline_cut_at, baseline_source_ref \
         FROM sim_clock WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    let (id, cut_at, source_ref) = row.unwrap_or((None, None, None));
    Ok(BaselineProvenance::new(id, cut_at, source_ref, now))
}

/// Read sim_clock and project into formula parameters. Returns
/// `NotSeeded` if the row doesn't exist so the bin can fall
/// through to the env-var seed.
pub async fn read_params(pool: &PgPool) -> Result<SimClockParams, ClockStorageError> {
    let mut connection = pool.acquire().await?;
    read_params_on(&mut connection, false).await
}

async fn read_params_on(
    connection: &mut PgConnection,
    for_update: bool,
) -> Result<SimClockParams, ClockStorageError> {
    let query = format!(
        "SELECT epoch_start_date, epoch_end_date, warp_factor, \
                wall_anchor, paused, paused_at, paused_offset_seconds, \
                restart_in_progress FROM sim_clock WHERE id = 1{}",
        if for_update { " FOR UPDATE" } else { "" }
    );
    let row: Option<(
        NaiveDate,
        Option<NaiveDate>,
        Option<f64>,
        Option<DateTime<Utc>>,
        bool,
        Option<DateTime<Utc>>,
        Option<f64>,
        bool,
    )> = sqlx::query_as(&query).fetch_optional(connection).await?;

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
    let mut connection = pool.acquire().await?;
    write_params_on(&mut connection, params).await
}

async fn write_params_on(
    connection: &mut PgConnection,
    params: &SimClockParams,
) -> Result<(), ClockStorageError> {
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
    .execute(connection)
    .await?;
    Ok(())
}

/// Change the recorded authority before publishing a served mapping.
/// The row lock also serializes direct Jobs writers; no cache rollback
/// can accidentally undo a concurrent successful transition.
pub async fn change_params<F>(
    pool: &PgPool,
    fallback: SimClockParams,
    mutator: F,
) -> Result<SimClockParams, ClockStorageError>
where
    F: FnOnce(&mut SimClockParams),
{
    let mut tx = pool.begin().await?;
    let mode: Option<String> = sqlx::query_scalar("SELECT mode FROM clock_mapping_mode WHERE id=1")
        .fetch_optional(&mut *tx)
        .await?;
    if mode.as_deref() != Some("sim") {
        return Err(ClockStorageError::Mapping(
            "sim writer has no matching original mode".into(),
        ));
    }
    let mut params = match read_params_on(&mut tx, true).await {
        Ok(params) => params,
        Err(ClockStorageError::NotSeeded) => fallback,
        Err(error) => return Err(error),
    };
    mutator(&mut params);
    write_params_on(&mut tx, &params).await?;
    tx.commit().await?;
    Ok(params)
}

/// A real startup, including wall mode, records its mode and new epoch
/// before opening its listener. No existing mode/history is backfilled.
pub async fn record_mode(pool: &PgPool, mode: ClockMode) -> Result<(), ClockStorageError> {
    sqlx::query(
        "INSERT INTO clock_mapping_mode(id,mode,epoch) VALUES (1,$1,gen_random_uuid()) \
        ON CONFLICT(id) DO UPDATE SET mode=EXCLUDED.mode,epoch=EXCLUDED.epoch",
    )
    .bind(match mode {
        ClockMode::Wall => "wall",
        ClockMode::Sim => "sim",
    })
    .execute(pool)
    .await?;
    Ok(())
}

/// Mode and formula come from one consistent native snapshot. A
/// process with a stale/different environment mode cannot serve a
/// second global timeline during a rolling restart.
pub async fn authoritative_now(
    pool: &PgPool,
    expected_mode: ClockMode,
) -> Result<ClockNow, ClockStorageError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mode: Option<String> = sqlx::query_scalar("SELECT mode FROM clock_mapping_mode WHERE id=1")
        .fetch_optional(&mut *tx)
        .await?;
    let expected = match expected_mode {
        ClockMode::Wall => "wall",
        ClockMode::Sim => "sim",
    };
    if mode.as_deref() != Some(expected) {
        return Err(ClockStorageError::Mapping(
            "process and original mode differ or mode is missing".into(),
        ));
    }
    let now = match expected_mode {
        ClockMode::Wall => ClockNow::wall(),
        ClockMode::Sim => read_params_on(&mut tx, false).await?.now(),
    };
    tx.commit().await?;
    Ok(now)
}
