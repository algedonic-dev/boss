//! An EventBus implementation that records published events for verification.
//!
//! Usage:
//! ```ignore
//! let bus = Arc::new(RecordingEventBus::new());
//! let publisher = DomainPublisher::new(bus.clone(), "catalog");
//! // ... call code that publishes events ...
//! bus.assert_event_emitted("catalog.model.created");
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};

/// EventBus that records every published event in memory.
/// All recorded events can be inspected after the test runs.
#[derive(Default)]
pub struct RecordingEventBus {
    events: Mutex<Vec<Event>>,
}

impl RecordingEventBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot of all published events, in order.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Number of events published so far.
    pub fn event_count(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Find events matching a kind exactly.
    pub fn events_by_kind(&self, kind: &str) -> Vec<Event> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .cloned()
            .collect()
    }

    /// Assert that at least one event with the given kind was emitted.
    /// Panics with a useful message listing all kinds that WERE emitted.
    pub fn assert_event_emitted(&self, kind: &str) -> Event {
        let events = self.events.lock().unwrap();
        let found = events.iter().find(|e| e.kind == kind);
        match found {
            Some(e) => e.clone(),
            None => {
                let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
                panic!(
                    "\n  expected event kind: {}\n  events actually emitted: {:?}\n  total events: {}\n",
                    kind,
                    kinds,
                    events.len(),
                );
            }
        }
    }

    /// Assert no event of the given kind was emitted.
    pub fn assert_event_not_emitted(&self, kind: &str) {
        let events = self.events.lock().unwrap();
        if let Some(e) = events.iter().find(|e| e.kind == kind) {
            panic!(
                "\n  expected event kind {} NOT to be emitted\n  but it was emitted with payload: {}\n",
                kind, e.payload,
            );
        }
    }

    /// Reset the recorded events (useful between phases of a test).
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

#[async_trait]
impl EventBus for RecordingEventBus {
    async fn publish(&self, event: Event) -> Result<(), EventBusError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    async fn subscribe(&self, _pattern: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        // Recording bus doesn't support subscriptions — tests verify via assertion methods.
        Err(EventBusError::SubscribeFailed(
            "RecordingEventBus does not support subscribe".to_string(),
        ))
    }
}
