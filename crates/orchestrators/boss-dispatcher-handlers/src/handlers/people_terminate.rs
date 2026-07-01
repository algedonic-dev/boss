//! `people.terminate` — PUT `/api/people/{id}/status` with
//! status=terminated. Reads a `terminate` block from step metadata.

use super::common::{StepEvent, dispatcher_actor_header};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct TerminateFields {
    id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    last_day: Option<String>,
}

pub struct PeopleTerminate {
    client: reqwest::Client,
    people_base: String,
}

impl PeopleTerminate {
    pub fn new(people_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            people_base: people_base.into(),
        })
    }
}

#[async_trait]
impl Handler for PeopleTerminate {
    fn name(&self) -> &'static str {
        "people.terminate"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let Some(raw) = step.metadata.get("terminate") else {
            return Ok(());
        };
        let t: TerminateFields = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode terminate: {e}")))?;

        let body = json!({
            "status": "terminated",
            "notes": t.reason.unwrap_or_else(|| "sim-driven".to_string()),
            "initiated_by": format!("rule:{}", ctx.rule_name),
            "last_day": t.last_day,
        });

        let url = format!(
            "{}/api/people/{}/status",
            self.people_base.trim_end_matches('/'),
            t.id
        );
        let resp = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {status}: {body}"
            )));
        }
        Ok(())
    }
}
