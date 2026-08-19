//! `messages.notify_job_terminal` — the packet's filer hears how it
//! ended.
//!
//! David ratified both halves of the rule together: "Once the user
//! feedback results in either a shipped change or some other terminal
//! state, it can be closed without the filer approving. But, we should
//! always notify the filer with the terminal state." The system MAY
//! close the packet itself; it MUST say so. A close nobody is told
//! about is indistinguishable, from the filer's side, from the
//! sixteen packets that sat at `submitted` while their work shipped.
//!
//! Fires on `jobs.job.closed` — ANY terminal, not just the one a
//! shipped change produces. `duplicate` and `declined` are terminals a
//! filer is owed an answer about for exactly the same reason.
//!
//! ## The channel is `boss-messages`, not a new one
//!
//! `POST /api/messages/send` with a caller-supplied deterministic id,
//! the same surface `messages.notify` uses for step-ready pushes. Its
//! `ON CONFLICT (id) DO NOTHING` insert is the idempotence guard:
//! JetStream is at-least-once, so a redelivered close re-runs this
//! handler, and the stable id `{id_prefix}:{job_id}:{recipient}`
//! collapses the second insert instead of stacking a duplicate inbox
//! row. No second channel, no second notion of "sent".
//!
//! ## What the message has to say
//!
//! Three things, or it is noise: WHICH packet (title, short id, and
//! the surface it was about), WHAT terminal it reached, and WHAT
//! satisfied it. The third is the one an inbox usually drops — "your
//! feedback was closed" tells the filer nothing they can check. When
//! `jobs.complete_linked_step` stamped a car onto the branch it
//! completed, that evidence is read back here and named; when triage
//! recorded a `finding` instead, that is named; when there is neither,
//! the message says the terminal and stops rather than inventing a
//! reason.
//!
//! Rule shape:
//! ```toml
//! [[rule]]
//! on_event = "jobs.job.closed"
//! when = "kind = \"user-feedback\""
//! [[rule.do]]
//! handler = "messages.notify_job_terminal"
//! args = { recipient_key = "\"submitted_by\"" }
//! ```

use super::common::{dispatcher_actor_header, dispatcher_reader_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

/// Default Job-metadata key naming who filed the packet. The
/// `user-feedback` chrome control writes `submitted_by`.
const DEFAULT_RECIPIENT_KEY: &str = "submitted_by";
/// Default dedup-id prefix. Distinct from `messages.notify`'s
/// `notify` / `done` prefixes so a packet's terminal message never
/// collapses onto a step notification it already sent.
const DEFAULT_ID_PREFIX: &str = "terminal";
/// Default step-metadata key the arrival evidence lands under — the
/// same default `jobs.complete_linked_step` writes.
const DEFAULT_EVIDENCE_KEY: &str = "arrived_from";

pub struct MessagesNotifyJobTerminal {
    client: reqwest::Client,
    jobs_base: String,
    messages_base: String,
}

impl MessagesNotifyJobTerminal {
    pub fn new(jobs_base: impl Into<String>, messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            messages_base: messages_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at
    /// local stand-ins for jobs-api and boss-messages).
    pub fn with_client(
        client: reqwest::Client,
        jobs_base: impl Into<String>,
        messages_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            messages_base: messages_base.into(),
        })
    }
}

/// First eight characters of an id — how the operator surfaces and the
/// train reports refer to a Job in prose.
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn str_arg<'a>(args: &'a [(String, Value)], name: &str, default: &'a str) -> &'a str {
    match arg(args, name) {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        _ => default,
    }
}

