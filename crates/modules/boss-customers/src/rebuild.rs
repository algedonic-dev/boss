//! Reproject `customers` from `audit_log` alone — the projection
//! half of the dual contract. Truncate-and-reproject from
//! `customers.customer.created`, deduped per id inside the statement
//! (repeated events per id must not abort the rebuild — the #128
//! lesson); the newest event (highest audit id) wins.
//!
//! `ORDER BY … audit_log.id` is deliberately QUALIFIED: the inner
//! SELECT aliases the payload id `AS id`, and Postgres binds an
//! unqualified `ORDER BY id` to that output alias — constant per
//! DISTINCT ON group, silently randomizing which event wins (caught
//! red in boss-campaigns; same shape here).

use sqlx::PgPool;

pub async fn rebuild_customers(pool: &PgPool) -> Result<u64, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("TRUNCATE customers")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    let res = sqlx::query(
        "INSERT INTO customers (id, name, email, phone, metadata, created_at) \
         SELECT ev.id, ev.name, ev.email, ev.phone, \
                COALESCE(ev.metadata, '{}'::jsonb), ev.ts \
           FROM ( \
             SELECT DISTINCT ON (payload->>'id') \
                    payload->>'id'      AS id, \
                    payload->>'name'    AS name, \
                    payload->>'email'   AS email, \
                    payload->>'phone'   AS phone, \
                    payload->'metadata' AS metadata, \
                    timestamp           AS ts \
               FROM audit_log \
              WHERE kind = 'customers.customer.created' \
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
