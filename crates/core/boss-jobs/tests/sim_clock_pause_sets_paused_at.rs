//! Regression: a sim_clock pause must set `paused_at`, not just `paused`.
//!
//! boss-clock's `now()` only freezes sim-time when
//! `(paused, paused_at) = (true, Some)`. If a pause sets `paused` alone,
//! the formula clock keeps advancing off `Utc::now()` — the clock never
//! freezes and the sim daemon never quiesces. That is exactly what broke
//! the demo's restart-epoch: the unpaused sim kept writing jobs/steps
//! while the audit_log trim ran, so the trim deleted a job's create event
//! while a later-committed step event survived → orphaned steps → the
//! jobs rebuild aborted on `steps_job_id_fkey`.
//!
//! This guards the shared pause primitive (`set_sim_clock_paused`, used by
//! the Pause/Resume buttons); restart-epoch's own pause-claim sets
//! `paused_at` the same way.
//!
//! Postgres-only: the in-memory adapter has no sim_clock table.

#![cfg(feature = "postgres")]

use boss_jobs::{JobsRepository, PgJobs};
use boss_testing::TestDb;

async fn paused_at(db: &TestDb) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("SELECT paused_at FROM sim_clock WHERE id = 1")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn pause_sets_paused_at_and_resume_clears_it() {
    let db = TestDb::new().await;
    // Only epoch_start_date is NOT NULL without a default; everything else
    // (id=1, paused=false, paused_at=NULL, ...) defaults.
    sqlx::query("INSERT INTO sim_clock (epoch_start_date) VALUES ('2025-03-31')")
        .execute(&db.pool)
        .await
        .expect("seed sim_clock");
    assert!(paused_at(&db).await.is_none(), "precondition: not paused");

    let repo = PgJobs::new(db.pool.clone());

    repo.set_sim_clock_paused(true).await.expect("pause");
    let (paused, at): (bool, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT paused, paused_at FROM sim_clock WHERE id = 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(paused, "paused flag set");
    assert!(
        at.is_some(),
        "pause MUST set paused_at, else boss-clock now() never freezes"
    );

    repo.set_sim_clock_paused(false).await.expect("resume");
    let (paused, at): (bool, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT paused, paused_at FROM sim_clock WHERE id = 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!paused, "resumed");
    assert!(at.is_none(), "resume MUST clear paused_at");
}
