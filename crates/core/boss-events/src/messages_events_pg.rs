//! Postgres-backed event writer for `boss-messages` — a sibling of
//! [`PgAuditWriter`] that writes to a **separate, purge-able** table
//! (`messages_events`) instead of the compliance-grade `audit_log`.
//!
//! Why split: messages (DMs + system signals + notifications) carry
//! PII and short-lived business context. Operators want a retention
//! policy ("purge messages older than 90 days") that's incompatible
//! with `audit_log`'s append-only-forever invariant. Splitting the
//! tables lets each have its own rules:
//!
//! | trait                | audit_log               | messages_events           |
//! |----------------------|-------------------------|---------------------------|
//! | append-only enforced | yes (mutation trigger)  | no — DELETE/UPDATE allowed |
//! | hash-chained         | yes                     | no — chain breaks on purge |
//! | REVOKE on PUBLIC     | yes                     | no — purge job needs DELETE |
//! | retention            | forever                 | tenant-defined (TTL)      |
//!
//! Everything else — wire shape, NATS publish, dev UX — is identical
//! to the audit-log path. boss-messages-api swaps in this writer at
//! startup; the rest of the codebase doesn't know or care.

use async_trait::async_trait;
use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use sqlx::PgPool;

pub struct PgMessagesEventWriter {
    pool: PgPool,
}

impl PgMessagesEventWriter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditWriter for PgMessagesEventWriter {
    async fn write(&self, event: &Event) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO messages_events (event_id, timestamp, source, kind, payload) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(event.id)
        .bind(event.timestamp)
        .bind(&event.source)
        .bind(&event.kind)
        .bind(&event.payload)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn write_batch(&self, events: &[Event]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let mut sql = String::from(
            "INSERT INTO messages_events (event_id, timestamp, source, kind, payload) VALUES ",
        );
        let mut bind_idx = 1usize;
        for i in 0..events.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${})",
                bind_idx,
                bind_idx + 1,
                bind_idx + 2,
                bind_idx + 3,
                bind_idx + 4,
            ));
            bind_idx += 5;
        }

        let mut q = sqlx::query(&sql);
        for event in events {
            q = q
                .bind(event.id)
                .bind(event.timestamp)
                .bind(&event.source)
                .bind(&event.kind)
                .bind(&event.payload);
        }
        q.execute(&self.pool).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
