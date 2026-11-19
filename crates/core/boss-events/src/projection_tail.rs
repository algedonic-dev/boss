//! A bounded committed audit prefix for incremental read projections. Raw JSON
//! reaches its domain decoder before any generic number representation.
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub const MAX_FACTS: i64 = 1024;
pub const MAX_BYTES: i64 = 8 * 1024 * 1024;
#[derive(Debug, Clone)]
pub struct Entry {
    pub seq: i64,
    pub event_id: Uuid,
    pub event_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub source: String,
    pub kind: String,
    pub payload: String,
    pub original_group: Option<crate::original_group::OriginalGroup>,
}
#[derive(Debug, Clone)]
pub struct Tail {
    pub observed_head: i64,
    pub observed_at: DateTime<Utc>,
    pub through: i64,
    pub through_at: DateTime<Utc>,
    pub facts: Vec<Entry>,
}
pub struct Selection<'a> {
    pub source: &'a str,
    pub kinds: &'a [&'a str],
    pub payload_keys: &'a [&'a str],
    pub metadata_keys: &'a [&'a str],
    /// For unselected kinds retain only the presence of these keys, never values.
    pub witness_keys: &'a [&'a str],
}
#[derive(sqlx::FromRow)]
struct Wire {
    seq: i64,
    event_id: Uuid,
    event_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    source: String,
    kind: String,
    payload: String,
    original_group: Option<serde_json::Value>,
}

// The size check and the fetch consume the SAME projected bytes. Domain
// metadata unrelated to this lens never travels to its worker/service loop.
const PROJECTED: &str = "WITH selected AS MATERIALIZED (SELECT id AS seq,event_id,timestamp AS event_at,created_at AS recorded_at,source,kind,original_group,CASE WHEN source=$4 AND kind=ANY($5::text[]) AND jsonb_typeof(payload)='object' THEN COALESCE((SELECT jsonb_object_agg(p.key,p.value) FROM jsonb_each(payload) p WHERE p.key=ANY($6::text[])),'{}'::jsonb) WHEN source=$4 AND (payload ?| $8::text[] OR (jsonb_typeof(payload->'metadata')='object' AND (payload->'metadata') ?| $8::text[]) OR (jsonb_typeof(payload->'step')='object' AND (payload->'step') ?| $8::text[]) OR (jsonb_typeof(payload->'step'->'metadata')='object' AND (payload->'step'->'metadata') ?| $8::text[])) THEN jsonb_build_object('__projection_witness',true) ELSE '{}'::jsonb END AS body FROM audit_log WHERE id>$1 AND id<=$2 ORDER BY id LIMIT $3), trimmed AS (SELECT *,CASE WHEN jsonb_typeof(body->'metadata')='object' THEN jsonb_set(body,'{metadata}',COALESCE((SELECT jsonb_object_agg(m.key,m.value) FROM jsonb_each(body->'metadata') m WHERE m.key=ANY($7::text[])),'{}'::jsonb)) ELSE body END AS narrowed FROM selected), projected AS (SELECT seq,event_id,event_at,recorded_at,source,kind,original_group,CASE WHEN jsonb_typeof(narrowed->'step'->'metadata')='object' THEN jsonb_set(narrowed,'{step,metadata}',COALESCE((SELECT jsonb_object_agg(m.key,m.value) FROM jsonb_each(narrowed->'step'->'metadata') m WHERE m.key=ANY($7::text[])),'{}'::jsonb)) ELSE narrowed END AS payload FROM trimmed)";

