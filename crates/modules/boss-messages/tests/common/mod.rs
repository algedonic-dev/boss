#![allow(dead_code)] // tests/common/ helpers used selectively across test files

//! Shared test scaffolding for the messages crate.
//!
//! Provides:
//! - `MessageTestApp` builder that wires InMemoryMessages + HTTP router
//!   + RecordingEventBus + DomainPublisher
//! - `message_fixture()` helper that builds a valid Message

use std::sync::Arc;

use axum::Router;
use boss_core::publisher::DomainPublisher;
#[cfg(feature = "postgres")]
use boss_events::PgAuditWriter;
use boss_messages::http::{MessageApiState, router};
use boss_messages::in_memory::InMemoryMessages;
use boss_messages::types::{Message, MessageKind};
use boss_testing::RecordingEventBus;
use chrono::Utc;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// Fully wired messages service for tests.
pub struct MessageTestApp {
    pub router: Router,
    pub bus: Arc<RecordingEventBus>,
}

impl MessageTestApp {
    /// Build a fresh test app with no messages.
    pub fn new() -> Self {
        Self::with_messages(vec![])
    }

    /// Build a test app pre-populated with the given messages.
    pub fn with_messages(messages: Vec<Message>) -> Self {
        let repo = Arc::new(InMemoryMessages::new(messages));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "messages");
        let state = MessageApiState {
            messages: repo,
            publisher: Some(publisher),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        };
        let router = router(state);
        Self { router, bus }
    }

    /// Build a test app whose publisher persists every emitted event
    /// to the given Postgres pool's `audit_log` table. Used by the
    /// audit_log E2E integration test.
    #[cfg(feature = "postgres")]
    pub fn with_audit_pool(pool: PgPool) -> Self {
        let repo = Arc::new(InMemoryMessages::new(vec![]));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "messages")
            .with_audit(Arc::new(PgAuditWriter::new(pool)));
        let state = MessageApiState {
            messages: repo,
            publisher: Some(publisher),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        };
        let router = router(state);
        Self { router, bus }
    }
}

/// Build a valid Message with sensible defaults.
pub fn message_fixture(id: &str) -> Message {
    Message {
        id: id.to_string(),
        sender_id: "emp-sender".to_string(),
        recipient_id: "emp-recipient".to_string(),
        subject: format!("Subject {id}"),
        body: format!("Body for {id}"),
        entity_ref: None,
        kind: MessageKind::DIRECT.into(),
        sent_at: Utc::now(),
        read_at: None,
        reply_to: None,
    }
}
