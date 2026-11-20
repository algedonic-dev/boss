//! The clock observes every roster pool through the people service's door
//! (backlog a6cb448a; design 4b27a65a, station-tree.md §3).
//!
//! The door owns every judgement — which pools declare a roster, the
//! snapshot, the owner's fence, whether anything changed — and takes no
//! input; this handler owns only the firing. A refusal of the grant is the
//! same on every redelivery, so it is permanent; anything else, including a
//! pool the door could not observe (409, naming it), retries on the next
//! delivery and lands on the dispatcher's firing record.
//!
//! WHY `pools.` AND NOT `people.`. What the firing changes is the station
//! tree's pool registry — a platform resource every deployment carries —
//! and the people service is only where the roster's count can be read.
//! `people.*` is a tenant's business effect by the ratchet in
//! boss-dispatcher's `the_product_rules_are_the_platforms` (gate bd3bd07a
//! was refused on it under the first name, `people.refresh-roster-pools`).
use super::common::{api_client, dispatcher_actor_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

pub struct PoolsObserveRosters {
    client: boss_core::machine_token::Client,
    people_base: String,
}

impl PoolsObserveRosters {
    pub fn new(people_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            people_base: people_base.into(),
        })
    }
}

#[async_trait]
impl Handler for PoolsObserveRosters {
    fn name(&self) -> &'static str {
        "pools.observe-rosters"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/people/roster-capacity/refresh",
            self.people_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url} response: {e}")))?;
        if status.is_success() {
            tracing::info!(rule = %ctx.rule_name, %body, "roster pools observed");
            return Ok(());
        }
        let message = format!("POST {url} returned {status}: {body}");
        Err(if status == reqwest::StatusCode::FORBIDDEN {
            HandlerError::Permanent(message)
        } else {
            HandlerError::Downstream(message)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::{Router, routing::post};
    use serde_json::json;

    async fn invoke_reply(status: StatusCode) -> Result<(), HandlerError> {
        let app = Router::new().route(
            "/api/people/roster-capacity/refresh",
            post(move |headers: HeaderMap| async move {
                let caller: serde_json::Value =
                    serde_json::from_str(headers["x-boss-user"].to_str().unwrap()).unwrap();
                assert_eq!(
                    caller["id"],
                    "rule:roster-pools-are-observed-every-five-minutes"
                );
                assert_eq!(caller["role"], "platform-admin");
                (status, axum::Json(json!({"observed": []}))).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = PoolsObserveRosters::new(format!("http://{addr}"))
            .invoke(
                &[],
                &InvocationContext {
                    rule_name: "roster-pools-are-observed-every-five-minutes".into(),
                    triggering_event_id: "tick-1".into(),
                    triggering_topic: "clock.tick".into(),
                    event_payload: json!({}),
                    event_timestamp: None,
                },
            )
            .await;
        server.abort();
        result
    }

    #[tokio::test]
    async fn an_observed_roster_ends_the_firing() {
        invoke_reply(StatusCode::OK).await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_grant_is_permanent_and_an_unobserved_pool_retries() {
        assert!(
            invoke_reply(StatusCode::FORBIDDEN)
                .await
                .unwrap_err()
                .is_permanent()
        );
        for status in [StatusCode::CONFLICT, StatusCode::SERVICE_UNAVAILABLE] {
            assert!(!invoke_reply(status).await.unwrap_err().is_permanent());
        }
    }
}
