//! End-to-end test: the PO batch endpoint persists backdated POs into
//! a real Postgres database.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::DomainPublisher;
use boss_inventory::http::{InventoryApiState, router};
use boss_inventory::postgres::PgInventory;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;

/// Seed a vendor row. `create_purchase_order_at` binds the PO's
/// `vendor` name string into `purchase_orders.vendor_id`, which has a
/// hard FK to `vendors(id)`. A fresh test DB has no vendors, so the
/// batch insert 500s on the FK unless the referenced vendor exists.
async fn seed_vendor(pool: &sqlx::PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO vendors (id, name, contact_name, contact_email, city, state, payment_terms, category) \
         VALUES ($1, $1, 'Test Contact', 'contact@example.com', 'Testville', 'CA', 'net-30', 'general') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

fn pg_router(pool: sqlx::PgPool) -> Router {
    let inventory = Arc::new(PgInventory::new(pool));
    let bus = RecordingEventBus::new();
    let publisher = DomainPublisher::new(bus.clone(), "inventory");
    let state = InventoryApiState {
        inventory,
        publisher: Some(publisher),
        clients: None,
        classes_client: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_create_persists_backdated_pos_in_postgres() {
    let db = TestDb::new().await;

    // Vendor rows must exist: `create_purchase_order_at` binds the PO's
    // `vendor` name into `purchase_orders.vendor_id`, which carries a
    // hard FK to `vendors(id)`. Seed the two vendors this batch uses.
    seed_vendor(&db.pool, "Optica Components").await;
    seed_vendor(&db.pool, "NetParts Direct").await;

    let app = pg_router(db.pool.clone());

    let body = json!([
        {
            "id": "PO-PG-1",
            "vendor": "Optica Components",
            "status": "received",
            "placed_on": "2022-06-01",
            "expected_on": "2022-06-15",
            "received_on": "2022-06-14",
            "lines": [
                { "part_sku": "PART-A", "qty": 10, "unit_cost_cents": 50 },
                { "part_sku": "PART-B", "qty": 5,  "unit_cost_cents": 120 }
            ]
        },
        {
            "id": "PO-PG-2",
            "vendor": "NetParts Direct",
            "status": "draft",
            "placed_on": "2026-04-01",
            "expected_on": "2026-04-11",
            "received_on": null,
            "lines": [
                { "part_sku": "PART-C", "qty": 20, "unit_cost_cents": 75 }
            ]
        }
    ]);

    let resp = TestRequest::post("/api/inventory/orders/batch")
        .json(&body)
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);

    // Verify via direct SQL: dates survived the round trip.
    let row: (
        String,
        String,
        chrono::NaiveDate,
        chrono::NaiveDate,
        Option<chrono::NaiveDate>,
    ) = sqlx::query_as(
        "SELECT vendor, status, placed_on, expected_on, received_on \
             FROM purchase_orders WHERE id = $1",
    )
    .bind("PO-PG-1")
    .fetch_one(&db.pool)
    .await
    .expect("PO-PG-1 should be persisted");
    assert_eq!(row.0, "Optica Components");
    assert_eq!(row.1, "received");
    assert_eq!(row.2, chrono::NaiveDate::from_ymd_opt(2022, 6, 1).unwrap());
    assert_eq!(row.3, chrono::NaiveDate::from_ymd_opt(2022, 6, 15).unwrap());
    assert_eq!(
        row.4,
        Some(chrono::NaiveDate::from_ymd_opt(2022, 6, 14).unwrap())
    );

    let line_count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM purchase_order_lines WHERE po_id = $1")
            .bind("PO-PG-1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(line_count.0, 2);

    let draft_row: (String, Option<chrono::NaiveDate>) =
        sqlx::query_as("SELECT status, received_on FROM purchase_orders WHERE id = $1")
            .bind("PO-PG-2")
            .fetch_one(&db.pool)
            .await
            .expect("PO-PG-2 should be persisted");
    assert_eq!(draft_row.0, "draft");
    assert_eq!(draft_row.1, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_create_is_idempotent() {
    let db = TestDb::new().await;
    seed_vendor(&db.pool, "Optica Components").await;
    let app = pg_router(db.pool.clone());

    let body = json!([
        {
            "id": "PO-IDEM-1",
            "vendor": "Optica Components",
            "status": "draft",
            "placed_on": "2025-01-05",
            "expected_on": "2025-01-19",
            "received_on": null,
            "lines": [
                { "part_sku": "PART-X", "qty": 3, "unit_cost_cents": 100 }
            ]
        }
    ]);

    TestRequest::post("/api/inventory/orders/batch")
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // Re-post the same payload: must not error, must not duplicate lines.
    TestRequest::post("/api/inventory/orders/batch")
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let line_count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM purchase_order_lines WHERE po_id = $1")
            .bind("PO-IDEM-1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        line_count.0, 1,
        "duplicate batch should not create extra lines"
    );
}
