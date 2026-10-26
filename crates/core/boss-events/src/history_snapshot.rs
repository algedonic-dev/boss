//! Bounded immutable history at a caller-owned repeatable-read cut.
//! The owning domain supplies its relevant event kinds and packet
//! identities; this adapter retains complete original group siblings.
use crate::tail_http::HistoricalEntry;
use futures::TryStreamExt;
use serde_json::Value;
use sqlx::PgConnection;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OriginalEntry {
    #[sqlx(flatten)]
    pub row: HistoricalEntry,
    pub original_group: Option<Value>,
}

// Keep PostgreSQL's canonical jsonb text until the byte bound has been
// checked. Re-serializing a Value would omit its spaces and undercount.
#[derive(sqlx::FromRow)]
struct WireEntry {
    seq: i64,
    event_id: uuid::Uuid,
    timestamp: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
    source: String,
    kind: String,
    payload: String,
    original_group: Option<Value>,
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
macro_rules! history_payload {
    ($packets:literal, $globals:literal, $projected:literal, $keys:literal, $siblings:literal) => {
        concat!("CASE WHEN ", $siblings, "::text[] IS NOT NULL AND NOT(kind=ANY(", $packets, ") OR kind=ANY(", $globals,
        ")) AND jsonb_typeof(payload)='object' THEN COALESCE((SELECT jsonb_object_agg(s.key,s.value) FROM jsonb_each(payload) s WHERE s.key=ANY(", $siblings,
        "::text[])),'{}'::jsonb) ELSE ", projected_payload!($projected, $keys), " END")
    };
}

const HISTORY_SQL: &str = concat!(
    relevant_ctes!(),
    " SELECT id AS seq,event_id,timestamp,created_at,source,kind,",
    history_payload!("$3", "$4", "$7", "$8", "$9"),
    " AS payload,original_group FROM audit_log WHERE id<=$1 AND (id IN(SELECT id FROM relevant) OR original_group->>'id' IN(SELECT group_id FROM relevant WHERE group_id IS NOT NULL)) ORDER BY id LIMIT $5"
);

/// Original history for an owned projection bootstrap or delta. Selection
/// pages read primary-key identities first; every payload fetch is bounded
/// by that small identity set, rather than repeating the audit-wide OR
/// scan on every viewer refresh. This is not a cold-read latency claim.
/// The caller owns a native cut/transaction and its statement deadline.
#[allow(clippy::too_many_arguments)]
pub async fn read_paged_on(
    conn: &mut PgConnection,
    cutoff: i64,
    after: i64,
    packet_ids: Option<&[String]>,
    packet_kinds: &[&str],
    global_kinds: &[&str],
    metadata: MetadataKeys<'_>,
    limit: i64,
    byte_limit: i64,
    page_size: i64,
) -> Result<(Vec<OriginalEntry>, i64), String> {
    read_paged_on_with_budget(
        conn,
        cutoff,
        after,
        packet_ids,
        packet_kinds,
        global_kinds,
        metadata,
        limit,
        byte_limit,
        page_size,
        None,
    )
    .await
}

/// The caller's absolute source budget also governs every nested page.
#[allow(clippy::too_many_arguments)]
pub async fn read_paged_on_with_budget(
    conn: &mut PgConnection,
    cutoff: i64,
    after: i64,
    packet_ids: Option<&[String]>,
    packet_kinds: &[&str],
    global_kinds: &[&str],
    metadata: MetadataKeys<'_>,
    limit: i64,
    byte_limit: i64,
    page_size: i64,
    budget: Option<crate::statement_budget::StatementControl<'_>>,
) -> Result<(Vec<OriginalEntry>, i64), String> {
    use std::collections::BTreeSet;
    if cutoff < 0
        || after < 0
        || after > cutoff
        || !(1..=1_000_000).contains(&limit)
        || !(1..=4096).contains(&page_size)
    {
        return Err("invalid native history bound".into());
    }
    if byte_limit < 0 {
        return Err("complete native membership history exceeds byte bound".into());
    }
    let started = std::time::Instant::now();
    let check = || {
        if started.elapsed() >= std::time::Duration::from_secs(60) {
            Err("native membership page deadline exhausted".to_owned())
        } else {
            Ok(())
        }
    };
    let mut selected = BTreeSet::new();
    let mut groups = BTreeSet::new();
    crate::statement_budget::refresh_control(conn, budget).await?;
    let globals: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id,original_group->>'id' FROM audit_log WHERE id<=$1 AND kind=ANY($2) ORDER BY id LIMIT $3")
        .bind(cutoff).bind(global_kinds).bind(limit + 1).fetch_all(&mut *conn).await.map_err(|e| e.to_string())?;
    for (id, group) in globals {
        selected.insert(id);
        if let Some(group) = group {
            groups.insert(group);
        }
    }
    let mut cursor = after;
    let mut scanned = 0_i64;
    loop {
        check()?;
        if selected.len() as i64 > limit {
            return Err("complete native membership history exceeds row bound".into());
        }
        crate::statement_budget::refresh_control(conn, budget).await?;
        let page: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM audit_log WHERE id>$1 AND id<=$2 ORDER BY id LIMIT $3",
        )
        .bind(cursor)
        .bind(cutoff)
        .bind(page_size)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
        let Some(last) = page.last().copied() else {
            break;
        };
        scanned += page.len() as i64;
        if scanned > 1_000_000 {
            return Err("native membership identity scan exceeds bound".into());
        }
        // A selection receipt names its original claim, whose immutable
        // event UUID is indexed. Match the old canonical UUID text exactly;
        // malformed references do not turn an unscoped lookup into a scan.
        crate::statement_budget::refresh_control(conn, budget).await?;
        let relevant: Vec<(i64, Option<String>)> = sqlx::query_as(r#"
            SELECT a.id,a.original_group->>'id' FROM audit_log a
            WHERE a.id=ANY($1) AND a.kind=ANY($3) AND (
                $2::text[] IS NULL OR original_packet_identity(a.kind,a.payload)=ANY($2)
                OR (a.kind='jobs.station.claimed' AND original_claim_packet_identity(a.payload,a.event_id,$4)=ANY($2))
                OR (a.kind='jobs.station.selection_evidence' AND EXISTS(
                    SELECT 1 FROM audit_log c WHERE c.event_id=CASE WHEN a.payload->>'claim_event_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN (a.payload->>'claim_event_id')::uuid ELSE NULL END
                    AND c.id>$5 AND c.id<=$4 AND c.kind='jobs.station.claimed'
                    AND original_claim_packet_identity(c.payload,c.event_id,$4)=ANY($2)
                ))
            ) ORDER BY a.id"#)
            .bind(&page).bind(packet_ids).bind(packet_kinds).bind(cutoff).bind(after)
            .fetch_all(&mut *conn).await.map_err(|e| e.to_string())?;
        for (id, group) in relevant {
            selected.insert(id);
            if let Some(group) = group {
                groups.insert(group);
            }
        }
        cursor = last;
    }
    check()?;
    // Group IDs use the existing original-group expression index. Fetch
    // all siblings through the SAME cut, including ones before `after`.
    let groups: Vec<_> = groups.into_iter().collect();
    crate::statement_budget::refresh_control(conn, budget).await?;
    let siblings: Vec<i64> = sqlx::query_scalar("SELECT id FROM audit_log WHERE id<=$1 AND original_group->>'id'=ANY($2) ORDER BY id LIMIT $3")
        .bind(cutoff).bind(&groups).bind(limit + 1).fetch_all(&mut *conn).await.map_err(|e| e.to_string())?;
    selected.extend(siblings);
    if selected.len() as i64 > limit {
        return Err("complete native membership history exceeds row bound".into());
    }
    let ids: Vec<_> = selected.into_iter().collect();
    let query = concat!(
        "SELECT id AS seq,event_id,timestamp,created_at,source,kind,(",
        history_payload!("$2", "$3", "$4", "$5", "$6"),
        ")::text AS payload,original_group FROM audit_log WHERE id=ANY($1) ORDER BY id"
    );
    let mut rows = Vec::new();
    let mut bytes = 0_i64;
    for page in ids.chunks(page_size as usize) {
        check()?;
        crate::statement_budget::refresh_control(conn, budget).await?;
        let mut stream = sqlx::query_as::<_, WireEntry>(query)
            .bind(page)
            .bind(packet_kinds)
            .bind(global_kinds)
            .bind(metadata.kinds)
            .bind(metadata.keys)
            .bind(metadata.siblings)
            .fetch(&mut *conn);
        while let Some(wire) = stream.try_next().await.map_err(|e| e.to_string())? {
            if rows.len() as i64 >= limit {
                return Err("complete native membership history exceeds row bound".into());
            }
            bytes = bytes
                .checked_add(i64::try_from(wire.payload.len()).map_err(|_| {
                    "complete native membership history exceeds byte bound".to_owned()
                })?)
                .filter(|total| *total <= byte_limit)
                .ok_or_else(|| {
                    "complete native membership history exceeds byte bound".to_owned()
                })?;
            rows.push(OriginalEntry {
                row: HistoricalEntry {
                    seq: wire.seq,
                    event_id: wire.event_id,
                    timestamp: wire.timestamp,
                    created_at: wire.created_at,
                    source: wire.source,
                    kind: wire.kind,
                    payload: serde_json::from_str(&wire.payload).map_err(|e| e.to_string())?,
                },
                original_group: wire.original_group,
            });
        }
    }
    Ok((rows, bytes))
}

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
/// judged against `limit` and `byte_limit`. Payloads are projected once
/// and streamed; an oversized input is refused before retaining its
/// next row, rather than fetching the complete input into memory.
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
    // full scan. Without this, the former COUNT ran the whole outer
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
    // Do not scan and project every payload merely to size a second
    // identical read. Keep the row LIMIT and count exact projected text
    // bytes while streaming. Retained payloads stay within byte_limit;
    // one current wire row may exceed it and is then discarded.
    let query = format!(
        "SELECT seq,event_id,timestamp,created_at,source,kind,payload::text AS payload,original_group FROM ({HISTORY_SQL}) bounded ORDER BY seq"
    );
    let mut stream = sqlx::query_as::<_, WireEntry>(&query)
        .bind(cutoff)
        .bind(packet_ids)
        .bind(packet_kinds)
        .bind(global_kinds)
        .bind(limit + 1)
        .bind(after)
        .bind(metadata.kinds)
        .bind(metadata.keys)
        .bind(metadata.siblings)
        .fetch(conn);
    let mut rows = Vec::new();
    let mut bytes = 0_i64;
    if byte_limit < 0 {
        return Err("complete native membership history exceeds byte bound".into());
    }
    while let Some(wire) = stream.try_next().await.map_err(|e| e.to_string())? {
        if rows.len() as i64 >= limit {
            return Err("complete native membership history exceeds row bound".into());
        }
        let row_bytes = i64::try_from(wire.payload.len())
            .map_err(|_| "complete native membership history exceeds byte bound".to_owned())?;
        bytes = bytes
            .checked_add(row_bytes)
            .filter(|total| *total <= byte_limit)
            .ok_or_else(|| "complete native membership history exceeds byte bound".to_owned())?;
        let payload = serde_json::from_str(&wire.payload).map_err(|e| e.to_string())?;
        rows.push(OriginalEntry {
            row: HistoricalEntry {
                seq: wire.seq,
                event_id: wire.event_id,
                timestamp: wire.timestamp,
                created_at: wire.created_at,
                source: wire.source,
                kind: wire.kind,
                payload,
            },
            original_group: wire.original_group,
        });
    }
    Ok((rows, bytes))
}
