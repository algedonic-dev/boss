//! Transactional event outbox + relay drain — Option B of
//! [docs/design/transactional-audit-log.md].
//!
//! `record_event_in_tx` is the emitting side: the event INSERT joins
//! the caller's domain transaction, so the durable hand-off is atomic
//! with the state change it describes — and the ref-check trigger
//! (`event_outbox_check_refs_trg`) runs *inside* that transaction,
//! aborting a write whose payload references a missing projection row
//! instead of punching a post-commit provenance hole (the 2026-07-13
//! phantom-account incident class).
//!
//! `drain_outbox_once` is the relay side, one batch: claim pending
//! rows in id order → INSERT into `audit_log` (the chain-hash trigger
//! runs there exactly as for legacy writers; the relay's short batch
//! transaction is the only holder of the chain lock per batch) →
//! commit → publish to the bus → stamp `delivered_at`. Every crash
//! point retries safely:
//!
//! - crash before the audit commit → rows untouched, re-drained.
//! - crash between audit commit and publish → audit row exists, row
//!   still pending; the re-drain skips the audit INSERT (NOT EXISTS
//!   by `event_id` — deliberately not ON CONFLICT, whose pre-conflict
//!   trigger fire would consume a sequence id and manufacture the id
//!   gaps the integrity checker treats as anomalies) and re-publishes.
//! - publish failure mid-batch → the batch STOPS (publish order is
//!   the outbox order; later events must not overtake a failed one),
//!   the failed row stays pending, the next drain resumes from it.
//!
//! Consumers tolerate the resulting at-least-once publishes — that is
//! the standing NAK-redelivery contract.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::port::EventBus;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// What one `drain_outbox_once` batch accomplished.
#[derive(Debug, Default, Clone, Copy)]
pub struct DrainStats {
    /// Rows fully delivered (audit + bus + stamped).
    pub delivered: u64,
    /// Audit rows actually inserted this batch (< claimed when a
    /// prior crashed drain already landed some).
    pub audit_inserted: u64,
}

/// Stage an event inside the caller's transaction. The INSERT fires
/// the outbox ref-check trigger, so a rejection surfaces here as
/// `Err` — the caller must abort (the transaction is already poisoned
/// by the failed statement, so commit would fail anyway).
pub async fn record_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    event: &Event,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO event_outbox (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event.id)
    .bind(event.timestamp)
    .bind(&event.source)
    .bind(&event.kind)
    .bind(&event.payload)
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Pending (undelivered) outbox rows. Used by the relay's idle check,
/// tests, and the epoch-restart quiescence gate.
pub async fn pending_count(pool: &PgPool) -> Result<i64, String> {
    sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE delivered_at IS NULL")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())
}

type OutboxRow = (i64, Uuid, DateTime<Utc>, String, String, serde_json::Value);

/// Drain one batch of pending rows through audit_log and the bus.
/// Returns Ok even when the bus is down — the undelivered rows simply
/// stay pending and the next drain retries them; only storage errors
/// are `Err`.
pub async fn drain_outbox_once(
    pool: &PgPool,
    bus: &Arc<dyn EventBus>,
    batch: i64,
) -> Result<DrainStats, String> {
    // Phase 1 — claim + audit-insert, one short transaction. FOR
    // UPDATE SKIP LOCKED lets a second relay instance (or the
    // epoch-restart TRUNCATE, which queues behind the row locks)
    // coexist without double-processing.
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT id, event_id, timestamp, source, kind, payload \
         FROM event_outbox \
         WHERE delivered_at IS NULL \
         ORDER BY id \
         LIMIT $1 \
         FOR UPDATE SKIP LOCKED",
    )
    .bind(batch)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        // Nothing to do; drop the empty tx.
        return Ok(DrainStats::default());
    }

    let mut audit_inserted = 0u64;
    for (_, event_id, timestamp, source, kind, payload) in &rows {
        let res = sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             SELECT $1, $2, $3, $4, $5 \
             WHERE NOT EXISTS (SELECT 1 FROM audit_log WHERE event_id = $1)",
        )
        .bind(event_id)
        .bind(timestamp)
        .bind(source)
        .bind(kind)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        audit_inserted += res.rows_affected();
    }
    tx.commit().await.map_err(|e| e.to_string())?;

    // Phase 2 — publish + stamp, per row, in order. A publish failure
    // stops the batch so order is preserved across the retry.
    let mut delivered = 0u64;
    for (id, event_id, timestamp, source, kind, payload) in rows {
        let event = Event {
            id: event_id,
            timestamp,
            source,
            kind,
            payload,
        };
        let event_kind = event.kind.clone();
        match bus.publish(event).await {
            Ok(()) => {
                sqlx::query("UPDATE event_outbox SET delivered_at = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                delivered += 1;
            }
            Err(e) => {
                tracing::warn!(
                    outbox_id = id,
                    kind = %event_kind,
                    error = %e,
                    "bus publish failed — row stays pending; batch stopped to preserve order"
                );
                break;
            }
        }
    }

    Ok(DrainStats {
        delivered,
        audit_inserted,
    })
}
