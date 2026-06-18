//! In-memory adapter for `MessageRepository`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::port::{MessageError, MessageRepository};
use crate::types::{Message, MessageKind};

pub struct InMemoryMessages {
    messages: RwLock<Vec<Message>>,
}

impl InMemoryMessages {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages: RwLock::new(messages),
        }
    }
}

#[async_trait]
impl MessageRepository for InMemoryMessages {
    async fn inbox(&self, recipient_id: &str) -> Result<Vec<Message>, MessageError> {
        let guard = self.messages.read().await;
        let mut msgs: Vec<Message> = guard
            .iter()
            .filter(|m| m.recipient_id == recipient_id)
            .cloned()
            .collect();
        msgs.sort_by_key(|m| std::cmp::Reverse(m.sent_at));
        Ok(msgs)
    }

    async fn unread_count(&self, recipient_id: &str) -> Result<u32, MessageError> {
        let guard = self.messages.read().await;
        let count = guard
            .iter()
            .filter(|m| m.recipient_id == recipient_id && m.read_at.is_none())
            .count();
        Ok(count as u32)
    }

    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError> {
        let guard = self.messages.read().await;
        Ok(guard.iter().find(|m| m.id == id).cloned())
    }

    async fn mark_read(&self, id: &str, read_at: DateTime<Utc>) -> Result<(), MessageError> {
        let mut guard = self.messages.write().await;
        if let Some(msg) = guard.iter_mut().find(|m| m.id == id) {
            msg.read_at = Some(read_at);
        }
        Ok(())
    }

    async fn send(&self, msg: &Message) -> Result<(), MessageError> {
        self.messages.write().await.push(msg.clone());
        Ok(())
    }

    async fn delete_message(&self, id: &str) -> Result<(), MessageError> {
        let mut guard = self.messages.write().await;
        let len_before = guard.len();
        guard.retain(|m| m.id != id);
        if guard.len() == len_before {
            return Err(MessageError::NotFound(format!("no message with ID {id}")));
        }
        Ok(())
    }

    async fn archive_message(&self, id: &str) -> Result<(), MessageError> {
        let mut guard = self.messages.write().await;
        if let Some(msg) = guard.iter_mut().find(|m| m.id == id) {
            msg.kind = MessageKind::ARCHIVED.into();
            Ok(())
        } else {
            Err(MessageError::NotFound(format!("no message with ID {id}")))
        }
    }

    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError> {
        let guard = self.messages.read().await;
        let mut thread: Vec<Message> = guard
            .iter()
            .filter(|m| m.id == message_id || m.reply_to.as_deref() == Some(message_id))
            .cloned()
            .collect();
        thread.sort_by_key(|m| m.sent_at);
        Ok(thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_message(id: &str, recipient: &str, hours_ago: i64, read: bool) -> Message {
        let sent_at = Utc::now() - chrono::Duration::hours(hours_ago);
        Message {
            id: id.to_string(),
            sender_id: "sender-001".to_string(),
            recipient_id: recipient.to_string(),
            subject: format!("Subject {id}"),
            body: format!("Body {id}"),
            entity_ref: None,
            kind: MessageKind::DIRECT.into(),
            sent_at,
            read_at: if read { Some(Utc::now()) } else { None },
            reply_to: None,
        }
    }

    fn test_repo() -> InMemoryMessages {
        InMemoryMessages::new(vec![
            test_message("msg-001", "emp-001", 3, false),
            test_message("msg-002", "emp-001", 1, false),
            test_message("msg-003", "emp-001", 5, true),
            test_message("msg-004", "emp-002", 2, false),
        ])
    }

    #[tokio::test]
    async fn inbox_returns_messages_for_recipient() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-001").await.unwrap();
        assert_eq!(inbox.len(), 3);
        assert!(inbox.iter().all(|m| m.recipient_id == "emp-001"));
    }

    #[tokio::test]
    async fn inbox_sorted_by_sent_at_desc() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-001").await.unwrap();
        for pair in inbox.windows(2) {
            assert!(pair[0].sent_at >= pair[1].sent_at);
        }
    }

    #[tokio::test]
    async fn inbox_empty_for_unknown_recipient() {
        let repo = test_repo();
        let inbox = repo.inbox("emp-999").await.unwrap();
        assert!(inbox.is_empty());
    }

    #[tokio::test]
    async fn unread_count_correct() {
        let repo = test_repo();
        let count = repo.unread_count("emp-001").await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn message_by_id_found() {
        let repo = test_repo();
        let msg = repo.message_by_id("msg-001").await.unwrap();
        assert!(msg.is_some());
        assert_eq!(msg.unwrap().id, "msg-001");
    }

    #[tokio::test]
    async fn message_by_id_not_found() {
        let repo = test_repo();
        assert!(repo.message_by_id("msg-999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mark_read_sets_read_at() {
        let repo = test_repo();
        assert!(
            repo.message_by_id("msg-001")
                .await
                .unwrap()
                .unwrap()
                .read_at
                .is_none()
        );
        repo.mark_read("msg-001", Utc::now()).await.unwrap();
        let msg = repo.message_by_id("msg-001").await.unwrap().unwrap();
        assert!(msg.read_at.is_some());
    }

    #[tokio::test]
    async fn mark_read_reduces_unread_count() {
        let repo = test_repo();
        let before = repo.unread_count("emp-001").await.unwrap();
        repo.mark_read("msg-001", Utc::now()).await.unwrap();
        let after = repo.unread_count("emp-001").await.unwrap();
        assert_eq!(after, before - 1);
    }

    #[tokio::test]
    async fn delete_message_removes_it() {
        let repo = test_repo();
        assert!(repo.message_by_id("msg-001").await.unwrap().is_some());
        repo.delete_message("msg-001").await.unwrap();
        assert!(repo.message_by_id("msg-001").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_message_not_found() {
        let repo = test_repo();
        let err = repo.delete_message("msg-999").await.unwrap_err();
        assert!(matches!(err, MessageError::NotFound(_)));
    }

    #[tokio::test]
    async fn archive_message_sets_kind() {
        let repo = test_repo();
        repo.archive_message("msg-001").await.unwrap();
        let msg = repo.message_by_id("msg-001").await.unwrap().unwrap();
        assert_eq!(msg.kind.as_str(), MessageKind::ARCHIVED);
    }

    #[tokio::test]
    async fn archive_message_not_found() {
        let repo = test_repo();
        let err = repo.archive_message("msg-999").await.unwrap_err();
        assert!(matches!(err, MessageError::NotFound(_)));
    }
}
