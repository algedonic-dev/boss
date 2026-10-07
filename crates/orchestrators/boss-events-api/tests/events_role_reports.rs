use boss_policy_client::role_guard::RoleGuardReporter;
use boss_policy_client::role_reader::{RegistryRoles, RoleSnapshotClock, SnapshotRoleReader};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_testing::TestRequest;
use serde_json::json;
use std::sync::Arc;

// These tests compare router outputs, not elapsed freshness. Gate load
// must not expire their captured authority between baseline and report.
// The role-reader expiry suite advances its own clock at the TTL boundary.
struct FrozenRoleClock(std::time::Instant);

impl RoleSnapshotClock for FrozenRoleClock {
    fn now(&self) -> std::time::Instant {
        self.0
    }
}
#[tokio::test]
async fn role_reports_preserve_protected_event_refusals_without_log_reads() {
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(FrozenRoleClock(std::time::Instant::now())),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"reader","aliases":[],"role":"platform-admin"}],"total":1}),
                json!({"data":[],"total":0}),
                json!([])
            )
            .unwrap())
        )
    );
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(50))
        .connect_lazy("postgres://boss@127.0.0.1:1/boss")
        .unwrap();
    let baseline = boss_events::tail_http::audit_tail_router(pool.clone())
        .merge(boss_events::outbox_http::outbox_router(pool.clone()));
    let app = boss_events::tail_http::audit_tail_router_with_reports(
        pool.clone(),
        Some(reporter.clone()),
    )
    .merge(boss_events::outbox_http::outbox_router_with_reports(
        pool,
        Some(reporter),
    ));
    for path in [
        "/api/events/stats",
        "/api/events/tail",
        "/api/events/stream",
        "/api/events/export",
        "/api/events/outbox/stats",
    ] {
        let original = TestRequest::get(path)
            .as_user("reader", "visitor")
            .send(&baseline)
            .await;
        let reported = TestRequest::get(path)
            .as_user("reader", "visitor")
            .send(&app)
            .await;
        reported.assert_status(axum::http::StatusCode::FORBIDDEN);
        assert_eq!(reported.body_text(), original.body_text());
    }
    // The effect doors remain tier-only even when a recorded role is broader.
    for path in [
        "/api/events/outbox/1/redeliver",
        "/api/events/outbox/1/resolve",
    ] {
        let original = TestRequest::post(path)
            .json(&json!({"reason":"fixture"}))
            .as_user("reader", "visitor")
            .send(&baseline)
            .await;
        let reported = TestRequest::post(path)
            .json(&json!({"reason":"fixture"}))
            .as_user("reader", "visitor")
            .send(&app)
            .await;
        reported.assert_status(axum::http::StatusCode::FORBIDDEN);
        assert_eq!(reported.body_text(), original.body_text());
    }
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 5);
    for row in report.rows {
        assert_eq!(row.observation.asserted_allowed, Some(false));
        assert_eq!(row.observation.recorded_allowed, Some(true));
    }
}

#[derive(Clone)]
struct UnreadLive(Arc<std::sync::atomic::AtomicUsize>);
#[async_trait::async_trait]
impl boss_events_api::gate_window_http::LiveTallies for UnreadLive {
    fn required_services(&self, _: boss_core::gate_evidence::Gate) -> Vec<String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        vec![]
    }
    async fn read(
        &self,
        _: boss_core::gate_evidence::Gate,
        _: &str,
    ) -> Result<serde_json::Value, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("unexpected candidate read".into())
    }
}
#[tokio::test]
async fn role_report_preserves_joined_window_refusal_without_upstream_reads() {
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(FrozenRoleClock(std::time::Instant::now())),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"reader","aliases":[],"role":"platform-admin"}],"total":1}),
                json!({"data":[],"total":0}),
                json!([])
            )
            .unwrap())
        )
    );
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = boss_events_api::gate_window_http::gate_window_router_with_reports(
        Arc::new(boss_core::gate_evidence::InMemoryGateEvidence::new(vec![])),
        Arc::new(UnreadLive(reads.clone())),
        Arc::new(boss_core::clock::WallClock),
        Some(reporter),
    );
    let response = TestRequest::get("/api/events/gate-window?gate=policy-check")
        .as_user("reader", "visitor")
        .send(&app)
        .await;
    response.assert_status(axum::http::StatusCode::FORBIDDEN);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].observation.asserted_allowed, Some(false));
    assert_eq!(report.rows[0].observation.recorded_allowed, Some(true));
}

