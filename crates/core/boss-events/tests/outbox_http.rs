//! The dead-letter door: `POST /api/events/outbox/{id}/redeliver` and
//! `POST /api/events/outbox/{id}/resolve` on boss-events-api (backlog
//! e22b692e, adversarial review H1).
//!
//! Contract under test: Operator tier only; the act is credited to the
//! signed caller (`x-boss-user`), never to anything in the body; a row
//! that is not an open dead letter is refused (404 no row, 409 anything
//! else) and nothing changes; the answer is the row and the staged fact
//! read back from the database, and never the refused payload.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use boss_core::event::Event;
use boss_events::outbox::{REDELIVERED_KIND, RESOLVED_KIND, record_event_in_tx};
use boss_events::outbox_http::outbox_router;
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &str = "REFUSED-PAYLOAD-NEVER-ANSWERED";

fn user(id: &str, role: &str, tier: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": tier,
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

fn operator() -> String {
    user("emp-op", "service-tech", "operator")
}

/// Stage one event and set it aside the way the relay does.
async fn dead_letter(db: &TestDb) -> i64 {
    let at = Utc.with_ymd_and_hms(2026, 9, 28, 6, 0, 0).unwrap();
    let e = Event {
        id: Uuid::new_v4(),
        timestamp: at,
        source: "http-test".into(),
        kind: "outbox.http.refused".into(),
        payload: serde_json::json!({ "blob": SECRET }),
    };
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query_scalar(
        "UPDATE event_outbox SET dead_lettered_at = $2, \
         dead_letter_reason = 'max payload size exceeded' \
         WHERE event_id = $1 RETURNING id",
    )
    .bind(e.id)
    .bind(at)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn post(
    app: &Router,
    uri: &str,
    user: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-boss-user", u);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn staged(db: &TestDb, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn open_dead_letters(db: &TestDb) -> i64 {
    boss_events::outbox::dead_lettered_count(&db.pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_operator_tier_may_act_and_a_refusal_changes_nothing() {
    let db = TestDb::new().await;
    let app = outbox_router(db.pool.clone());
    let id = dead_letter(&db).await;

    let cto = user("emp-001", "cto", "user");
    let plain = user("emp-2", "service-tech", "user");
    for (who, header) in [
        ("anonymous", None),
        ("a cto at user tier", Some(cto.as_str())),
        ("a plain user", Some(plain.as_str())),
    ] {
        for (path, body) in [
            ("redeliver", serde_json::json!({})),
            ("resolve", serde_json::json!({"reason": "audit-only"})),
        ] {
            let (status, text) = post(
                &app,
                &format!("/api/events/outbox/{id}/{path}"),
                header,
                body,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who} {path}: {text}");
        }
    }
    assert_eq!(open_dead_letters(&db).await, 1, "nothing moved");
    assert!(staged(&db, REDELIVERED_KIND).await.is_empty());
    assert!(staged(&db, RESOLVED_KIND).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn redeliver_credits_the_signed_caller_and_answers_the_row_and_the_fact_read_back() {
    let db = TestDb::new().await;
    let app = outbox_router(db.pool.clone());
    let id = dead_letter(&db).await;

    // A body naming an actor is ignored: the act is the header's.
    let (status, text) = post(
        &app,
        &format!("/api/events/outbox/{id}/redeliver"),
        Some(&operator()),
        serde_json::json!({"actor": "emp-forged", "actor_id": "emp-forged"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "never the payload: {text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["letter"]["outbox_id"], id);
    assert_eq!(v["letter"]["event_kind"], "outbox.http.refused");
    assert_eq!(
        v["letter"]["dead_letter_reason"],
        "max payload size exceeded"
    );
    assert_eq!(v["letter"]["overtaken_by"], 0);
    assert_eq!(v["row"]["state"], "pending");

    let acts = staged(&db, REDELIVERED_KIND).await;
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["_actor"], "emp-op", "credited to the signed caller");
    let act_id: i64 = sqlx::query_scalar("SELECT id FROM event_outbox WHERE kind = $1")
        .bind(REDELIVERED_KIND)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(v["row"]["act_outbox_id"], act_id, "names the staged fact");
    assert_eq!(v["row"]["act_kind"], REDELIVERED_KIND);

    // Now pending, not dead-lettered: a second redeliver is a conflict.
    let (status, text) = post(
        &app,
        &format!("/api/events/outbox/{id}/redeliver"),
        Some(&operator()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("pending"), "{text}");
    assert_eq!(staged(&db, REDELIVERED_KIND).await.len(), 1);

    let (status, text) = post(
        &app,
        "/api/events/outbox/987654321/redeliver",
        Some(&operator()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_needs_a_reason_and_answers_the_rows_own_resolution() {
    let db = TestDb::new().await;
    let app = outbox_router(db.pool.clone());
    let id = dead_letter(&db).await;
    let uri = format!("/api/events/outbox/{id}/resolve");

    for body in [serde_json::json!({}), serde_json::json!({"reason": "   "})] {
        let (status, text) = post(&app, &uri, Some(&operator()), body.clone()).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {status} {text}"
        );
    }
    assert!(staged(&db, RESOLVED_KIND).await.is_empty());

    let (status, text) = post(
        &app,
        &uri,
        Some(&operator()),
        serde_json::json!({"reason": "audit-only by decision"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "never the payload: {text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["row"]["state"], "dead-lettered, resolved");
    assert_eq!(v["row"]["resolution"], "audit-only by decision");
    assert_eq!(v["row"]["act_kind"], RESOLVED_KIND);
    assert!(v["row"]["act_outbox_id"].as_i64().is_some(), "{v}");
    assert!(v["letter"]["overtaken_by"].is_null(), "{v}");
    let acts = staged(&db, RESOLVED_KIND).await;
    assert_eq!(acts[0]["_actor"], "emp-op");
    assert_eq!(open_dead_letters(&db).await, 0);

    // A resolution is final.
    let (status, text) = post(
        &app,
        &uri,
        Some(&operator()),
        serde_json::json!({"reason": "again"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("final"), "{text}");
}

async fn get(app: &Router, uri: &str, user: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(u) = user {
        req = req.header("x-boss-user", u);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `GET /api/events/outbox/stats` (backlog 72c50b8b): the read beside
/// `/api/events/stats`, behind the same door — operator or auditor
/// tier, or a role with global read (the forge's `audit-readonly`
/// probe reader) — answering what the relay still owes.
#[tokio::test(flavor = "multi_thread")]
async fn outbox_stats_names_the_undrained_write_behind_the_stats_door() {
    let db = TestDb::new().await;
    let app = outbox_router(db.pool.clone());
    let uri = "/api/events/outbox/stats";

    let plain = user("emp-2", "service-tech", "user");
    for header in [None, Some(plain.as_str())] {
        let (status, text) = get(&app, uri, header).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{header:?}: {text}");
    }

    let e = Event {
        id: Uuid::new_v4(),
        timestamp: Utc.with_ymd_and_hms(2026, 9, 28, 6, 0, 0).unwrap(),
        source: "http-test".into(),
        kind: "outbox.http.undrained".into(),
        payload: serde_json::json!({}),
    };
    let mut tx = db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &e).await.unwrap();
    tx.commit().await.unwrap();
    dead_letter(&db).await;

    let reader = user("probe-reader", "audit-readonly", "user");
    for who in [operator(), reader] {
        let (status, text) = get(&app, uri, Some(&who)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        // The dead letter is not pending and not undrained: the relay
        // wrote its audit row before setting it aside, so the count is
        // scoped to pending rows and the dead letter reports on its own.
        assert_eq!(v["pending"], 1, "{v}");
        assert_eq!(v["dead_lettered_open"], 1, "{v}");
        assert_eq!(v["undrained"]["count"], 1, "{v}");
        assert!(v["undrained"]["oldest_created_at"].is_string(), "{v}");
        assert!(v["undrained"]["behind_head_seconds"].is_number(), "{v}");
        assert_eq!(v["lag"]["window_hours"], 24, "{v}");
        assert_eq!(v["lag"]["delivered"], 0, "{v}");
        assert!(v["lag"]["p95_seconds"].is_null(), "{v}");
    }

    let (status, text) = get(
        &app,
        "/api/events/outbox/stats?window_hours=6",
        Some(&operator()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("\"window_hours\":6"), "{text}");
    for bad in ["0", "721", "x"] {
        let (status, text) = get(
            &app,
            &format!("/api/events/outbox/stats?window_hours={bad}"),
            Some(&operator()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {text}");
    }
}
