//! Step 2 of the account detail view: integration tests for the
//! account team and notes HTTP handlers.
//!
//! These exercise the full HTTP → Postgres path against TestDb.
//! Coverage:
//!   - assign / list / unassign account team round-trip
//!   - reassign produces a different auto-note than first assign
//!   - notes list, create, soft-delete round-trip
//!   - soft-deleted notes hide from default list
//!   - missing actor_id returns 4xx (type-layer rejection)

#![cfg(feature = "postgres")]

use axum::http::StatusCode;
use boss_accounts::account_notes::account_notes_router;
use boss_accounts::account_team_members::account_team_router;
use boss_testing::{TestDb, TestRequest};
use sqlx::PgPool;

const ACCOUNT_ID: &str = "account-crm-test";
const EMP_ALPHA: &str = "emp-alpha";
const EMP_BETA: &str = "emp-beta";
const EMP_ACTOR: &str = "emp-actor";

/// Insert the bare-minimum referenced rows so the cross-table FKs
/// (account_team_members → accounts, employees → locations) all
/// resolve.
async fn seed_refs(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone) \
         VALUES ('loc-hq', 'HQ', 'hq', 'UTC') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("seed location");

    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id) \
         VALUES ($1, 'Test Account', 'Dr. Test', 'Austin', 'TX', 'silver', '2024-01-01', $2)",
    )
    .bind(ACCOUNT_ID)
    .bind(EMP_ACTOR)
    .execute(pool)
    .await
    .expect("seed account");

    for emp in [EMP_ALPHA, EMP_BETA, EMP_ACTOR] {
        sqlx::query(
            "INSERT INTO employees \
                (id, name, email, role, department, hire_date, location, employment_type, status) \
             VALUES ($1, $2, $3, 'service-tech', 'service', '2024-01-15', 'loc-hq', 'full-time', 'active')",
        )
        .bind(emp)
        .bind(format!("Test {emp}"))
        .bind(format!("{emp}@boss.io"))
        .execute(pool)
        .await
        .expect("seed employee");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn assign_then_list_returns_the_assignment() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let body = serde_json::json!({
        "employee_id": EMP_ALPHA,
        "role": "customer-success",
        "actor_id": EMP_ACTOR,
    });
    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["employee_id"], EMP_ALPHA);
    assert_eq!(arr[0]["role"], "customer-success");
}

#[tokio::test(flavor = "multi_thread")]
async fn assign_writes_an_interaction_note() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let body = serde_json::json!({
        "employee_id": EMP_ALPHA,
        "role": "customer-success",
        "actor_id": EMP_ACTOR,
    });
    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // The auto-note should now be in account_notes with kind='interaction'.
    let (kind, body, author): (String, String, String) = sqlx::query_as(
        "SELECT kind, body, actor_id FROM account_notes \
         WHERE account_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ACCOUNT_ID)
    .fetch_one(&db.pool)
    .await
    .expect("auto-posted note should exist");

    assert_eq!(kind, "interaction");
    assert!(body.contains("assigned"));
    assert!(body.contains(EMP_ALPHA));
    assert_eq!(author, EMP_ACTOR);
}

