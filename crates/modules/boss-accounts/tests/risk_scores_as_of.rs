//! `GET /api/people/accounts/risk-scores` says WHEN each score was made
//! (backlog 8ddaefcd; page audit 08b0c4f8 GAP 12, 2026-09-23).
//!
//! The read returned the score, the factors and nothing about time, so
//! /watchlist painted predictions from a batch that had stopped weeks
//! earlier exactly like last night's — and the batch feeding it HAD
//! stopped reaching the system of record (backlog 9599babc). This drives
//! the real query over `ml_predictions`: each score carries its own
//! prediction's `created_at` (the newest per account, not the first),
//! the list carries the newest across every account, and a score older
//! than the batch cadence says so.

use axum::http::StatusCode;
use boss_accounts::account_risk_scores::risk_scores_router;
use boss_testing::{TestDb, TestRequest};
use chrono::{DateTime, Duration, Utc};

const PATH: &str = "/api/people/accounts/risk-scores?limit=200&min_score=0";

async fn predict(db: &TestDb, id: &str, account: &str, score: i64, at: DateTime<Utc>) {
    let payload = serde_json::json!({
        "score": score,
        "top_factor": "no recent invoice",
        "factors": {
            "days_since_last_invoice": 40,
            "open_ticket_count": 0,
            "has_active_contract": false,
            "days_since_last_note": null,
        },
    });
    sqlx::query(
        "INSERT INTO ml_predictions (id, model_id, entity_type, entity_id, score, payload, created_at) \
         VALUES ($1, 'mdl-account-churn-risk-v1', 'account', $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(account)
    .bind(score as f64 / 100.0)
    .bind(payload)
    .bind(at)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn each_score_carries_its_prediction_time_and_the_list_its_newest() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO ml_models (id, name, kind, version, status) \
         VALUES ('mdl-account-churn-risk-v1', 'account-churn-risk', 'heuristic-formula', 'v1', 'active') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    for (id, name) in [
        ("acct-fresh", "Fresh Taproom"),
        ("acct-stopped", "Stopped Pub"),
    ] {
        sqlx::query("INSERT INTO accounts (id, name) VALUES ($1, $2)")
            .bind(id)
            .bind(name)
            .execute(&db.pool)
            .await
            .unwrap();
    }

    // Microsecond precision: Postgres TIMESTAMPTZ holds no finer.
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let last_night = now - Duration::hours(3);
    let weeks_ago = now - Duration::days(20);
    // acct-fresh has an older prediction too: the read takes the newest.
    predict(&db, "p-fresh-old", "acct-fresh", 40, weeks_ago).await;
    predict(&db, "p-fresh-new", "acct-fresh", 55, last_night).await;
    predict(&db, "p-stopped", "acct-stopped", 70, weeks_ago).await;

    let app = risk_scores_router(db.pool.clone());
    let resp = TestRequest::get(PATH).as_smoke().send(&app).await;
    resp.assert_status(StatusCode::OK);
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();

    let at = |v: &serde_json::Value| -> DateTime<Utc> {
        v.as_str()
            .unwrap_or_else(|| panic!("not a timestamp: {v}"))
            .parse()
            .unwrap()
    };
    let rows = list["accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{list}");
    let row = |id: &str| rows.iter().find(|r| r["account_id"] == id).unwrap();

    assert_eq!(
        row("acct-fresh")["score"],
        55,
        "the newest prediction per account"
    );
    assert_eq!(at(&row("acct-fresh")["scored_at"]), last_night);
    assert_eq!(row("acct-fresh")["stale"], false);
    assert_eq!(at(&row("acct-stopped")["scored_at"]), weeks_ago);
    assert_eq!(row("acct-stopped")["stale"], true);

    assert_eq!(at(&list["scored_as_of"]), last_night);
    assert_eq!(list["stale_after_hours"], 26);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_scored_has_no_as_of_time() {
    let db = TestDb::new().await;
    let app = risk_scores_router(db.pool.clone());
    let resp = TestRequest::get(PATH).as_smoke().send(&app).await;
    resp.assert_status(StatusCode::OK);
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(list["scored_as_of"], serde_json::Value::Null);
    assert_eq!(list["total_scored"], 0);
}
