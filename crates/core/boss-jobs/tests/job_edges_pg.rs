//! The job_edges registry — job-to-job links as data
//! (department-flow-dashboards Q1, decided 2026-08-09: registry).
//!
//! Contracts pinned:
//! 1. **The registry seeds exactly the declared edges** — instruments
//!    derive topology from rows, never from hardcoded key names. The
//!    roster, every field of every row including the `abort` dial, is
//!    stated once for BOTH adapters in
//!    `the_adapters_agree_on_the_job_edges_registry_pg.rs`, which
//!    replaced the Postgres-only roster test that lived here (backlog
//!    be459ab9).
//! 2. **Resolution is prefix-aware**: an exact Job id resolves; an
//!    unambiguous prefix of length >= 8 resolves (the folklore's
//!    dominant shape, measured live); a garbage value does not.
//! 3. **The dial is real in both directions**: shipped default is
//!    'abort' since migration 105 (the folklore was cleaned first);
//!    dialing an edge back to 'warn' permits the dirty write again.
//!    Both directions pinned by flipping one edge.

use boss_testing::TestDb;

async fn seed_job(pool: &sqlx::PgPool, kind: &str) -> uuid::Uuid {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, $2, 'custom', 'main', 'T', 'emp-o', 'standard', 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .bind(kind)
    .execute(pool)
    .await
    .expect("job");
    job_id
}

/// Writes go through a connection with the ref-check hatch RE-ENABLED:
/// TestDb sets `audit_log.ref_check = 'off'` database-wide (its
/// restore hatch, which this trigger honors), so exercising the guard
/// requires turning it back on for the session — otherwise every
/// assertion here passes vacuously against a disabled trigger.
async fn guarded_conn(pool: &sqlx::PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut conn = pool.acquire().await.expect("conn");
    sqlx::query("SET audit_log.ref_check = 'on'")
        .execute(&mut *conn)
        .await
        .expect("re-enable ref check");
    conn
}

async fn set_meta(
    conn: &mut sqlx::PgConnection,
    id: uuid::Uuid,
    metadata: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE jobs SET metadata = $2 WHERE id = $1")
        .bind(id)
        .bind(metadata)
        .execute(conn)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn warn_permits_dirty_links_and_prefixes_resolve() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // Shipped default is abort (105); this test pins the warn
    // direction of the dial explicitly.
    sqlx::query(
        "UPDATE job_edges SET on_missing = 'warn'          WHERE source_kind = 'ship-a-change' AND field_path = 'backlog_item'",
    )
    .execute(pool)
    .await
    .expect("dial back to warn");

    let target = seed_job(pool, "pr-train").await;
    let src = seed_job(pool, "ship-a-change").await;

    let mut conn = guarded_conn(pool).await;
    // Exact id: clean.
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "train": target.to_string() }),
    )
    .await
    .expect("exact id resolves");

    // Unambiguous 8-char prefix: the folklore's shape, resolves.
    let prefix = target.to_string()[..8].to_string();
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": prefix }),
    )
    .await
    .expect("unambiguous >=8 prefix resolves");

    // Garbage under on_missing=warn: warns, still lands.
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": "not-a-job-anywhere" }),
    )
    .await
    .expect("warn must not break the writer");

    // List field on the train side, mixed clean + prefix.
    let p1 = seed_job(pool, "ship-a-change").await;
    set_meta(
        &mut conn,
        target,
        serde_json::json!({ "boarded_jobs": [p1.to_string(), src.to_string()[..8]] }),
    )
    .await
    .expect("list edges resolve per element");
}

#[tokio::test]
async fn the_abort_dial_refuses_what_warn_permits() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    sqlx::query(
        "UPDATE job_edges SET on_missing = 'abort' \
         WHERE source_kind = 'ship-a-change' AND field_path = 'backlog_item'",
    )
    .execute(pool)
    .await
    .expect("dial to abort");

    let src = seed_job(pool, "ship-a-change").await;
    let mut conn = guarded_conn(pool).await;
    let err = set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": "not-a-job-anywhere" }),
    )
    .await
    .expect_err("abort must refuse the unresolvable link");
    let msg = err.to_string();
    assert!(
        msg.contains("unresolvable Job"),
        "refusal names the disease: {msg}"
    );
}

/// The guard's refusal reaches the caller as a 400 CARRYING ITS
/// MESSAGE, not a bare 500 (`8424fb8d` — it took the guard's own
/// builder two attempts to understand the blank 500). Exercised at
/// the classification seam the HTTP handlers share.
#[tokio::test(flavor = "multi_thread")]
async fn the_trigger_text_classifies_as_a_caller_error() {
    let db = TestDb::new().await;
    let mut conn = guarded_conn(&db.pool).await;
    let job_id = uuid::Uuid::new_v4();
    let err = sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on, metadata) \
         VALUES ($1, 'ship-a-change', 'custom', 'x', 'T', 'emp-o', 'standard', 'open', CURRENT_DATE, \
                 '{\"backlog_item\": \"totally-not-a-job\"}')",
    )
    .bind(job_id)
    .execute(&mut *conn)
    .await
    .expect_err("the guard must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("job edge") && msg.contains("unresolvable"),
        "the classification signature the HTTP mapping keys on must hold: {msg}"
    );
}

/// THE SAME REFUSAL AT EVERY DOOR. The create and update paths map the
/// guard's text to a 400 (persist_error_response); the metadata PATCH
/// — the door `boss gate --park-after` and the auto-park handler write
/// through — answered a bare 500 "storage failure: … references
/// unresolvable Job …" (b683f1cc, measured 2026-09-11 21:20Z). A 5xx
/// reads as an outage-class transient to a caller, who retries or
/// records `lost`; the refusal is the caller's own bad reference and
/// must read as one. Exercised through the HTTP handler over a database
/// with the ref check ON, the way production runs it.
#[tokio::test(flavor = "multi_thread")]
async fn the_metadata_patch_reports_an_unresolvable_edge_as_the_callers_error() {
    use axum::http::StatusCode;
    use boss_jobs::PgJobs;
    use boss_jobs::http::{JobsApiState, router};
    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
    use boss_testing::{RecordingEventBus, TestRequest};
    use std::sync::Arc;

    let db = TestDb::new().await;
    // TestDb turns the ref check OFF at the database level for bulk
    // seeds; turn it back on and open a pool whose connections see it.
    sqlx::query(&format!(
        r#"ALTER DATABASE "{}" SET audit_log.ref_check = 'on'"#,
        db.name()
    ))
    .execute(&db.pool)
    .await
    .expect("re-enable the ref check");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&db.url())
        .await
        .expect("guarded pool");
    let job_id = seed_job(&pool, "ship-a-change").await;

    let bus = RecordingEventBus::new();
    let publisher = boss_core::publisher::DomainPublisher::new(
        bus.clone() as Arc<dyn boss_core::port::EventBus>,
        "jobs",
    );
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState::minimal(
        Arc::new(PgJobs::new(pool.clone())),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ));

    let resp = TestRequest::new(
        axum::http::Method::PATCH,
        format!("/api/jobs/{job_id}/metadata"),
    )
    .json(&serde_json::json!({"boards_after": "00000000-0000-4000-8000-000000000000"}))
    .as_user("emp-ceo", "ceo")
    .send(&app)
    .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    let text = resp.body_text();
    assert!(
        text.contains("job edge") && text.contains("unresolvable"),
        "the 4xx carries the guard's own sentence: {text}"
    );
}
