//! Postgres adapter round-trip: rule CRUD + audit log integrity.

#![cfg(feature = "postgres")]

use boss_policy::port::PolicyRepository;
use boss_policy::postgres::PgPolicy;
use boss_policy::{Action, PolicyRule, Resource, Scope, UserOverride};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn rule_upsert_and_lookup_round_trip() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let rule = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::Territory);
    policy.upsert_rule(&rule, "test").await.expect("upsert");

    let back = policy
        .rule_for(&rule.id)
        .await
        .expect("rule_for")
        .expect("present");
    assert_eq!(back.role, "sales-rep");
    assert_eq!(back.scope, Scope::Territory);
    assert!(back.active);

    // Upserting again updates scope + leaves id stable.
    let rule2 = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::All);
    policy
        .upsert_rule(&rule2, "test2")
        .await
        .expect("re-upsert");
    let back2 = policy.rule_for(&rule.id).await.unwrap().unwrap();
    assert_eq!(back2.scope, Scope::All);

    // Two audit rows exist (both upserts).
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM policy_rule_audit WHERE target_id = $1")
            .bind(&rule.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn deactivate_rule_hides_from_list() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let r = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::Self_);
    policy.upsert_rule(&r, "t").await.unwrap();
    assert_eq!(policy.list_rules().await.unwrap().len(), 1);

    policy.deactivate_rule(&r.id, "t").await.unwrap();
    assert_eq!(policy.list_rules().await.unwrap().len(), 0);

    // rule_for still returns the row (with active=false) for admin inspection.
    let back = policy.rule_for(&r.id).await.unwrap().unwrap();
    assert!(!back.active);

    // One upsert + one deactivate in audit.
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM policy_rule_audit WHERE target_id = $1")
            .bind(&r.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_override_respects_expiry() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let active = UserOverride {
        id: "ov-active".into(),
        user_id: "emp-1".into(),
        resource: Resource::employee(),
        action: Action::Read,
        scope: Scope::All,
        reason: "covering HR".into(),
        expires_at: None,
    };
    let expired = UserOverride {
        id: "ov-expired".into(),
        user_id: "emp-1".into(),
        resource: Resource::job(),
        action: Action::Close,
        scope: Scope::All,
        reason: "old".into(),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    };
    policy.upsert_user_override(&active, "t").await.unwrap();
    policy.upsert_user_override(&expired, "t").await.unwrap();

    let list = policy.list_user_overrides("emp-1").await.unwrap();
    assert_eq!(list.len(), 1, "expired override should be filtered out");
    assert_eq!(list[0].id, "ov-active");
}
