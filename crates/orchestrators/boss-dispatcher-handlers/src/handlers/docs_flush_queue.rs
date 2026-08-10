//! `docs.flush_queue` — a recorded design decision queues its doc's
//! flush job (cea82de0, link 1 of the decision-flush loop).
//!
//! Consumes S1's `docs.design.decision_recorded` and POSTs the docs
//! API's flush-jobs endpoint for the doc. Decisions arrive one event
//! per answered question; the endpoint snapshots ALL pending
//! decisions per doc, and a doc with none left answers 400 — which
//! this handler treats as the no-op it is, so an answer burst queues
//! usefully and re-fires settle clean.
//!
//! The WORKER stays scheduled-by-hand until its tree/remote question
//! is decided (it commits on the operator's checkout and pushes
//! origin blind — the item names it; docs-as-data owns the answer).

use super::common::dispatcher_actor_header;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use std::sync::Arc;

pub struct DocsFlushQueue {
    client: reqwest::Client,
    docs_base: String,
}

impl DocsFlushQueue {
    pub fn new(docs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            docs_base: docs_base.into(),
        })
    }
    pub fn with_client(client: reqwest::Client, docs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            docs_base: docs_base.into(),
        })
    }
}

#[async_trait]
impl Handler for DocsFlushQueue {
    fn name(&self) -> &'static str {
        "docs.flush_queue"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let Some(doc_path) = ctx.event_payload.get("doc_path").and_then(|v| v.as_str()) else {
            // Malformed payload: nothing to retry into existence.
            return Ok(());
        };
        let url = format!(
            "{}/api/design/flush-jobs",
            self.docs_base.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&url)
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .json(&serde_json::json!({ "doc_path": doc_path }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            // No pending decisions: an earlier event in the burst (or
            // a completed flush) already covered this doc.
            reqwest::StatusCode::BAD_REQUEST => Ok(()),
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(HandlerError::Downstream(format!(
                    "POST {url} returned {s}: {body}"
                )))
            }
        }
    }
}
