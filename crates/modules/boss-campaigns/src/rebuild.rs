//! Reproject `campaigns` from `audit_log` alone — the projection
//! half of the dual contract. Truncate-and-reproject from
//! `campaigns.campaign.created`, deduped per id inside the statement
//! (upsert-style kinds repeat per id, and one INSERT … ON CONFLICT
//! DO UPDATE may not touch the same row twice — the #128 lesson);
//! the newest event (highest audit id) wins.
//!
//! `ORDER BY … audit_log.id` is deliberately QUALIFIED: the inner
//! SELECT aliases the payload id `AS id`, and Postgres binds an
//! unqualified `ORDER BY id` to that output alias — constant per
//! DISTINCT ON group, silently randomizing which event wins.

use sqlx::PgPool;

pub async fn rebuild_campaigns(pool: &PgPool) -> Result<u64, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("TRUNCATE campaigns")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    let res = sqlx::query(
        "INSERT INTO campaigns (id, name, status, starts_on, ends_on, metadata, created_at) \
         SELECT ev.id, ev.name, COALESCE(ev.status, 'active'), \
                ev.starts_on::date, ev.ends_on::date, \
                COALESCE(ev.metadata, '{}'::jsonb), ev.ts \
           FROM ( \
             SELECT DISTINCT ON (payload->>'id') \
                    payload->>'id'        AS id, \
                    payload->>'name'      AS name, \
                    payload->>'status'    AS status, \
                    payload->>'starts_on' AS starts_on, \
                    payload->>'ends_on'   AS ends_on, \
                    payload->'metadata'   AS metadata, \
                    timestamp             AS ts \
               FROM audit_log \
              WHERE kind = 'campaigns.campaign.created' \
                AND payload->>'id' IS NOT NULL \
              ORDER BY payload->>'id', audit_log.id DESC \
           ) ev",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(res.rows_affected())
}
