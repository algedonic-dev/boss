//! Stage durations — per-step wall-clock latency for one Workflow
//! kind (backlog `a5096c8f`'s server half; the UI home is the
//! department-dashboards doc's Q4).
//!
//! Contracts pinned:
//! 1. A stage's duration is `step.done` wall time minus `step.ready`
//!    wall time (`audit_log.created_at` — the flow.rs doctrine), per
//!    step, aggregated per COALESCE(spec_slug, title) — the fleet
//!    grouping, so the numbers land on the same nodes.
//! 2. Kind-scoped and window-bounded: other kinds' steps and
//!    completions outside the trailing window contribute nothing.
//! 3. A step with only a ready event (still waiting) contributes
//!    nothing — this measures completed hops only.
//!
//! `created_at` is trigger-assigned, so tests pin structure (which
//! rows enter, how they group, non-negative durations), not exact
//! percentiles.

use boss_testing::TestDb;
use boss_views::postgres::PgViewsRepo;
use boss_views::stages::StageDurationsRepo;

async fn seed_job(pool: &sqlx::PgPool, kind: &str) -> uuid::Uuid {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, $2, 'account', 'acc-1', 'T', 'emp-o', 'standard', 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .bind(kind)
    .execute(pool)
    .await
    .expect("job");
    job_id
}

async fn seed_step(
    pool: &sqlx::PgPool,
    job_id: uuid::Uuid,
    slug: Option<&str>,
    title: &str,
) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO steps (id, job_id, kind, spec_slug, title, status, sort_order) \
         VALUES ($1, $2, 'task', $3, $4, 'completed', 1)",
    )
    .bind(id)
    .bind(job_id)
    .bind(slug)
    .bind(title)
    .execute(pool)
    .await
    .expect("step");
    id
}

async fn seed_event(pool: &sqlx::PgPool, kind: &str, step_id: uuid::Uuid) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, NOW(), 'test', $2, $3)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(kind)
    .bind(serde_json::json!({ "step_id": step_id.to_string() }))
    .execute(pool)
    .await
    .expect("event");
}

#[tokio::test]
async fn durations_group_by_slug_and_count_completed_hops_only() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    let job = seed_job(pool, "wholesale-keg-order").await;
    let other = seed_job(pool, "direct-shop-order").await;

    // Two completed hops on "brew" (ready then done), one on the
    // title-fallback step, one still-waiting step, one other-kind hop.
    let b1 = seed_step(pool, job, Some("brew"), "Brew").await;
    let b2 = seed_step(pool, job, Some("brew"), "Brew").await;
    let fallback = seed_step(pool, job, None, "Deliver").await;
    let waiting = seed_step(pool, job, Some("ship"), "Ship").await;
    let foreign = seed_step(pool, other, Some("brew"), "Brew").await;

    for s in [b1, b2, fallback, foreign] {
        seed_event(pool, "step.ready.task", s).await;
        seed_event(pool, "step.done.task", s).await;
    }
    seed_event(pool, "step.ready.task", waiting).await;

    let repo = PgViewsRepo::new(db.pool.clone());
    let out = repo
        .stage_durations("wholesale-keg-order", 7)
        .await
        .expect("stage durations");

    assert_eq!(out.workflow_kind, "wholesale-keg-order");
    assert_eq!(out.window_days, 7);

    let stage = |slug: &str| {
        out.stages
            .iter()
            .find(|s| s.slug == slug)
            .unwrap_or_else(|| panic!("no stage {slug:?} in {:?}", out.stages))
    };

    let brew = stage("brew");
    assert_eq!(
        brew.completed, 2,
        "two completed brew hops (not the other kind's)"
    );
    assert!(brew.p50_seconds >= 0.0 && brew.max_seconds >= brew.p50_seconds);

    assert_eq!(
        stage("Deliver").completed,
        1,
        "slug-less steps group by title"
    );
    assert!(
        !out.stages.iter().any(|s| s.slug == "ship"),
        "a step that only became ready has no duration yet"
    );
}

async fn seed_step_at(
    pool: &sqlx::PgPool,
    job_id: uuid::Uuid,
    slug: &str,
    sort_order: i32,
) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO steps (id, job_id, kind, spec_slug, title, status, sort_order) \
         VALUES ($1, $2, 'task', $3, $3, 'completed', $4)",
    )
    .bind(id)
    .bind(job_id)
    .bind(slug)
    .bind(sort_order)
    .execute(pool)
    .await
    .expect("step");
    id
}

/// The per-run rows (`stage_runs`): last N Jobs of the kind, newest
/// first, each with its steps in spec order — a completed hop carries
/// a duration, a still-waiting hop carries None, and other kinds'
/// Jobs never enter.
#[tokio::test]
async fn stage_runs_list_recent_jobs_with_per_step_durations() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    let older = seed_job(pool, "pr-train").await;
    let newer = seed_job(pool, "pr-train").await;
    let foreign = seed_job(pool, "wholesale-keg-order").await;
    // Deterministic recency regardless of insert-timestamp ties.
    sqlx::query("UPDATE jobs SET created_at = now() - interval '2 hours' WHERE id = $1")
        .bind(older)
        .execute(pool)
        .await
        .unwrap();

    let o_ci = seed_step_at(pool, older, "ci", 1).await;
    let o_merged = seed_step_at(pool, older, "merged", 2).await;
    let n_ci = seed_step_at(pool, newer, "ci", 1).await;
    let f_step = seed_step_at(pool, foreign, "ci", 1).await;

    // older: both hops complete; newer: ci still waiting; foreign: complete.
    for s in [o_ci, o_merged, f_step] {
        seed_event(pool, "step.ready.task", s).await;
        seed_event(pool, "step.done.task", s).await;
    }
    seed_event(pool, "step.ready.task", n_ci).await;

    let repo = PgViewsRepo::new(db.pool.clone());
    let out = repo.stage_runs("pr-train", 10).await.expect("stage runs");

    assert_eq!(out.workflow_kind, "pr-train");
    assert_eq!(out.runs.len(), 2, "the foreign kind never enters");
    assert_eq!(
        out.runs[0].job_id,
        newer.to_string(),
        "newest first: {:?}",
        out.runs.iter().map(|r| &r.job_id).collect::<Vec<_>>()
    );

    let newer_run = &out.runs[0];
    assert_eq!(newer_run.stages.len(), 1);
    assert_eq!(newer_run.stages[0].slug, "ci");
    assert!(
        newer_run.stages[0].seconds.is_none(),
        "a waiting hop has no duration"
    );

    let older_run = &out.runs[1];
    assert_eq!(
        older_run
            .stages
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["ci", "merged"],
        "steps arrive in spec order"
    );
    assert!(
        older_run
            .stages
            .iter()
            .all(|s| s.seconds.unwrap_or(-1.0) >= 0.0)
    );

    // The limit truncates from the old end.
    let capped = repo.stage_runs("pr-train", 1).await.expect("capped");
    assert_eq!(capped.runs.len(), 1);
    assert_eq!(capped.runs[0].job_id, newer.to_string());
}
