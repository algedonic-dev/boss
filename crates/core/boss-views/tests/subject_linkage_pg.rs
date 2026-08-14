//! Every way an event names its Subject must survive the projection.
//!
//! This is the shape-drift class, and it has bitten twice. The
//! projection read the flat `payload.subject_id` keys only, while
//! v1.1.0 had moved Subjects to the nested identity-first
//! `{"id": …, "subject_kind": …}` — so 21,979 events carrying perfect
//! identity projected as unlinked. Separately, step events name their
//! Job rather than their Subject, leaving another 449,859 one join
//! short. Together the log was 16% linked when ~77% was reachable, and
//! "everything that happened to this Subject" was a claim the data did
//! not support.
//!
//! Unit tests cannot see any of this: the resolution lives in SQL, and
//! the bug is a disagreement between that SQL and the shapes real
//! producers emit. So this test writes each shape into `audit_log` and
//! asserts what comes out the other side.

use boss_testing::TestDb;
use sqlx::Row;

/// Insert one audit_log row with the given payload, returning its id.
async fn write_event(pool: &sqlx::PgPool, kind: &str, payload: serde_json::Value) -> i64 {
    let row = sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'test', $1, $2) RETURNING id",
    )
    .bind(kind)
    .bind(&payload)
    .fetch_one(pool)
    .await
    .expect("audit row inserts");
    row.get::<i64, _>("id")
}

