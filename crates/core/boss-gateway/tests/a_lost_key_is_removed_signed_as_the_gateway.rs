//! A LOST KEY IS STILL REMOVABLE AFTER PEOPLE'S STORAGE ADMITS ONLY THE
//! GATEWAY (backlog e199c02d, 2026-09-28).
//!
//! boss-people's webauthn storage now admits one caller id,
//! `boss_core::actor::GATEWAY_ACTOR_ID`, and refuses every other
//! whatever its role (`gateway_gate` in
//! `crates/modules/boss-people/src/webauthn.rs`, pinned there against
//! Postgres). The one road a person has to remove a lost authenticator
//! is the gateway's session-bound
//! `DELETE /api/auth/passkey/credentials/{id}`, so that road must reach
//! people signed as exactly that id — ONE `x-boss-user`, not the
//! session's, and not the gateway's with the session's appended after
//! it (backlog 18b9e09d: people reads the first header). If it did not,
//! the storage rule would lock the owner out of his own key management.
//!
//! boss-gateway cannot depend on boss-people (the tier rule), so the
//! people half here is a stub that applies the same rule to the same
//! constant: it answers 204 only to a request whose every identity
//! header names the gateway, and 403 otherwise, recording what it saw.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use boss_gateway::passkey::{PasskeyState, passkey_router};
use boss_gateway::session::{COOKIE_NAME, Session};
use serde_json::Value;
use tower::ServiceExt;
use webauthn_rs::prelude::{Url, WebauthnBuilder};

const KEY: &[u8] = b"lost-key-removal-test-key";
const EMPLOYEE: &str = "emp-lost-key-1";
const CREDENTIAL_ID: &str = "bG9zdC1rZXktY3JlZC1pZA";

/// (path, every x-boss-user id the request carried)
type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

async fn gateway_only_people() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap| {
        let log = log.clone();
        async move {
            let ids: Vec<String> = headers
                .get_all("x-boss-user")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .filter_map(|v| serde_json::from_str::<Value>(v).ok())
                .map(|u| u["id"].as_str().unwrap_or_default().to_string())
                .collect();
            let admitted = ids == [boss_core::actor::GATEWAY_ACTOR_ID];
            log.lock().unwrap().push((uri.path().to_string(), ids));
            if admitted {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::FORBIDDEN
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn router(people_base: String) -> Router {
    let origin = Url::parse("https://boss.test").unwrap();
    passkey_router(Arc::new(PasskeyState {
        session_key: KEY.to_vec(),
        http: boss_gateway::machine_client::MachineClient::unstamped(reqwest::Client::builder())
            .unwrap(),
        people_base: people_base.clone(),
        jobs_base: people_base,
        webauthn: WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .build()
            .unwrap(),
        audit: boss_gateway::audit::AuthAudit::disabled(),
    }))
}

#[tokio::test]
async fn the_owners_lost_key_removal_reaches_people_signed_as_the_gateway_alone() {
    let (people, seen) = gateway_only_people().await;
    let router = router(people);
    let mut sess = Session::new("lost-key", 600);
    sess.employee_id = Some(EMPLOYEE.to_string());
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/auth/passkey/credentials/{CREDENTIAL_ID}"))
        .header("cookie", format!("{COOKIE_NAME}={}", sess.encode(KEY)))
        .body(Body::empty())
        .unwrap();
    let status = router.oneshot(req).await.unwrap().status();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            format!("/api/people/{EMPLOYEE}/webauthn-credentials/{CREDENTIAL_ID}"),
            vec![boss_core::actor::GATEWAY_ACTOR_ID.to_string()],
        )],
        "the removal reached people once, carrying the gateway's id and no other"
    );
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a store that admits only the gateway still removes the owner's lost key"
    );
}
