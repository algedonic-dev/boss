//! Port (trait) defining the message repository contract.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::types::Message;

#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

#[async_trait]
pub trait MessageRepository: Send + Sync {
    async fn inbox(&self, recipient_id: &str) -> Result<Vec<Message>, MessageError>;
    async fn unread_count(&self, recipient_id: &str) -> Result<u32, MessageError>;
    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError>;
    /// Mark a message read at the given timestamp. Caller picks the
    /// timestamp so the same value can be carried in the
    /// `messages.message.read` event payload — letting a rebuild
    /// reconstruct the projection's `read_at` exactly.
    async fn mark_read(&self, id: &str, read_at: DateTime<Utc>) -> Result<(), MessageError>;
    async fn send(&self, msg: &Message) -> Result<(), MessageError>;
    async fn delete_message(&self, id: &str) -> Result<(), MessageError>;
    async fn archive_message(&self, id: &str) -> Result<(), MessageError>;
    /// Return all messages in a thread (the root message + all replies).
    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError>;
}
