use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::port::{EventStore, EventStoreError};
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory event store adapter. Useful for dev/test.
/// Swap for SQLite, Postgres, or append-only log in production.
pub struct InMemoryEventStore {
    events: Arc<RwLock<Vec<Event>>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn append(&self, event: &Event) -> Result<(), EventStoreError> {
        let mut events = self.events.write().await;
        if let Some(existing) = events.iter().find(|existing| existing.id == event.id) {
            if existing.timestamp == event.timestamp
                && existing.source == event.source
                && existing.kind == event.kind
                && existing.payload == event.payload
            {
                return Ok(());
            }
            return Err(EventStoreError::AppendFailed(format!(
                "event {}: immutable identity conflict; original fact preserved",
                event.id
            )));
        }
        events.push(event.clone());
        Ok(())
    }

    async fn query_by_kind(&self, kind: &str) -> Result<Vec<Event>, EventStoreError> {
        let events = self.events.read().await;
        let results = events.iter().filter(|e| e.kind == kind).cloned().collect();
        Ok(results)
    }

    async fn query_by_source(&self, source: &str) -> Result<Vec<Event>, EventStoreError> {
        let events = self.events.read().await;
        let results = events
            .iter()
            .filter(|e| e.source == source)
            .cloned()
            .collect();
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_identity_is_idempotent_only_for_the_same_fact() {
        let store = InMemoryEventStore::new();
        let original = Event::new(
            "test",
            "events.outbox.pruned",
            serde_json::json!({"_actor":"emp-first"}),
            chrono::Utc::now(),
        );
        store.append(&original).await.unwrap();
        store.append(&original).await.unwrap();
        assert_eq!(store.query_by_source("test").await.unwrap().len(), 1);
        for field in ["source", "kind", "timestamp", "payload"] {
            let mut conflicting = original.clone();
            match field {
                "source" => conflicting.source = "other".into(),
                "kind" => conflicting.kind = "events.outbox.dead_lettered".into(),
                "timestamp" => conflicting.timestamp += chrono::Duration::nanoseconds(1),
                "payload" => conflicting.payload["_actor"] = serde_json::json!("emp-second"),
                _ => unreachable!(),
            }
            assert!(store.append(&conflicting).await.is_err());
        }
        assert_eq!(
            store.query_by_source("test").await.unwrap()[0].payload,
            original.payload
        );
    }

    #[tokio::test]
    async fn concurrent_append_cannot_split_one_identity() {
        let store = Arc::new(InMemoryEventStore::new());
        let first = Event::new(
            "test",
            "events.outbox.pruned",
            serde_json::json!({"_actor":"emp-first"}),
            chrono::Utc::now(),
        );
        let mut second = first.clone();
        second.payload["_actor"] = serde_json::json!("emp-second");
        let (a, b) = tokio::join!(store.append(&first), store.append(&second));
        assert_ne!(a.is_ok(), b.is_ok());
        let stored = store.query_by_source("test").await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].payload,
            if a.is_ok() {
                first.payload
            } else {
                second.payload
            }
        );
        store.append(&stored[0]).await.unwrap();
        assert_eq!(store.query_by_source("test").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn append_and_query() {
        let store = InMemoryEventStore::new();

        let e1 = Event::new(
            "svc-a",
            "order.created",
            serde_json::json!({}),
            chrono::Utc::now(),
        );
        let e2 = Event::new(
            "svc-b",
            "order.created",
            serde_json::json!({}),
            chrono::Utc::now(),
        );
        let e3 = Event::new(
            "svc-a",
            "order.shipped",
            serde_json::json!({}),
            chrono::Utc::now(),
        );

        store.append(&e1).await.unwrap();
        store.append(&e2).await.unwrap();
        store.append(&e3).await.unwrap();

        let by_kind = store.query_by_kind("order.created").await.unwrap();
        assert_eq!(by_kind.len(), 2);

        let by_source = store.query_by_source("svc-a").await.unwrap();
        assert_eq!(by_source.len(), 2);
    }
}
