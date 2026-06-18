//! End-to-end: drive calendar writes through PgCalendar +
//! PgAuditWriter, snapshot calendar_reservations, drop, rebuild
//! from `audit_log`, assert exact match.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_calendar::http::{CalendarApiState, router};
use boss_calendar::rebuild_calendar;
use boss_calendar::{CalendarClient, PgCalendar};
use boss_core::calendar::{ReservationRequest, ReservationStrength, TimeWindow, reason};
use boss_core::job::Subject;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ReservationRow {
    id: uuid::Uuid,
    resource_kind: String,
    resource_id: String,
    start_ts: DateTime<Utc>,
    end_ts: DateTime<Utc>,
    reason_kind: String,
    reason_ref_id: String,
    strength: String,
    notes: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    cancelled_at: Option<DateTime<Utc>>,
}

async fn snapshot(pool: &PgPool) -> Vec<ReservationRow> {
    sqlx::query_as("SELECT id, resource_kind, resource_id, start_ts, end_ts, reason_kind, reason_ref_id, strength, notes, created_by, created_at, cancelled_at FROM calendar_reservations ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

fn build_app(pool: PgPool) -> (Router, Arc<dyn CalendarClient>) {
    let calendar: Arc<dyn CalendarClient> = Arc::new(PgCalendar::new(pool.clone()));
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "calendar")
        .with_audit(Arc::new(PgAuditWriter::new(pool)));
    let state = CalendarApiState {
        calendar: calendar.clone(),
        publisher: Some(publisher),
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), calendar)
}

fn t(h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, h, 0, 0).unwrap()
}

async fn post_reserve(app: &Router, req: &ReservationRequest) -> uuid::Uuid {
    let body = serde_json::to_vec(req).unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/calendar/reservations")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    uuid::Uuid::parse_str(v["id"].as_str().unwrap()).unwrap()
}

async fn delete_reserve(app: &Router, id: uuid::Uuid) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/calendar/reservations/{id}?actor=test"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_reservations() {
    let db = TestDb::new().await;
    let (app, _calendar) = build_app(db.pool.clone());

    // 1. Reserve two windows.
    let r1 = ReservationRequest {
        subject: Subject::new("employee", "emp-001"),
        window: TimeWindow::new(t(9), t(10)).unwrap(),
        reason_kind: reason::JOB_STEP.to_string(),
        reason_ref_id: "step-001".into(),
        strength: ReservationStrength::Hard,
        notes: Some("morning".into()),
        created_by: "tester".into(),
    };
    let r2 = ReservationRequest {
        subject: Subject::new("employee", "emp-001"),
        window: TimeWindow::new(t(14), t(15)).unwrap(),
        reason_kind: reason::MEETING.to_string(),
        reason_ref_id: "mtg-001".into(),
        strength: ReservationStrength::Soft,
        notes: None,
        created_by: "tester".into(),
    };
    let id1 = post_reserve(&app, &r1).await;
    let _id2 = post_reserve(&app, &r2).await;

    // 2. Cancel one.
    delete_reserve(&app, id1).await;

    // 3. Snapshot.
    let before = snapshot(&db.pool).await;
    assert_eq!(before.len(), 2);
    assert_eq!(
        before.iter().filter(|r| r.cancelled_at.is_some()).count(),
        1
    );

    // 4. Verify audit_log events.
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'calendar.reservation.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_count.0, 3, "2 reserved + 1 cancelled");

    // 5. Wipe + rebuild.
    let report = rebuild_calendar(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.reservations_inserted, 2);
    assert_eq!(report.reservations_cancelled, 1);

    // 6. Reconstructed projection must match.
    let after = snapshot(&db.pool).await;
    assert_eq!(before, after, "calendar_reservations mismatch");
}

/// Determinism (correctness-protocol §4 idempotence / §5 determinism):
/// when a `calendar.reservation.cancelled` event carries no
/// `cancelled_at` in its payload, the rebuild must stamp the
/// projection from the EVENT's own timestamp — never wall-clock.
/// A wall-clock fallback makes the rebuild non-deterministic:
/// replaying the same log at two instants would land two different
/// `cancelled_at` values, so a replay-diff would report spurious
/// divergence. `cancelled_at` is a business "happened-on" fact, not
/// an operational lifecycle column.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_cancel_without_cancelled_at_uses_event_time() {
    let db = TestDb::new().await;
    let (app, _calendar) = build_app(db.pool.clone());

    // Reserve one window — emits a `reserved` event whose payload is
    // a full Reservation with `cancelled_at: null`.
    let r = ReservationRequest {
        subject: Subject::new("employee", "emp-001"),
        window: TimeWindow::new(t(9), t(10)).unwrap(),
        reason_kind: reason::JOB_STEP.to_string(),
        reason_ref_id: "step-001".into(),
        strength: ReservationStrength::Hard,
        notes: None,
        created_by: "tester".into(),
    };
    let id = post_reserve(&app, &r).await;

    // Reuse that payload (cancelled_at already null) as a cancel event
    // stamped at a historical instant clearly distinct from "now".
    let (payload,): (serde_json::Value,) = sqlx::query_as(
        "SELECT payload FROM audit_log \
         WHERE kind = 'calendar.reservation.reserved' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let event_time = Utc.with_ymd_and_hms(2025, 1, 15, 8, 30, 0).unwrap();
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, 'calendar', 'calendar.reservation.cancelled', $3)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(event_time)
    .bind(&payload)
    .execute(&db.pool)
    .await
    .unwrap();

    // Rebuild from the log.
    let report = rebuild_calendar(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.reservations_cancelled, 1);

    // The reconstructed cancellation time must be the event time, not
    // the wall-clock instant the rebuild happened to run at.
    let row = snapshot(&db.pool)
        .await
        .into_iter()
        .find(|r| r.id == id)
        .expect("reservation row present");
    assert_eq!(
        row.cancelled_at,
        Some(event_time),
        "rebuild must stamp cancelled_at from event time, not wall-clock",
    );
}
