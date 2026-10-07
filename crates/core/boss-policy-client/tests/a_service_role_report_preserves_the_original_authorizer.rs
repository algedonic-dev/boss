use boss_policy_client::role_reader::{
    MonotonicRoleSnapshotClock, RegistryRoles, SnapshotRoleReader,
};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope, User};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn service_shutdown_cancels_the_refresh_source_and_invalidates_its_snapshot() {
    use async_trait::async_trait;
    use boss_core::role_of_record::RoleLookupError;
    use boss_policy_client::role_reader::RoleSnapshotSource;
    struct Paused(Arc<tokio::sync::Notify>);
    #[async_trait]
    impl RoleSnapshotSource for Paused {
        async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
            self.0.notify_one();
            std::future::pending().await
        }
    }
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let ticket = roles.begin_refresh();
    assert!(
        roles.finish_refresh(
            ticket,
            Ok(RegistryRoles::from_sources(
                json!({"data":[],"total":0}),
                json!({"data":[],"total":0}),
                json!([]),
            )
            .unwrap())
        )
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/peer", listener.local_addr().unwrap());
    let app =
        axum::Router::new().route(
            "/peer",
            axum::routing::get(
                |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<
                    std::net::SocketAddr,
                >| async move { peer.ip().to_string() },
            ),
        );
    let running = roles.clone();
    let source = Arc::new(Paused(entered.clone()));
    let server = tokio::spawn(async move {
        boss_policy_client::role_service::serve_with_refresh(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            running,
            source,
            Arc::new(ReportMode::Report),
            Duration::from_secs(3600),
            async {
                let _ = stopped.await;
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    let response = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.unwrap(), "127.0.0.1");
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        roles.lookup("actor").is_err(),
        "stopped owner must not leave a fresh clean snapshot"
    );
}

#[tokio::test]
async fn service_wiring_compares_roles_but_uses_the_original_policy_for_inventory() {
    let original = Arc::new(
        FakePolicyClient::builder()
            .allow("writer", Action::Create, Resource::class(), Scope::All)
            .allow("reader", Action::Read, Resource::policy_rule(), Scope::All)
            .build(),
    );
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":"automation:fixture", "aliases":[], "role":"visitor"}],"total":1}),
        json!({"data":[],"total":0}), json!([]),
    ).unwrap())));
    let tally = Arc::new(ReportTally::new(8));
    let wiring = boss_policy_client::role_service::assemble(
        "fixture",
        "/api/fixture/actor-role-reports",
        original,
        roles,
        Arc::new(ReportMode::Report),
        tally.clone(),
    );
    let mut writer = User::service("fixture");
    writer.role = "writer".into();
    assert!(
        wiring
            .policy
            .check(&writer, Action::Create, Resource::class())
            .await
            .unwrap()
            .is_allowed()
    );
    assert_eq!(tally.snapshot().rows[0].observation.would_deny, Some(true));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/fixture/actor-role-reports",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, wiring.inventory).await.unwrap();
    });
    let http = reqwest::Client::new();
    assert_eq!(http.get(&url).send().await.unwrap().status().as_u16(), 403);
    let mut reader = User::service("reader");
    reader.role = "reader".into();
    let response = http
        .get(url)
        .header("x-boss-user", serde_json::to_string(&reader).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["service"], "fixture");
    assert_eq!(body["report"]["durable_window"], false);
    assert_eq!(
        tally.snapshot().rows.len(),
        1,
        "reading reports must not observe the observer"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn shutdown_stops_refresh_before_a_connected_event_stream_drains() {
    use async_trait::async_trait;
    use boss_core::role_of_record::RoleLookupError;
    use boss_policy_client::role_reader::RoleSnapshotSource;
    use futures::StreamExt;
    struct Cancelled(Arc<tokio::sync::Notify>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    struct Source {
        entered: Arc<tokio::sync::Notify>,
        cancelled: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl RoleSnapshotSource for Source {
        async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
            let _cancellation = Cancelled(self.cancelled.clone());
            self.entered.notify_one();
            std::future::pending().await
        }
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let source = Arc::new(Source {
        entered: entered.clone(),
        cancelled: cancelled.clone(),
    });
    let app = axum::Router::new().route(
        "/stream",
        axum::routing::get(|| async {
            let initial = futures::stream::once(async {
                Ok::<_, std::convert::Infallible>(
                    axum::response::sse::Event::default().data("connected"),
                )
            });
            axum::response::Sse::new(initial.chain(futures::stream::pending()))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        boss_policy_client::role_service::serve_with_refresh(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            roles,
            source,
            Arc::new(ReportMode::Report),
            Duration::from_secs(3600),
            async {
                let _ = stopped.await;
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    let mut response = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(
        response
            .chunk()
            .await
            .unwrap()
            .unwrap()
            .windows(9)
            .any(|bytes| bytes == b"connected")
    );
    stop.send(()).unwrap();
    let cancellation = tokio::time::timeout(Duration::from_secs(1), cancelled.notified()).await;
    assert!(
        !server.is_finished(),
        "the connected stream must still own graceful drain"
    );
    drop(response);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        cancellation.is_ok(),
        "shutdown must cancel refresh before waiting for the stream to drain"
    );
}