/// `budget` is the caller's, measured from just before the read: the claims
/// source passes its history's (production's 2 s, `PgClaimsHistory::new`),
/// and a fixture states its own. It was a literal 2 s here, so a test that
/// judged what a projection FOLDS lost to a loaded gate drive through this
/// read whatever budget its history was given (backlog ebbf38c6).
pub async fn read(
    pool: &sqlx::PgPool,
    after: i64,
    limit: i64,
    selection: Selection<'_>,
    budget: crate::statement_budget::StatementBudget,
) -> Result<Tail, String> {
    use crate::statement_budget::release_before_commit;
    use std::collections::BTreeMap;
    if after < 0
        || !(1..=MAX_FACTS).contains(&limit)
        || selection.source.is_empty()
        || selection.source.len() > 128
        || selection.kinds.is_empty()
        || selection.kinds.len() > 64
        || selection.payload_keys.is_empty()
        || selection.payload_keys.len() > 64
        || selection.metadata_keys.len() > 64
        || selection.witness_keys.len() > 64
    {
        return Err("unsupported native projection prefix bounds".into());
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let result=async {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.map_err(|e|e.to_string())?;
        budget.refresh(&mut tx).await?;
        let observed_at:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
        budget.refresh(&mut tx).await?;
        let head:Option<(i64,DateTime<Utc>)>=sqlx::query_as("SELECT id,created_at FROM audit_log ORDER BY id DESC LIMIT 1").fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?;
        let (observed_head,head_at)=head.unwrap_or((0,observed_at));
        if after>observed_head {return Err("projection cursor is beyond the observed audit head".into())}
        let size_sql=format!("{PROJECTED} SELECT COALESCE(SUM(octet_length(payload::text)+octet_length(source)+octet_length(kind)+COALESCE(octet_length(original_group::text),0)+128),0)::bigint FROM projected");
        budget.refresh(&mut tx).await?;
        let bytes:i64=sqlx::query_scalar(&size_sql).bind(after).bind(observed_head).bind(limit).bind(selection.source).bind(selection.kinds).bind(selection.payload_keys).bind(selection.metadata_keys).bind(selection.witness_keys)
            .fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
        if bytes>MAX_BYTES {return Err("native projection prefix exceeds8MiB".into())}
        let rows_sql=format!("{PROJECTED} SELECT seq,event_id,event_at,recorded_at,source,kind,payload::text AS payload,original_group FROM projected ORDER BY seq");
        budget.refresh(&mut tx).await?;
        let rows:Vec<Wire>=sqlx::query_as(&rows_sql).bind(after).bind(observed_head).bind(limit).bind(selection.source).bind(selection.kinds).bind(selection.payload_keys).bind(selection.metadata_keys).bind(selection.witness_keys)
            .fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
        let mut references=BTreeMap::new();
        for row in &rows {if let Some(raw)=&row.original_group {
            let reference:crate::original_group::GroupReference=serde_json::from_value(raw.clone()).map_err(|e|e.to_string())?;
            if let Some(old)=references.insert(reference.id,reference.clone()) && old!=reference {return Err("audit rows disagree on their original group reference".into())}
        }}
        let roster_members=references.values().try_fold(0i64,|n,r|n.checked_add(r.event_count).ok_or("original roster size overflow"))?;
        if !(0..=MAX_BYTES/256).contains(&roster_members) {return Err("native projection original rosters exceed bounded memory".into())}
        let references:Vec<_>=references.into_values().collect();
        let groups=crate::original_groups::read_groups_on_with_budget(&mut tx,&references,observed_head,MAX_FACTS,crate::original_groups::GROUP_READ_BATCH,Some(budget.checked(&||Ok(())))).await?;
        let groups:BTreeMap<_,_>=groups.into_iter().map(|g|(g.reference.id,g)).collect();
        let mut through=rows.last().map_or(after,|r|r.seq);
        loop {
            let old=through;
            for group in groups.values() {
                let lo=group.audit_sequences.iter().flatten().copied().min().ok_or("original group has no visible member")?;
                let hi=group.audit_sequences.iter().flatten().copied().max().ok_or("original group has no visible member")?;
                if lo<=after && hi>after {return Err("projection cursor bisects an original transaction".into())}
                if lo<=through && (!group.complete || hi>through) {through=through.min(lo-1);}
            }
            if old==through {break}
        }
        if !rows.is_empty() && through<=after {return Err("no complete original transaction fits the current native prefix/batch bound".into())}
        let mut full_bytes=bytes;
        let mut facts=Vec::new();
        for row in rows.into_iter().filter(|r|r.seq<=through) {
            let group=match row.original_group {
                None=>None,
                Some(reference)=>{
                    let reference:crate::original_group::GroupReference=serde_json::from_value(reference).map_err(|e|e.to_string())?;
                    let group=groups.get(&reference.id).ok_or("original group disappeared from its snapshot")?;
                    if !group.validate(through)? || !group.event_ids.iter().zip(&group.audit_sequences).any(|(id,seq)|*id==row.event_id && *seq==Some(row.seq)) {return Err("native fact differs from its complete original transaction".into())}
                    full_bytes=full_bytes.checked_add(serde_json::to_string(group).map_err(|e|e.to_string())?.len() as i64).ok_or("native prefix size overflow")?;
                    if full_bytes>MAX_BYTES {return Err("native projection prefix with original evidence exceeds8MiB".into())}
                    Some(group.clone())
                },
            };
            facts.push(Entry{seq:row.seq,event_id:row.event_id,event_at:row.event_at,recorded_at:row.recorded_at,source:row.source,kind:row.kind,payload:row.payload,original_group:group});
        }
        let through_at=facts.last().map_or(head_at,|r|r.recorded_at);
        Ok(Tail{observed_head,observed_at,through,through_at,facts})
    }.await;
    match result {
        Ok(tail) => {
            if let Err(error) = release_before_commit(&mut tx).await {
                let rollback = tx.rollback().await;
                return Err(format!("{error}; rollback acknowledgement: {rollback:?}"));
            }
            tx.commit()
                .await
                .map_err(|e| format!("read transaction acknowledgement unavailable: {e}"))?;
            Ok(tail)
        }
        Err(error) => match tx.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!(
                "{error}; rollback acknowledgement unavailable: {rollback}"
            )),
        },
    }
}
