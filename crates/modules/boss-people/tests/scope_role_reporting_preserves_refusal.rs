use boss_policy_client::role_guard::RoleGuardReporter;
use boss_policy_client::role_reader::{
    MonotonicRoleSnapshotClock, RegistryRoles, SnapshotRoleReader,
};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_testing::TestRequest;
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn roster_report_preserves_a_guest_refusal_without_reading_the_database() {
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":"guest-reader","aliases":[],"role":"platform-admin"}],"total":1}),
        json!({"data":[],"total":0}), json!([])).unwrap())));
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
    let app = boss_people::scope::scope_router_with_reports(pool, Some(reporter));
    for path in [
        "/api/people/emp-other/scope",
        "/api/people/by-email/hidden@example.com/bootstrap",
    ] {
        let response = TestRequest::get(path)
            .as_user("guest-reader", "visitor")
            .send(&app)
            .await;
        response.assert_status(axum::http::StatusCode::FORBIDDEN);
        assert!(!response.body_text().contains("emp-other"));
        assert!(!response.body_text().contains("hidden@example.com"));
    }
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 2);
    for row in report.rows {
        assert_eq!(row.observation.asserted_allowed, Some(false));
        assert_eq!(row.observation.recorded_allowed, Some(true));
    }
}

mod common;

#[tokio::test]
async fn recorded_role_denial_preserves_successful_scope_and_bootstrap_outputs() {
    use boss_people::port::PeopleRepository;
    let db = boss_testing::TestDb::new().await;
    boss_people::PgPeople::new(db.pool.clone())
        .create_employee(&common::employee_fixture("emp-target"))
        .await
        .unwrap();
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[{"id":"scope-reader","aliases":[],"role":"visitor"}],"total":1}),
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
    let baseline = boss_people::scope::scope_router(db.pool.clone());
    let reported = boss_people::scope::scope_router_with_reports(db.pool.clone(), Some(reporter));
    for path in [
        "/api/people/emp-target/scope",
        "/api/people/by-email/emp-target@boss.io/bootstrap",
    ] {
        let original = TestRequest::get(path)
            .as_user("scope-reader", "platform-admin")
            .send(&baseline)
            .await;
        original.assert_status(axum::http::StatusCode::OK);
        let response = TestRequest::get(path)
            .as_user("scope-reader", "platform-admin")
            .send(&reported)
            .await;
        response.assert_status(axum::http::StatusCode::OK);
        assert_eq!(response.body_text(), original.body_text());
        let value: serde_json::Value = serde_json::from_str(&response.body_text()).unwrap();
        assert_eq!(value["id"], "emp-target");
        assert_eq!(value["role"], "service-tech");
        assert_eq!(value["department"], "service");
        assert_eq!(value["territory_account_ids"], json!([]));
        assert_eq!(value["direct_report_ids"], json!([]));
    }
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 2);
    for row in report.rows {
        assert_eq!(row.observation.asserted_allowed, Some(true));
        assert_eq!(row.observation.recorded_allowed, Some(false));
    }
}
