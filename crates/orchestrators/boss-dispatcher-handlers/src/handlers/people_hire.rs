//! `people.hire` — POST a new Employee row to `/api/people`.
//! Reads a `hire` block from step metadata.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
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
    client: boss_core::machine_token::Client,
    people_base: String,
}

impl PeopleHire {
    pub fn new(people_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
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
        // No default department (backlog 4096e63e). This used to fill
        // the literal `operations`, an employee department Class the
        // collapse onto the departments registry retired (c87e3d6d) —
        // so the people door refused the handler's own invention. Which
        // department a hire joins is the tenant's fact, declared in its
        // seeds/departments.toml; a hire step that names none is a
        // data error that fails identically on every redelivery, so it
        // is Permanent (the runner's 422) rather than a retry.
        let department = h.department.filter(|d| !d.is_empty()).ok_or_else(|| {
            HandlerError::Permanent(format!(
                "hire {}: the hire block names no department — set `department` to a code \
                 of the departments registry (GET /api/departments)",
                h.id
            ))
        })?;

        let body = json!({
            "id": h.id,
            "name": h.name,
            "role": h.role,
            "department": department,
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
            .header("x-sim-origin", common::sim_origin_value())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A people base nothing listens on: a hire that reaches the POST
    /// fails `Downstream`, so a `Permanent` answer proves the refusal
    /// came BEFORE any write was attempted.
    fn handler() -> Arc<PeopleHire> {
        PeopleHire::new("http://127.0.0.1:9")
    }

    fn ctx(hire: serde_json::Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "people-hire-on-hr-hire-step-done".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.done.hr-hire".into(),
            event_payload: json!({
                "job_id": "job-1",
                "step_id": "step-1",
                "kind": "hr-hire",
                "completed_on": "2026-09-28",
                "metadata": { "hire": hire },
            }),
        }
    }

    /// Backlog 4096e63e: a hire naming no department was written into
    /// `operations`, an employee department Class the collapse onto the
    /// departments registry retired (c87e3d6d) — so the people door
    /// refused the default this handler invented. No literal can be
    /// right for every tenant; the refusal names where departments live.
    #[tokio::test]
    async fn a_hire_naming_no_department_is_refused_naming_the_registry() {
        for hire in [
            json!({ "id": "emp-x", "name": "X", "role": "analyst" }),
            json!({ "id": "emp-x", "name": "X", "role": "analyst", "department": "" }),
        ] {
            match handler().invoke(&[], &ctx(hire.clone())).await {
                Err(HandlerError::Permanent(msg)) => assert!(
                    msg.contains("emp-x")
                        && msg.contains("department")
                        && msg.contains("departments registry"),
                    "the refusal names the hire and the registry: {msg}"
                ),
                other => panic!("{hire}: a hire with no department must be refused, got {other:?}"),
            }
        }
    }

    /// The department the step names is the one sent — only the
    /// missing case is refused.
    #[tokio::test]
    async fn a_hire_naming_a_department_reaches_the_people_door() {
        let hire =
            json!({ "id": "emp-x", "name": "X", "role": "analyst", "department": "production" });
        assert!(
            matches!(
                handler().invoke(&[], &ctx(hire)).await,
                Err(HandlerError::Downstream(m)) if m.contains("/api/people")
            ),
            "a named department goes to the POST (which fails here: nothing listens)"
        );
    }
}
