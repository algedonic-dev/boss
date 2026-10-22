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

// The relevant set, spelled once: the bound pre-check and the full
// history read must judge the same rows (CLAUDE.md §9a). Packet rows are
// read only above `$6`, the caller's cut; global rows (registry and
// clock facts) from the first row of the log, because they are few and
// the state at the cut is built from all of them (design c195e83e).
macro_rules! relevant_ctes {
    () => {
        "WITH claims AS MATERIALIZED (SELECT event_id FROM audit_log WHERE id>$6 AND id<=$1 AND kind='jobs.station.claimed' AND ($2::text[] IS NULL OR original_claim_packet_identity(payload,event_id,$1)=ANY($2))), relevant AS MATERIALIZED (SELECT id,original_group->>'id' AS group_id FROM audit_log WHERE id<=$1 AND (kind=ANY($4) OR (id>$6 AND kind=ANY($3) AND ($2::text[] IS NULL OR original_packet_identity(kind,payload)=ANY($2) OR event_id IN(SELECT event_id FROM claims) OR (kind='jobs.station.selection_evidence' AND payload->>'claim_event_id' IN(SELECT event_id::text FROM claims))))) ORDER BY id LIMIT $5)"
    };
}

const RELEVANT_COUNT_SQL: &str =
    concat!(relevant_ctes!(), " SELECT COUNT(*)::bigint FROM relevant");

/// The `payload` column of an `audit_log` row, with its top-level
/// `metadata` object cut to the keys bound at `$keys` when the row's kind
/// is one bound at `$kinds`; every other row, and a row whose metadata is
/// not an object, is read whole. Spelled once for every read that takes
/// a `MetadataKeys`, so the bytes a bound judges are the bytes that move
/// (CLAUDE.md §9a).
///
/// WHY (backlog 45b25020, measured 2026-10-22): the station-flow read
/// refused its 24 h and 48 h windows on the 128 MiB byte bound while the
/// rows it needed were small. 500 `jobs.job.updated` rows were 12.9 MB,
/// 98% of it metadata no station predicate reads (`brief`, receipts,
/// plans) — about 430 MB a day of Job updates against a bound that is
/// not meant to grow with the log.
#[macro_export]
macro_rules! projected_payload {
    ($kinds:literal, $keys:literal) => {
        concat!(
            "CASE WHEN kind=ANY(",
            $kinds,
            "::text[]) AND jsonb_typeof(payload->'metadata')='object' \
             THEN jsonb_set(payload,'{metadata}',COALESCE((SELECT jsonb_object_agg(m.key,m.value) \
             FROM jsonb_each(payload->'metadata') m WHERE m.key=ANY(",
            $keys,
            "::text[])),'{}'::jsonb)) ELSE payload END"
        )
    };
}

// A row of neither the packet (`$3`) nor the global (`$4`) kinds is in
// the history only as an original sibling, read so its group is whole;
// with `$9` bound it carries only those top-level keys of its payload.
//
// WHY (backlog 4744f5bd, measured 2026-10-22 over 48 h, 189,054 rows):
// after the metadata cut above, the kinds the membership read applies
// were 83 MB and every other kind in the window 104 MB, the ceiling on
// what their siblings move — `step.done.task`, `step.done.gate-verdict`
// and `jobs.metadata.conditionally_merged` each a fresh copy of the
// metadata the Job row had just had cut — and the 48 h window refused
// the 128 MiB bound. Cut to their identity keys those kinds are 3.5 MB.
const HISTORY_SQL: &str = concat!(
    relevant_ctes!(),
    " SELECT id AS seq,event_id,timestamp,created_at,source,kind,\
     CASE WHEN $9::text[] IS NOT NULL AND NOT (kind=ANY($3) OR kind=ANY($4)) \
     AND jsonb_typeof(payload)='object' THEN COALESCE((SELECT jsonb_object_agg(s.key,s.value) \
     FROM jsonb_each(payload) s WHERE s.key=ANY($9::text[])),'{}'::jsonb) ELSE ",
    projected_payload!("$7", "$8"),
    " END AS payload,original_group FROM audit_log WHERE id<=$1 AND (id IN(SELECT id FROM relevant) OR original_group->>'id' IN(SELECT group_id FROM relevant WHERE group_id IS NOT NULL)) ORDER BY id LIMIT $5"
);

/// What of each row its reader evaluates. A row of one of `kinds`
/// carries only `keys` of its metadata; with `siblings` named, a row of
/// no kind the read was asked for — present only as another row's
/// original sibling — carries only those top-level keys of its payload.
/// The default, no kinds and no siblings, reads every payload whole.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataKeys<'a> {
    pub kinds: &'a [&'a str],
    pub keys: &'a [String],
    pub siblings: Option<&'a [&'a str]>,
}

/// The bounded history at `cutoff`: every global row, every packet row
/// above `after` (0 reads packet rows from genesis), and every original
/// sibling of either, wherever it sits — a group that straddles `after`
/// is read whole (c195e83e Q2). Answers the rows and their payload bytes,
/// judged against `limit` and `byte_limit` before any payload moves.
/// Rows `metadata` names carry only the keys it names, siblings of no
/// asked-for kind only the keys `metadata.siblings` names, and the bytes
/// judged are the bytes those projected rows carry.
#[allow(clippy::too_many_arguments)]
pub async fn read_on(
    conn: &mut PgConnection,
    cutoff: i64,
    after: i64,
    packet_ids: Option<&[String]>,
    packet_kinds: &[&str],
    global_kinds: &[&str],
    metadata: MetadataKeys<'_>,
    limit: i64,
    byte_limit: i64,
) -> Result<(Vec<OriginalEntry>, i64), String> {
    if cutoff < 0 || after < 0 || after > cutoff || !(1..=1_000_000).contains(&limit) {
        return Err("invalid native history bound".into());
    }
    // Every relevant row is also an outer row, so a relevant set over
    // the bound already fixes the answer: refuse it here, before the
    // full scan. Without this, the COUNT below ran the whole outer
    // query, whose group-sibling arm has no index, and the default
    // flow basis held a REPEATABLE READ transaction on production for
    // 16+ minutes (1.2M history rows against 250k) only to refuse
    // anyway (f5d2a8f5, 2026-10-17).
    let relevant: i64 = sqlx::query_scalar(RELEVANT_COUNT_SQL)
        .bind(cutoff)
        .bind(packet_ids)
        .bind(packet_kinds)
        .bind(global_kinds)
        .bind(limit + 1)
        .bind(after)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    if relevant > limit {
        return Err("complete native membership history exceeds row bound".into());
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
        .bind(after)
        .bind(metadata.kinds)
        .bind(metadata.keys)
        .bind(metadata.siblings)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    if count > limit {
        return Err("complete native membership history exceeds row bound".into());
    }
    if bytes > byte_limit {
        return Err("complete native membership history exceeds byte bound".into());
    }
    let rows = sqlx::query_as::<_, OriginalEntry>(HISTORY_SQL)
        .bind(cutoff)
        .bind(packet_ids)
        .bind(packet_kinds)
        .bind(global_kinds)
        .bind(limit + 1)
        .bind(after)
        .bind(metadata.kinds)
        .bind(metadata.keys)
        .bind(metadata.siblings)
        .fetch_all(conn)
        .await
        .map_err(|e| e.to_string())?;
    if rows.len() as i64 > limit {
        return Err("complete native membership history exceeds row bound".into());
    }
    Ok((rows, bytes))
}
