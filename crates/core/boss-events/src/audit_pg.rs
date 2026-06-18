//! Shared Postgres-backed `AuditWriter` implementation.
//!
//! Every service binary that has a Postgres pool wires this writer
//! into its `DomainPublisher` so emitted events also persist to the
//! `audit_log` table. Before this existed, the writer was duplicated
//! in `boss-catalog`'s binary and missing from the other five — only
//! catalog events showed up in the audit log.

use async_trait::async_trait;
use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use sqlx::PgPool;

/// Postgres-backed audit writer. Inserts one row per emitted event.
pub struct PgAuditWriter {
    pool: PgPool,
}

impl PgAuditWriter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditWriter for PgAuditWriter {
    async fn write(&self, event: &Event) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
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

    /// Bulk insert: one multi-row INSERT for the entire batch. The
    /// per-row default `write` implementation does N round-trips with
    /// N fsync waits; this collapses to one. The 5-binds-per-row
    /// budget keeps us well under the postgres 65535-bind limit even
    /// at large batch sizes (5 × 10000 rows = 50K binds).
    async fn write_batch(&self, events: &[Event]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let mut sql = String::from(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) VALUES ",
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