struct CountedLive {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    snapshot: serde_json::Value,
}
#[async_trait::async_trait]
impl boss_events_api::gate_window_http::LiveTallies for CountedLive {
    fn required_services(&self, _: boss_core::gate_evidence::Gate) -> Vec<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        vec!["policy".into()]
    }
    async fn read(
        &self,
        _: boss_core::gate_evidence::Gate,
        service: &str,
    ) -> Result<serde_json::Value, String> {
        assert_eq!(service, "policy");
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }
}
#[tokio::test]
async fn recorded_denial_preserves_the_complete_joined_window_and_original_reads() {
    use chrono::{Duration, TimeZone};
    let now = chrono::Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).unwrap();
    let since = now - Duration::hours(74);
    let log = Arc::new(boss_core::gate_evidence::InMemoryGateEvidence::new(vec![
        boss_core::event::Event {
            id: uuid::Uuid::new_v4(),
            timestamp: since,
            source: "policy".into(),
            kind: "policy.check.recording_began".into(),
            payload: json!({"service":"policy","mode":"report","since":since,"instance":"current"}),
        },
    ]));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let live = Arc::new(CountedLive {
        calls: calls.clone(),
        snapshot: json!({"switch":"policy check","file":"fixture-mode","mode":"report","mode_error":null,
            "rows":[],"overflow":0,"recording_since":since,"clean_since":since,"not_clean":[],
            "evidence":{"recorder":true,"instance":"current","lost":0,"unstated":0,"retrying":false,"last_error":null}}),
    });
    let clock = Arc::new(boss_core::clock::FixedClock::new(now));
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(FrozenRoleClock(std::time::Instant::now())),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"window-reader","aliases":[],"role":"visitor"}],"total":1}),
                json!({"data":[],"total":0}),
                json!([])
            )
            .unwrap())
        )
    );
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let baseline = boss_events_api::gate_window_http::gate_window_router(
        log.clone(),
        live.clone(),
        clock.clone(),
    );
    let reported = boss_events_api::gate_window_http::gate_window_router_with_reports(
        log,
        live,
        clock,
        Some(reporter),
    );
    let path = "/api/events/gate-window?gate=policy-check";
    let original = TestRequest::get(path)
        .as_user("window-reader", "platform-admin")
        .send(&baseline)
        .await;
    original.assert_status(axum::http::StatusCode::OK);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let response = TestRequest::get(path)
        .as_user("window-reader", "platform-admin")
        .send(&reported)
        .await;
    response.assert_status(axum::http::StatusCode::OK);
    assert_eq!(response.body_text(), original.body_text());
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "comparison must not repeat upstream reads"
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].observation.asserted_allowed, Some(true));
    assert_eq!(report.rows[0].observation.recorded_allowed, Some(false));
}

