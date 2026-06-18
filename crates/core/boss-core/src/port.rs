use crate::agent::{
    AgentId, AgentSpec, BudgetDecision, ClaimId, ClaimedMessage, Cost, Message, MessageId, Outcome,
    RunCompletion, RunHandle, RunId, Window,
};
use crate::event::Event;
use async_trait::async_trait;

/// Port: publish and subscribe to events.
///
/// This is the central nervous system of Boss.
/// Adapters can implement this with in-memory channels,
/// NATS, Kafka, Redis Streams — domain doesn't care.
#[async_trait]
pub trait EventBus: Send + Sync {
    /// Publish an event to the bus
    async fn publish(&self, event: Event) -> Result<(), EventBusError>;

    /// Subscribe to events matching a kind pattern (e.g., "agent.*")
    async fn subscribe(&self, pattern: &str) -> Result<Box<dyn EventStream>, EventBusError>;
}

/// A stream of events from a subscription.
#[async_trait]
pub trait EventStream: Send + Sync {
    /// Receive the next event. Returns None if the stream is closed.
    async fn next(&mut self) -> Option<Event>;
}

/// Port: persist and retrieve events.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append an event to the store
    async fn append(&self, event: &Event) -> Result<(), EventStoreError>;

    /// Retrieve events by kind, ordered by timestamp
    async fn query_by_kind(&self, kind: &str) -> Result<Vec<Event>, EventStoreError>;

    /// Retrieve all events from a given source
    async fn query_by_source(&self, source: &str) -> Result<Vec<Event>, EventStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EventBusError {
    #[error("failed to publish event: {0}")]
    PublishFailed(String),
    #[error("failed to subscribe: {0}")]
    SubscribeFailed(String),
    #[error("connection lost: {0}")]
    ConnectionLost(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    #[error("failed to append event: {0}")]
    AppendFailed(String),
    #[error("query failed: {0}")]
    QueryFailed(String),
}

// ---------------------------------------------------------------------------
// Cybernetics ports
// ---------------------------------------------------------------------------

/// Port: durable per-agent inbox.
///
/// Messages are enqueued when they arrive from the bus, claimed by the
/// Cybernetics loop just before dispatch, and ack'd or nack'd when the
/// agent run completes. Implementations must serialize claims so that the
/// same message is not dispatched twice concurrently.
#[async_trait]
pub trait MessageQueue: Send + Sync {
    /// Append a message to the target agent's inbox.
    async fn enqueue(&self, message: Message) -> Result<MessageId, QueueError>;

    /// Claim the next available message for the given agent. Returns `None`
    /// if the queue is empty. A claimed message is invisible to other
    /// claimers until `ack` or `nack` is called, or the claim expires.
    async fn claim_next(&self, agent: &AgentId) -> Result<Option<ClaimedMessage>, QueueError>;

    /// Mark a claimed message as processed and record its outcome.
    async fn ack(&self, claim_id: ClaimId, outcome: Outcome) -> Result<(), QueueError>;

    /// Release a claim back to the queue so the message can be retried.
    async fn nack(&self, claim_id: ClaimId, reason: String) -> Result<(), QueueError>;

    /// Number of unclaimed messages waiting for the given agent.
    async fn depth(&self, agent: &AgentId) -> Result<usize, QueueError>;

    /// Depths for every agent with a non-empty queue.
    async fn depths(&self) -> Result<Vec<(AgentId, usize)>, QueueError>;
}

/// Port: cost accounting and budget enforcement.
#[async_trait]
pub trait CostLedger: Send + Sync {
    /// Record a cost attributed to an agent.
    async fn record(&self, agent: &AgentId, cost: Cost) -> Result<(), LedgerError>;

    /// Total cost for an agent over the given window.
    async fn spent(&self, agent: &AgentId, window: Window) -> Result<Cost, LedgerError>;

    /// Total cost across all agents on this VM over the given window.
    async fn vm_spent(&self, window: Window) -> Result<Cost, LedgerError>;

    /// Decide whether the agent is allowed to run right now, given its spec.
    async fn check_budget(
        &self,
        agent: &AgentId,
        spec: &AgentSpec,
    ) -> Result<BudgetDecision, LedgerError>;
}

/// Port: spawn and track agent runs.
#[async_trait]
pub trait AgentDispatcher: Send + Sync {
    /// Start a run for the given agent with a single claimed message.
    /// Returns immediately with a handle; the run continues in the background.
    async fn dispatch(
        &self,
        spec: &AgentSpec,
        message: ClaimedMessage,
    ) -> Result<RunHandle, DispatchError>;

    /// Snapshot of currently in-flight runs on this VM.
    async fn running(&self) -> Result<Vec<RunHandle>, DispatchError>;

    /// Cancel an in-flight run.
    async fn cancel(&self, run_id: &RunId) -> Result<(), DispatchError>;

    /// Subscribe to a stream of terminal run completions. The Cybernetics
    /// loop reads from this to know when to ack messages and record cost.
    async fn completions(&self) -> Result<Box<dyn RunCompletions>, DispatchError>;
}

/// Stream of run completions from a dispatcher.
#[async_trait]
pub trait RunCompletions: Send + Sync {
    /// Receive the next completion. Returns `None` if the dispatcher shut down.
    async fn next(&mut self) -> Option<RunCompletion>;
}

/// Port: static agent configuration for this VM.
#[async_trait]
pub trait AgentRegistry: Send + Sync {
    async fn list(&self) -> Result<Vec<AgentSpec>, RegistryError>;
    async fn get(&self, agent: &AgentId) -> Result<Option<AgentSpec>, RegistryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("enqueue failed: {0}")]
    EnqueueFailed(String),
    #[error("claim failed: {0}")]
    ClaimFailed(String),
    #[error("unknown claim: {0}")]
    UnknownClaim(ClaimId),
    #[error("storage error: {0}")]
    Storage(String),
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("record failed: {0}")]
    RecordFailed(String),
    #[error("query failed: {0}")]
    QueryFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("agent {0} is at its concurrency limit")]
    CapacityExceeded(AgentId),
    #[error("unknown run: {0}")]
    UnknownRun(RunId),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry lookup failed: {0}")]
    LookupFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports must be object-safe — the whole point is polymorphism via trait
    /// objects. These compile-only assertions lock that in.
    #[test]
    fn ports_are_object_safe() {
        fn takes<T: ?Sized>() {}
        takes::<dyn EventBus>();
        takes::<dyn EventStream>();
        takes::<dyn EventStore>();
        takes::<dyn MessageQueue>();
        takes::<dyn CostLedger>();
        takes::<dyn AgentDispatcher>();
        takes::<dyn RunCompletions>();
        takes::<dyn AgentRegistry>();
    }
}
