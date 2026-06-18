//! In-memory [`MessageQueue`] adapter.
//!
//! Per-agent FIFO inbox with at-most-one claim per message. Good for dev,
//! tests, and single-VM demos. Production replaces this with a Postgres
//! adapter — the `MessageQueue` trait is the contract both honor.

use async_trait::async_trait;
use boss_core::agent::{AgentId, ClaimId, ClaimedMessage, Message, MessageId, Outcome};
use boss_core::port::{MessageQueue, QueueError};
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

/// State tracked per claim so we can ack/nack.
#[derive(Debug, Clone)]
struct ClaimState {
    agent: AgentId,
    message: Message,
    attempt: u32,
}

#[derive(Debug, Default)]
struct Inner {
    queues: HashMap<AgentId, VecDeque<(Message, u32)>>,
    claims: HashMap<ClaimId, ClaimState>,
}

/// Per-agent FIFO in-memory inbox with at-most-one claim per message.
#[derive(Clone, Default)]
pub struct InMemoryMessageQueue {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryMessageQueue {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MessageQueue for InMemoryMessageQueue {
    async fn enqueue(&self, message: Message) -> Result<MessageId, QueueError> {
        let mut inner = self.inner.lock().await;
        let id = message.id;
        let agent = message.target.clone();
        inner
            .queues
            .entry(agent)
            .or_default()
            .push_back((message, 0));
        Ok(id)
    }

    async fn claim_next(&self, agent: &AgentId) -> Result<Option<ClaimedMessage>, QueueError> {
        let mut inner = self.inner.lock().await;
        let Some(q) = inner.queues.get_mut(agent) else {
            return Ok(None);
        };
        let Some((message, attempt)) = q.pop_front() else {
            return Ok(None);
        };
        let claim_id = ClaimId::new();
        let attempt = attempt + 1;
        let claimed = ClaimedMessage {
            claim_id,
            message: message.clone(),
            claimed_at: Utc::now(),
            attempt,
        };
        inner.claims.insert(
            claim_id,
            ClaimState {
                agent: agent.clone(),
                message,
                attempt,
            },
        );
        Ok(Some(claimed))
    }

    async fn ack(&self, claim_id: ClaimId, _outcome: Outcome) -> Result<(), QueueError> {
        let mut inner = self.inner.lock().await;
        inner
            .claims
            .remove(&claim_id)
            .ok_or(QueueError::UnknownClaim(claim_id))?;
        Ok(())
    }

    async fn nack(&self, claim_id: ClaimId, _reason: String) -> Result<(), QueueError> {
        let mut inner = self.inner.lock().await;
        let state = inner
            .claims
            .remove(&claim_id)
            .ok_or(QueueError::UnknownClaim(claim_id))?;
        // Re-queue at the front so the retry happens before newer messages.
        inner
            .queues
            .entry(state.agent)
            .or_default()
            .push_front((state.message, state.attempt));
        Ok(())
    }

    async fn depth(&self, agent: &AgentId) -> Result<usize, QueueError> {
        let inner = self.inner.lock().await;
        Ok(inner.queues.get(agent).map(|q| q.len()).unwrap_or(0))
    }

    async fn depths(&self) -> Result<Vec<(AgentId, usize)>, QueueError> {
        let inner = self.inner.lock().await;
        Ok(inner
            .queues
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(a, q)| (a.clone(), q.len()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::agent::Cost;

    fn agent(slug: &str) -> AgentId {
        AgentId::try_new(slug).unwrap()
    }

    fn msg(target: &AgentId, kind: &str) -> Message {
        Message::new(target.clone(), kind, serde_json::json!({}))
    }

    #[tokio::test]
    async fn empty_queue_has_zero_depth_and_no_claim() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        assert_eq!(q.depth(&a).await.unwrap(), 0);
        assert!(q.claim_next(&a).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn enqueue_then_claim_preserves_message() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        let m = msg(&a, "work.plan");
        let expected_id = m.id;

        let returned_id = q.enqueue(m.clone()).await.unwrap();
        assert_eq!(returned_id, expected_id);
        assert_eq!(q.depth(&a).await.unwrap(), 1);

        let claimed = q.claim_next(&a).await.unwrap().unwrap();
        assert_eq!(claimed.message.id, expected_id);
        assert_eq!(claimed.attempt, 1);
        assert_eq!(q.depth(&a).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn claim_is_isolated_to_target_agent() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        let b = agent("doctor");
        q.enqueue(msg(&a, "k")).await.unwrap();
        assert!(q.claim_next(&b).await.unwrap().is_none());
        assert_eq!(q.depth(&a).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn ack_removes_claim_and_cannot_be_repeated() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        q.enqueue(msg(&a, "k")).await.unwrap();
        let claimed = q.claim_next(&a).await.unwrap().unwrap();

        q.ack(claimed.claim_id, Outcome::Cancelled).await.unwrap();
        let err = q
            .ack(claimed.claim_id, Outcome::Cancelled)
            .await
            .unwrap_err();
        assert!(matches!(err, QueueError::UnknownClaim(_)));
    }

    #[tokio::test]
    async fn nack_requeues_at_front_with_incremented_attempt() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        q.enqueue(msg(&a, "first")).await.unwrap();
        q.enqueue(msg(&a, "second")).await.unwrap();

        let first = q.claim_next(&a).await.unwrap().unwrap();
        assert_eq!(first.message.kind, "first");
        assert_eq!(first.attempt, 1);

        q.nack(first.claim_id, "transient".into()).await.unwrap();
        assert_eq!(q.depth(&a).await.unwrap(), 2);

        let again = q.claim_next(&a).await.unwrap().unwrap();
        assert_eq!(again.message.kind, "first");
        assert_eq!(again.attempt, 2);
    }

    #[tokio::test]
    async fn depths_only_reports_non_empty_queues() {
        let q = InMemoryMessageQueue::new();
        let a = agent("planner");
        let b = agent("doctor");
        q.enqueue(msg(&a, "k")).await.unwrap();
        q.enqueue(msg(&a, "k")).await.unwrap();
        q.enqueue(msg(&b, "k")).await.unwrap();
        let claimed = q.claim_next(&b).await.unwrap().unwrap();
        q.ack(
            claimed.claim_id,
            Outcome::Success {
                cost: Cost::ZERO,
                response: serde_json::json!({}),
            },
        )
        .await
        .unwrap();

        let mut depths = q.depths().await.unwrap();
        depths.sort_by(|(x, _), (y, _)| x.as_str().cmp(y.as_str()));
        assert_eq!(depths, vec![(a, 2)]);
    }
}
