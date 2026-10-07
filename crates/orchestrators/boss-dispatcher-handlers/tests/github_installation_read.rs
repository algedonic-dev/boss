use boss_dispatcher_handlers::handlers::credential_issuer::installation_read::InstallationReader;
use boss_dispatcher_handlers::handlers::credential_issuer::installation_read::{
    ExpectedInstallation, compare_permissions, parse_snapshot,
};
use boss_dispatcher_handlers::handlers::credential_issuer::{
    GitHubApi, GitHubAppRoot, Unconfigured,
};
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock, Mutex};

static APP_KEY: LazyLock<rsa::RsaPrivateKey> =
    LazyLock::new(|| rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap());

fn root() -> GitHubAppRoot {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let pem = APP_KEY.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
    GitHubAppRoot::from_values(Some("42"), Some("77"), Some(&pem)).unwrap()
}

async fn http_reader(status: u16, body: Value) -> (Arc<GitHubApi>, Arc<Mutex<Vec<String>>>) {
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        response::IntoResponse,
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let app = Router::new().fallback(move |request: Request<Body>| {
        let capture = capture.clone();
        let body = body.clone();
        async move {
            use base64::Engine;
            use rsa::signature::Verifier;
            capture
                .lock()
                .unwrap()
                .push(format!("{} {}", request.method(), request.uri()));
            assert_eq!(request.method(), "GET");
            assert_eq!(request.uri().path(), "/app/installations/77");
            let bearer = request.headers()["authorization"].to_str().unwrap();
            let token = bearer.strip_prefix("Bearer ").unwrap();
            let pieces: Vec<_> = token.split('.').collect();
            assert_eq!(pieces.len(), 3);
            let claims: Value = serde_json::from_slice(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(pieces[1])
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(claims["iss"], 42);
            let signature = rsa::pkcs1v15::Signature::try_from(
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(pieces[2])
                    .unwrap()
                    .as_slice(),
            )
            .unwrap();
            rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(APP_KEY.to_public_key())
                .verify(
                    format!("{}.{}", pieces[0], pieces[1]).as_bytes(),
                    &signature,
                )
                .unwrap();
            (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (GitHubApi::new(url, root()).unwrap(), requests)
}

#[tokio::test]
async fn the_adapter_uses_only_the_authenticated_installation_get() {
    let (reader, requests) = http_reader(200, response()).await;
    assert_eq!(reader.app_id().unwrap(), 42);
    assert_eq!(reader.default_installation_id().unwrap(), 77);
    assert_eq!(
        reader
            .read_installation(&expected())
            .await
            .unwrap()
            .account_id,
        123
    );
    assert_eq!(*requests.lock().unwrap(), ["GET /app/installations/77"]);
}

#[tokio::test]
async fn non200_and_malformed_responses_never_become_empty_grants_or_raw_errors() {
    for status in [201, 401, 403, 404, 422, 500] {
        let (reader, requests) =
            http_reader(status, json!({"secret_sentinel":"private response"})).await;
        let error = reader.read_installation(&expected()).await.unwrap_err();
        assert!(!error.contains("private response"));
        assert!(!error.contains("secret_sentinel"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
    let (reader, _) = http_reader(200, json!({"secret_sentinel":"private response"})).await;
    assert!(reader.read_installation(&expected()).await.is_err());
}

#[tokio::test]
async fn a_different_app_expectation_refuses_before_any_request() {
    let (reader, requests) = http_reader(200, response()).await;
    let mut selection = expected();
    selection.app_id = 43;
    assert!(reader.read_installation(&selection).await.is_err());
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_redirect_cannot_move_the_issuer_read_to_another_endpoint() {
    use axum::{Router, http::StatusCode, response::IntoResponse};
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let app = Router::new().fallback(move |request: axum::http::Request<axum::body::Body>| {
        let capture = capture.clone();
        async move {
            capture
                .lock()
                .unwrap()
                .push(request.uri().path().to_string());
            (StatusCode::FOUND, [("location", "/unexpected")]).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reader = GitHubApi::new(url, root()).unwrap();
    assert!(
        reader
            .read_installation(&expected())
            .await
            .unwrap_err()
            .contains("302")
    );
    assert_eq!(*requests.lock().unwrap(), ["/app/installations/77"]);
}

#[tokio::test]
async fn unavailable_transport_refuses_without_an_invented_snapshot() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let reader = GitHubApi::new(format!("http://{address}"), root()).unwrap();
    assert_eq!(
        reader.read_installation(&expected()).await.unwrap_err(),
        "installation GET transport unavailable"
    );
}

#[tokio::test]
async fn an_unconfigured_issuer_reports_unavailable_identity_and_observation() {
    let reader = Unconfigured("test root unavailable".into());
    assert!(reader.app_id().is_err());
    assert!(reader.default_installation_id().is_err());
    assert_eq!(
        reader.read_installation(&expected()).await.unwrap_err(),
        "test root unavailable"
    );
}

fn expected() -> ExpectedInstallation {
    ExpectedInstallation {
        app_id: 42,
        installation_id: 77,
        account_login: "example-org".into(),
        account_id: None,
    }
}

fn response() -> Value {
    json!({"id":77,"app_id":42,"account":{"id":123,"login":"example-org"},
        "permissions":{"contents":"write","metadata":"read","future_permission":"read"},
        "repository_selection":"selected","suspended_at":null})
}

#[test]
fn a_complete_snapshot_conserves_unknown_permissions_and_observed_identity() {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-03T20:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let snapshot = parse_snapshot(response(), &expected(), at).unwrap();
    assert_eq!(snapshot.permissions.len(), 3);
    assert_eq!(snapshot.http_status, 200);
    assert_eq!(snapshot.permissions["future_permission"], "read");
    assert_eq!(snapshot.account_id, 123);
    assert_eq!(snapshot.expected_account_id, None);
    assert_eq!(snapshot.observed_at, at);
}

#[test]
fn absent_or_malformed_consumed_fields_refuse_instead_of_empty_grants() {
    for field in [
        "id",
        "app_id",
        "account",
        "permissions",
        "repository_selection",
        "suspended_at",
    ] {
        let mut body = response();
        body.as_object_mut().unwrap().remove(field);
        assert!(
            parse_snapshot(body, &expected(), chrono::Utc::now()).is_err(),
            "{field}"
        );
    }
    for malformed in [
        json!(null),
        json!([]),
        json!({"contents":7}),
        json!({"contents":""}),
    ] {
        let mut body = response();
        body["permissions"] = malformed;
        assert!(parse_snapshot(body, &expected(), chrono::Utc::now()).is_err());
    }
    let mut empty = response();
    empty["permissions"] = json!({});
    assert!(
        parse_snapshot(empty, &expected(), chrono::Utc::now())
            .unwrap()
            .permissions
            .is_empty()
    );
}

#[test]
fn every_expected_identity_is_checked_without_inventing_numeric_expectations() {
    for (field, value) in [("id", json!(78)), ("app_id", json!(43))] {
        let mut body = response();
        body[field] = value;
        assert!(parse_snapshot(body, &expected(), chrono::Utc::now()).is_err());
    }
    let mut body = response();
    body["account"]["login"] = json!("other-org");
    assert!(parse_snapshot(body, &expected(), chrono::Utc::now()).is_err());
    let mut expectation = expected();
    expectation.account_id = Some(124);
    assert!(parse_snapshot(response(), &expectation, chrono::Utc::now()).is_err());
    expectation.account_id = Some(123);
    assert_eq!(
        parse_snapshot(response(), &expectation, chrono::Utc::now())
            .unwrap()
            .expected_account_id,
        Some(123)
    );
    for login in ["", " ", " example-org", "example-org "] {
        expectation.account_login = login.into();
        assert!(parse_snapshot(response(), &expectation, chrono::Utc::now()).is_err());
    }
}

#[test]
fn a_comparison_distinguishes_missing_insufficient_and_unordered_levels() {
    let snapshot = parse_snapshot(response(), &expected(), chrono::Utc::now()).unwrap();
    let required = std::collections::BTreeMap::from([
        ("contents".into(), "read".into()),
        ("metadata".into(), "write".into()),
        ("missing_permission".into(), "write".into()),
        ("future_permission".into(), "future_level".into()),
    ]);
    let compared = compare_permissions(&snapshot, &required);
    assert_eq!(compared.missing, ["missing_permission"]);
    assert_eq!(compared.insufficient, ["metadata"]);
    assert_eq!(compared.unordered, ["future_permission"]);
    assert_eq!(snapshot.permissions.len(), 3);
}
