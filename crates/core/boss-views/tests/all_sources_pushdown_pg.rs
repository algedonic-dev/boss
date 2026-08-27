//! Pushdown on every source, and the placeholder numbering that
//! differs between them.
//!
//! Each source has a different prefix of bound parameters before the
//! filter's own terms: subjects has just the limit ($1), jobs and
//! steps also carry an owner-scope array ($2), so their pushdown
//! starts at $3. Getting that offset wrong binds a filter value into
//! the scope slot — which does not error, it silently returns the
//! wrong rows. These tests exist for that failure, not for the happy
//! path.

use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::TestDb;
use boss_views::port::{ViewResolver, ViewsRepo};
use boss_views::types::{ViewInput, ViewLayout, ViewSource, Visibility};
use std::sync::Arc;

fn user(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "ops".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// Read everything, so these tests exercise pushdown rather than
/// policy.
fn open_policy() -> Arc<dyn PolicyClient> {
    let mut b = FakePolicyClient::builder();
    for r in [
        Resource::job(),
        Resource::step(),
        Resource::subject(),
        Resource::event(),
    ] {
        b = b.allow("ops", Action::Read, r, Scope::All);
    }
    Arc::new(b.build())
}

async fn seed(pool: &sqlx::PgPool) {
    for (kind, id, label) in [
        ("account", "acc-keep", "Keeper Brewing"),
        ("account", "acc-other", "Other Co"),
        ("vendor", "vnd-1", "Hop Supply"),
    ] {
        sqlx::query("INSERT INTO subjects (kind, id, label) VALUES ($1, $2, $3)")
            .bind(kind)
            .bind(id)
            .bind(label)
            .execute(pool)
            .await
            .expect("subject inserts");
    }

    for (kind, subject_id, owner, status) in [
        ("wholesale-keg-order", "acc-keep", "emp-alice", "open"),
        ("wholesale-keg-order", "acc-other", "emp-bob", "open"),
        ("sale", "acc-keep", "emp-alice", "closed"),
    ] {
        sqlx::query(
            "INSERT INTO jobs \
                (id, kind, subject_kind, subject_id, title, owner_id, priority, status, \
                 opened_on) \
             VALUES (gen_random_uuid(), $1, 'account', $2, 'T', $3, 'standard', $4, \
                     CURRENT_DATE)",
        )
        .bind(kind)
        .bind(subject_id)
        .bind(owner)
        .bind(status)
        .execute(pool)
        .await
        .expect("job inserts");
    }
}

async fn run(
    pool: &sqlx::PgPool,
    policy: Arc<dyn PolicyClient>,
    source: ViewSource,
    filter: &str,
    who: &User,
) -> boss_views::types::ViewResults {
    let repo = boss_views::PgViewsRepo::new(pool.clone());
    let view = repo
        .create(
            &who.id,
            &ViewInput {
                title: "t".into(),
                source,
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

#[tokio::test(flavor = "multi_thread")]
async fn subjects_push_down_on_kind_and_id() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let u = user("emp-alice");

    let accounts = run(
        &db.pool,
        open_policy(),
        ViewSource::Subjects,
        "kind = \"account\"",
        &u,
    )
    .await;
    assert_eq!(accounts.matched, 2);
    assert_eq!(accounts.pushed_down, 1);

    // `id` is TEXT on subjects, so it pushes — unlike the uuid ids
    // elsewhere.
    let one = run(
        &db.pool,
        open_policy(),
        ViewSource::Subjects,
        "id = \"acc-keep\"",
        &u,
    )
    .await;
    assert_eq!(one.matched, 1);
    assert_eq!(one.pushed_down, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_push_down_alongside_the_owner_scope() {
    // THE numbering case. Jobs bind the limit at $1 and the owner
    // scope at $2, so filter terms start at $3. Bind a filter value
    // into $2 and it lands in the scope array: no error, wrong rows.
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let scoped: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ops", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );

    // alice owns two jobs; one is a wholesale order.
    let alice_all = run(
        &db.pool,
        scoped.clone(),
        ViewSource::Jobs,
        "",
        &user("emp-alice"),
    )
    .await;
    assert_eq!(alice_all.matched, 2, "self scope holds without a filter");

    let alice_filtered = run(
        &db.pool,
        scoped.clone(),
        ViewSource::Jobs,
        "kind = \"wholesale-keg-order\"",
        &user("emp-alice"),
    )
    .await;
    assert_eq!(
        alice_filtered.matched, 1,
        "scope AND filter both applied, not one or the other"
    );
    assert_eq!(alice_filtered.pushed_down, 1);

    // Bob owns the other wholesale order. Same filter, different
    // scope: if the filter value had leaked into the scope slot these
    // two would not differ.
    let bob = run(
        &db.pool,
        scoped,
        ViewSource::Jobs,
        "kind = \"wholesale-keg-order\"",
        &user("emp-bob"),
    )
    .await;
    assert_eq!(bob.matched, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_push_multiple_terms_and_a_set() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let u = user("emp-alice");

    let two_terms = run(
        &db.pool,
        open_policy(),
        ViewSource::Jobs,
        "kind = \"wholesale-keg-order\" AND status = \"open\"",
        &u,
    )
    .await;
    assert_eq!(two_terms.matched, 2);
    assert_eq!(two_terms.pushed_down, 2);

    let set = run(
        &db.pool,
        open_policy(),
        ViewSource::Jobs,
        "kind = \"wholesale-keg-order\" OR kind = \"sale\"",
        &u,
    )
    .await;
    assert_eq!(set.matched, 3);
    assert_eq!(set.pushed_down, 2, "an OR-set counts both terms");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpushable_filter_still_answers_via_the_residual() {
    // `title` has no descriptor entry, so nothing pushes — and the
    // residual must still apply it.
    let db = TestDb::new().await;
    seed(&db.pool).await;

    let out = run(
        &db.pool,
        open_policy(),
        ViewSource::Jobs,
        "title = \"T\"",
        &user("emp-alice"),
    )
    .await;
    assert_eq!(out.pushed_down, 0, "nothing pushable");
    assert_eq!(out.matched, 3, "residual still filtered correctly");
}
