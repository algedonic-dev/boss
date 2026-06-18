//! Audit log trait — allows the DomainPublisher to persist events.
//!
//! Services that have a Postgres pool provide an AuditWriter implementation.
//! The publisher calls it fire-and-forget after each emit.

use crate::event::Event;
use async_trait::async_trait;

/// Writes audit log entries. Implementations persist to Postgres.
#[async_trait]
pub trait AuditWriter: Send + Sync {
    /// Persist a single event. Errors are non-fatal — the publisher
    /// logs and continues on failure rather than failing the underlying
    /// domain write.
    async fn write(&self, event: &Event) -> Result<(), String>;

    /// Persist a batch of events. Default implementation loops `write`,
    /// which keeps every existing implementation correct. Implementations
    /// backed by a real database SHOULD override this with one bulk
    /// statement to collapse N round-trips and N fsyncs into one.
    ///
    /// Used by `DomainPublisher::publish_batch` on the bulk write path.
    async fn write_batch(&self, events: &[Event]) -> Result<(), String> {
        for e in events {
            self.write(e).await?;
        }
        Ok(())
    }
}
