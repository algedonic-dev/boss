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
    read_groups_on(
        connection,
        std::slice::from_ref(reference),
        cutoff,
        limit,
        1,
    )
    .await?
    .pop()
    .ok_or_else(|| "original group read returned nothing".into())
}

/// How many groups one batched read asks for (design c195e83e Q5,
/// approved 2026-10-17). The membership read used to issue one query
/// per group, which is two round trips for each of tens of thousands of
/// groups in a day's window. A batch boundary changes no verdict: every
/// group is judged by `assemble` from its own header and rows alone.
pub const GROUP_READ_BATCH: usize = 500;

type Header = (Uuid, i32, Option<i64>, Option<String>, Option<String>);

/// Read `references` in batches of `batch` ids, answering in the same
/// order, each verified exactly as a single read verifies it. Any one
/// group that fails refuses the whole read, as the single read did for
/// the caller that looped over it.
pub async fn read_groups_on(
    connection: &mut PgConnection,
    references: &[GroupReference],
    cutoff: i64,
    limit: i64,
    batch: usize,
) -> Result<Vec<OriginalGroup>, String> {
    if cutoff < 0 || !(1..=4096).contains(&limit) || batch == 0 {
        return Err("unsupported or oversized original transaction group".into());
    }
    let mut out = Vec::with_capacity(references.len());
    for chunk in references.chunks(batch) {
        if chunk.iter().any(|reference| {
            reference.schema_version != 1
                || reference.event_count < 1
                || reference.event_count > limit
        }) {
            return Err("unsupported or oversized original transaction group".into());
        }
        let ids: Vec<Uuid> = chunk.iter().map(|r| r.id).collect();
        let headers: Vec<Header> = sqlx::query_as(
            "SELECT id,schema_version,event_count,member_hash,original_group_member_hash(id,$2) \
             FROM original_event_groups WHERE id=ANY($1)",
        )
        .bind(&ids)
        .bind(limit + 1)
        .fetch_all(&mut *connection)
        .await
        .map_err(|e| e.to_string())?;
        // Each reference's count is already bounded by `limit`, so the
        // batch's rows are bounded by their sum; one row past it means
        // some roster is larger than its reference, which `assemble`
        // then refuses by count.
        let expected: i64 = chunk.iter().map(|r| r.event_count).sum();
        let rows: Vec<(Uuid, Uuid, Option<i64>)> = sqlx::query_as(
            "SELECT m.group_id,m.event_id,a.id FROM original_event_group_members m \
             LEFT JOIN audit_log a ON a.event_id=m.event_id AND a.id<=$2 \
             WHERE m.group_id=ANY($1) ORDER BY m.group_id,m.member_seq LIMIT $3",
        )
        .bind(&ids)
        .bind(cutoff)
        .bind(expected + 1)
        .fetch_all(&mut *connection)
        .await
        .map_err(|e| e.to_string())?;
        let mut members = std::collections::HashMap::<Uuid, Vec<(Uuid, Option<i64>)>>::new();
        for (group, event, seq) in rows {
            members.entry(group).or_default().push((event, seq));
        }
        for reference in chunk {
            let header = headers.iter().find(|h| h.0 == reference.id);
            let rows = members.get(&reference.id).cloned().unwrap_or_default();
            out.push(assemble(reference, header, rows)?);
        }
    }
    Ok(out)
}

/// One group's verdict from its own header and roster rows: nothing
/// another group in the same batch holds can change it.
fn assemble(
    reference: &GroupReference,
    header: Option<&Header>,
    rows: Vec<(Uuid, Option<i64>)>,
) -> Result<OriginalGroup, String> {
    let Some((_, version, Some(count), Some(recorded_hash), Some(actual_hash))) = header else {
        return Err("original group header is missing or unsealed".into());
    };
    if *version != reference.schema_version
        || *count != reference.event_count
        || recorded_hash != &reference.member_hash
        || actual_hash != &reference.member_hash
    {
        return Err("original roster does not match its audit-anchored hash/count".into());
    }
    if rows.len() as i64 != *count {
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
