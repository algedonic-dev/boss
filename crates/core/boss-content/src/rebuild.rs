//! Rebuild the `bulletins` + `bulletin_dismissals` projections from
//! `audit_log`. Tenth (and final per-service) projection rebuilder.
//!
//! State events consumed:
//! - `content.bulletin.created` / `.updated` — full Bulletin row
//! - `content.bulletin.deleted` — `{id, deleted_at}`
//! - `content.bulletin.dismissed` — `{bulletin_id, employee_id, dismissed_at}`
//!
//! Manual sections (manual_sections + manual_section_history) are
//! out of scope for this commit — separate rebuilder.

use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::types::Bulletin;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("content");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub bulletins_upserted: u64,
    pub bulletins_deleted: u64,
    pub dismissals_inserted: u64,
}

#[derive(Debug, Deserialize)]
struct DismissPayload {
    bulletin_id: Uuid,
    employee_id: String,
    dismissed_at: DateTime<Utc>,
}

pub async fn rebuild_content(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &[
            "DELETE FROM bulletin_dismissals",
            "DELETE FROM bulletins",
        ],
        "kind LIKE 'content.bulletin.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "content.bulletin.created" | "content.bulletin.updated" => {
                    let b: Bulletin = match serde_json::from_value(ev.payload.clone()) {
                        Ok(b) => b,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad Bulletin payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_bulletin(&mut *conn, &b, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.bulletins_upserted += 1;
                    Ok(Applied::Yes)
                }
                "content.bulletin.deleted" => {
                    let id_str = ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    let id = id_str.and_then(|s| Uuid::parse_str(&s).ok());
                    if let Some(id) = id {
                        let n = sqlx::query("DELETE FROM bulletins WHERE id = $1")
                            .bind(id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.bulletins_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "content.bulletin.dismissed" => {
                    let p: DismissPayload = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(event_id = ev.audit_id, kind = %ev.kind, error = %e, "skipping bad dismissal payload");
                            return Ok(Applied::Skipped);
                        }
                    };
                    let res = sqlx::query(
                        "INSERT INTO bulletin_dismissals (bulletin_id, employee_id, dismissed_at) \
                         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                    )
                    .bind(p.bulletin_id)
                    .bind(&p.employee_id)
                    .bind(p.dismissed_at)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?;
                    if res.rows_affected() > 0 {
                        report.dismissals_inserted += 1;
                        Ok(Applied::Yes)
                    } else {
                        // ON CONFLICT DO NOTHING — already inserted, or
                        // bulletin was deleted before this dismissal
                        // could land. Either way the projection is in
                        // the right state.
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown content.bulletin.* event kind");
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

async fn upsert_bulletin(
    tx: &mut sqlx::PgConnection,
    b: &Bulletin,
    ts: DateTime<Utc>,
) -> Result<(), RebuildError> {
    let _ = ts; // explicit timestamps live on the Bulletin itself.
    sqlx::query(
        "INSERT INTO bulletins (id, title, body, actor_id, posted_on, expires_on, priority, \
                                 audience, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (id) DO UPDATE SET \
            title = EXCLUDED.title, \
            body = EXCLUDED.body, \
            actor_id = EXCLUDED.actor_id, \
            posted_on = EXCLUDED.posted_on, \
            expires_on = EXCLUDED.expires_on, \
            priority = EXCLUDED.priority, \
            audience = EXCLUDED.audience, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(b.id)
    .bind(&b.title)
    .bind(&b.body)
    .bind(&b.actor_id)
    .bind(b.posted_on)
    .bind(b.expires_on)
    .bind(b.priority.as_str())
    .bind(&b.audience.0)
    .bind(b.created_at)
    .bind(b.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    Ok(())
}
