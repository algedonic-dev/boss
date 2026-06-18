//! `messages.notify` — turn a `step.ready.<kind>` event into an inbox
//! message to the responsible role's on-call member.
//!
//! This is the **push** side of the human-powered-state-machine
//! dispatcher. The **pull** side (the `/api/jobs/assignments` My Day
//! query) is what actually drives work; this handler adds awareness —
//! when a step becomes Ready we resolve its `authority_role` to the
//! active employees who hold it and message the deterministic on-call
//! member (lowest id), linking the message to the Job. One message per
//! ready step — no role-wide fan-out. Steps with no `authority_role`
//! (generic / outcome kinds an operator picks off a queue) are a no-op.

use super::common::{StepEvent, dispatcher_actor_header};
use crate::rules::expr::Value;
use crate::rules::handler::{Handler, HandlerError, InvocationContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct EmployeeLite {
    id: String,
}

pub struct MessagesNotify {
    client: reqwest::Client,
    people_base: String,
    messages_base: String,
}

impl MessagesNotify {
    pub fn new(people_base: impl Into<String>, messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            people_base: people_base.into(),
            messages_base: messages_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// mock server; production passes a fresh client).
    pub fn with_client(
        client: reqwest::Client,
        people_base: impl Into<String>,
        messages_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            people_base: people_base.into(),
            messages_base: messages_base.into(),
        })
    }
}

#[async_trait]
impl Handler for MessagesNotify {
    fn name(&self) -> &'static str {
        "messages.notify"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        // `authority_role` lives in the step's metadata. No role => a
        // generic / outcome step an operator picks off a queue; nothing
        // to route, so no-op.
        let Some(role) = ev
            .metadata
            .get("authority_role")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty())
        else {
            return Ok(());
        };

        // Resolve the role to its active members; notify the
        // deterministic on-call member (lowest id), mirroring the
        // assignment pick so the recipient is a stable choice.
        let people_url = format!(
            "{}/api/people?role={}&status=active",
            self.people_base.trim_end_matches('/'),
            role,
        );
        let resp = self
            .client
            .get(&people_url)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {people_url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {people_url} returned {status}: {body}"
            )));
        }
        let mut emps: Vec<EmployeeLite> = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("people response not JSON: {e}")))?;
        emps.sort_by(|a, b| a.id.cmp(&b.id));
        // No active member in the role — leave it for the pull-side role
        // queue; nothing to notify.
        let Some(recipient) = emps.first() else {
            return Ok(());
        };

        let subject = format!("Ready: {} step needs the {} team", ev.kind, role);
        let body = format!(
            "A '{}' step is ready on job {}. Pick it up from My Day.",
            ev.kind, ev.job_id
        );
        let msg = json!({
            // Deterministic id `notify:{step_id}:{recipient}`. A
            // redelivered `step.ready.<kind>` event (JetStream
            // at-least-once) re-runs this handler; the stable id collapses
            // on the messages `ON CONFLICT (id) DO NOTHING` insert instead
            // of stacking a duplicate inbox row. Per-recipient so a future
            // role-fan-out keys cleanly; one row per (step, recipient).
            "id": format!("notify:{}:{}", ev.step_id, recipient.id),
            "sender_id": "automation:dispatcher",
            "recipient_id": recipient.id,
            "subject": subject,
            "body": body,
            "kind": "signal",
            "entity_ref": {
                "entity_type": "job",
                "entity_id": ev.job_id,
                "entity_path": format!("/jobs/{}", ev.job_id),
            },
        });
        let msg_url = format!(
            "{}/api/messages/send",
            self.messages_base.trim_end_matches('/')
        );
        let mresp = self
            .client
            .post(&msg_url)
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .json(&msg)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {msg_url}: {e}")))?;
        if !mresp.status().is_success() {
            let status = mresp.status();
            let body = mresp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {msg_url} returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "notify-assignee-on-step-ready".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.ready.bill-approval".into(),
            event_payload: payload,
        }
    }

    #[tokio::test]
    async fn no_authority_role_is_noop() {
        // metadata present but no authority_role -> Ok without any HTTP
        // call (the URLs are unreachable; a call would error).
        let h = MessagesNotify::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let payload = serde_json::json!({
            "job_id": "11111111-1111-1111-1111-111111111111",
            "step_id": "22222222-2222-2222-2222-222222222222",
            "kind": "outcome",
            "subject_kind": "vendor",
            "subject_id": "vnd-1",
            "metadata": { "outcome_kind": "completed" }
        });
        let res = h.invoke(&[], &ctx(payload)).await;
        assert!(res.is_ok(), "no-role step should be a no-op: {res:?}");
    }

    #[tokio::test]
    async fn malformed_payload_errors() {
        let h = MessagesNotify::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        let res = h
            .invoke(&[], &ctx(serde_json::json!("not-an-object")))
            .await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }
}
