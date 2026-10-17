//! Bounded immutable history at a caller-owned repeatable-read cut.
//! The owning domain supplies its relevant event kinds and packet
//! identities; this adapter retains complete original group siblings.
use crate::tail_http::HistoricalEntry;
use serde_json::Value;
use sqlx::PgConnection;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OriginalEntry {
    #[sqlx(flatten)]
    pub row: HistoricalEntry,
    pub original_group: Option<Value>,
}

const HISTORY_SQL: &str = "WITH claims AS MATERIALIZED (SELECT event_id FROM audit_log WHERE id<=$1 AND kind='jobs.station.claimed' AND ($2::text[] IS NULL OR original_claim_packet_identity(payload,event_id,$1)=ANY($2))), relevant AS MATERIALIZED (SELECT id,original_group->>'id' AS group_id FROM audit_log WHERE id<=$1 AND (kind=ANY($4) OR (kind=ANY($3) AND ($2::text[] IS NULL OR original_packet_identity(kind,payload)=ANY($2) OR event_id IN(SELECT event_id FROM claims) OR (kind='jobs.station.selection_evidence' AND payload->>'claim_event_id' IN(SELECT event_id::text FROM claims))))) ORDER BY id LIMIT $5) SELECT id AS seq,event_id,timestamp,created_at,source,kind,payload,original_group FROM audit_log WHERE id<=$1 AND (id IN(SELECT id FROM relevant) OR original_group->>'id' IN(SELECT group_id FROM relevant WHERE group_id IS NOT NULL)) ORDER BY id LIMIT $5";

pub async fn read_on(
    conn: &mut PgConnection,
    cutoff: i64,
    packet_ids: Option<&[String]>,
    packet_kinds: &[&str],
    global_kinds: &[&str],
    limit: i64,
) -> Result<Vec<OriginalEntry>, String> {
    if cutoff < 0 || !(1..=1_000_000).contains(&limit) {
        return Err("invalid native history bound".into());
    }
    // Judge the same bounded query BEFORE transferring its payloads.
    // A post-fetch byte check can exhaust the service's memory before
    // it gets the chance to refuse a large historical input.
    let summary = format!(
        "SELECT COUNT(*)::bigint,COALESCE(SUM(octet_length(payload::text)),0)::bigint FROM ({HISTORY_SQL}) bounded"
    );
    let (count, bytes): (i64, i64) = sqlx::query_as(&summary)
        .bind(cutoff)
        .bind(packet_ids)
        .bind(packet_kinds)
        .bind(global_kinds)
        .bind(limit + 1)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    if count > limit {
        return Err("complete native membership history exceeds row bound".into());
    }
    if bytes > 128 * 1024 * 1024 {
        return Err("complete native membership history exceeds byte bound".into());
    }
    let rows = sqlx::query_as::<_, OriginalEntry>(HISTORY_SQL)
        .bind(cutoff)
        .bind(packet_ids)
        .bind(packet_kinds)
        .bind(global_kinds)
        .bind(limit + 1)
        .fetch_all(conn)
        .await
        .map_err(|e| e.to_string())?;
    if rows.len() as i64 > limit {
        return Err("complete native membership history exceeds row bound".into());
    }
    Ok(rows)
}
