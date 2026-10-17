//! Whole original producer groups, verified against a hash/count
//! reference copied from a chain-bound audit row. A packet's own
//! history slice is never a completeness witness for its siblings.
pub use crate::original_group::{GroupReference, OriginalGroup};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub async fn read_group(
    pool: &PgPool,
    reference: &GroupReference,
    cutoff: i64,
    limit: i64,
) -> Result<OriginalGroup, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    let result = read_group_on(&mut tx, reference, cutoff, limit).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(result)
}

/// Also usable inside the caller's existing consistent historical
/// read, so group evidence and packet/clock cuts share one snapshot.
pub async fn read_group_on(
    connection: &mut PgConnection,
    reference: &GroupReference,
    cutoff: i64,
    limit: i64,
) -> Result<OriginalGroup, String> {
    if reference.schema_version != 1
        || cutoff < 0
        || !(1..=4096).contains(&limit)
        || reference.event_count < 1
        || reference.event_count > limit
    {
        return Err("unsupported or oversized original transaction group".into());
    }
    let header: Option<(i32, Option<i64>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT schema_version,event_count,member_hash,original_group_member_hash(id,$2) \
         FROM original_event_groups WHERE id=$1",
    )
    .bind(reference.id)
    .bind(limit + 1)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|e| e.to_string())?;
    let Some((version, Some(count), Some(recorded_hash), Some(actual_hash))) = header else {
        return Err("original group header is missing or unsealed".into());
    };
    if version != reference.schema_version
        || count != reference.event_count
        || recorded_hash != reference.member_hash
        || actual_hash != reference.member_hash
    {
        return Err("original roster does not match its audit-anchored hash/count".into());
    }
    let rows: Vec<(Uuid, Option<i64>)> = sqlx::query_as(
        "SELECT m.event_id,a.id FROM original_event_group_members m \
         LEFT JOIN audit_log a ON a.event_id=m.event_id AND a.id<=$2 \
         WHERE m.group_id=$1 ORDER BY m.member_seq LIMIT $3",
    )
    .bind(reference.id)
    .bind(cutoff)
    .bind(limit + 1)
    .fetch_all(connection)
    .await
    .map_err(|e| e.to_string())?;
    if rows.len() as i64 != count {
        return Err("original roster count is incomplete or exceeds the bound".into());
    }
    let complete = rows.iter().all(|(_, seq)| seq.is_some());
    let (event_ids, audit_sequences) = rows.into_iter().unzip();
    Ok(OriginalGroup {
        reference: reference.clone(),
        event_ids,
        audit_sequences,
        complete,
    })
}
