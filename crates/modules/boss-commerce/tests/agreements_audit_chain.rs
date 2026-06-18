//! Audit-chain test for `service_agreements`.
//!
//! The agreements POST handler must emit an audit_log event for every
//! write, and `rebuild_commerce` must replay it — otherwise every
//! agreement disappears on rebuild.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_commerce::agreements::agreements_router;
use boss_commerce::rebuild_commerce;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct AgreementRow {
    id: String,
    account_id: String,
    status: String,
    annual_value_cents: i64,
}

async fn snapshot(pool: &PgPool) -> Vec<AgreementRow> {
    sqlx::query_as(
        "SELECT id, account_id, status, annual_value_cents \
         FROM service_agreements ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "commerce")
        .with_audit(Arc::new(PgAuditWriter::new(pool.clone())));
    agreements_router(
        pool,
        Some(publisher),
        Arc::new(boss_clock_client::WallClockClient),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn agreement_survives_rebuild() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // Initial create at status=active.
    TestRequest::post("/api/commerce/agreements")
        .json(&serde_json::json!({
            "id": "sa-001",
            "account_id": "acc-001",
            "agreement_type": "full-service",
            "status": "active",
            "start_date": "2026-01-01",
            "end_date": "2026-12-31",
            "annual_value_cents": 1_200_000,
            "currency": "USD",
            "billing_frequency": "monthly",
            "auto_renew": true,
            "covers_parts": true,
            "covers_labor": true,
            "covers_travel": false,
            "pm_visits_per_year": 4,
            "response_sla_hours": 8,
            "owner_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Re-POST with status=expired — handler's ON CONFLICT DO
    // UPDATE flips status. We want the transition to survive.
    TestRequest::post("/api/commerce/agreements")
        .json(&serde_json::json!({
            "id": "sa-001",
            "account_id": "acc-001",
            "agreement_type": "full-service",
            "status": "expired",
            "start_date": "2026-01-01",
            "end_date": "2026-12-31",
            "annual_value_cents": 1_500_000,
            "currency": "USD",
            "billing_frequency": "monthly",
            "auto_renew": false,
            "covers_parts": true,
            "covers_labor": true,
            "covers_travel": false,
            "pm_visits_per_year": 4,
            "response_sla_hours": 8,
            "owner_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let pre = snapshot(&db.pool).await;
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].status, "expired");
    assert_eq!(pre[0].annual_value_cents, 1_500_000);

    // Wipe + rebuild. The agreements row must reappear with the
    // post-update field values from audit_log alone.
    sqlx::query("DELETE FROM service_agreements")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_commerce(&db.pool).await.expect("rebuild");
    assert!(
        report.agreements_upserted >= 2,
        "rebuild should replay both upsert events, got {report:?}"
    );

    let post = snapshot(&db.pool).await;
    assert_eq!(
        pre, post,
        "agreement must round-trip exactly through audit_log"
    );
}
