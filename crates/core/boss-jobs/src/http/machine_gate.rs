//! The machine door's write gate (feedback 7fcd78fa, phase 1).
//!
//! The jobs API trusts `x-boss-user` verbatim — that is the machine
//! door, and until now it authenticated nothing. This layer requires
//! the shared machine token (`boss_core::machine_token`) on every
//! state-changing request WHEN the process has a token configured.
//! Reads stay open in phase 1; they join in phase 2 once every
//! legitimate caller demonstrably carries the token.
//!
//! A standalone middleware rather than a `JobsApiState` field: the
//! gate guards the whole door (including scheduling/cadence routers
//! merged beside the jobs router), and the binary is the one place
//! that knows it is exposed on a network rather than mounted in a
//! test harness.
//!
//! With NO token configured the gate admits everything — deploy-order
//! safety. Code lands everywhere first; the token turning on is a
//! pure ops action, and unsetting it is the rollback.

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use boss_core::machine_token;

/// Layer body: `.layer(middleware::from_fn(move |req, next|
/// machine_gate(token.clone(), req, next)))` where `token` is
/// `machine_token::from_env()` read once at startup.
pub async fn machine_gate(expected: Option<String>, req: Request, next: Next) -> Response {
    let Some(expected) = expected else {
        return next.run(req).await;
    };
    // Reads stay open in phase 1. HEAD/OPTIONS ride with GET — they
    // change nothing and blocking OPTIONS would break CORS preflight
    // before the browser ever sends the real request.
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let provided = req
        .headers()
        .get(machine_token::HEADER)
        .and_then(|v| v.to_str().ok());
    if machine_token::verify(&expected, provided) {
        return next.run(req).await;
    }
    // 401 with the header named: the first symptom of a writer missed
    // by the activation runbook is this line in its log, and it should
    // read as "attach the token", not as a mystery.
    (
        StatusCode::UNAUTHORIZED,
        format!(
            "machine door: writes require the `{}` header to match the configured \
             machine token (feedback 7fcd78fa phase 1); this caller sent {}",
            machine_token::HEADER,
            if provided.is_some() {
                "a token that does not match"
            } else {
                "no token"
            },
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::{get, put};
    use tower::ServiceExt;

    fn app(token: Option<&str>) -> Router {
        let token = token.map(str::to_string);
        Router::new()
            .route("/api/jobs/{id}", put(|| async { "written" }))
            .route("/api/jobs/{id}", get(|| async { "read" }))
            .layer(axum::middleware::from_fn(move |req, next| {
                machine_gate(token.clone(), req, next)
            }))
    }

    async fn status(app: Router, method: &str, header: Option<&str>) -> StatusCode {
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri("/api/jobs/j1");
        if let Some(h) = header {
            req = req.header(machine_token::HEADER, h);
        }
        let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        resp.status()
    }

    #[tokio::test]
    async fn unconfigured_door_admits_writes_unchanged() {
        // Deploy-order safety: the gate ships everywhere before the
        // token exists, and must be inert until ops sets it.
        assert_eq!(status(app(None), "PUT", None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn configured_door_refuses_a_tokenless_write() {
        assert_eq!(
            status(app(Some("s3cret")), "PUT", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn configured_door_refuses_a_wrong_token() {
        assert_eq!(
            status(app(Some("s3cret")), "PUT", Some("guess")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn configured_door_admits_the_right_token() {
        assert_eq!(
            status(app(Some("s3cret")), "PUT", Some("s3cret")).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn reads_stay_open_in_phase_one() {
        // Phase 2 flips this test, deliberately: reads join only once
        // every legitimate caller demonstrably carries the token.
        assert_eq!(
            status(app(Some("s3cret")), "GET", None).await,
            StatusCode::OK
        );
    }
}