async fn linkage(pool: &sqlx::PgPool, audit_id: i64) -> (Option<String>, Option<String>) {
    let row = sqlx::query("SELECT subject_kind, subject_id FROM event_facts WHERE audit_id = $1")
        .bind(audit_id)
        .fetch_one(pool)
        .await
        .expect("projected row exists");
    (
        row.get::<Option<String>, _>("subject_kind"),
        row.get::<Option<String>, _>("subject_id"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn every_subject_shape_projects_with_linkage() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // 1. Flat keys — the pre-v1.1.0 shape, still emitted by domain
    //    events. This was the only one the projection understood.
    let flat = write_event(
        pool,
        "ledger.tax.remitted",
        serde_json::json!({"subject_kind": "account", "subject_id": "acc-flat"}),
    )
    .await;

    // 2. Nested identity-first — what v1.1.0 moved Subjects to, and
    //    what `jobs.job.created` actually emits today.
    let nested = write_event(
        pool,
        "jobs.job.created",
        serde_json::json!({"subject": {"subject_kind": "account", "id": "acc-nested"}}),
    )
    .await;

    // 3. Through the Job — a step event names its Job, and the Job
    //    knows its Subject. The largest kinds in the log look like
    //    this.
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, 'wholesale-keg-order', 'account', 'acc-viajob', 'T', 'emp-1', 'standard', \
                 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .expect("job inserts");
    let via_job = write_event(
        pool,
        "jobs.step.updated",
        serde_json::json!({"job_id": job_id.to_string(), "status": "completed"}),
    )
    .await;

    // 4. Genuinely unlinked — no subject, no job. Must stay NULL
    //    rather than acquiring a linkage from somewhere.
    let orphan = write_event(
        pool,
        "products.inventory.upserted",
        serde_json::json!({"sku": "KEG-HALF", "qty": 4}),
    )
    .await;

    // 5. A job_id that is not a uuid. The join casts the COLUMN to
    //    text rather than the payload value to uuid, so this must
    //    simply not match — casting the other way would fail the whole
    //    batch on one malformed row.
    let bad_job_ref = write_event(
        pool,
        "jobs.step.updated",
        serde_json::json!({"job_id": "not-a-uuid"}),
    )
    .await;

    let report = boss_views::rebuild_event_facts(pool)
        .await
        .expect("rebuild succeeds");
    assert_eq!(report.rows_projected, 5, "every event projects");

    assert_eq!(
        linkage(pool, flat).await,
        (Some("account".into()), Some("acc-flat".into())),
        "flat subject keys"
    );
    assert_eq!(
        linkage(pool, nested).await,
        (Some("account".into()), Some("acc-nested".into())),
        "nested identity-first subject — the shape v1.1.0 moved to"
    );
    assert_eq!(
        linkage(pool, via_job).await,
        (Some("account".into()), Some("acc-viajob".into())),
        "resolved through the Job the step belongs to"
    );
    assert_eq!(
        linkage(pool, orphan).await,
        (None, None),
        "an event with no subject and no job must stay unlinked"
    );
    assert_eq!(
        linkage(pool, bad_job_ref).await,
        (None, None),
        "a malformed job_id must not match, and must not fail the batch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registered_edge_whose_field_is_absent_yields_no_kind_either() {
    // Kind and id must resolve as a PAIR. `products.inventory.upserted`
    // is registered against `product_sku` -> product, so resolving the
    // two independently stamped `subject_kind = product` on an event
    // with no `product_sku` at all — a row claiming to be about a
    // Subject it could not name. Downstream that reads as a product
    // with a missing id rather than as an unlinked event.
    let db = TestDb::new().await;
    let pool = &db.pool;

    let absent = write_event(
        pool,
        "products.inventory.upserted",
        serde_json::json!({"sku": "KEG-HALF", "qty": 4}),
    )
    .await;

    boss_views::rebuild_event_facts(pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        linkage(pool, absent).await,
        (None, None),
        "no id means no kind — never a half-resolved Subject"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registered_domain_id_resolves_through_the_registry() {
    // The domain-id case: the event names its Subject by `product_sku`,
    // and `subject_edges` is what says so. This crate holds no per-kind
    // knowledge — adding a kind is a row, not a branch.
    let db = TestDb::new().await;
    let pool = &db.pool;

    let ev = write_event(
        pool,
        "products.inventory.upserted",
        serde_json::json!({"product_sku": "FP-HAZY-1-2-BBL", "on_hand": 12}),
    )
    .await;

    boss_views::rebuild_event_facts(pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        linkage(pool, ev).await,
        (Some("product".into()), Some("FP-HAZY-1-2-BBL".into())),
        "resolved via the subject_edges registry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_lifecycle_event_resolves_through_its_own_id() {
    // `jobs.job.closed` and `jobs.job.status_changed` name their Job as
    // their own `id`, not as `job_id`. Keying the hop only on `job_id`
    // left them unlinked despite being about a Job that knows its
    // Subject.
    //
    // Reading `id` as a Job reference is safe because the join is the
    // evidence: a value that matches a `jobs` row IS that Job. The two
    // negatives below are the guard.
    let db = TestDb::new().await;
    let pool = &db.pool;

    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, 'wholesale-keg-order', 'account', 'acc-closed', 'T', 'emp-1', 'standard', \
                 'closed', CURRENT_DATE)",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .expect("job inserts");

    let closed = write_event(
        pool,
        "jobs.job.closed",
        serde_json::json!({"id": job_id.to_string(), "outcome": "delivered"}),
    )
    .await;

    // A prefixed `id` on a kind with no registered edge must not link.
    // (Deliberately not `messages.message.sent` — that kind IS
    // registered against `id`, so it resolves through the registry and
    // proves nothing about the hop.)
    let unrelated = write_event(
        pool,
        "some.unregistered.thing",
        serde_json::json!({"id": "msg-not-a-job", "body": "hi"}),
    )
    .await;

    // A uuid `id` that matches no job must not link either.
    let stray_uuid = write_event(
        pool,
        "some.other.thing",
        serde_json::json!({"id": uuid::Uuid::new_v4().to_string()}),
    )
    .await;

    boss_views::rebuild_event_facts(pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        linkage(pool, closed).await,
        (Some("account".into()), Some("acc-closed".into())),
        "a job-lifecycle event resolves through the Job named by its id"
    );
    assert_eq!(
        linkage(pool, unrelated).await,
        (None, None),
        "an unregistered kind with a prefixed id links to nothing"
    );
    assert_eq!(
        linkage(pool, stray_uuid).await,
        (None, None),
        "a uuid matching no job links to nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_subject_wins_over_the_job_hop() {
    // Precedence matters: an event that carries its own Subject AND
    // names a Job is about the Subject it names. Resolving through the
    // Job instead would silently re-attribute it.
    let db = TestDb::new().await;
    let pool = &db.pool;

    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, 'wholesale-keg-order', 'account', 'acc-from-job', 'T', 'emp-1', 'standard', \
                 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .expect("job inserts");

    let both = write_event(
        pool,
        "jobs.step.updated",
        serde_json::json!({
            "job_id": job_id.to_string(),
            "subject_kind": "asset",
            "subject_id": "ast-explicit",
        }),
    )
    .await;

    boss_views::rebuild_event_facts(pool)
        .await
        .expect("rebuild succeeds");

    assert_eq!(
        linkage(pool, both).await,
        (Some("asset".into()), Some("ast-explicit".into())),
        "the event's own Subject wins over the Job's"
    );
}
