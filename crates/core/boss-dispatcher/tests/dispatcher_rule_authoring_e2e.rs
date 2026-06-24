//! Authoring writes for `dispatcher_rules`: the versioning + activation
//! invariants behind the rule-authoring UX (mirrors the step_plugins
//! registry tests). draft → publish (activates, retires prior active) →
//! retire; `validate` rejects un-loadable rules before they persist.

use std::collections::HashMap;

use boss_dispatcher::rules::authoring::{
    create_draft, get_active, list_versions, publish, retire, validate,
};
use boss_dispatcher::rules::registry::{RawDoStep, RawRule};
use boss_testing::TestDb;

fn rule(name: &str, on_event: &str, when: Option<&str>) -> RawRule {
    RawRule {
        name: name.to_string(),
        on_event: on_event.to_string(),
        when: when.map(|s| s.to_string()),
        do_steps: vec![RawDoStep {
            handler: "jobs.spawn".into(),
            args: HashMap::new(),
        }],
        delay: None,
        version: 1,
    }
}

async fn status_of(db: &TestDb, name: &str, version: i32) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM dispatcher_rules WHERE name = $1 AND version = $2")
        .bind(name)
        .bind(version)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
}

async fn active_count(db: &TestDb, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM dispatcher_rules WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_publish_retire_lifecycle() {
    let db = TestDb::new().await;

    // create draft v1
    let d1 = create_draft(&db.pool, &rule("r1", "step.done.x", None))
        .await
        .unwrap();
    assert_eq!(d1.version, 1);
    assert_eq!(d1.status, "draft");
    assert!(
        get_active(&db.pool, "r1").await.is_err(),
        "draft is not active"
    );

    // publish → v1 active
    let a1 = publish(&db.pool, "r1").await.unwrap();
    assert_eq!((a1.version, a1.status.as_str()), (1, "active"));
    assert_eq!(get_active(&db.pool, "r1").await.unwrap().version, 1);

    // create draft v2 + publish → v2 active, v1 retired, still exactly one active
    create_draft(&db.pool, &rule("r1", "step.done.y", None))
        .await
        .unwrap();
    let a2 = publish(&db.pool, "r1").await.unwrap();
    assert_eq!((a2.version, a2.status.as_str()), (2, "active"));
    assert_eq!(status_of(&db, "r1", 1).await.as_deref(), Some("retired"));
    assert_eq!(get_active(&db.pool, "r1").await.unwrap().version, 2);
    assert_eq!(active_count(&db, "r1").await, 1, "one active per name");

    // list_versions: both, oldest first
    let versions: Vec<i32> = list_versions(&db.pool, "r1")
        .await
        .unwrap()
        .iter()
        .map(|v| v.version)
        .collect();
    assert_eq!(versions, vec![1, 2]);

    // retire → no active
    retire(&db.pool, "r1").await.unwrap();
    assert_eq!(status_of(&db, "r1", 2).await.as_deref(), Some("retired"));
    assert!(get_active(&db.pool, "r1").await.is_err());
    assert_eq!(active_count(&db, "r1").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_rejects_unloadable_rule_and_create_draft_persists_nothing() {
    // bad `when` expr
    assert!(validate(&rule("bad", "step.done.x", Some("a AND"))).is_err());
    // bad topic (empty segment)
    assert!(validate(&rule("bad", "step..x", None)).is_err());
    // a clean rule validates
    assert!(validate(&rule("ok", "step.done.x", Some("a > 1"))).is_ok());

    // create_draft rejects an invalid rule and writes no row
    let db = TestDb::new().await;
    assert!(
        create_draft(&db.pool, &rule("bad", "step..x", None))
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM dispatcher_rules WHERE name = 'bad'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "invalid draft must not persist");
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_without_draft_errors() {
    let db = TestDb::new().await;
    assert!(publish(&db.pool, "nonexistent").await.is_err());
}