/// What satisfied the terminal, in one clause a reader can act on.
///
/// Evidence first (a car that shipped, named by title and short id),
/// then the triage finding, then nothing. Never a guess: a terminal
/// with no recorded reason gets a message that says so by omission
/// rather than one that invents a cause.
fn satisfied_by(job: &serde_json::Value, evidence_key: &str) -> Option<String> {
    let steps = job.get("steps").and_then(|v| v.as_array());
    if let Some(evidence) = steps.and_then(|steps| {
        steps
            .iter()
            .filter_map(|s| s.get("metadata")?.get(evidence_key))
            .next()
    }) {
        let title = evidence
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let car = evidence.get("car").and_then(|v| v.as_str()).unwrap_or("");
        let mut clause = if title.is_empty() {
            format!("the change {}", short(car))
        } else {
            format!("the change \"{}\" ({})", title, short(car))
        };
        if let Some(generation) = evidence.get("generation").and_then(|v| v.as_str()) {
            clause.push_str(&format!(", live at generation {generation}"));
        }
        return Some(clause);
    }
    steps
        .and_then(|steps| {
            steps
                .iter()
                .filter_map(|s| s.get("metadata")?.get("finding")?.as_str())
                .find(|f| !f.trim().is_empty())
        })
        .map(|finding| format!("what triage found: {finding}"))
}

#[async_trait]
impl Handler for MessagesNotifyJobTerminal {
    fn name(&self) -> &'static str {
        "messages.notify_job_terminal"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let recipient_key = str_arg(args, "recipient_key", DEFAULT_RECIPIENT_KEY);
        let id_prefix = str_arg(args, "id_prefix", DEFAULT_ID_PREFIX);
        let evidence_key = str_arg(args, "evidence_key", DEFAULT_EVIDENCE_KEY);

        // A malformed close marker is not something a redelivery can
        // fix — no-op rather than retry forever.
        let Some(job_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        let job: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} response not JSON: {e}")))?;
        let metadata = job.get("metadata").cloned().unwrap_or(json!({}));

