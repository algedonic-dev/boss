//! The epoch restart deletes what the SIMULATOR did and keeps what
//! people did.
//!
//! The demo loop resets a simulated company — a year of brewing,
//! selling and shipping that is meant to be disposable — by deleting
//! `audit_log` past the seed baseline and replaying the rebuilders.
//! Real operator input lives in the same log: feedback typed from the
//! chrome bar, a design doc under review, a step a person completed.
//! The trim was destroying it, and did: a lap rolled mid-session and
//! took an entire day's feedback corpus, filed by a real user, with
//! it. Nothing failed and nothing was logged; the Jobs were simply
//! gone the next time the board was read.
//!
//! The rule keys on `_simulated`, which is stamped from the event's
//! ORIGIN — the request carried `x-sim-origin`, or the dispatcher
//! inherited it from the event it reacted to. An earlier version of
//! this exempted three hardcoded platform kinds instead, which could
//! only ever be right by accident: a tenant Job a human worked was
//! still destroyed.
//!
//! The subtle half is mixed origin. A Job's events do NOT share one:
//! a person can complete a step on a Job the simulator created.
//! Keeping that person's event while deleting the simulated create
//! leaves an orphan step, and `steps_job_id_fkey` then aborts the
//! whole jobs rebuild — a failure this path has already had once.

#![cfg(feature = "postgres")]

use boss_jobs::postgres::trim_epoch_audit_log;
use boss_testing::TestDb;

/// Insert an audit row directly. The append-only trigger rejects
/// UPDATE/DELETE, not INSERT, so seeding this way is fine.
async fn seed(db: &TestDb, kind: &str, payload: serde_json::Value) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload)
         VALUES (gen_random_uuid(), $1, 'test', now(), $2)
         RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .fetch_one(&db.pool)
    .await
    .expect("seed audit row")
}

/// An event belonging to `job`, from the simulator or from a person.
async fn job_event(db: &TestDb, job: &str, simulated: bool, key: &str) -> i64 {
    seed(
        db,
        "jobs.step.completed",
        serde_json::json!({ key: job, "_simulated": simulated }),
    )
    .await
}

async fn surviving(db: &TestDb) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read log")
}

#[tokio::test]
async fn simulated_work_goes_and_real_work_stays() {
    let db = TestDb::new().await;
    let baseline = seed(&db, "seed.marker", serde_json::json!({})).await;

    let feedback = "11111111-1111-1111-1111-111111111111";
    let brew = "22222222-2222-2222-2222-222222222222";

    let human_created = seed(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": feedback, "_simulated": false }),
    )
    .await;
    let human_step = job_event(&db, feedback, false, "job_id").await;
    let sim_created = seed(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": brew, "_simulated": true }),
    )
    .await;
    let sim_step = job_event(&db, brew, true, "job_id").await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 2, "exactly the simulator's two rows");

    let left = surviving(&db).await;
    assert!(
        left.contains(&baseline),
        "the seed baseline is never touched"
    );
    assert!(left.contains(&human_created) && left.contains(&human_step));
    assert!(!left.contains(&sim_created) && !left.contains(&sim_step));
}

/// The orphan-step trap. A person acting on a simulated Job makes that
/// Job's history mixed — and deleting half of it aborts the rebuild.
#[tokio::test]
async fn a_job_a_person_touched_survives_whole() {
    let db = TestDb::new().await;
    let baseline = seed(&db, "seed.marker", serde_json::json!({})).await;

    let job = "33333333-3333-3333-3333-333333333333";
    // The simulator opened it and did most of the work…
    let created = seed(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": job, "_simulated": true }),
    )
    .await;
    let sim_a = job_event(&db, job, true, "job_id").await;
    let sim_b = job_event(&db, job, true, "job_id").await;
    // …then a person completed a step on it.
    let human = job_event(&db, job, false, "job_id").await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 0, "one human event preserves the whole Job");

    let left = surviving(&db).await;
    for (id, what) in [
        (created, "the create event"),
        (sim_a, "a simulated step"),
        (sim_b, "another simulated step"),
        (human, "the human's step"),
    ] {
        assert!(
            left.contains(&id),
            "{what} must survive: deleting part of a mixed-origin Job orphans steps \
             and aborts the jobs rebuild on steps_job_id_fkey"
        );
    }
}

/// Absence is treated as real. The conservative direction for a DELETE
/// is to keep, and every publisher stamps the field — so an unflagged
/// row above the baseline is a bug to notice, not data to destroy.
#[tokio::test]
async fn an_unflagged_event_is_kept() {
    let db = TestDb::new().await;
    let baseline = seed(&db, "seed.marker", serde_json::json!({})).await;
    let unflagged = seed(
        &db,
        "assets.asset.installed",
        serde_json::json!({ "id": "asset-1" }),
    )
    .await;
    let simulated = seed(
        &db,
        "assets.asset.installed",
        serde_json::json!({ "id": "asset-2", "_simulated": true }),
    )
    .await;

    trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");

    let left = surviving(&db).await;
    assert!(
        left.contains(&unflagged),
        "unflagged is kept, not destroyed"
    );
    assert!(!left.contains(&simulated));
}

/// Nothing at or below the baseline is ever touched, however it is
/// marked — that is the seed the new epoch replays from.
#[tokio::test]
async fn the_baseline_is_untouchable() {
    let db = TestDb::new().await;
    let seed_row = seed(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": "seeded", "_simulated": true }),
    )
    .await;
    let baseline = seed(&db, "seed.marker", serde_json::json!({})).await;
    let after = seed(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": "later", "_simulated": true }),
    )
    .await;

    trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");

    let left = surviving(&db).await;
    assert!(left.contains(&seed_row), "below the baseline is seed data");
    assert!(left.contains(&baseline));
    assert!(!left.contains(&after));
}
