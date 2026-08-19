//! `messages.expire_for_job` — retire the inbox notifications about a
//! job that has finished.
//!
//! David, 2026-08-14: "We still need a way to automatically expire
//! inbox messages for jobs that have moved past relevancy."
//!
//! A ready-step notification is true when it is sent and stops being
//! true the moment the work behind it is done. Nothing retracted them,
//! so the admin inbox reached 2,058 unread — overwhelmingly signals
//! about steps closed days ago. The count was not a backlog; it was
//! sediment, and an inbox nobody can read is a channel that does not
//! exist.
//!
//! Fires on `jobs.job.closed`, which is the honest trigger: a closed
//! job is exactly the moment its notifications stop being about
//! anything. One call per close, expiring across every recipient at
//! once rather than per person.
//!
//! What it does NOT touch is the substance of it, and the messages
//! port carries the reasoning: only unread `signal` rows, never a
//! `direct`. A person asking another person something does not stop
//! asking because a job closed, and clearing directs would empty the
//! one category the inbox's needs-you filter is built on.

use super::common::post_json;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

pub struct MessagesExpireForJob {
    client: reqwest::Client,
    messages_base: String,
}

impl MessagesExpireForJob {
    pub fn new(messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            messages_base: messages_base.into(),
        })
    }

    pub fn with_client(client: reqwest::Client, messages_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            messages_base: messages_base.into(),
        })
    }
}

#[async_trait]
impl Handler for MessagesExpireForJob {
    fn name(&self) -> &'static str {
        "messages.expire_for_job"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // The JOB_CLOSED payload carries the job's own id under `id`.
        let job_id = ctx
            .event_payload
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                // Permanent, not Downstream: a payload with no id fails
                // identically on every redelivery, so the runner should
                // terminate it rather than spend the budget.
                HandlerError::Permanent("jobs.job.closed carried no `id`".to_string())
            })?;

        // `/jobs/{id}` catches the job-level notifications and the
        // `/jobs/{id}/steps/{step}` ones beneath it in one prefix. No
        // trailing slash, so the job-level path matches too; reaching a
        // longer sibling id would need one job id to be a prefix of
        // another, which uuids rule out.
        let prefix = format!("/jobs/{job_id}");
        let url = format!(
            "{}/api/messages/expire",
            self.messages_base.trim_end_matches('/')
        );

        // `post_json` rather than a hand-rolled request: it stamps both
        // the rule-as-actor header and `x-sim-origin`. I wrote this by
        // hand first and the dispatcher-actor-stamp lint caught the
        // missing sim origin — without it, simulated traffic could
        // expire real messages.
        post_json(
            &self.client,
            &url,
            &serde_json::json!({ "entity_path_prefix": prefix }),
            &ctx.rule_name,
        )
        .await
    }
}
