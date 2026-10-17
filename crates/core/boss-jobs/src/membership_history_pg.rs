//! The read side owns one repeatable-read transaction. Original packet
//! state, complete group rosters, registry history, clock head and the
//! caller's current authorized identity census cannot come from separate cuts.
use crate::{
    events as e,
    membership_replay::{ReplayGroup, ReplayInput, Unavailable},
    move_diagnosis::HistoricalEvent,
    port::JobScope,
};
use boss_clock_client::mapping_history::{ClockMappingFact, ClockTimeline};
use boss_events::original_group::GroupReference;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};

const PACKET_KINDS: &[&str] = &[
    e::JOB_CREATED,
    e::JOB_UPDATED,
    e::STEP_CREATED,
    e::STEP_UPDATED,
    e::STEP_FIRST_RECORDED,
    e::STATION_CLAIMED,
    e::STATION_SELECTION_EVIDENCE,
];
const GLOBAL_KINDS: &[&str] = &[
    e::STATION_PUBLISHED,
    e::STATION_RETIRED,
    e::WORKFLOW_PUBLISHED,
    e::WORKFLOW_RETIRED,
    "clock.mapping.recorded",
];

pub async fn snapshot(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
) -> Result<ReplayInput, String> {
    if *scope == JobScope::None {
        return Err("caller may read no packet membership".into());
    }
    if !(1..=1_000_000).contains(&limit) {
        return Err("invalid membership history bound".into());
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    // BEGIN time precedes the first snapshot read. A later wall read
    // would claim an unobserved interval between that cut and the read.
    let (until, cutoff): (DateTime<Utc>, i64) =
        sqlx::query_as("SELECT transaction_timestamp(),COALESCE(MAX(id),0) FROM audit_log")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    if since >= until {
        return Err("membership window must precede its native snapshot".into());
    }
    let authorized = if *scope == JobScope::All {
        None
    } else {
        let (owner, owners, accounts) = match scope {
            JobScope::OwnerIs(id) => (Some(id.as_str()), None, None),
            JobScope::OwnerIn(ids) => (None, Some(ids.clone()), None),
            JobScope::AccountIn(ids) => (None, None, Some(ids.clone())),
            _ => return Err("unsupported native membership scope".into()),
        };
        let ids:Vec<String>=sqlx::query_scalar("SELECT id::text FROM jobs WHERE ($1::text IS NOT NULL AND owner_id=$1) OR ($2::text[] IS NOT NULL AND owner_id=ANY($2)) OR ($3::text[] IS NOT NULL AND subject_kind IN('account','employee') AND subject_id=ANY($3)) ORDER BY id LIMIT $4")
            .bind(owner).bind(owners).bind(accounts).bind(limit+1).fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
        if ids.len() as i64 > limit {
            return Err("current authorized packet census exceeds bound".into());
        }
        Some(ids.into_iter().collect::<BTreeSet<_>>())
    };
    let ids = authorized
        .as_ref()
        .map(|ids| ids.iter().cloned().collect::<Vec<_>>());
    let rows = boss_events::history_snapshot::read_on(
        &mut tx,
        cutoff,
        ids.as_deref(),
        PACKET_KINDS,
        GLOBAL_KINDS,
        limit,
    )
    .await?;
    // A committed original awaiting relay is evidence of an incomplete
    // audit prefix, even when none of its siblings has reached the log.
    let registry_kinds = GLOBAL_KINDS
        .iter()
        .copied()
        .filter(|kind| *kind != "clock.mapping.recorded")
        .collect::<Vec<_>>();
    let pending:Option<DateTime<Utc>>=sqlx::query_scalar(r#"
        SELECT MIN(o.created_at) FROM event_outbox o
        WHERE o.created_at<$1
          AND NOT EXISTS(SELECT 1 FROM audit_log a WHERE a.event_id=o.event_id AND a.id<=$2)
          AND (o.kind=ANY($5) OR (o.kind=ANY($4) AND (
              $3::text[] IS NULL OR original_packet_identity(o.kind,o.payload)=ANY($3)
              OR (o.kind='jobs.station.claimed' AND original_claim_packet_identity(o.payload,o.event_id,$2,TRUE)=ANY($3))
              OR (o.kind='jobs.station.selection_evidence' AND EXISTS(
                  SELECT 1 FROM (
                      SELECT event_id,payload FROM event_outbox WHERE kind='jobs.station.claimed'
                      UNION ALL
                      SELECT event_id,payload FROM audit_log WHERE id<=$2 AND kind='jobs.station.claimed'
                  ) owner WHERE owner.event_id::text=o.payload->>'claim_event_id'
                    AND original_claim_packet_identity(owner.payload,owner.event_id,$2,TRUE)=ANY($3)
              ))
          )))
    "#)
        .bind(until).bind(cutoff).bind(ids.as_deref()).bind(PACKET_KINDS).bind(registry_kinds)
        .fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
    // Outbox insertion is an original database wall capture preceding
    // commit. Event.timestamp may be business time, including the future;
    // it cannot bound the affected wall-clock interval.
    let source_unavailable = pending
        .into_iter()
        .map(|captured| Unavailable {
            since: captured.max(since),
            until,
            reason: "committed original membership or registry facts await audit relay".into(),
        })
        .collect();
    // Negative conservation check only: current identities can reveal
    // an original admission or Step creation missing from both durable
    // originals. They cannot stand in for that original's past state.
    let packet_prefix_complete:bool=sqlx::query_scalar("WITH admissions AS MATERIALIZED (SELECT kind,payload->>'id' AS id,payload->>'job_id' AS job_id FROM audit_log WHERE id<=$2 AND kind IN('jobs.job.created','jobs.step.created') UNION ALL SELECT kind,payload->>'id',payload->>'job_id' FROM event_outbox WHERE kind IN('jobs.job.created','jobs.step.created')) SELECT NOT EXISTS(SELECT j.id FROM jobs j LEFT JOIN admissions a ON a.kind='jobs.job.created' AND a.id=j.id::text WHERE ($1::text[] IS NULL OR j.id::text=ANY($1)) AND a.id IS NULL UNION ALL SELECT s.id FROM steps s JOIN jobs j ON j.id=s.job_id LEFT JOIN admissions a ON a.kind='jobs.step.created' AND a.id=s.id::text AND a.job_id=s.job_id::text WHERE ($1::text[] IS NULL OR j.id::text=ANY($1)) AND a.id IS NULL)")
        .bind(ids.as_deref()).bind(cutoff).fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
    let head: Option<String> =
        sqlx::query_scalar("SELECT revision::text FROM clock_mapping_head WHERE id=1")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    let mut groups = BTreeMap::<String, ReplayGroup>::new();
    for row in rows {
        let reference = row
            .original_group
            .map(serde_json::from_value::<GroupReference>)
            .transpose()
            .map_err(|e| format!("invalid original group reference: {e}"))?;
        let key = reference
            .as_ref()
            .map(|r| r.id.to_string())
            .unwrap_or_else(|| format!("legacy:{}", row.row.seq));
        if !groups.contains_key(&key) {
            let original = match reference.as_ref() {
                Some(reference) => Some(
                    boss_events::original_groups::read_group_on(&mut tx, reference, cutoff, 4096)
                        .await?,
                ),
                None => None,
            };
            groups.insert(
                key.clone(),
                ReplayGroup {
                    original,
                    rows: vec![],
                },
            );
        }
        let event = boss_core::event::Event {
            id: row.row.event_id,
            timestamp: row.row.timestamp,
            source: row.row.source,
            kind: row.row.kind,
            payload: row.row.payload,
        };
        if let Some(group) = groups.get_mut(&key) {
            if group.original.as_ref().map(|original| &original.reference) != reference.as_ref() {
                return Err(
                    "original sibling reference disagrees with its verified audit-anchored group"
                        .into(),
                );
            }
            group.rows.push(HistoricalEvent {
                seq: row.row.seq,
                created_at: Some(row.row.created_at),
                event,
            });
        }
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by_key(|g| g.rows.iter().map(|r| r.seq).max());
    let mut facts = Vec::new();
    for group in &groups {
        for row in &group.rows {
            if row.event.kind == "clock.mapping.recorded" {
                let mut value = row.event.payload.clone();
                let object = value
                    .as_object_mut()
                    .ok_or("original clock fact is not an object")?;
                object.insert("audit_created_at".into(), serde_json::json!(row.created_at));
                object.insert("seq".into(), serde_json::json!(row.seq));
                object.insert(
                    "complete_group".into(),
                    serde_json::json!(
                        group
                            .original
                            .as_ref()
                            .is_some_and(|o| o.validate(cutoff) == Ok(true))
                    ),
                );
                facts.push(
                    serde_json::from_value::<ClockMappingFact>(value)
                        .map_err(|e| format!("invalid original clock mapping: {e}"))?,
                );
            }
        }
    }
    facts.sort_by_key(|f| f.seq);
    let clock = ClockTimeline::build(
        &facts,
        head.as_deref(),
        since - chrono::Duration::microseconds(1),
        until,
    );
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(ReplayInput {
        groups,
        since,
        until,
        audit_cutoff: cutoff,
        actor,
        scope: scope.clone(),
        currently_authorized: authorized,
        clock,
        registry_prefix_complete: true,
        packet_prefix_complete,
        source_unavailable,
    })
}
