//! The epoch restart must not destroy real operator input.
//!
//! The demo loop resets the SIMULATED company — a year of brewing,
//! selling and shipping that is meant to be disposable — by deleting
//! `audit_log` past the seed baseline and replaying the rebuilders.
//! But platform meta-work lives in the same log: feedback a person
//! typed from the chrome bar, a design doc under review, a JobKind
//! somebody authored. Those are not sim output, and the trim was
//! deleting them.
//!
//! It is not hypothetical. A lap rolled at 21:18 mid-session and took
//! an entire day's feedback corpus — twelve items filed by a real
//! user, plus the design review they had just completed — with it.
//! Nothing failed; the Jobs were simply gone the next time the board
//! was read.
//!
//! The subtle half is that a Job and its steps must survive TOGETHER.
//! `steps.job_id` is a foreign key to `jobs`, so preserving one and
//! trimming the other manufactures orphan steps and the jobs rebuild
//! aborts — which is a failure this reset path has already had once.

#![cfg(feature = "postgres")]

use boss_jobs::postgres::trim_epoch_audit_log;
use boss_testing::TestDb;

/// Insert an audit row directly. The append-only trigger rejects
/// UPDATE/DELETE, not INSERT, so seeding this way is fine.
async fn seed_event(db: &TestDb, kind: &str, payload: serde_json::Value) -> i64 {
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

async fn seed_job(db: &TestDb, id: &str, kind: &str) {
    sqlx::query(
        "INSERT INTO jobs (id, kind, job_kind_version, subject_kind, subject_id, title,
                           owner_id, status, priority, opened_on)
         VALUES ($1::uuid, $2, 1, 'custom', '/x', 'T', 'emp-1', 'open', 'standard', '2025-06-01')",
    )
    .bind(id)
    .bind(kind)
    .execute(&db.pool)
    .await
    .expect("seed job");
}

#[tokio::test]
async fn trim_keeps_platform_meta_work_and_drops_the_simulated_epoch() {
    let db = TestDb::new().await;

    let feedback = "11111111-1111-1111-1111-111111111111";
    let review = "22222222-2222-2222-2222-222222222222";
    let brew = "33333333-3333-3333-3333-333333333333";
    seed_job(&db, feedback, "user-feedback").await;
    seed_job(&db, review, "design-doc-review").await;
    seed_job(&db, brew, "morning-brew").await;

    // Baseline: everything at or below this id is seed data the trim
    // never touches.
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    // The Job-created event names the job in `id`; step events name it
    // in `job_id`. Both shapes have to be recognised or a Job survives
    // without its steps.
    let fb_created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": feedback, "kind": "user-feedback" }),
    )
    .await;
    let fb_step = seed_event(
        &db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": feedback, "step_id": "s1" }),
    )
    .await;
    let review_step = seed_event(
        &db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": review, "step_id": "s2" }),
    )
    .await;

    // Simulated epoch traffic — the whole point of the reset.
    let brew_step = seed_event(
        &db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": brew, "step_id": "s3" }),
    )
    .await;
    let sale = seed_event(
        &db,
        "commerce.invoice.created",
        serde_json::json!({ "id": "inv-1", "amount_cents": 100 }),
    )
    .await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 2, "only the simulated rows should go");

    let surviving: Vec<i64> = sqlx::query_scalar("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read log");

    for (id, what) in [
        (baseline, "the seed baseline"),
        (fb_created, "the feedback Job's create event"),
        (fb_step, "the feedback Job's step event"),
        (review_step, "the design-review Job's step event"),
    ] {
        assert!(
            surviving.contains(&id),
            "{what} must survive the epoch trim — it is operator input, not sim output"
        );
    }
    for (id, what) in [(brew_step, "a brew step"), (sale, "an invoice")] {
        assert!(
            !surviving.contains(&id),
            "{what} is simulated epoch traffic and must be trimmed"
        );
    }
}

/// The failure this reset path has actually had: a Job preserved
/// without its steps (or the reverse) breaks `steps_job_id_fkey` and
/// the whole rebuild aborts. Both id shapes must resolve to the same
/// Job for the pair to travel together.
#[tokio::test]
async fn a_preserved_job_keeps_its_steps() {
    let db = TestDb::new().await;
    let feedback = "44444444-4444-4444-4444-444444444444";
    seed_job(&db, feedback, "user-feedback").await;

    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;
    let created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": feedback }),
    )
    .await;
    let steps: Vec<i64> = {
        let mut v = Vec::new();
        for n in 0..3 {
            v.push(
                seed_event(
                    &db,
                    "jobs.step.updated",
                    serde_json::json!({ "job_id": feedback, "step_id": format!("s{n}") }),
                )
                .await,
            );
        }
        v
    };

    trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");

    let surviving: Vec<i64> = sqlx::query_scalar("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read log");
    assert!(surviving.contains(&created));
    for s in steps {
        assert!(
            surviving.contains(&s),
            "a preserved Job kept without its steps orphans them and aborts the rebuild"
        );
    }
}

/// A Job of a NON-platform kind gets no exemption, however it is
/// shaped. Without this the rule could quietly widen into "preserve
/// anything with a job_id", which would defeat the reset.
#[tokio::test]
async fn tenant_jobs_are_not_exempt() {
    let db = TestDb::new().await;
    let brew = "55555555-5555-5555-5555-555555555555";
    seed_job(&db, brew, "morning-brew").await;

    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;
    seed_event(&db, "jobs.job.created", serde_json::json!({ "id": brew })).await;
    seed_event(
        &db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": brew }),
    )
    .await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 2, "tenant Jobs are exactly what the reset clears");
}
