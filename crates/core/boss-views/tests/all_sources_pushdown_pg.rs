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

/// Backlog 2b5ad29a: a department's IN / WORKING / OUT question is
/// `jobs.metadata.department`, and no View could ask it — neither
/// `metadata` nor `partition` was a field of the jobs source. Both are,
/// and both push into SQL rather than filtering the newest 5,000 rows.
#[tokio::test(flavor = "multi_thread")]
async fn jobs_reach_their_metadata_and_partition_in_sql() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on, \
             metadata, partition, simulated) \
         VALUES (gen_random_uuid(), 'backlog-item', 'account', 'acc-keep', 'T', 'emp-alice', \
                 'standard', 'open', CURRENT_DATE, '{\"department\": \"it\"}', 'simulated', true)",
    )
    .execute(&db.pool)
    .await
    .expect("job with metadata inserts");
    let u = user("emp-alice");

    let it = run(
        &db.pool,
        open_policy(),
        ViewSource::Jobs,
        "metadata.department = \"it\"",
        &u,
    )
    .await;
    assert_eq!(it.matched, 1, "the one job whose department is it");
    assert_eq!(
        it.pushed_down, 1,
        "metadata.<path> pushes, like payload.<path>"
    );
    assert_eq!(it.rows[0]["metadata"]["department"], "it");

    let sim = run(
        &db.pool,
        open_policy(),
        ViewSource::Jobs,
        "partition = \"simulated\"",
        &u,
    )
    .await;
    assert_eq!(sim.matched, 1);
    assert_eq!(sim.pushed_down, 1, "partition is a text column and pushes");
}

/// The served fields ARE the row: one list per source builds the SELECT,
/// the JSON row and what `GET /api/views/sources` offers, so the column
/// picker cannot offer a field the row lacks or miss one it carries
/// (backlog 4a8939b5 — the events picker had lost two). Checked against
/// real rows because the row is built from SQL.
#[tokio::test(flavor = "multi_thread")]
async fn every_source_row_carries_exactly_the_served_fields() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    sqlx::query(
        "INSERT INTO steps (id, job_id, kind, title, assignee_id, status, sort_order) \
         SELECT gen_random_uuid(), id, 'checklist', 'S', 'emp-alice', 'ready', 1 FROM jobs LIMIT 1",
    )
    .execute(&db.pool)
    .await
    .expect("step inserts");
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'test', 'test.happened', '{}')",
    )
    .execute(&db.pool)
    .await
    .expect("event inserts");
    // The events source reads the projection, which only its rebuilder
    // writes.
    boss_views::rebuild_event_facts(&db.pool)
        .await
        .expect("event_facts rebuilds");

    let served = boss_views::query::view_sources();
    for schema in &served.sources {
        let out = run(
            &db.pool,
            open_policy(),
            schema.source,
            "",
            &user("emp-alice"),
        )
        .await;
        let row = out
            .rows
            .first()
            .unwrap_or_else(|| panic!("{}: a seeded row", schema.source.as_str()));
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut fields: Vec<&str> = schema.fields.iter().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(keys, fields, "{}", schema.source.as_str());
    }
}

/// The events source binds the scan ceiling at `$1` like every other
/// source now (it used to interpolate it and start its terms at `$1`),
/// so its filter terms start at `$2`. A text term and a payload path
/// both push, and both still answer.
#[tokio::test(flavor = "multi_thread")]
async fn events_push_down_behind_the_bound_ceiling() {
    let db = TestDb::new().await;
    for (kind, sku) in [
        ("stock.moved", "FP-1"),
        ("stock.moved", "FP-2"),
        ("other", "FP-1"),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES (gen_random_uuid(), NOW(), 'test', $1, $2)",
        )
        .bind(kind)
        .bind(serde_json::json!({ "sku": sku }))
        .execute(&db.pool)
        .await
        .expect("event inserts");
    }
    boss_views::rebuild_event_facts(&db.pool)
        .await
        .expect("event_facts rebuilds");
    let u = user("emp-alice");

    let both = run(
        &db.pool,
        open_policy(),
        ViewSource::Events,
        "kind = \"stock.moved\" AND payload.sku = \"FP-1\"",
        &u,
    )
    .await;
    assert_eq!(both.matched, 1);
    assert_eq!(both.pushed_down, 2);
}
