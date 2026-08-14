//! The `steps` View source, and the scoping detail that makes it
//! different from Jobs.
//!
//! Steps were the last of the four primitives without a read surface.
//! The detail worth pinning is the column the owner scope lands on: a
//! Job's owner is `owner_id`, but a Step's owner is whoever it is
//! ASSIGNED to. Applying a Job-shaped scope to Steps would filter on a
//! column Steps do not have, and "my ready steps" — the question this
//! source exists to answer — would return nothing.

use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::TestDb;
use boss_views::port::{ViewResolver, ViewsRepo};
use boss_views::types::{ViewInput, ViewLayout, ViewSource, Visibility};
use std::sync::Arc;

fn user(id: &str, role: &str) -> User {
    User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// One Job with two Steps, assigned to different people.
async fn seed(pool: &sqlx::PgPool) {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, 'wholesale-keg-order', 'account', 'acc-1', 'T', 'emp-owner', 'standard', \
                 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .expect("job inserts");

    for (assignee, status, kind) in [
        ("emp-alice", "ready", "checklist"),
        ("emp-bob", "completed", "sign-off"),
    ] {
        sqlx::query(
            "INSERT INTO steps (id, job_id, kind, title, assignee_id, status, sort_order) \
             VALUES ($1, $2, $3, 'S', $4, $5, 1)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(job_id)
        .bind(kind)
        .bind(assignee)
        .bind(status)
        .execute(pool)
        .await
        .expect("step inserts");
    }
}

async fn run(
    pool: &sqlx::PgPool,
    policy: Arc<dyn PolicyClient>,
    filter: &str,
    who: &User,
) -> boss_views::types::ViewResults {
    let repo = boss_views::PgViewsRepo::new(pool.clone());
    let view = repo
        .create(
            &who.id,
            &ViewInput {
                title: "t".into(),
                source: ViewSource::Steps,
                filter: filter.into(),
                columns: vec![],
                layout: ViewLayout::Table,
                visibility: Visibility::Private,
            },
        )
        .await
        .expect("view creates");
    boss_views::PgViewResolver::new(pool.clone(), policy)
        .resolve(&view, who, 50)
        .await
        .expect("resolve succeeds")
}

/// Everything readable, so the source itself can be exercised.
fn unrestricted() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("ops", Action::Read, Resource::step(), Scope::All)
            .build(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_are_readable_and_filterable() {
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let all = run(&db.pool, unrestricted(), "", &user("emp-alice", "ops")).await;
    assert_eq!(all.matched, 2, "both steps visible to an unrestricted role");

    let ready = run(
        &db.pool,
        unrestricted(),
        "status = \"ready\"",
        &user("emp-alice", "ops"),
    )
    .await;
    assert_eq!(ready.matched, 1);
    assert_eq!(ready.pushed_down, 1, "status pushes into SQL");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_scoped_role_sees_the_steps_assigned_to_them() {
    // THE case. `Scope::Self_` yields OwnerIs{user_id}, and for Steps
    // that must land on `assignee_id`. Filtering `owner_id` instead —
    // the Job-shaped reading — would match nothing, because Steps have
    // no such column, and "my ready steps" would be permanently empty.
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("brewer", Action::Read, Resource::step(), Scope::Self_)
            .build(),
    );

    let alice = run(&db.pool, policy.clone(), "", &user("emp-alice", "brewer")).await;
    assert_eq!(alice.matched, 1, "alice sees only her own step");

    let bob = run(&db.pool, policy.clone(), "", &user("emp-bob", "brewer")).await;
    assert_eq!(bob.matched, 1, "bob sees only his");

    let stranger = run(&db.pool, policy, "", &user("emp-nobody", "brewer")).await;
    assert_eq!(stranger.matched, 0, "someone with no steps sees none");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_role_with_no_step_grant_sees_nothing() {
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let denied: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::deny_all());
    let out = run(&db.pool, denied, "", &user("emp-alice", "guest")).await;
    assert_eq!(out.matched, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_and_pushdown_compose() {
    // The scope clause is $2 and pushdown terms start at $3; getting
    // that numbering wrong swaps a bound value into the wrong column.
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("brewer", Action::Read, Resource::step(), Scope::Self_)
            .build(),
    );

    let mine_and_ready = run(
        &db.pool,
        policy.clone(),
        "status = \"ready\"",
        &user("emp-alice", "brewer"),
    )
    .await;
    assert_eq!(mine_and_ready.matched, 1);

    // Bob's step is completed, so the same filter under his scope
    // matches nothing — proving both clauses apply, not just one.
    let bobs = run(
        &db.pool,
        policy,
        "status = \"ready\"",
        &user("emp-bob", "brewer"),
    )
    .await;
    assert_eq!(bobs.matched, 0);
}
