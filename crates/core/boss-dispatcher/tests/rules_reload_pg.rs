//! Live reload of the rules registry (backlog `1e576baf`): the
//! dispatcher loads `dispatcher_rules` once at boot, so an authored
//! rule silently does nothing until the next restart. The fix is a
//! supervision loop in the binary that polls a content fingerprint
//! and rebuilds + rebinds both runners when it moves. These tests pin
//! the two primitives the loop stands on:
//!
//! 1. **The fingerprint tracks content, not just row count** — an
//!    INSERT moves it, and so does a status flip on an existing row
//!    (publish/retire UPDATE in place; a count-based fingerprint
//!    would miss exactly the authoring lifecycle this exists for).
//! 2. **`rules_changed` resolves on divergence** — it returns the new
//!    fingerprint once the table no longer matches the one handed in,
//!    and keeps waiting while it does.

use std::time::Duration;

use boss_dispatcher::rules::registry::{rules_changed, rules_fingerprint};
use boss_testing::TestDb;

async fn seed_rule(pool: &sqlx::PgPool, name: &str, status: &str) {
    sqlx::query(
        "INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps) \
         VALUES ($1, 1, $2, 'step.done.x', NULL, '[{\"handler\":\"jobs.spawn\",\"args\":{}}]'::jsonb)",
    )
    .bind(name)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fingerprint_moves_on_insert_and_status_flip() {
    let db = TestDb::new().await;

    let empty = rules_fingerprint(&db.pool).await.unwrap();
    let empty_again = rules_fingerprint(&db.pool).await.unwrap();
    assert_eq!(empty, empty_again, "stable when nothing changes");

    seed_rule(&db.pool, "r-reload", "active").await;
    let after_insert = rules_fingerprint(&db.pool).await.unwrap();
    assert_ne!(empty, after_insert, "an INSERT moves the fingerprint");

    // Retire in place — same row count, different content. This is the
    // authoring lifecycle (publish retires the prior active version).
    sqlx::query("UPDATE dispatcher_rules SET status = 'retired' WHERE name = 'r-reload'")
        .execute(&db.pool)
        .await
        .unwrap();
    let after_flip = rules_fingerprint(&db.pool).await.unwrap();
    assert_ne!(
        after_insert, after_flip,
        "a status UPDATE moves the fingerprint at unchanged row count"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_changed_resolves_on_divergence_and_waits_while_matching() {
    let db = TestDb::new().await;
    seed_rule(&db.pool, "r-watch", "active").await;
    let fp = rules_fingerprint(&db.pool).await.unwrap();

    // While the table matches, the watcher waits: give it a generous
    // window to (wrongly) resolve and assert it does not.
    let waiting = rules_changed(&db.pool, &fp, Duration::from_millis(25));
    let premature = tokio::time::timeout(Duration::from_millis(250), waiting).await;
    assert!(
        premature.is_err(),
        "must keep waiting while the table matches the fingerprint"
    );

    // Diverge from a spawned task; the watcher must resolve with the
    // new fingerprint within a bounded wait.
    let pool = db.pool.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        seed_rule(&pool, "r-watch-2", "active").await;
    });
    let new_fp = tokio::time::timeout(
        Duration::from_secs(5),
        rules_changed(&db.pool, &fp, Duration::from_millis(25)),
    )
    .await
    .expect("watcher resolves once the table diverges");
    writer.await.unwrap();
    assert_ne!(fp, new_fp, "resolves with the fingerprint that moved");
    assert_eq!(
        new_fp,
        rules_fingerprint(&db.pool).await.unwrap(),
        "returned fingerprint is the current one"
    );
}
