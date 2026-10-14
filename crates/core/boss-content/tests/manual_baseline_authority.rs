use axum::{body::Body, http::Request};
use boss_policy_client::{FakePolicyClient, PolicyClient, Scope, User, controls};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn hidden_inventory_requires_explicit_scope_all_authority_before_reading_rows() {
    let db = boss_testing::TestDb::new().await;
    let user = User::service("test-observer");
    for scope in [None, Some(Scope::Self_), Some(Scope::All)] {
        let mut builder = FakePolicyClient::builder();
        if let Some(scope) = &scope {
            builder = builder.allow(
                user.role.clone(),
                controls::READ_MANUAL_INVENTORY.action(),
                controls::READ_MANUAL_INVENTORY.resource(),
                scope.clone(),
            );
        }
        let app = boss_content::manual_baseline_http::router(
            boss_content::manual_baseline_http::BaselineApiState {
                repo: Arc::new(boss_content::PgContent::new(db.pool.clone())),
                policy: Arc::new(builder.build()),
                publisher: None,
            },
        );
        let request = Request::builder()
            .uri("/api/content/manual-inventory")
            .header("x-boss-user", serde_json::to_string(&user).unwrap())
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            if scope == Some(Scope::All) { 409 } else { 403 }
        );
    }
}

#[tokio::test]
async fn shipped_grants_do_not_authorize_new_privileged_controls() {
    let policy = FakePolicyClient::builder().with_default_rules().build();
    for role in boss_policy_client::defaults::default_rules()
        .iter()
        .map(|rule| rule.role.clone())
    {
        let mut user = User::service("test-observer");
        user.role = role;
        for pair in [
            controls::READ_MANUAL_INVENTORY,
            controls::CREATE_MANUAL_BASELINE,
        ] {
            assert!(
                !policy.ask(&user, pair).await.unwrap().is_allowed(),
                "shipped role {} unexpectedly covers {}",
                user.role,
                pair.resource_name()
            );
        }
    }
}

#[tokio::test]
async fn audience_read_or_one_observer_grant_cannot_admit_a_baseline() {
    let db = boss_testing::TestDb::new().await;
    let user = User::service("test-observer");
    for pair in [
        controls::READ_MANUAL_INVENTORY,
        controls::CREATE_MANUAL_BASELINE,
    ] {
        let policy = FakePolicyClient::builder()
            .allow(
                user.role.clone(),
                pair.action(),
                pair.resource(),
                Scope::All,
            )
            .build();
        let app = boss_content::manual_baseline_http::router(
            boss_content::manual_baseline_http::BaselineApiState {
                repo: Arc::new(boss_content::PgContent::new(db.pool.clone())),
                policy: Arc::new(policy),
                publisher: None,
            },
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/content/manual-baseline")
            .header("x-boss-user", serde_json::to_string(&user).unwrap())
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"admission_id":uuid::Uuid::new_v4(),"expected_sha256":"test"})
                    .to_string(),
            ))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), 403);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn complete_hidden_inventory_and_observation_keep_the_request_actor() {
    use http_body_util::BodyExt;
    let db = boss_testing::TestDb::new().await;
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO manual_sections (id,slug,title,body,published,audience,created_at,updated_at) VALUES ($1,'hidden','Hidden','Private',false,'{\"roles\":[\"secret\"]}','2010-01-01','2010-01-01')").bind(id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO manual_section_history (section_id,version,title,body,audience,edited_by,edited_at) VALUES ($1,1,'Hidden','Private','{\"roles\":[\"secret\"]}','emp-old','2010-01-01')").bind(id).execute(&db.pool).await.unwrap();
    let mut user = User::service("test-observer");
    user.id = "emp-observer".into();
    let mut builder = FakePolicyClient::builder();
    for pair in [
        controls::READ_MANUAL_INVENTORY,
        controls::CREATE_MANUAL_BASELINE,
    ] {
        builder = builder.allow(
            user.role.clone(),
            pair.action(),
            pair.resource(),
            Scope::All,
        );
    }
    let app = boss_content::manual_baseline_http::router(
        boss_content::manual_baseline_http::BaselineApiState {
            repo: Arc::new(boss_content::PgContent::new(db.pool.clone())),
            policy: Arc::new(builder.build()),
            publisher: Some(boss_core::publisher::DomainPublisher::new(
                boss_testing::RecordingEventBus::new(),
                "content",
            )),
        },
    );
    let read = Request::builder()
        .uri("/api/content/manual-inventory")
        .header("x-boss-user", serde_json::to_string(&user).unwrap())
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(read).await.unwrap();
    assert_eq!(response.status(), 200);
    let inventory: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(inventory["sections"][0]["published"], false);
    assert_eq!(inventory["history"][0]["edited_by"], "emp-old");
    assert!(inventory["history"][0]["id"].as_i64().unwrap() > 0);
    let body = serde_json::json!({"admission_id":uuid::Uuid::new_v4(),"expected_sha256":inventory["sha256"]});
    let write = Request::builder()
        .method("POST")
        .uri("/api/content/manual-baseline")
        .header("x-boss-user", serde_json::to_string(&user).unwrap())
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(write).await.unwrap();
    assert_eq!(response.status(), 200);
    let baseline: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(baseline["observed_by"], "emp-observer");
    assert_eq!(baseline["inventory"], inventory);
    assert_ne!(baseline["event_id"], body["admission_id"]);
    assert_eq!(baseline["event_source"], "content");
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind='content.manual.baseline_observed'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["_actor"], "emp-observer");
    assert_eq!(payload["event_id"], baseline["event_id"]);
    assert_ne!(
        payload["observed_at"],
        inventory["sections"][0]["created_at"]
    );
}

#[tokio::test]
async fn a_dark_authoritative_policy_cannot_fall_back_to_audience_access() {
    let db = boss_testing::TestDb::new().await;
    let dark = boss_testing::dark_port();
    let policy = boss_policy_client::ReqwestPolicyClient::new("test", dark.base.clone());
    let app = boss_content::manual_baseline_http::router(
        boss_content::manual_baseline_http::BaselineApiState {
            repo: Arc::new(boss_content::PgContent::new(db.pool.clone())),
            policy: Arc::new(policy),
            publisher: None,
        },
    );
    let read = Request::builder()
        .uri("/api/content/manual-inventory")
        .header(
            "x-boss-user",
            serde_json::to_string(&User::service("test-observer")).unwrap(),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(read).await.unwrap().status(), 503);
}