#[tokio::test(flavor = "multi_thread")]
async fn reassign_replaces_the_existing_assignment_and_notes_the_change() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    // First assignment.
    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .json(&serde_json::json!({
            "employee_id": EMP_ALPHA,
            "role": "customer-success",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Reassign to a different employee.
    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .json(&serde_json::json!({
            "employee_id": EMP_BETA,
            "role": "customer-success",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Exactly one row in account_team_members (UNIQUE constraint).
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM account_team_members WHERE account_id = $1")
            .bind(ACCOUNT_ID)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);

    // The current row points to EMP_BETA.
    let (employee_id,): (String,) = sqlx::query_as(
        "SELECT employee_id FROM account_team_members \
         WHERE account_id = $1 AND role = 'customer-success'",
    )
    .bind(ACCOUNT_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(employee_id, EMP_BETA);

    // Two notes total: original assign + reassignment.
    let (note_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM account_notes WHERE account_id = $1")
            .bind(ACCOUNT_ID)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(note_count, 2);

    // The latest note mentions "reassigned".
    let (latest_body,): (String,) = sqlx::query_as(
        "SELECT body FROM account_notes WHERE account_id = $1 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ACCOUNT_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        latest_body.contains("reassigned"),
        "expected reassign note, got: {latest_body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unassign_removes_row_and_writes_audit_note() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/account-team"))
        .json(&serde_json::json!({
            "employee_id": EMP_ALPHA,
            "role": "customer-success",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete(format!(
        "/api/people/accounts/{ACCOUNT_ID}/account-team/customer-success?actor_id={EMP_ACTOR}"
    ))
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM account_team_members WHERE account_id = $1")
            .bind(ACCOUNT_ID)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);

    let (latest_body,): (String,) = sqlx::query_as(
        "SELECT body FROM account_notes WHERE account_id = $1 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ACCOUNT_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(latest_body.contains("unassigned"));
}

#[tokio::test(flavor = "multi_thread")]
async fn unassign_unknown_role_returns_400() {
    // Post-#95: role validation is opt-in via ClassesClient. When
    // a classes client is wired and the role isn't in the registry,
    // the handler returns 400. Without classes the handler can't
    // tell unknown from absent and returns 404.
    use boss_classes_client::FakeClassesClient;
    use boss_core::primitives::ClassRef;
    let classes: std::sync::Arc<dyn boss_classes_client::ClassesClient> =
        std::sync::Arc::new(FakeClassesClient::with(vec![
            ClassRef::new("employee", "territory-rep"),
            ClassRef::new("employee", "customer-success"),
            ClassRef::new("employee", "service-manager"),
            ClassRef::new("employee", "finance-contact"),
            ClassRef::new("employee", "executive-sponsor"),
        ]));
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        Some(classes),
    );

    let resp = TestRequest::delete(format!(
        "/api/people/accounts/{ACCOUNT_ID}/account-team/billing-ops?actor_id={EMP_ACTOR}"
    ))
    .send(&app)
    .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_note_and_list_returns_it() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_notes_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/notes"))
        .json(&serde_json::json!({
            "kind": "call",
            "body": "Quarterly check-in with Dr. Test",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/notes"))
        .send(&app)
        .await;
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "call");
    assert_eq!(arr[0]["body"], "Quarterly check-in with Dr. Test");
    assert_eq!(arr[0]["actor_id"], EMP_ACTOR);
    assert!(arr[0]["deleted_at"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn soft_deleted_note_hides_from_list_but_stays_in_table() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_notes_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let create = TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/notes"))
        .json(&serde_json::json!({
            "kind": "note",
            "body": "to be deleted",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await;
    create.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = serde_json::from_slice(&create.body_bytes).unwrap();
    let note_id = body["id"].as_str().unwrap().to_string();

    TestRequest::delete(format!(
        "/api/people/accounts/{ACCOUNT_ID}/notes/{note_id}?actor_id={EMP_ACTOR}"
    ))
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    // GET hides it.
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/notes"))
        .send(&app)
        .await;
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert!(list.as_array().unwrap().is_empty());

    // The row is still in the table with deleted_at populated.
    let (deleted_by,): (Option<String>,) =
        sqlx::query_as("SELECT deleted_by FROM account_notes WHERE id = $1")
            .bind(&note_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(deleted_by.as_deref(), Some(EMP_ACTOR));
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_already_deleted_note_returns_404() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_notes_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let create = TestRequest::post(format!("/api/people/accounts/{ACCOUNT_ID}/notes"))
        .json(&serde_json::json!({
            "kind": "note",
            "body": "x",
            "actor_id": EMP_ACTOR,
        }))
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&create.body_bytes).unwrap();
    let note_id = body["id"].as_str().unwrap().to_string();

    // First delete: 204.
    TestRequest::delete(format!(
        "/api/people/accounts/{ACCOUNT_ID}/notes/{note_id}?actor_id={EMP_ACTOR}"
    ))
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    // Second delete: 404 because the WHERE clause excludes already-deleted rows.
    TestRequest::delete(format!(
        "/api/people/accounts/{ACCOUNT_ID}/notes/{note_id}?actor_id={EMP_ACTOR}"
    ))
    .send(&app)
    .await
    .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Batch endpoints — used by the simulator replay path
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn batch_account_team_persists_assignments() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_team_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let body = serde_json::json!([
        {
            "id": "cat-sim-000001",
            "account_id": ACCOUNT_ID,
            "employee_id": EMP_ALPHA,
            "role": "customer-success",
            "notes": "Assigned by replay bootstrap"
        }
    ]);
    let resp = TestRequest::post("/api/people/account-account-team/batch")
        .json(&body)
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    let (emp, role): (String, String) =
        sqlx::query_as("SELECT employee_id, role FROM account_team_members WHERE account_id = $1")
            .bind(ACCOUNT_ID)
            .fetch_one(&db.pool)
            .await
            .expect("assignment persisted");
    assert_eq!(emp, EMP_ALPHA);
    assert_eq!(role, "customer-success");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_note_with_explicit_id_preserves_backdated_occurred_at() {
    // The sim supplies a deterministic `id` + a backdated
    // `occurred_at` through the single create endpoint (the batch
    // path is gone); both must land verbatim.
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_notes_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let url = format!("/api/people/accounts/{ACCOUNT_ID}/notes");
    TestRequest::post(&url)
        .json(&serde_json::json!({
            "id": "cn-sim-00000001",
            "actor_id": EMP_ACTOR,
            "kind": "interaction",
            "body": "Opportunity opp-123 closed won — $150000",
            "occurred_at": "2022-06-15T12:00:00Z"
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post(&url)
        .json(&serde_json::json!({
            "id": "cn-sim-00000002",
            "actor_id": EMP_ALPHA,
            "kind": "interaction",
            "body": "Service ticket opened — cooling alarm",
            "occurred_at": "2023-02-01T09:30:00Z"
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let rows: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, body, occurred_at FROM account_notes \
         WHERE account_id = $1 ORDER BY occurred_at ASC",
    )
    .bind(ACCOUNT_ID)
    .fetch_all(&db.pool)
    .await
    .expect("notes persisted");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "cn-sim-00000001");
    assert!(rows[0].1.contains("closed won"));
    assert_eq!(
        rows[0].2,
        chrono::DateTime::parse_from_rfc3339("2022-06-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    assert_eq!(rows[1].0, "cn-sim-00000002");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_note_with_explicit_id_is_idempotent() {
    // Re-posting the same deterministic `id` (a sim replay) converges
    // on a single row — `upsert_note` is ON CONFLICT DO NOTHING.
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = account_notes_router(
        db.pool.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );

    let url = format!("/api/people/accounts/{ACCOUNT_ID}/notes");
    let body = serde_json::json!({
        "id": "cn-sim-idem-1",
        "actor_id": EMP_ACTOR,
        "kind": "note",
        "body": "Check-in call",
        "occurred_at": "2024-03-01T12:00:00Z"
    });
    TestRequest::post(&url)
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post(&url)
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM account_notes WHERE account_id = $1")
        .bind(ACCOUNT_ID)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1, "duplicate id should not insert twice");
}
