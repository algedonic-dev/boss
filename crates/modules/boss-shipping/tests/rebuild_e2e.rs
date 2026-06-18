//! End-to-end: drive shipping writes through PgShipping +
//! PgAuditWriter, snapshot `shipments` + `shipment_assets`, drop,
//! rebuild from `audit_log`, assert exact match.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_shipping::PgShipping;
use boss_shipping::http::{ShippingApiState, router};
use boss_shipping::rebuild_shipping;
use boss_shipping::types::*;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ShipmentRow {
    id: String,
    direction: String,
    status: String,
    carrier: Option<String>,
    tracking_number: Option<String>,
    origin: String,
    destination: String,
    po_id: Option<String>,
    order_id: Option<String>,
    account_id: Option<String>,
    created_on: chrono::NaiveDate,
    shipped_on: Option<chrono::NaiveDate>,
    estimated_delivery: Option<chrono::NaiveDate>,
    delivered_on: Option<chrono::NaiveDate>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ShipmentSystemRow {
    shipment_id: String,
    asset_id: String,
}

async fn snapshot_shipments(pool: &PgPool) -> Vec<ShipmentRow> {
    sqlx::query_as("SELECT id, direction, status, carrier, tracking_number, origin, destination, po_id, order_id, account_id, created_on, shipped_on, estimated_delivery, delivered_on, created_at, updated_at FROM shipments ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_shipment_systems(pool: &PgPool) -> Vec<ShipmentSystemRow> {
    sqlx::query_as(
        "SELECT shipment_id, asset_id FROM shipment_assets ORDER BY shipment_id, asset_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    let shipping = Arc::new(PgShipping::new(pool.clone()));
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "shipping")
        .with_audit(Arc::new(PgAuditWriter::new(pool)));
    let state = ShippingApiState {
        shipping,
        publisher: Some(publisher),
        classes_client: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

fn fixture(id: &str, status: ShipmentStatus, systems: Vec<&str>) -> Shipment {
    Shipment {
        id: id.to_string(),
        direction: ShipmentDirection::Outbound,
        status,
        carrier: Some(Carrier::new("fedex")),
        tracking_number: Some(format!("1Z{id}")),
        origin: "HQ Warehouse".into(),
        destination: "Customer Alpha".into(),
        asset_ids: systems.into_iter().map(String::from).collect(),
        po_id: None,
        order_id: Some(format!("ORD-{id}")),
        account_id: Some("acc-001".into()),
        created_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 2).unwrap()),
        estimated_delivery: Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 5).unwrap()),
        delivered_on: None,
        line_items: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_shipments_and_systems() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Two shipments — one with multiple systems, one with one.
    let s1 = fixture(
        "ship-001",
        ShipmentStatus::PICKED_UP.into(),
        vec!["SYS-A", "SYS-B"],
    );
    let s2 = fixture("ship-002", ShipmentStatus::IN_TRANSIT.into(), vec!["SYS-C"]);
    for s in [&s1, &s2] {
        TestRequest::post("/api/shipping/shipments")
            .json(s)
            .send(&app)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // 2. Update s1 — advance status, change asset_ids list.
    let mut s1_updated = s1.clone();
    s1_updated.status = ShipmentStatus::DELIVERED.into();
    s1_updated.delivered_on = Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 4).unwrap());
    s1_updated.asset_ids = vec!["SYS-A".into(), "SYS-D".into()];
    TestRequest::put(format!("/api/shipping/shipments/{}", s1.id))
        .json(&s1_updated)
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Snapshot.
    let shipments_before = snapshot_shipments(&db.pool).await;
    let systems_before = snapshot_shipment_systems(&db.pool).await;
    assert_eq!(shipments_before.len(), 2);
    assert_eq!(systems_before.len(), 3, "ship-001: 2 + ship-002: 1");

    // 4. Audit_log has 3 events (2 created + 1 updated).
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'shipping.shipment.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_count.0, 3, "got {} events", event_count.0);

    // 5. Wipe + rebuild.
    sqlx::query("DELETE FROM shipment_assets")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM shipments")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_shipping(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.shipments_upserted, 3, "2 created + 1 updated");

    // 6. Reconstructed projections must match originals exactly.
    let shipments_after = snapshot_shipments(&db.pool).await;
    let systems_after = snapshot_shipment_systems(&db.pool).await;
    assert_eq!(shipments_before, shipments_after, "shipments mismatch");
    assert_eq!(systems_before, systems_after, "shipment_assets mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_shipment_delete() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let s = fixture(
        "ship-doomed",
        ShipmentStatus::LABEL_CREATED.into(),
        vec!["SYS-X"],
    );
    TestRequest::post("/api/shipping/shipments")
        .json(&s)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete(format!("/api/shipping/shipments/{}", s.id))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shipments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    let report = rebuild_shipping(&db.pool).await.unwrap();
    assert!(report.shipments_upserted >= 1);
    assert!(report.shipments_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shipments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}