        // The filer, falling back to the Job's owner. A packet with
        // neither has nobody to tell — silence beats a message
        // addressed to no one.
        let Some(recipient) = metadata
            .get(recipient_key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                job.get("owner_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
        else {
            return Ok(());
        };

        // The terminal state, as the packet itself recorded it. The
        // close marker carries the declared outcome; a catch-all close
        // has none, and then the Job's status IS the terminal.
        let terminal = ctx
            .event_payload
            .get("outcome")
            .and_then(|v| v.as_str())
            .or_else(|| metadata.get("outcome").and_then(|v| v.as_str()))
            .or_else(|| job.get("status").and_then(|v| v.as_str()))
            .unwrap_or("closed")
            .to_string();

        let title = job.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let about = job
            .get("subject")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let subject_line = if title.is_empty() {
            format!("Closed as {terminal} — packet {}", short(job_id))
        } else {
            format!("Closed as {terminal}: {title}")
        };
        let mut body = format!(
            "Your feedback packet {} ({}){} reached the terminal \"{terminal}\".",
            if title.is_empty() { "—" } else { title },
            short(job_id),
            if about.is_empty() {
                String::new()
            } else {
                format!(", about {about}")
            },
        );
        match satisfied_by(&job, evidence_key) {
            Some(cause) => body.push_str(&format!(" Satisfied by {cause}.")),
            None => body.push_str(" No further work is recorded against it."),
        }
        body.push_str(" Opening this message goes straight to the packet.");

        let msg = json!({
            // Deterministic per (packet, recipient): a redelivered
            // `jobs.job.closed` re-runs this handler and the messages
            // `ON CONFLICT (id) DO NOTHING` insert collapses the
            // second write instead of stacking a duplicate inbox row.
            "id": format!("{id_prefix}:{job_id}:{recipient}"),
            "sender_id": "automation:dispatcher",
            "recipient_id": recipient,
            "subject": subject_line,
            "body": body,
            "kind": "signal",
            // Link the PACKET, not a step: the message is about the
            // whole item reaching a terminal, and its steps are all
            // terminal too by the time this fires.
            "entity_ref": {
                "entity_type": "job",
                "entity_id": job_id,
                "entity_path": format!("/jobs/{job_id}"),
            },
        });
        let msg_url = format!(
            "{}/api/messages/send",
            self.messages_base.trim_end_matches('/')
        );
        let mresp = self
            .client
            .post(&msg_url)
            .header("content-type", "application/json")
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
    use axum::{Json, Router, routing::get, routing::post};
    use std::sync::Mutex;

    const PACKET: &str = "22222222-2222-2222-2222-222222222222";
    const CAR: &str = "11111111-1111-1111-1111-111111111111";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "notify-filer-on-feedback-terminal".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    fn marker(outcome: serde_json::Value) -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "outcome": outcome,
            "closed_on": "2026-08-13",
            "parent_step_id": null,
        })
    }

    /// A closed packet whose `build` branch was completed by a merged
    /// car — the evidence `jobs.complete_linked_step` stamps.
    fn packet_with_evidence() -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "title": "Feedback on /system/flow",
            "status": "closed",
            "owner_id": "emp-bootstrap-admin",
            "subject": { "subject_kind": "custom", "id": "/system/flow" },
            "metadata": { "submitted_by": "emp-filer", "outcome": "completed" },
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                  "metadata": { "disposition": "build" } },
                { "id": "s-build", "spec_slug": "build", "status": "completed",
                  "metadata": { "arrived_from": {
                      "car": CAR,
                      "title": "Close the feedback loop",
                      "generation": "abc1234"
                  }}},
            ],
        })
    }

    type Sent = Arc<Mutex<Vec<serde_json::Value>>>;

    async fn mock_services(job: serde_json::Value) -> (String, String, Sent) {
        let jobs = Router::new().route(
            "/api/jobs/{id}",
            get(move || {
                let job = job.clone();
                async move { Json(job) }
            }),
        );
        let jl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jaddr = jl.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(jl, jobs).await.unwrap() });

        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let cap = sent.clone();
        let messages = Router::new().route(
            "/api/messages/send",
            post(move |Json(body): Json<serde_json::Value>| {
                let cap = cap.clone();
                async move {
                    cap.lock().unwrap().push(body);
                    Json(json!({ "ok": true }))
                }
            }),
        );
        let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let maddr = ml.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(ml, messages).await.unwrap() });

        (format!("http://{jaddr}"), format!("http://{maddr}"), sent)
    }

    /// The obligation: the FILER is told, and the message names the
    /// packet, the terminal, and what satisfied it. All three, or the
    /// message is a notification the reader can do nothing with.
    #[tokio::test]
    async fn the_filer_is_told_which_packet_which_terminal_and_what_satisfied_it() {
        let (jobs, messages, sent) = mock_services(packet_with_evidence()).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!("completed"))))
            .await
            .expect("notifies");

        let msgs = sent.lock().unwrap().clone();
        assert_eq!(msgs.len(), 1, "one message: {msgs:?}");
        let m = &msgs[0];
        assert_eq!(
            m["recipient_id"], "emp-filer",
            "the FILER hears, not the owner"
        );

        let subject = m["subject"].as_str().unwrap_or_default();
        let body = m["body"].as_str().unwrap_or_default();
        assert!(
            subject.contains("Feedback on /system/flow"),
            "must name WHICH packet: {subject}"
        );
        assert!(
            subject.contains("completed"),
            "must name the terminal state: {subject}"
        );
        assert!(
            body.contains("Close the feedback loop") && body.contains(short(CAR)),
            "must name what satisfied it — the car, by title and id: {body}"
        );
        assert!(
            body.contains("abc1234"),
            "the generation it went live in is evidence too: {body}"
        );
        assert_eq!(m["entity_ref"]["entity_path"], format!("/jobs/{PACKET}"));
        assert_eq!(m["entity_ref"]["entity_type"], "job");
    }

    /// Redelivery is at-least-once. The id is stable per (packet,
    /// recipient) so the messages `ON CONFLICT (id) DO NOTHING` insert
    /// collapses the second write rather than stacking a duplicate.
    #[tokio::test]
    async fn a_rerun_posts_the_same_deterministic_id() {
        let (jobs, messages, sent) = mock_services(packet_with_evidence()).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        for _ in 0..2 {
            h.invoke(&[], &ctx(marker(json!("completed"))))
                .await
                .expect("notifies");
        }
        let msgs = sent.lock().unwrap().clone();
        assert_eq!(msgs.len(), 2, "the handler does re-post on redelivery");
        assert_eq!(
            msgs[0]["id"], msgs[1]["id"],
            "…with an identical id, which is what the ON CONFLICT insert collapses"
        );
        assert_eq!(msgs[0]["id"], format!("terminal:{PACKET}:emp-filer"));
    }

    /// EVERY terminal, not only the shipped one. A duplicate is an
    /// answer the filer is owed for the same reason.
    #[tokio::test]
    async fn a_duplicate_terminal_also_notifies() {
        let mut packet = packet_with_evidence();
        packet["metadata"]["outcome"] = json!("duplicate");
        packet["steps"] = json!([
            { "id": "s-triage", "spec_slug": "triage", "status": "completed",
              "metadata": { "disposition": "duplicate", "finding": "same as packet 8ac21f10" } },
        ]);
        let (jobs, messages, sent) = mock_services(packet).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!("duplicate"))))
            .await
            .expect("notifies");

        let m = sent.lock().unwrap()[0].clone();
        assert!(
            m["subject"]
                .as_str()
                .unwrap_or_default()
                .contains("duplicate")
        );
        assert!(
            m["body"]
                .as_str()
                .unwrap_or_default()
                .contains("same as packet 8ac21f10"),
            "with no car to name, the triage finding is what satisfied it: {}",
            m["body"]
        );
    }

    /// No filer recorded → the Job's owner. Somebody opened this
    /// packet and somebody is accountable for it.
    #[tokio::test]
    async fn the_owner_hears_when_no_filer_was_recorded() {
        let mut packet = packet_with_evidence();
        packet["metadata"] = json!({ "outcome": "completed" });
        let (jobs, messages, sent) = mock_services(packet).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!("completed"))))
            .await
            .expect("notifies");
        assert_eq!(
            sent.lock().unwrap()[0]["recipient_id"],
            "emp-bootstrap-admin"
        );
    }

    /// Neither a filer nor an owner is the only silence. A message
    /// addressed to nobody is worse than none.
    #[tokio::test]
    async fn a_packet_with_nobody_to_tell_stays_silent() {
        let mut packet = packet_with_evidence();
        packet["metadata"] = json!({});
        packet["owner_id"] = json!("");
        let (jobs, messages, sent) = mock_services(packet).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!("completed"))))
            .await
            .expect("no-op");
        assert!(sent.lock().unwrap().is_empty());
    }

    /// A terminal with no recorded cause says so by omission. The
    /// message never invents a reason it cannot show the reader.
    #[tokio::test]
    async fn a_terminal_with_no_evidence_never_invents_a_cause() {
        let mut packet = packet_with_evidence();
        packet["steps"] = json!([
            { "id": "s-triage", "spec_slug": "triage", "status": "completed", "metadata": {} },
        ]);
        let (jobs, messages, sent) = mock_services(packet).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!("declined"))))
            .await
            .expect("notifies");
        let body = sent.lock().unwrap()[0]["body"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            body.contains("declined"),
            "the terminal still lands: {body}"
        );
        assert!(
            body.contains("No further work is recorded"),
            "absence is stated, not filled in: {body}"
        );
    }

    /// A catch-all close carries a null `outcome`; the Job's status is
    /// then the terminal. The binder gets a present-but-null key
    /// either way (the close-marker contract), so this is a shape the
    /// handler must read, not one it can assume away.
    #[tokio::test]
    async fn a_null_outcome_falls_back_to_the_recorded_state() {
        let mut packet = packet_with_evidence();
        packet["metadata"] = json!({ "submitted_by": "emp-filer" });
        let (jobs, messages, sent) = mock_services(packet).await;
        let h = MessagesNotifyJobTerminal::with_client(reqwest::Client::new(), jobs, messages);
        h.invoke(&[], &ctx(marker(json!(null))))
            .await
            .expect("notifies");
        assert!(
            sent.lock().unwrap()[0]["subject"]
                .as_str()
                .unwrap_or_default()
                .contains("closed"),
            "with no declared outcome the Job's status is the terminal"
        );
    }

    #[tokio::test]
    async fn a_close_marker_with_no_id_is_a_no_op() {
        let h = MessagesNotifyJobTerminal::new("http://127.0.0.1:1", "http://127.0.0.1:1");
        // Unreachable bases: a no-op is the only outcome that cannot
        // error, which is what proves nothing was fetched or sent.
        let res = h
            .invoke(&[], &ctx(json!({ "closed_on": "2026-08-13" })))
            .await;
        assert!(
            res.is_ok(),
            "a malformed marker retries into nothing: {res:?}"
        );
    }
}
