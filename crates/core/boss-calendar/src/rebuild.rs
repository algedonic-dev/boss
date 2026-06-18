//! Rebuild the `calendar_reservations` projection from `audit_log`.
//!
//! Eighth projection rebuilder in the event-canonical arc. See
//! `docs/design/projection-rebuilders.md`.
//!
//! State events consumed:
//! - `calendar.reservation.reserved` — full post-INSERT
//!   `Reservation` payload.
//! - `calendar.reservation.cancelled` — full post-cancel
//!   `Reservation` payload (one event per row a cancel cascade
//!   affected). Replay sets `cancelled_at` on the matching id.

use boss_core::calendar::Reservation;
use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("calendar");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub reservations_inserted: u64,
    pub reservations_cancelled: u64,
}

pub async fn rebuild_calendar(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM calendar_reservations"],
        "kind LIKE 'calendar.reservation.%'",
        async |conn, ev| {
            let reservation: Reservation = match serde_json::from_value(ev.payload.clone()) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        event_id = ev.audit_id,
                        kind = %ev.kind,
                        error = %e,
                        "skipping calendar event with non-Reservation payload"
                    );
                    return Ok(Applied::Skipped);
                }
            };
            match ev.kind.as_str() {
                "calendar.reservation.reserved" => {
                    insert_reservation(&mut *conn, &reservation)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.reservations_inserted += 1;
                    Ok(Applied::Yes)
                }
                "calendar.reservation.cancelled" => {
                    // Deterministic fallback: stamp from the event's own
                    // timestamp, never wall-clock. A `cancelled` event
                    // normally carries `cancelled_at`; when it doesn't,
                    // the audit row's `ts` is the authoritative cancel
                    // time and keeps the rebuild replay-stable.
                    let cancelled_at = reservation.cancelled_at.unwrap_or(ev.ts);
                    let n = sqlx::query(
                        "UPDATE calendar_reservations SET cancelled_at = $1 \
                         WHERE id = $2 AND cancelled_at IS NULL",
                    )
                    .bind(cancelled_at)
                    .bind(*reservation.id.inner().as_uuid())
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .rows_affected();
                    if n > 0 {
                        report.reservations_cancelled += 1;
                        Ok(Applied::Yes)
                    } else {
                        // Cancel for a reservation we never INSERTed (or
                        // already cancelled) — projection's already in
                        // the right state.
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown calendar.* event kind");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

async fn insert_reservation(
    tx: &mut sqlx::PgConnection,
    r: &Reservation,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO calendar_reservations \
         (id, resource_kind, resource_id, start_ts, end_ts, \
          reason_kind, reason_ref_id, strength, notes, created_by, \
          created_at, cancelled_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(*r.id.inner().as_uuid())
    .bind(&r.subject.kind)
    .bind(&r.subject.id)
    .bind(r.window.start)
    .bind(r.window.end)
    .bind(&r.reason_kind)
    .bind(&r.reason_ref_id)
    .bind(r.strength.db_value())
    .bind(r.notes.as_deref())
    .bind(&r.created_by)
    .bind(r.created_at)
    .bind(r.cancelled_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}
