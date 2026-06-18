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
        self.events.write().await.push(event.clone());
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
