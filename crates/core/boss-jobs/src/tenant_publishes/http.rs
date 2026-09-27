//! `POST /api/tenant/publishes` — the one door a tenant publish stamp
//! is written through (backlog 42da8bd2).
//!
//! Operator machinery like every tenant batch door the same publish
//! writes through (`agents`, `departments`, `sensors`, `credentials`):
//! it admits `crate::trust::is_trusted` and nothing wider. The body is
//! [`NewStamp`]; the door credits the signed caller and stamps its own
//! clock (a record's timestamp — `wall_now`, the source no-wallclock.sh
//! sanctions for exactly this), records row + event through the port,
//! and answers 201 with the [`Stamp`] as recorded, so the verb prints
//! the door's receipt rather than its own belief about it.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use boss_policy_client::CurrentUser;

use super::{NewStamp, TenantPublishes, TenantPublishesError};
use crate::trust::is_trusted;

pub struct TenantPublishesApiState {
    pub repo: Arc<dyn TenantPublishes>,
}

pub fn router(state: TenantPublishesApiState) -> Router {
    Router::new()
        .route("/api/tenant/publishes", post(record))
        .with_state(Arc::new(state))
}

async fn record(
    State(state): State<Arc<TenantPublishesApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewStamp>,
) -> Response {
    if !is_trusted(&user) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "a tenant publish stamp is operator machinery",
                "actor_id": user.id,
                "why": "the stamp is written by the actor that ran `boss tenant publish`, \
                        signed at operator tier like every door that publish writes through",
            })),
        )
            .into_response();
    }
    let stamp = match body.credited(&user.id, boss_clock_client::wall_now()) {
        Ok(s) => s,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    match state.repo.record(&stamp).await {
        Ok(()) => (StatusCode::CREATED, Json(stamp)).into_response(),
        Err(TenantPublishesError::Storage(m)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, m).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::tenant_publishes::{InMemoryTenantPublishes, Stamp, TENANT_PUBLISHED};

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    async fn post_stamp(
        repo: &Arc<InMemoryTenantPublishes>,
        body: serde_json::Value,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let app = router(TenantPublishesApiState {
            repo: repo.clone() as Arc<dyn TenantPublishes>,
        });
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/tenant/publishes")
            .header("content-type", "application/json");
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let resp = app
            .oneshot(
                req.body(Body::from(body.to_string()))
                    .expect("request builds"),
            )
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn body() -> serde_json::Value {
        serde_json::json!({
            "tenant_id": "algedonic", "boss_commit": "6d14407c",
            "took": ["departments"], "writes": 14,
        })
    }

    /// The defect this door closes: a publish from the operator's seat
    /// records the row AND the fact, credited to the signed caller.
    #[tokio::test]
    async fn an_operator_stamp_is_recorded_with_its_event_and_credited_to_the_caller() {
        let repo = Arc::new(InMemoryTenantPublishes::default());
        let (status, text) = post_stamp(
            &repo,
            body(),
            Some(header(
                "agent-claude",
                "platform-admin",
                AccessTier::Operator,
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let answered: Stamp = serde_json::from_str(&text).expect("the answer is the stamp");
        assert_eq!(answered.published_by, "agent-claude");
        assert_eq!(answered.tenant_id, "algedonic");
        assert_eq!(answered.took, vec!["departments".to_string()]);
        assert_eq!(answered.writes, 14);

        let rows = repo.rows();
        assert_eq!(rows.len(), 1);
        let (row, event) = &rows[0];
        assert_eq!(row, &answered, "the answer is what was recorded");
        assert_eq!(event.kind, TENANT_PUBLISHED);
        assert_eq!(event.payload["_actor"], "agent-claude");
        assert_eq!(event.payload["published_by"], "agent-claude");
    }

    #[tokio::test]
    async fn a_caller_below_operator_tier_or_unsigned_is_refused_and_nothing_is_recorded() {
        let repo = Arc::new(InMemoryTenantPublishes::default());
        for user in [
            None,
            Some(header("emp-032", "employee", AccessTier::User)),
            Some(header(
                "automation:run-car-probe-reader",
                "audit-readonly",
                AccessTier::Auditor,
            )),
        ] {
            let (status, text) = post_stamp(&repo, body(), user.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{user:?}: {text}");
        }
        assert!(repo.rows().is_empty());
    }

    #[tokio::test]
    async fn a_body_that_names_an_actor_or_no_tenant_is_refused() {
        let repo = Arc::new(InMemoryTenantPublishes::default());
        let op = || {
            Some(header(
                "agent-claude",
                "platform-admin",
                AccessTier::Operator,
            ))
        };
        let mut forged = body();
        forged["published_by"] = serde_json::json!("emp-david");
        let (status, text) = post_stamp(&repo, forged, op()).await;
        assert!(status.is_client_error(), "{status}: {text}");

        let mut empty = body();
        empty["tenant_id"] = serde_json::json!("");
        let (status, text) = post_stamp(&repo, empty, op()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(repo.rows().is_empty());
    }
}
