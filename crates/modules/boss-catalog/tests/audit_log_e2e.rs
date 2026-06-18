//! End-to-end test for the kb → audit_log chain.
//!
//! Posts a write through the live HTTP router and asserts the
//! resulting domain event lands in the `audit_log` table via the
//! same `PgAuditWriter` the production binary uses.
//!
//! Catches the regression class "added a new write handler but
//! forgot to wire it through `DomainPublisher`" and "constructed
//! the publisher without `with_audit`."

#![cfg(feature = "postgres")]

mod common;

use axum::http::StatusCode;
use boss_testing::{TestDb, TestRequest};
use common::{KbTestApp, model_fixture};

#[tokio::test(flavor = "multi_thread")]
async fn post_model_lands_in_audit_log() {
    let db = TestDb::new().await;
    let app = KbTestApp::with_audit_pool(db.pool.clone());

    let model = model_fixture("Boss-AUDIT-CATALOG");
    TestRequest::post("/api/catalog/models")
        .json(&model)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    // Query the audit log directly via the same TestDb pool. The row
    // must show up under the kb source with the model_created
    // kind and the sku in its payload.
    let row: (String, String, serde_json::Value) = sqlx::query_as(
        "SELECT source, kind, payload FROM audit_log \
         WHERE kind = 'kb.model.created' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .expect("audit_log row should exist after POST");

    assert_eq!(row.0, "kb");
    assert_eq!(row.1, "kb.model.created");
    assert_eq!(row.2["sku"], "Boss-AUDIT-CATALOG");
}
