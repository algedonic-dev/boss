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

use super::common::{StepEvent, dispatcher_actor_header, dispatcher_reader_header};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
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
            .header("x-boss-user", dispatcher_reader_header())
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

        // Name the Subject, not just the step kind. Seven feedback
        // Jobs produce seven identical "Ready: task step needs the
        // platform-admin team" lines, and an inbox where every row
        // reads the same is a list you scroll past.
        let subject = format!("Ready: {} — {}", ev.kind, ev.subject_id);
        let body = format!(
            "A '{}' step is ready on {} {}, waiting on the {} team. \
             Opening this message goes straight to the step.",
            ev.kind, ev.subject_kind, ev.subject_id, role
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
            // Link to the STEP, not the Job. The notification exists
            // because one specific step became ready; landing on the
            // Job leaves the reader to find it again among the others,
            // which is work the message already did. `/jobs/{job}/
            // steps/{step}` is the full-page step surface, so the link
            // opens the thing the message is about.
            //
            // `entity_type` follows the entity: nothing keys on it
            // (the inbox renders `entity_path` directly and shows the
            // type only as a label), and calling a step a job would be
            // a small lie that costs nothing to avoid.
            "entity_ref": {
                "entity_type": "step",
                "entity_id": ev.step_id,
                "entity_path": format!("/jobs/{}/steps/{}", ev.job_id, ev.step_id),
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

    /// Stand-ins for `boss-people` and `boss-messages`. The messages
    /// side captures the posted body so the test can assert what an
    /// operator would actually receive — nothing pinned that before,
    /// which is why the link could point anywhere without a failure.
    async fn mock_services() -> (
        String,
        String,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        use axum::{
            Json, Router,
            routing::{get, post},
        };

        let people = Router::new().route(
            "/api/people",
            get(|| async {
                // Deliberately out of id order: the handler picks the
                // deterministic on-call member (lowest id).
                Json(serde_json::json!([{ "id": "emp-zz" }, { "id": "emp-aa" }]))
            }),
        );
        let people_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let people_addr = people_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(people_listener, people).await.unwrap() });

        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let cap = captured.clone();
        let messages = Router::new().route(
            "/api/messages/send",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = Some(body);
                    Json(serde_json::json!({ "ok": true }))
                }
            }),
        );
        let msg_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let msg_addr = msg_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(msg_listener, messages).await.unwrap() });

        (
            format!("http://{people_addr}"),
            format!("http://{msg_addr}"),
            captured,
        )
    }

    fn ready_payload() -> serde_json::Value {
        serde_json::json!({
            "job_id": "11111111-1111-1111-1111-111111111111",
            "step_id": "22222222-2222-2222-2222-222222222222",
            "kind": "review-design",
            "subject_kind": "custom",
            "subject_id": "docs/design/operating-system-view.md",
            "metadata": { "authority_role": "platform-admin" }
        })
    }

    /// The notification exists because one specific step became ready,
    /// so it must open that step. Linking to the Job leaves the reader
    /// to find it again among the others — work the message already
    /// did.
    #[tokio::test]
    async fn links_to_the_step_not_the_job() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx(ready_payload())).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(
            sent["entity_ref"]["entity_path"],
            "/jobs/11111111-1111-1111-1111-111111111111/steps/22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(sent["entity_ref"]["entity_type"], "step");
        assert_eq!(
            sent["entity_ref"]["entity_id"],
            "22222222-2222-2222-2222-222222222222"
        );
    }

    /// An inbox where every row reads the same is a list you scroll
    /// past. Seven feedback Jobs produced seven identical "Ready: task
    /// step needs the platform-admin team" lines.
    #[tokio::test]
    async fn subject_names_the_subject() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx(ready_payload())).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        let subject = sent["subject"].as_str().unwrap_or_default();
        assert!(
            subject.contains("docs/design/operating-system-view.md"),
            "subject must identify WHICH item: {subject}"
        );
        assert!(subject.contains("review-design"), "subject: {subject}");
        // The role still has to reach the reader; it moved to the body.
        assert!(
            sent["body"]
                .as_str()
                .unwrap_or_default()
                .contains("platform-admin"),
            "body must still name the responsible team: {}",
            sent["body"]
        );
    }

    /// Redelivery is at-least-once, so the id has to be stable per
    /// (step, recipient) or a JetStream retry stacks a duplicate row.
    #[tokio::test]
    async fn notifies_the_lowest_id_holder_with_a_stable_id() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        h.invoke(&[], &ctx(ready_payload())).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-aa");
        assert_eq!(
            sent["id"],
            "notify:22222222-2222-2222-2222-222222222222:emp-aa"
        );
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
