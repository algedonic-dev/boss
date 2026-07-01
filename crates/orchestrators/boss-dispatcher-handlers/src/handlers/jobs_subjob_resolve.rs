//! `jobs.subjob_resolve` — the D7 delegate-subjob write-back.
//!
//! Fires when a *child* Job (one spawned by a `delegate-subjob` step)
//! closes. The dispatcher rule listens on `jobs.job.closed` and gates
//! on the closing Job carrying a `parent_step_id` in its metadata
//! (set by `jobs.spawn` when the Job was delegated). This handler then
//! resolves the parent step:
//!
//!   1. `GET /api/jobs/{child}` → read the child's
//!      `metadata.parent_step_id`, `metadata.parent_job_id`, and the
//!      terminal `metadata.outcome`.
//!   2. `GET /api/jobs/{parent_job}` → locate the parent step row and
//!      grab its current metadata (so the write-back merges rather than
//!      wipes — PATCH-on-PUT replaces top-level `metadata` wholesale).
//!   3. `PUT /api/jobs/{parent_job}/steps/{parent_step}` with
//!      `status = "completed"` and the child outcome merged into
//!      `metadata.subjob_outcome`.
//!
//! Completing the parent step drives the parent Job's own re-eval (the
//! delegate-subjob step's downstream predicates can now flip), closing
//! the D6/D7 loop: spawn-on-ready → run child → resolve-on-close.
//!
//! No args are required — everything the handler needs rides on the
//! child Job it can fetch from the close event's `id`. The optional
//! `outcome_metadata_key` arg overrides which child-metadata key holds
//! the terminal outcome (defaults to `outcome`, what
//! `close_job_on_terminal` stamps).

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

use super::common::dispatcher_actor_header;

pub struct JobsSubjobResolve {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsSubjobResolve {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// wiremock server).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    async fn get_job(&self, job_id: &str) -> Result<serde_json::Value, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
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
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} response not JSON: {e}")))
    }
}

#[async_trait]
impl Handler for JobsSubjobResolve {
    fn name(&self) -> &'static str {
        "jobs.subjob_resolve"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // The `jobs.job.closed` payload carries the closing Job's id.
        let child_id = ctx
            .event_payload
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("job.closed payload missing `id`".to_string()))?
            .to_string();

        // Fetch the child Job. `GET /api/jobs/{id}` flattens the Job
        // fields and adds a `steps` array; the fields we need live at
        // the top level under `metadata`.
        let child = self.get_job(&child_id).await?;
        let child_meta = child.get("metadata").cloned().unwrap_or(json!({}));

        // Not a delegated child → nothing to resolve. The rule's `when`
        // already gates on this; the handler re-checks so a mis-wired
        // rule degrades to a no-op rather than a spurious PUT.
        let Some(parent_step_id) = child_meta.get("parent_step_id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let parent_job_id = child_meta
            .get("parent_job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "child Job {child_id} has parent_step_id but no parent_job_id"
                ))
            })?;

        // The child's terminal outcome — what we write back. Prefer the
        // child Job metadata (stamped by close_job_on_terminal); fall
        // back to the close event payload's top-level `outcome`.
        let outcome_key = match arg(args, "outcome_metadata_key") {
            Some(Value::String(s)) => s.as_str(),
            _ => "outcome",
        };
        let outcome = child_meta
            .get(outcome_key)
            .and_then(|v| v.as_str())
            .or_else(|| ctx.event_payload.get("outcome").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        // Fetch the parent Job to read the parent step's current
        // metadata — PATCH-on-PUT replaces top-level `metadata`
        // wholesale, so we merge `subjob_outcome` into the existing keys
        // rather than clobber them.
        let parent = self.get_job(parent_job_id).await?;
        let parent_step_meta = parent
            .get("steps")
            .and_then(|v| v.as_array())
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(parent_step_id))
            })
            .and_then(|s| s.get("metadata").cloned())
            .unwrap_or(json!({}));

        let mut merged_meta = match parent_step_meta {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        merged_meta.insert("subjob_outcome".to_string(), json!(outcome));

        let step_url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.jobs_base.trim_end_matches('/'),
            parent_job_id,
            parent_step_id,
        );
        let put_body = json!({
            "status": "completed",
            "metadata": serde_json::Value::Object(merged_meta),
        });
        let resp = self
            .client
            .put(&step_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .json(&put_body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {step_url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {step_url} returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "resolve-subjob-on-close".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    #[tokio::test]
    async fn noop_when_close_payload_missing_id() {
        let h = JobsSubjobResolve::new("http://127.0.0.1:1");
        let res = h
            .invoke(&[], &ctx(json!({ "closed_on": "2026-06-01" })))
            .await;
        assert!(matches!(res, Err(HandlerError::Downstream(_))));
    }
}
