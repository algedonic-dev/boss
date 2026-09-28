//! Postgres-backed coverage for the tenant publish stamp door (backlog
//! 42da8bd2): the REAL `POST /api/tenant/publishes` router over the Pg
//! adapter lands the `tenant_publishes` row AND its `tenant.published`
//! outbox event, credited to the signed caller — the pair a machine-
//! door publish used to leave neither of.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_jobs::tenant_publishes::http::{TenantPublishesApiState, router};
use boss_jobs::tenant_publishes::{PgTenantPublishes, TenantPublishes};
use boss_testing::TestDb;
use http_body_util::BodyExt;
use tower::ServiceExt;

const OPERATOR: &str = r#"{"id":"agent-claude","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn post(db: &TestDb, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let app = router(TenantPublishesApiState {
        repo: Arc::new(PgTenantPublishes::new(db.pool.clone())) as Arc<dyn TenantPublishes>,
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tenant/publishes")
                .header("content-type", "application/json")
                .header("x-boss-user", OPERATOR)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stamp_through_the_door_lands_the_row_and_its_event_in_one_write() {
    let db = TestDb::new().await;
    for took in [serde_json::json!([]), serde_json::json!(["departments"])] {
        let (status, answer) = post(
            &db,
            serde_json::json!({
                "tenant_id": "algedonic", "boss_commit": "6d14407c",
                "took": took, "writes": 14,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["published_by"], "agent-claude");
    }

    let rows: Vec<(String, String, String, Vec<String>, i32)> = sqlx::query_as(
        "SELECT tenant_id, published_by, boss_commit, took, writes \
         FROM tenant_publishes ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "algedonic".into(),
                "agent-claude".into(),
                "6d14407c".into(),
                vec![],
                14
            ),
            (
                "algedonic".into(),
                "agent-claude".into(),
                "6d14407c".into(),
                vec!["departments".to_string()],
                14
            ),
        ]
    );

    let events: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT source, payload FROM event_outbox WHERE kind = 'tenant.published' \
         ORDER BY jsonb_array_length(payload->'took')",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    for (source, payload) in &events {
        assert_eq!(source, "tenant");
        assert_eq!(payload["tenant_id"], "algedonic");
        assert_eq!(payload["published_by"], "agent-claude");
        assert_eq!(payload["_actor"], "agent-claude");
        assert_eq!(payload["writes"], 14);
    }
    assert_eq!(events[1].1["took"], serde_json::json!(["departments"]));
}
