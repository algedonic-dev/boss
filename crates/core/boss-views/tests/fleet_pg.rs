//! The fleet view — every in-flight Job of a kind, projected onto its
//! Workflow's step shape.
//!
//! Three contracts pinned here:
//!
//! 1. **Group key is `COALESCE(NULLIF(spec_slug,''), title)`.** Steps
//!    materialized before migration 100 carry no slug (verified live:
//!    23 active wholesale-keg steps), and a Workflow authored without
//!    slugs never gets them. The overlay must not silently drop those
//!    rows — they group under their title as an honest fallback.
//! 2. **Only in-flight steps of open Jobs of the requested kind.**
//!    Closed Jobs and other kinds contribute nothing; completed and
//!    pending steps contribute nothing. Depth is the live set only.
//! 3. **Oldest wait is wall-clock, from `audit_log.created_at`.** The
//!    projection tier carries sim time only (see `flow.rs` — the same
//!    doctrine); a node with no `step.ready.*` audit row reports no
//!    age rather than a fabricated one.

use boss_testing::TestDb;
use boss_views::fleet::FleetRepo;
use boss_views::postgres::PgViewsRepo;

async fn seed_job(pool: &sqlx::PgPool, kind: &str, status: &str) -> uuid::Uuid {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, $2, 'account', 'acc-1', 'T', 'emp-owner', 'standard', $3, CURRENT_DATE)",
    )
    .bind(job_id)
    .bind(kind)
    .bind(status)
    .execute(pool)
    .await
    .expect("job inserts");
    job_id
}

#[allow(clippy::too_many_arguments)]
async fn seed_step(
    pool: &sqlx::PgPool,
    job_id: uuid::Uuid,
    slug: Option<&str>,
    title: &str,
    status: &str,
    assignee: Option<&str>,
    role: Option<&str>,
) -> uuid::Uuid {
    let step_id = uuid::Uuid::new_v4();
    let metadata = match role {
        Some(r) => serde_json::json!({ "authority_role": r }),
        None => serde_json::json!({}),
    };
    sqlx::query(
        "INSERT INTO steps (id, job_id, kind, spec_slug, title, assignee_id, status, sort_order, metadata) \
         VALUES ($1, $2, 'task', $3, $4, $5, $6, 1, $7)",
    )
    .bind(step_id)
    .bind(job_id)
    .bind(slug)
    .bind(title)
    .bind(assignee)
    .bind(status)
    .bind(metadata)
    .execute(pool)
    .await
    .expect("step inserts");
    step_id
}

/// A `step.ready.<kind>` audit row for a step — the wall-clock birth
/// of its wait. `created_at` is trigger-assigned; the test asserts
/// presence, not a controlled value.
async fn seed_ready_event(pool: &sqlx::PgPool, step_id: uuid::Uuid) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, NOW(), 'boss-jobs', 'step.ready.task', $2)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(serde_json::json!({ "step_id": step_id.to_string(), "kind": "task" }))
    .execute(pool)
    .await
    .expect("audit inserts");
}

#[tokio::test]
async fn fleet_aggregates_in_flight_steps_by_slug_with_title_fallback() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // Two open Jobs of the kind under test.
    let job1 = seed_job(pool, "wholesale-keg-order", "open").await;
    let job2 = seed_job(pool, "wholesale-keg-order", "open").await;
    // Excluded entirely: a closed Job of the kind, an open Job of
    // another kind — their steps must not appear in any count.
    let closed = seed_job(pool, "wholesale-keg-order", "closed").await;
    let other = seed_job(pool, "direct-shop-order", "open").await;

    // "brew": two ready steps, one unassigned, both role-gated.
    let brew1 = seed_step(
        pool,
        job1,
        Some("brew"),
        "Brew",
        "ready",
        None,
        Some("brewer"),
    )
    .await;
    let brew2 = seed_step(
        pool,
        job2,
        Some("brew"),
        "Brew",
        "ready",
        Some("emp-a"),
        Some("brewer"),
    )
    .await;
    // "ship": one active step, assigned, no role.
    seed_step(
        pool,
        job1,
        Some("ship"),
        "Ship",
        "active",
        Some("emp-b"),
        None,
    )
    .await;
    // Pre-migration-100 shape: no slug — groups under its title.
    seed_step(pool, job2, None, "Deliver", "ready", None, None).await;
    // Non-live statuses contribute nothing.
    seed_step(
        pool,
        job1,
        Some("brew"),
        "Brew",
        "completed",
        Some("emp-c"),
        None,
    )
    .await;
    seed_step(pool, job1, Some("plan"), "Plan", "pending", None, None).await;
    // Excluded Jobs' steps contribute nothing.
    seed_step(pool, closed, Some("brew"), "Brew", "ready", None, None).await;
    seed_step(pool, other, Some("brew"), "Brew", "ready", None, None).await;

    // Wall-clock birth events for the two live brew steps only.
    seed_ready_event(pool, brew1).await;
    seed_ready_event(pool, brew2).await;

    let repo = PgViewsRepo::new(db.pool.clone());
    let fleet = repo
        .fleet("wholesale-keg-order")
        .await
        .expect("fleet query");

    assert_eq!(fleet.workflow_kind, "wholesale-keg-order");
    assert_eq!(fleet.open_jobs, 2, "open Jobs of the kind, not steps");

    let node = |slug: &str| {
        fleet
            .nodes
            .iter()
            .find(|n| n.slug == slug)
            .unwrap_or_else(|| panic!("no node for slug {slug:?} in {:?}", fleet.nodes))
    };

    let brew = node("brew");
    assert_eq!((brew.ready, brew.active, brew.unassigned), (2, 0, 1));
    assert_eq!(
        brew.by_role.get("brewer").copied(),
        Some(2),
        "both live brew steps are brewer-gated"
    );
    assert!(
        brew.oldest_ready_wall.is_some(),
        "ready steps with audit rows report a wall-clock birth"
    );

    let ship = node("ship");
    assert_eq!((ship.ready, ship.active, ship.unassigned), (0, 1, 0));
    assert!(ship.by_role.is_empty());

    let deliver = node("Deliver");
    assert_eq!(
        (deliver.ready, deliver.active, deliver.unassigned),
        (1, 0, 1),
        "slug-less steps group under their title, not vanish"
    );
    assert!(
        deliver.oldest_ready_wall.is_none(),
        "no audit row → no fabricated age"
    );

    assert!(
        !fleet.nodes.iter().any(|n| n.slug == "plan"),
        "pending steps are not in flight"
    );
}
