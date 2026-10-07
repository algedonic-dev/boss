use boss_policy_client::role_reader::{MonotonicRoleSnapshotClock, SnapshotRoleReader};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope, User};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn inventory_requires_original_read_authority_and_records_no_comparison() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "report-reader",
                Action::Read,
                Resource::policy_rule(),
                Scope::All,
            )
            .build(),
    );
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let tally = Arc::new(ReportTally::new(8));
    let app = boss_policy_client::role_inventory::router(
        "people",
        "/api/people/actor-role-reports",
        policy,
        roles,
        Arc::new(ReportMode::Report),
        tally.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/people/actor-role-reports",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mut reader = User::service("reader");
    reader.role = "report-reader".into();
    let response = client
        .get(&url)
        .header("x-boss-user", serde_json::to_string(&reader).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["service"], "people");
    assert_eq!(value["snapshot"]["state"], "never-loaded");
    assert_eq!(value["report"]["durable_window"], false);
    assert!(tally.snapshot().rows.is_empty());
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
