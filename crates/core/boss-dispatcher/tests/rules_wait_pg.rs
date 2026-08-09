//! `wait_for_rules` — the empty-at-boot dead-air fix (backlog
//! `823fcb22`, mechanism 2).
//!
//! The runner loads `dispatcher_rules` once; before this fix, an
//! empty table at boot (init/seed race, or simply a fresh deployment
//! whose first rule arrives later) logged "no rules registered" and
//! returned — permanently consuming nothing, indistinguishable from
//! healthy until someone asks why no side effects ever fire.
//!
//! Contracts:
//! 1. **Rules arriving during the wait are picked up** — the seed
//!    race self-heals instead of dead-airing.
//! 2. **The wait is bounded and loud, not infinite**: past
//!    `max_wait` it proceeds with whatever exists (an empty registry
//!    logged as an error), so a deployment that genuinely has no
//!    rules still boots the rest of the dispatcher.

use std::time::Duration;

use boss_dispatcher::rules::registry::wait_for_rules;
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn rules_arriving_during_the_wait_are_picked_up() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();

    // TestDb seeds the full rules set via the schema; empty the table
    // to reproduce the boot race.
    sqlx::query("DELETE FROM dispatcher_rules")
        .execute(&pool)
        .await
        .expect("empty the table");

    let insert_pool = pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        sqlx::query(
            "INSERT INTO dispatcher_rules \
             (name, version, status, on_event, when_expr, do_steps, delay, \
              schedule_cadence, schedule_anchor, schedule_calendar) \
             VALUES ('late-seed', 1, 'active', 'step.done.task', NULL, \
                     '[{\"handler\":\"messages.notify\",\"args\":{}}]'::jsonb, \
                     NULL, NULL, NULL, NULL)",
        )
        .execute(&insert_pool)
        .await
        .expect("late seed");
    });

    let raw = wait_for_rules(&pool, Duration::from_millis(100), Duration::from_secs(10))
        .await
        .expect("load");
    assert_eq!(raw.rules.len(), 1, "the late seed is picked up");
    assert_eq!(raw.rules[0].name, "late-seed");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_is_bounded_and_proceeds_empty() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    sqlx::query("DELETE FROM dispatcher_rules")
        .execute(&pool)
        .await
        .expect("empty the table");

    let started = std::time::Instant::now();
    let raw = wait_for_rules(&pool, Duration::from_millis(50), Duration::from_millis(400))
        .await
        .expect("load");
    assert!(raw.rules.is_empty(), "genuinely-empty proceeds empty");
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "the wait actually waited before giving up"
    );
}
