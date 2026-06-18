//! End-to-end test for the Vendor CRM sub-router (procurement session
//! 1). Round-trips contacts / interactions / account-team / contracts
//! through a real Postgres DB to validate schema + adapter + HTTP.

#![cfg(feature = "postgres")]

use axum::http::StatusCode;
use boss_inventory::procurement::http::{ProcurementApiState, router};
use boss_testing::{TestDb, TestRequest};
use serde_json::json;

async fn seed_vendor_and_employee(db: &TestDb) {
    // vendors row — required FK target for every vendor_* table.
    sqlx::query(
        "INSERT INTO vendors (id, name, contact_name, contact_email, city, state, \
                              payment_terms, category) \
         VALUES ('vnd-1', 'Acme Optics', 'Legacy Primary', 'legacy@acme.test', \
                 'San Jose', 'CA', 'net-30', 'optics')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone) \
         VALUES ('loc-hq', 'HQ', 'hq', 'UTC') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // employees row — required FK target for interactions.actor_id +
    // account_team.employee_id + contracts.signed_by_employee_id. Procurement
    // folks live under the `finance` department (per job-kinds seeding);
    // the employees.department CHECK doesn't list 'procurement'.
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, location, \
                                employment_type, hire_date, status) \
         VALUES ('emp-001', 'Test Buyer', 'buyer@boss.test', 'parts-buyer', 'finance', \
                 'loc-hq', 'full-time', CURRENT_DATE, 'active') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn contact_upsert_and_list_round_trip() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });

    // Create primary contact.
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/contacts")
        .json(&json!({
            "id": "vc-1",
            "vendor_id": "vnd-1",
            "name": "Jane Rep",
            "role": "sales-rep",
            "email": "jane@acme.test",
            "phone": "+1-555-0100",
            "territory": "west-coast",
            "specialties": ["networking-components", "optics"],
            "is_primary": true
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    // List should return our contact.
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::get("/api/inventory/vendors/vnd-1/contacts")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "vc-1");
    assert_eq!(arr[0]["is_primary"], true);
    assert_eq!(arr[0]["specialties"].as_array().unwrap().len(), 2);

    // Upsert same id — should update, not error.
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/contacts")
        .json(&json!({
            "id": "vc-1",
            "vendor_id": "vnd-1",
            "name": "Jane Rep (promoted)",
            "role": "account-manager",
            "email": "jane@acme.test",
            "is_primary": true
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    assert_eq!(body["role"], "account-manager");
    assert_eq!(body["name"], "Jane Rep (promoted)");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_primary_contact_per_vendor() {
    // Partial unique index on (vendor_id) WHERE is_primary must reject a
    // second primary for the same vendor.
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });

    let first = TestRequest::post("/api/inventory/vendors/vnd-1/contacts")
        .json(&json!({
            "id": "vc-1", "vendor_id": "vnd-1", "name": "A", "role": "sales-rep",
            "email": "a@x.test", "is_primary": true
        }))
        .send(&app)
        .await;
    first.assert_status(StatusCode::OK);

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let second = TestRequest::post("/api/inventory/vendors/vnd-1/contacts")
        .json(&json!({
            "id": "vc-2", "vendor_id": "vnd-1", "name": "B", "role": "sales-rep",
            "email": "b@x.test", "is_primary": true
        }))
        .send(&app)
        .await;
    // Partial index rejects the second primary.
    second.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test(flavor = "multi_thread")]
async fn interaction_insert_list_and_soft_delete() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/interactions")
        .json(&json!({
            "id": "vi-1",
            "vendor_id": "vnd-1",
            "actor_id": "emp-001",
            "kind": "call",
            "body": "Confirmed Q2 pricing; sales rep to send revised rate card by Friday.",
            "commitments": [
                {"summary": "Send revised rate card", "due_by": "2026-05-01"}
            ],
            "linked_part_sku": "NET-CARD-V2"
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::get("/api/inventory/vendors/vnd-1/interactions")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["kind"], "call");
    assert_eq!(body[0]["commitments"].as_array().unwrap().len(), 1);

    // Soft delete.
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::delete("/api/inventory/vendors/vnd-1/interactions/vi-1")
        .json(&json!({"deleted_by": "emp-001"}))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::NO_CONTENT);

    // List must exclude soft-deleted rows.
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::get("/api/inventory/vendors/vnd-1/interactions")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn account_team_unique_per_role() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/account-team")
        .json(&json!({
            "id": "vat-1", "vendor_id": "vnd-1", "employee_id": "emp-001",
            "role": "primary"
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    // Upsert the same (vendor, role) with a different id — should
    // update the row, not create a duplicate.
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/account-team")
        .json(&json!({
            "id": "vat-2", "vendor_id": "vnd-1", "employee_id": "emp-001",
            "role": "primary", "notes": "hand-off from original"
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::get("/api/inventory/vendors/vnd-1/account-team")
        .send(&app)
        .await;
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    let arr = body.as_array().unwrap();
    // One row per (vendor, role). The id stayed at vat-1 (ON CONFLICT
    // on (vendor_id, role) is an update, not a replace).
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["notes"], "hand-off from original");
}

#[tokio::test(flavor = "multi_thread")]
async fn contract_upsert_with_nested_terms() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/contracts")
        .json(&json!({
            "id": "vct-1",
            "vendor_id": "vnd-1",
            "kind": "volume-commit",
            "title": "Acme Q2 2026 volume commit",
            "effective_on": "2026-04-01",
            "expires_on": "2026-06-30",
            "auto_renew": false,
            "terms": {
                "annual_units": 500,
                "tier_pricing": [
                    {"min": 0, "unit_cost_cents": 320_000},
                    {"min": 200, "unit_cost_cents": 295_000}
                ]
            },
            "status": "active",
            "signed_by_employee_id": "emp-001"
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::get("/api/inventory/vendors/vnd-1/contracts?status=active")
        .send(&app)
        .await;
    let body: serde_json::Value = resp.assert_json::<serde_json::Value>();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "volume-commit");
    assert_eq!(arr[0]["terms"]["annual_units"], 500);
    assert_eq!(arr[0]["terms"]["tier_pricing"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn path_vs_body_vendor_id_mismatch_is_400() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db).await;
    let app = router(ProcurementApiState {
        publisher: None,
        pool: db.pool.clone(),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    let resp = TestRequest::post("/api/inventory/vendors/vnd-1/contacts")
        .json(&json!({
            "id": "vc-9", "vendor_id": "vnd-OTHER", "name": "X",
            "role": "sales-rep", "email": "x@x.test"
        }))
        .send(&app)
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}
