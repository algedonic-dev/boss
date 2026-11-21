//! The clock expires lapsed resource leases through the jobs API's sweep
//! door (backlog 3aeaf08c; design a236103a D4).
//!
//! The door owns the judgement — its own clock, the generation fence, the
//! cap — and this handler owns only the firing: POST until a batch comes
//! back short of its limit, at most `MAX_BATCHES` times, and then fail
//! LOUD rather than report a quiet pass over a backlog it did not drain.
//! A failure retries on the next delivery and, if it persists, lands on
//! the dispatcher's firing record as every handler's does.
use super::common::{api_client, dispatcher_actor_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

/// Batches in one firing. The door caps a batch at 64, so a firing drains
/// up to 512 lapsed leases; a backlog larger than that every five minutes
/// is a fault worth an error, not a reason to loop longer.
const MAX_BATCHES: usize = 8;

pub struct JobsSweepResourceLeases {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsSweepResourceLeases {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }
}

#[async_trait]
impl Handler for JobsSweepResourceLeases {
    fn name(&self) -> &'static str {
        "jobs.sweep-resource-leases"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/jobs/resource-leases/sweep",
            self.jobs_base.trim_end_matches('/')
        );
        let mut total = 0;
        for _ in 0..MAX_BATCHES {
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
            if !status.is_success() {
                // A refusal of the grant or the bound is the same on every
                // redelivery: say so once and stop, rather than burn the
                // budget on an answer that cannot change.
                let message = format!("POST {url} returned {status}: {body}");
                return Err(
                    if status == reqwest::StatusCode::FORBIDDEN
                        || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
                    {
                        HandlerError::Permanent(message)
                    } else {
                        HandlerError::Downstream(message)
                    },
                );
            }
            let report: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
                HandlerError::Downstream(format!("lease sweep response is not JSON: {e}; {body}"))
            })?;
            let (Some(expired), Some(limit), Some(saturated)) = (
                report["expired"].as_u64(),
                report["limit"].as_u64(),
                report["saturated"].as_bool(),
            ) else {
                return Err(HandlerError::Downstream(format!(
                    "lease sweep report lacks expired/limit/saturated: {body}"
                )));
            };
            if expired > limit || saturated != (expired == limit) {
                return Err(HandlerError::Downstream(format!(
                    "lease sweep report is inconsistent: {body}"
                )));
            }
            total += expired;
            if !saturated {
                tracing::info!(rule = %ctx.rule_name, expired = total, "resource lease sweep completed");
                return Ok(());
            }
        }
        Err(HandlerError::Downstream(format!(
            "{MAX_BATCHES} full lease-sweep batches ({total} leases expired) and more remain \
             lapsed: the backlog outran one firing"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::{Router, routing::post};
    use serde_json::json;
    use std::sync::Mutex;

    /// Serve `replies` in order to POSTs on the sweep door and return the
    /// handler's result and the number of POSTs it made.
    async fn invoke_replies(
        replies: Vec<(StatusCode, serde_json::Value)>,
    ) -> (Result<(), HandlerError>, usize) {
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        let calls = Arc::new(Mutex::new(0usize));
        let counted = calls.clone();
        let app = Router::new().route(
            "/api/jobs/resource-leases/sweep",
            post(move |headers: HeaderMap| {
                let replies = replies.clone();
                let counted = counted.clone();
                async move {
                    let caller: serde_json::Value =
                        serde_json::from_str(headers["x-boss-user"].to_str().unwrap()).unwrap();
                    assert_eq!(
                        caller["id"],
                        "rule:resource-leases-are-swept-every-five-minutes"
                    );
                    assert_eq!(caller["role"], "platform-admin");
                    *counted.lock().unwrap() += 1;
                    let (status, body) = replies
                        .lock()
                        .unwrap()
                        .next()
                        .expect("no more replies scripted");
                    (status, axum::Json(body)).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = JobsSweepResourceLeases::new(format!("http://{addr}"))
            .invoke(
                &[],
                &InvocationContext {
                    rule_name: "resource-leases-are-swept-every-five-minutes".into(),
                    triggering_event_id: "tick-1".into(),
                    triggering_topic: "clock.tick".into(),
                    event_payload: json!({}),
                    event_timestamp: None,
                },
            )
            .await;
        server.abort();
        let n = *calls.lock().unwrap();
        (result, n)
    }

    fn batch(expired: u64) -> (StatusCode, serde_json::Value) {
        (
            StatusCode::OK,
            json!({"observed_at":"2026-11-06T12:00:00Z","expired":expired,"limit":64,"saturated":expired == 64}),
        )
    }

    #[tokio::test]
    async fn a_short_batch_ends_the_firing() {
        let (result, calls) = invoke_replies(vec![batch(3)]).await;
        result.unwrap();
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn a_full_batch_fires_again_until_one_comes_back_short() {
        let (result, calls) = invoke_replies(vec![batch(64), batch(64), batch(0)]).await;
        result.unwrap();
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn a_backlog_that_outruns_the_firing_is_an_error_not_a_pass() {
        let (result, calls) = invoke_replies(vec![batch(64); MAX_BATCHES]).await;
        let error = result.unwrap_err();
        assert!(!error.is_permanent(), "the next firing retries: {error}");
        assert_eq!(
            calls, MAX_BATCHES,
            "bounded: it never loops past its batches"
        );
    }

    #[tokio::test]
    async fn a_refused_grant_is_permanent_and_a_dark_api_retries() {
        let (refused, _) =
            invoke_replies(vec![(StatusCode::FORBIDDEN, json!("not granted"))]).await;
        assert!(refused.unwrap_err().is_permanent());
        let (dark, _) = invoke_replies(vec![(StatusCode::SERVICE_UNAVAILABLE, json!("db"))]).await;
        assert!(!dark.unwrap_err().is_permanent());
    }

    #[tokio::test]
    async fn a_report_that_does_not_add_up_is_refused() {
        for reply in [
            json!({}),
            json!({"expired":65,"limit":64,"saturated":true}),
            json!({"expired":64,"limit":64,"saturated":false}),
            json!({"expired":3,"limit":64,"saturated":true}),
        ] {
            let (result, _) = invoke_replies(vec![(StatusCode::OK, reply)]).await;
            assert!(!result.unwrap_err().is_permanent());
        }
    }
}
