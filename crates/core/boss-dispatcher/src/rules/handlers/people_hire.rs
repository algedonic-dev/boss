//! `people.hire` — POST a new Employee row to `/api/people`.
//! Reads a `hire` block from step metadata.

use super::common::{self, StepEvent};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct HireFields {
    id: String,
    name: String,
    role: String,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    hire_date: Option<String>,
    #[serde(default)]
    employment_type: Option<String>,
    #[serde(default)]
    annual_salary_cents: Option<i64>,
    #[serde(default)]
    manager_id: Option<String>,
}

pub struct PeopleHire {
    client: reqwest::Client,
    people_base: String,
}

impl PeopleHire {
    pub fn new(people_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            people_base: people_base.into(),
        })
    }
}

#[async_trait]
impl Handler for PeopleHire {
    fn name(&self) -> &'static str {
        "people.hire"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let Some(raw) = step.metadata.get("hire") else {
            return Ok(());
        };
        let h: HireFields = serde_json::from_value(raw.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode hire: {e}")))?;
        let completed_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        let body = json!({
            "id": h.id,
            "name": h.name,
            "role": h.role,
            "department": h.department.unwrap_or_else(|| "operations".to_string()),
            "location": h.location.unwrap_or_else(|| "loc-brewery-brewhouse".to_string()),
            "email": h.email.unwrap_or_else(|| format!("{}@example.brewery", h.id)),
            "hire_date": h.hire_date.unwrap_or_else(|| completed_on.to_string()),
            "employment_type": h.employment_type.unwrap_or_else(|| "full-time".to_string()),
            "annual_salary_cents": h.annual_salary_cents,
            "manager_id": h.manager_id,
            "status": "active",
            "skills": [],
            "certifications": [],
        });

        let url = format!("{}/api/people", self.people_base.trim_end_matches('/'));
        // Lenient inline POST (the `post_json` doc carves out exactly this
        // case). A redelivered hire (JetStream at-least-once) re-POSTs the
        // same employee; the people-api returns 409 "already exists" and
        // leaves the existing row untouched — the people-api pre-checks
        // existence (SELECT EXISTS), it is not an ON CONFLICT upsert. Treat that 409 as a clean no-op SUCCESS so a
        // redelivery doesn't NAK → dead-letter. This is a liveness fix, not
        // a leak fix: only the handler's error tolerance changes, never the
        // create. Every other non-2xx still surfaces as Downstream → NAK.
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header(
                "x-boss-user",
                common::dispatcher_actor_header(&ctx.rule_name),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        let status = resp.status();
        let resp_body = resp.text().await.unwrap_or_default();
        Err(HandlerError::Downstream(format!(
            "POST {url} returned {status}: {resp_body}"
        )))
    }
}
