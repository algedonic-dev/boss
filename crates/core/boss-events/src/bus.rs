use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::port::{EventBus, EventBusError, EventStream};
use tokio::sync::broadcast;
use tracing::debug;

/// In-memory event bus adapter. Good for single-process, local dev, and testing.
/// Swap this for NATS/Kafka/Redis adapter in production — the domain doesn't change.
pub struct InMemoryEventBus {
    sender: broadcast::Sender<Event>,
}

impl InMemoryEventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }
}

#[async_trait]
impl EventBus for InMemoryEventBus {
    async fn publish(&self, event: Event) -> Result<(), EventBusError> {
        debug!(kind = %event.kind, source = %event.source, "publishing event");
        self.sender
            .send(event)
            .map(|_| ())
            .map_err(|e| EventBusError::PublishFailed(e.to_string()))
    }

    async fn subscribe(&self, pattern: &str) -> Result<Box<dyn EventStream>, EventBusError> {
        let receiver = self.sender.subscribe();
        Ok(Box::new(InMemoryEventStream {
            receiver,
            pattern: pattern.to_string(),
        }))
    }
}

struct InMemoryEventStream {
    receiver: broadcast::Receiver<Event>,
    pattern: String,
}

#[async_trait]
impl EventStream for InMemoryEventStream {
    async fn next(&mut self) -> Option<Event> {
        loop {
            match self.receiver.recv().await {
                Ok(event) if matches_pattern(&self.pattern, &event.kind) => {
                    return Some(event);
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(lagged = n, "event stream lagged, skipping missed events");
                    continue;
                }
            }
        }
    }
}

/// NATS-compatible subject matching:
/// - `*` matches exactly one token
/// - `>` matches one or more remaining tokens (must be the final token)
/// - otherwise tokens must match exactly
///
/// Examples:
/// - `agent.*` matches `agent.health` but not `agent.health.check`
/// - `agent.>` matches `agent.health` and `agent.health.check`
/// - `>` matches any non-empty subject
fn matches_pattern(pattern: &str, subject: &str) -> bool {
    let p: Vec<&str> = pattern.split('.').collect();
    let s: Vec<&str> = subject.split('.').collect();
    for (i, tok) in p.iter().enumerate() {
        if *tok == ">" {
            return i == p.len() - 1 && s.len() > i;
        }
        if i >= s.len() {
            return false;
        }
        if *tok != "*" && *tok != s[i] {
            return false;
        }
    }
    p.len() == s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matching() {
        // single-token wildcard
        assert!(matches_pattern("*", "anything"));
        assert!(!matches_pattern("*", "a.b"));
        assert!(matches_pattern("agent.*", "agent.health"));
        assert!(matches_pattern("agent.*", "agent.restart"));
        assert!(!matches_pattern("agent.*", "agent.health.check"));
        assert!(!matches_pattern("agent.*", "cli.command"));

        // multi-token wildcard
        assert!(matches_pattern(">", "anything"));
        assert!(matches_pattern(">", "a.b.c"));
        assert!(matches_pattern("agent.>", "agent.health"));
        assert!(matches_pattern("agent.>", "agent.health.check"));
        assert!(!matches_pattern("agent.>", "agent"));
        assert!(!matches_pattern("agent.>", "cli.command"));

        // exact
        assert!(matches_pattern("cli.command", "cli.command"));
        assert!(!matches_pattern("cli.command", "cli.other"));
    }

    #[tokio::test]
    async fn publish_and_subscribe() {
        let bus = InMemoryEventBus::new(16);
        let mut stream = bus.subscribe("test.*").await.unwrap();

        let event = Event::new(
            "test-src",
            "test.hello",
            serde_json::json!({"msg": "hi"}),
            chrono::Utc::now(),
        );
        bus.publish(event.clone()).await.unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next())
            .await
            .unwrap();

        assert!(received.is_some());
        assert_eq!(received.unwrap().kind, "test.hello");
    }
}
