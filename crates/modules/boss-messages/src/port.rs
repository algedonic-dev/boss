//! Port (trait) defining the message repository contract.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use crate::types::Message;

#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the stamp
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. Idempotency guards sit
/// AHEAD of the recording, so a collapsed replay records nothing.
#[async_trait]
pub trait MessageRepository: Send + Sync {
    async fn inbox(&self, recipient_id: &str) -> Result<Vec<Message>, MessageError>;
    async fn unread_count(&self, recipient_id: &str) -> Result<u32, MessageError>;
    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError>;
    /// Mark a message read at the given timestamp. Caller picks the
    /// timestamp so the same value can be carried in the
    /// `messages.message.read` event payload — letting a rebuild
    /// reconstruct the projection's `read_at` exactly.
    /// Records `messages.message.read` (`{id, read_at}`) in-tx —
    /// only when the row actually updated (a phantom id records
    /// nothing).
    async fn mark_read(
        &self,
        id: &str,
        read_at: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.sent` (full row state) in-tx — only
    /// when the INSERT actually inserted (a redelivered notification
    /// collapses on ON CONFLICT (id) and records nothing).
    async fn send(&self, msg: &Message, stamp: &EventStamp) -> Result<(), MessageError>;
    /// Records `messages.message.deleted` (`{id, deleted_at}`) in-tx
    /// after the row actually deleted.
    async fn delete_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.archived` (`{id, archived_at}`)
    /// in-tx after the row actually updated.
    async fn archive_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Return all messages in a thread (the root message + all replies).
    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError>;
}
