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

use super::common::{
    StepEvent, dispatcher_actor_header, dispatcher_reader_header, sim_origin_value,
};
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

        // An ASSIGNEE wins over a role, and a step with neither is the
        // only no-op.
        //
        // This used to key on `authority_role` alone, reasoning that a
        // step without one was "generic — an operator picks it off a
        // queue". That is true of outcome steps and false of the
        // `task` StepType, which is documented as "Simple assigned
        // task for HR, IT, admin" with `required_roles = []`:
        // assignment IS its routing mechanism. So filing a task FOR
        // someone told them nothing, and the manual notification the
        // Job model exists to remove had to be sent by hand.
        //
        // The assignee takes precedence because it is the more
        // specific claim: a role says someone like you should do this,
        // an assignee says you specifically.
        //
        // Measured before changing it: of 39,347 steps, 2,550 carry an
        // assignee and no role, and 2,525 of those are on simulated
        // Jobs — about 14 extra messages per sim-day, in a system
        // already sending thousands. Proportionate, not a storm.
        let recipient_id: Option<String> = ev.assignee_id.map(str::to_string);
        let role = ev
            .metadata
            .get("authority_role")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty());
        if recipient_id.is_none() && role.is_none() {
            return Ok(());
        }

        // With an assignee, there is nothing to resolve — that IS the
        // recipient. Only the role path needs a lookup.
        let (recipient, waiting_on) = match (&recipient_id, role) {
            (Some(id), _) => (id.clone(), format!("assigned to {id}")),
            (None, Some(r)) => {
                // Resolve the role to its active members; notify the
                // deterministic on-call member (lowest id), mirroring
                // the assignment pick so the recipient is stable.
                let people_url = format!(
                    "{}/api/people?role={}&status=active",
                    self.people_base.trim_end_matches('/'),
                    r,
                );
                let resp = self
                    .client
                    .get(&people_url)
                    .header("x-boss-user", dispatcher_reader_header())
                    .header("x-sim-origin", sim_origin_value())
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
                let mut emps: Vec<EmployeeLite> = resp.json().await.map_err(|e| {
                    HandlerError::Downstream(format!("people response not JSON: {e}"))
                })?;
                emps.sort_by(|a, b| a.id.cmp(&b.id));
                // No active member in the role — leave it for the
                // pull-side role queue; nothing to notify.
                let Some(first) = emps.first() else {
                    return Ok(());
                };
                (first.id.clone(), format!("waiting on the {r} team"))
            }
            (None, None) => return Ok(()),
        };

        // Name the Subject, not just the step kind. Seven feedback
        // Jobs produce seven identical "Ready: task step needs the
        // platform-admin team" lines, and an inbox where every row
        // reads the same is a list you scroll past.
        let subject = format!("Ready: {} — {}", ev.kind, ev.subject_id);
        let body = format!(
            "A '{}' step is ready on {} {}, {}. \
             Opening this message goes straight to the step.",
            ev.kind, ev.subject_kind, ev.subject_id, waiting_on
        );
        let msg = json!({
            // Deterministic id `notify:{step_id}:{recipient}`. A
            // redelivered `step.ready.<kind>` event (JetStream
            // at-least-once) re-runs this handler; the stable id collapses
            // on the messages `ON CONFLICT (id) DO NOTHING` insert instead
            // of stacking a duplicate inbox row. Per-recipient so a future
            // role-fan-out keys cleanly; one row per (step, recipient).
            "id": format!("notify:{}:{}", ev.step_id, recipient),
            "sender_id": "automation:dispatcher",
            "recipient_id": recipient,
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
            .header("x-sim-origin", sim_origin_value())
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
    /// The defect this handler was filed for. A `task` step is
    /// documented as "Simple assigned task for HR, IT, admin" with
    /// `required_roles = []` — assignment IS its routing mechanism —
    /// and this handler used to key on `authority_role` alone, so
    /// filing a task FOR someone told them nothing.
    ///
    /// Caught by the inbox rather than by code: two backlog-items with
    /// gated triage steps notified automatically, while two ad-hoc
    /// tasks assigned to the same person needed a message sent by
    /// hand.
    #[tokio::test]
    async fn an_assigned_step_notifies_its_assignee_with_no_role() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["assignee_id"] = serde_json::json!("emp-bootstrap-admin");
        // No authority_role at all — the case that used to be a no-op.
        payload["metadata"] = serde_json::json!({});
        h.invoke(&[], &ctx(payload)).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-bootstrap-admin");
    }

    /// An assignee is the more specific claim: a role says someone
    /// like you should do this, an assignee says you specifically. So
    /// when both are present the person wins, and the role's on-call
    /// member (emp-aa in the mock) is NOT the recipient.
    #[tokio::test]
    async fn the_assignee_wins_over_the_role() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["assignee_id"] = serde_json::json!("emp-named");
        h.invoke(&[], &ctx(payload)).await.expect("notify");

        let sent = captured
            .lock()
            .unwrap()
            .clone()
            .expect("a message was sent");
        assert_eq!(sent["recipient_id"], "emp-named");
        assert_ne!(sent["recipient_id"], "emp-aa");
    }

    /// Neither signal is still the only no-op. Outcome steps an
    /// operator picks off a queue must not generate an inbox row each.
    #[tokio::test]
    async fn a_step_with_neither_assignee_nor_role_stays_silent() {
        let (people, messages, captured) = mock_services().await;
        let h = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
        let mut payload = ready_payload();
        payload["metadata"] = serde_json::json!({});
        h.invoke(&[], &ctx(payload)).await.expect("no-op");
        assert!(captured.lock().unwrap().is_none(), "nothing should be sent");
    }

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
    async fn neither_signal_makes_no_http_call_at_all() {
        // Renamed from `no_authority_role_is_noop`, which became an
        // overclaim: a step with no authority_role but WITH an assignee
        // now notifies. The narrower truth this still proves is the
        // valuable one — with neither signal the handler returns
        // without touching the network, since the URLs are unreachable
        // and any call would error.
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
