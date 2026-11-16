//! The clock repairs missed protocol evaluation without resending business work.
use super::common::{api_client, dispatcher_actor_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

pub struct JobsReconcileReadiness {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsReconcileReadiness {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }
}

#[async_trait]
impl Handler for JobsReconcileReadiness {
    fn name(&self) -> &'static str {
        "jobs.reconcile-readiness"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/jobs/readiness",
            self.jobs_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url} response: {e}")))?;
        if !status.is_success() {
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        let report: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            HandlerError::Downstream(format!("readiness response is not JSON: {e}; {body}"))
        })?;
        if report["complete"] != true
            || report["global"] != true
            || report["satisfied_pending"].as_u64() != Some(0)
            || !report["errors"].as_array().is_some_and(Vec::is_empty)
        {
            return Err(HandlerError::Downstream(format!(
                "readiness recovery did not certify a complete global zero: {body}"
            )));
        }
        tracing::info!(rule = %ctx.rule_name, report = %body, "readiness recovery completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::{Json, Router, routing::post};
    use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
    use serde_json::json;

    async fn invoke_reply(reply: serde_json::Value) -> Result<(), HandlerError> {
        let app = Router::new().route(
            "/api/jobs/readiness",
            post(move |headers: HeaderMap| {
                let reply = reply.clone();
                async move {
                    let caller: serde_json::Value =
                        serde_json::from_str(headers["x-boss-user"].to_str().unwrap()).unwrap();
                    assert_eq!(caller["id"], "rule:pending-readiness-every-five-minutes");
                    Json(reply)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = JobsReconcileReadiness::new(format!("http://{addr}"))
            .invoke(
                &[],
                &InvocationContext {
                    rule_name: "pending-readiness-every-five-minutes".into(),
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
    async fn a_clock_firing_requires_a_complete_global_zero_after_recovery() {
        invoke_reply(json!({"complete":true,"global":true,"satisfied_pending":0,"errors":[]}))
            .await
            .unwrap();
        for reply in [
            json!({}),
            json!({"complete":false,"global":true,"satisfied_pending":0,"errors":["registry unreadable"]}),
            json!({"complete":true,"global":false,"satisfied_pending":0,"errors":[]}),
            json!({"complete":true,"global":true,"satisfied_pending":1,"errors":[]}),
            json!({"complete":true,"global":true,"satisfied_pending":0,"errors":["unacknowledged write"]}),
        ] {
            let error = invoke_reply(reply).await.unwrap_err();
            assert!(
                !error.is_permanent(),
                "an incomplete sweep must retry: {error}"
            );
        }
    }
}