#[tokio::test]
async fn recorded_denial_preserves_native_audit_and_outbox_read_outputs() {
    use boss_core::audit::AuditWriter;
    use chrono::TimeZone;
    let db = boss_testing::TestDb::new().await;
    let event = boss_core::event::Event {
        id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).unwrap(),
        source: "fixture".into(),
        kind: "fixture.recorded".into(),
        payload: json!({"value":"conserved"}),
    };
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write(&event)
        .await
        .unwrap();
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(FrozenRoleClock(std::time::Instant::now())),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"audit-reader","aliases":[],"role":"visitor"}],"total":1}),
                json!({"data":[],"total":0}),
                json!([])
            )
            .unwrap())
        )
    );
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let baseline = boss_events::tail_http::audit_tail_router(db.pool.clone())
        .merge(boss_events::outbox_http::outbox_router(db.pool.clone()));
    let reported = boss_events::tail_http::audit_tail_router_with_reports(
        db.pool.clone(),
        Some(reporter.clone()),
    )
    .merge(boss_events::outbox_http::outbox_router_with_reports(
        db.pool.clone(),
        Some(reporter),
    ));
    for path in [
        "/api/events/stats",
        "/api/events/tail?limit=1",
        "/api/events/export?limit=1",
        "/api/events/outbox/stats",
    ] {
        let original = TestRequest::get(path)
            .as_user("audit-reader", "platform-admin")
            .send(&baseline)
            .await;
        original.assert_status(axum::http::StatusCode::OK);
        let response = TestRequest::get(path)
            .as_user("audit-reader", "platform-admin")
            .send(&reported)
            .await;
        response.assert_status(axum::http::StatusCode::OK);
        assert_eq!(response.body_text(), original.body_text(), "{path}");
        if path.contains("tail") || path.contains("export") {
            assert!(response.body_text().contains("conserved"));
        }
    }
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 4);
    for row in report.rows {
        assert_eq!(row.observation.asserted_allowed, Some(true));
        assert_eq!(row.observation.recorded_allowed, Some(false));
    }
}

#[tokio::test]
async fn recorded_denial_preserves_the_native_stream_frame_without_repeated_comparison() {
    use boss_core::audit::AuditWriter;
    use futures::StreamExt;
    use tower::ServiceExt;
    let db = boss_testing::TestDb::new().await;
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(FrozenRoleClock(std::time::Instant::now())),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"stream-reader","aliases":[],"role":"visitor"}],"total":1}),
                json!({"data":[],"total":0}),
                json!([])
            )
            .unwrap())
        )
    );
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let mut user = boss_policy_client::User::service("stream-reader");
    user.id = "stream-reader".into();
    user.role = "platform-admin".into();
    user.access_tier = boss_policy_client::AccessTier::User;
    let request = || {
        axum::http::Request::builder()
            .uri("/api/events/stream?source=stream-fixture")
            .header("x-boss-user", serde_json::to_string(&user).unwrap())
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let original = boss_events::tail_http::audit_tail_router(db.pool.clone())
        .oneshot(request())
        .await
        .unwrap();
    let response =
        boss_events::tail_http::audit_tail_router_with_reports(db.pool.clone(), Some(reporter))
            .oneshot(request())
            .await
            .unwrap();
    assert_eq!(original.status(), axum::http::StatusCode::OK);
    assert_eq!(response.status(), original.status());
    assert_eq!(
        response.headers().get("content-type"),
        original.headers().get("content-type")
    );
    let mut original_frames = original.into_body().into_data_stream();
    let mut reported_frames = response.into_body().into_data_stream();
    for frames in [&mut original_frames, &mut reported_frames] {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(700), frames.next())
                .await
                .is_err()
        );
    }
    let event = boss_core::event::Event {
        id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        source: "stream-fixture".into(),
        kind: "fixture.recorded".into(),
        payload: json!({"value":"conserved-stream"}),
    };
    boss_events::PgAuditWriter::new(db.pool.clone())
        .write(&event)
        .await
        .unwrap();
    let original = tokio::time::timeout(std::time::Duration::from_secs(5), original_frames.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reported = tokio::time::timeout(std::time::Duration::from_secs(5), reported_frames.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reported, original);
    assert!(
        String::from_utf8(reported.to_vec())
            .unwrap()
            .contains("conserved-stream")
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].observation.asserted_allowed, Some(true));
    assert_eq!(report.rows[0].observation.recorded_allowed, Some(false));
}
