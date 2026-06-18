//! End-to-end test for the Marketing Asset KB. Round-trips create /
//! list / update / supersede / retire through a real Postgres DB.

#![cfg(feature = "postgres")]

use axum::http::StatusCode;
use boss_catalog::marketing_assets::http::{MarketingAssetsApiState, router};
use boss_testing::{TestDb, TestRequest};
use serde_json::json;

async fn seed_employee(db: &TestDb) {
    // Plan-validation note: the employees role+department CHECKs
    // don't whitelist marketing yet (see session 2 follow-up — the
    // schema migration to admit marketing roles lands alongside this
    // commit). Tests use ceo/executive to get past the constraint;
    // real marketers will use the new roles once the migration is
    // applied end-to-end.
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone) \
         VALUES ('loc-hq', 'HQ', 'hq', 'UTC') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, location, \
                                employment_type, hire_date, status) \
         VALUES ('emp-mkt-1', 'Marketer One', 'mkt@boss.test', 'ceo', \
                 'executive', 'loc-hq', 'full-time', CURRENT_DATE, 'active') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

fn app(db: &TestDb) -> axum::Router {
    // No Class registry wired — the gate is permissive (Phase A), so
    // these round-trips exercise storage/CRUD without registry checks.
    router(MarketingAssetsApiState {
        pool: db.pool.clone(),
        classes_client: None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn create_list_get_round_trip() {
    let db = TestDb::new().await;
    seed_employee(&db).await;

    let resp = TestRequest::post("/api/catalog/marketing-assets")
        .json(&json!({
            "id": "ma-1",
            "title": "Hero shot — Halcyon M22",
            "kind": "photo",
            "description": "Primary hero image for Q2 campaign",
            "file_url": "https://assets.boss.test/hero-m22.jpg",
            "tags": ["hero", "m22", "q2-campaign"],
            "linked_device_skus": ["LMN-M22-V3"],
            "linked_campaign_ids": ["cmp-q2-2026"],
            "owner_id": "emp-mkt-1"
        }))
        .send(&app(&db))
        .await;
    resp.assert_status(StatusCode::OK);
    let body = resp.assert_json::<serde_json::Value>();
    assert_eq!(body["id"], "ma-1");
    assert_eq!(body["kind"], "photo");
    assert_eq!(body["tags"].as_array().unwrap().len(), 3);

    let list_resp = TestRequest::get("/api/catalog/marketing-assets")
        .send(&app(&db))
        .await;
    list_resp.assert_status(StatusCode::OK);
    let list = list_resp.assert_json::<serde_json::Value>();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let get_resp = TestRequest::get("/api/catalog/marketing-assets/ma-1")
        .send(&app(&db))
        .await;
    get_resp.assert_status(StatusCode::OK);
    let one = get_resp.assert_json::<serde_json::Value>();
    assert_eq!(one["title"], "Hero shot — Halcyon M22");
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_by_kind() {
    let db = TestDb::new().await;
    seed_employee(&db).await;
    for (id, kind) in [("ma-p", "photo"), ("ma-v", "video"), ("ma-d", "deck")] {
        TestRequest::post("/api/catalog/marketing-assets")
            .json(&json!({"id": id, "title": id, "kind": kind}))
            .send(&app(&db))
            .await
            .assert_status(StatusCode::OK);
    }
    let resp = TestRequest::get("/api/catalog/marketing-assets?kind=video")
        .send(&app(&db))
        .await;
    let body = resp.assert_json::<serde_json::Value>();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "ma-v");
}

#[tokio::test(flavor = "multi_thread")]
async fn supersede_builds_history_chain() {
    let db = TestDb::new().await;
    seed_employee(&db).await;

    // v1
    TestRequest::post("/api/catalog/marketing-assets")
        .json(&json!({"id": "ma-v1", "title": "Hero v1", "kind": "photo"}))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);
    // v2 supersedes v1
    TestRequest::post("/api/catalog/marketing-assets/ma-v1/supersede")
        .json(&json!({"id": "ma-v2", "title": "Hero v2", "kind": "photo"}))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);
    // v3 supersedes v2
    TestRequest::post("/api/catalog/marketing-assets/ma-v2/supersede")
        .json(&json!({"id": "ma-v3", "title": "Hero v3", "kind": "photo"}))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);

    // Walking from v3 gives the full chain.
    let resp = TestRequest::get("/api/catalog/marketing-assets/ma-v3/history")
        .send(&app(&db))
        .await;
    resp.assert_status(StatusCode::OK);
    let chain = resp.assert_json::<serde_json::Value>();
    let arr = chain.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["id"], "ma-v1");
    assert_eq!(arr[1]["id"], "ma-v2");
    assert_eq!(arr[2]["id"], "ma-v3");
}

#[tokio::test(flavor = "multi_thread")]
async fn retire_hides_from_default_list() {
    let db = TestDb::new().await;
    seed_employee(&db).await;
    TestRequest::post("/api/catalog/marketing-assets")
        .json(&json!({"id": "ma-retire", "title": "Old asset", "kind": "doc"}))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/catalog/marketing-assets/ma-retire/retire")
        .json(&json!({}))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);

    // Default list hides retired rows.
    let default = TestRequest::get("/api/catalog/marketing-assets")
        .send(&app(&db))
        .await;
    let arr = default.assert_json::<serde_json::Value>();
    assert_eq!(arr.as_array().unwrap().len(), 0);

    // include_retired=true surfaces them.
    let included = TestRequest::get("/api/catalog/marketing-assets?include_retired=true")
        .send(&app(&db))
        .await;
    let arr = included.assert_json::<serde_json::Value>();
    assert_eq!(arr.as_array().unwrap().len(), 1);
    assert_eq!(arr[0]["id"], "ma-retire");
    assert!(arr[0]["retired_at"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn update_merges_with_existing_row() {
    let db = TestDb::new().await;
    seed_employee(&db).await;
    TestRequest::post("/api/catalog/marketing-assets")
        .json(&json!({
            "id": "ma-u",
            "title": "Original",
            "kind": "doc",
            "tags": ["a", "b"]
        }))
        .send(&app(&db))
        .await
        .assert_status(StatusCode::OK);
    // Patch just title + tags.
    let resp = TestRequest::put("/api/catalog/marketing-assets/ma-u")
        .json(&json!({"title": "Updated", "tags": ["x"]}))
        .send(&app(&db))
        .await;
    let body = resp.assert_json::<serde_json::Value>();
    assert_eq!(body["title"], "Updated");
    assert_eq!(body["tags"].as_array().unwrap().len(), 1);
    assert_eq!(body["tags"][0], "x");
    assert_eq!(body["kind"], "doc"); // untouched
}
