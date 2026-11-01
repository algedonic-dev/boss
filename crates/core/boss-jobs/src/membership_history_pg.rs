//! The read side owns one repeatable-read transaction. Original packet
//! state, complete group rosters, registry history, clock head and the
//! caller's current authorized identity census cannot come from separate cuts.
//!
//! THE FOLD STARTS AT A WITNESSED CUT (design c195e83e, Q1-Q5 approved
//! 2026-10-17). Until then this read folded every packet from genesis:
//! on 2026-10-17 scope All matched 1,235,314 rows against the 250,000-row
//! bound, so every station answered UNKNOWN and the read held a
//! production transaction for sixteen minutes to say so. The bounds stay;
//! a bound that grows with the log is not a bound. What changed is where
//! the fold starts: each carried packet's latest original snapshot before
//! the window, then the window's own original groups.
use crate::{
    events as e,
    membership_replay::{InitialState, ReplayGroup, ReplayInput, Unavailable},
    move_diagnosis::HistoricalEvent,
    port::JobScope,
    registry::WorkflowSpec,
    station_queue::StationPredicate,
    stations::StationSpec,
};
use boss_clock_client::mapping_history::{ClockMappingFact, ClockTimeline};
use boss_core::job::JobStatus;
use boss_events::history_snapshot::{MetadataKeys, OriginalEntry};
use boss_events::original_group::GroupReference;
use boss_events::original_groups::GROUP_READ_BATCH;
use boss_events::statement_budget::{
    StatementBudget, StatementControl, refresh_control as refresh,
};
use chrono::{DateTime, NaiveDate, Utc};
use futures::TryStreamExt;
use sqlx::{PgConnection, PgPool};
use std::collections::{BTreeMap, BTreeSet};

mod observation;
pub use observation::observe;

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
/// The rows a starting-state snapshot is made of: a Job's state, a
/// Step's state, and a claim receipt that carries a Step's state.
const SNAPSHOT_KINDS: &[&str] = &[
    e::JOB_CREATED,
    e::JOB_UPDATED,
    e::STEP_CREATED,
    e::STEP_UPDATED,
    e::STEP_FIRST_RECORDED,
    e::STATION_CLAIMED,
];
/// One byte bound for everything this read transfers: the registry read
/// that names the evaluated keys, the window's history and the starting
/// state share it, as they share `limit`.
const BYTE_BOUND: i64 = 128 * 1024 * 1024;
/// The rows whose metadata the replay reads only through a station
/// predicate or a Step's agent block, so they carry only those keys
/// (backlog 45b25020). Claim receipts and first records are read whole:
/// their payload is the exact receipt the replay restores a Step from.
const PROJECTED_KINDS: &[&str] = &[
    e::JOB_CREATED,
    e::JOB_UPDATED,
    e::STEP_CREATED,
    e::STEP_UPDATED,
];
/// The payload keys `membership_replay::packet_identity` reads a row's
/// packet from, other than a claim's receipt (a claim is a packet kind,
/// read whole). A row of neither `PACKET_KINDS` nor `GLOBAL_KINDS` — an
/// original sibling, read only so its group is whole, and which the
/// replay reads for nothing but its packet — carries these and nothing
/// else (backlog 4744f5bd); pinned to the reader by
/// `a_sibling_cut_to_its_identity_keys_has_the_same_packet_identity`.
const IDENTITY_KEYS: &[&str] = &["job_id", "step", "id"];
/// The registry publications every station the replay can evaluate is
/// built from: authored stations, and the workflows stations derive from.
const REGISTRY_KINDS: &[&str] = &[
    e::STATION_PUBLISHED,
    e::STATION_RETIRED,
    e::WORKFLOW_PUBLISHED,
    e::WORKFLOW_RETIRED,
];

pub async fn snapshot(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
) -> Result<ReplayInput, String> {
    snapshot_batched(pool, since, actor, scope, limit, GROUP_READ_BATCH).await
}

pub async fn census(
    pool: &PgPool,
    scope: &JobScope,
    limit: i64,
) -> Result<Option<BTreeSet<String>>, String> {
    if *scope == JobScope::None || !(1..=1_000_000).contains(&limit) {
        return Err("invalid native membership census scope or bound".into());
    }
    let mut conn = pool.acquire().await.map_err(|why| why.to_string())?;
    census_on(&mut conn, scope, limit, None).await
}

pub async fn projection_source(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    previous: Option<crate::membership_projection::ProjectionCursor>,
    limit: i64,
) -> Result<crate::membership_projection::ProjectionSource, String> {
    projection_source_with_budget(pool, since, actor, scope, previous, limit, None).await
}

pub async fn projection_source_with_budget(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    previous: Option<crate::membership_projection::ProjectionCursor>,
    limit: i64,
    budget: Option<&crate::membership_replay::ReplayBudget>,
) -> Result<crate::membership_projection::ProjectionSource, String> {
    projection_source_observed(pool, since, actor, scope, previous, limit, budget, None).await
}

#[allow(clippy::too_many_arguments)]
async fn projection_source_observed(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    previous: Option<crate::membership_projection::ProjectionCursor>,
    limit: i64,
    budget: Option<&crate::membership_replay::ReplayBudget>,
    mut observation: Option<&mut crate::membership_observation::Observation>,
) -> Result<crate::membership_projection::ProjectionSource, String> {
    if *scope == JobScope::None || !(1..=1_000_000).contains(&limit) {
        return Err("invalid native projection source scope or bound".into());
    }
    if let Some(budget) = budget {
        budget.check()?;
    }
    let check = || budget.map_or(Ok(()), crate::membership_replay::ReplayBudget::check);
    let statement_budget = budget.map(|budget| {
        StatementBudget::until(tokio::time::Instant::from_std(budget.deadline()))
            .with_statement_limit(std::time::Duration::from_secs(20))
            .checked(&check)
    });
    let mut connection = match budget {
        Some(budget) => {
            let acquire = pool.acquire();
            tokio::pin!(acquire);
            // Acquiring a pool permit has no dispatched source SQL to
            // retain. Cancellation may drop this pending acquisition;
            // actual transaction work below always awaits its rollback.
            let mut checks = tokio::time::interval(std::time::Duration::from_millis(10));
            loop {
                tokio::select! {
                    result = &mut acquire => break result.map_err(|why| why.to_string())?,
                    _ = checks.tick() => budget.check().map_err(|why| format!("native membership source pool wait: {why}"))?,
                }
            }
        }
        None => pool.acquire().await.map_err(|why| why.to_string())?,
    };
    use sqlx::Connection;
    let mut tx = connection.begin().await.map_err(|why| why.to_string())?;
    let result = async {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|why| why.to_string())?;
        sqlx::query("SET LOCAL statement_timeout = '20s'")
            .execute(&mut *tx)
            .await
            .map_err(|why| why.to_string())?;
        refresh(&mut tx, statement_budget).await?;
        let native: (DateTime<Utc>, i64) =
            sqlx::query_as("SELECT transaction_timestamp(),COALESCE(MAX(id),0) FROM audit_log")
                .fetch_one(&mut *tx)
                .await
                .map_err(|why| why.to_string())?;
        if let Some(report) = observation.as_deref_mut() {
            report.until = Some(native.0);
            report.audit_cutoff = Some(native.1);
        }
        if since >= native.0
            || previous
                .as_ref()
                .is_some_and(|old| old.until >= native.0 || old.audit_cutoff > native.1)
        {
            return Err("projection cursor is beyond its native snapshot".into());
        }
        let input = snapshot_on(
            &mut tx,
            previous.as_ref().map_or(since, |old| old.until),
            actor.clone(),
            scope,
            limit,
            GROUP_READ_BATCH,
            BYTE_BOUND,
            native,
            previous.as_ref().map(|old| old.audit_cutoff),
            None,
            since,
            true,
            statement_budget,
            observation,
        )
        .await?;
        let mut replacement_ids = BTreeSet::new();
        if let Some(old) = &previous {
            if let (Some(current), Some(previous)) =
                (&input.currently_authorized, &old.currently_authorized)
            {
                replacement_ids.extend(current.difference(previous).cloned());
            }
            // A relay may commit a packet row whose insertion timestamp predates
            // the old wall cut. Catch up only its whole visible original group.
            for group in &input.groups {
                if group.rows.iter().any(|row| {
                    row.seq <= old.audit_cutoff || row.created_at.is_none_or(|at| at < old.until)
                }) {
                    replacement_ids.extend(
                        group
                            .rows
                            .iter()
                            .filter_map(crate::membership_replay::packet_identity)
                            .filter(|id| {
                                input
                                    .currently_authorized
                                    .as_ref()
                                    .is_none_or(|ids| ids.contains(id))
                            }),
                    );
                }
            }
            // Close the replacement set over every complete original group.
            loop {
                let before = replacement_ids.len();
                for group in &input.groups {
                    if group
                        .rows
                        .iter()
                        .filter_map(crate::membership_replay::packet_identity)
                        .any(|id| replacement_ids.contains(&id))
                    {
                        replacement_ids.extend(
                            group
                                .rows
                                .iter()
                                .filter_map(crate::membership_replay::packet_identity)
                                .filter(|id| {
                                    input
                                        .currently_authorized
                                        .as_ref()
                                        .is_none_or(|ids| ids.contains(id))
                                }),
                        );
                    }
                }
                if replacement_ids.len() == before {
                    break;
                }
            }
        }
        let replacement_history = if replacement_ids.is_empty() {
            None
        } else {
            Some(
                snapshot_on(
                    &mut tx,
                    since,
                    actor,
                    scope,
                    limit,
                    GROUP_READ_BATCH,
                    BYTE_BOUND,
                    native,
                    None,
                    Some(&replacement_ids),
                    since,
                    true,
                    statement_budget,
                    None,
                )
                .await?,
            )
        };
        if let Some(budget) = budget {
            budget.check()?;
        }
        Ok(crate::membership_projection::ProjectionSource {
            input,
            replacement_history,
            replacement_ids,
        })
    }
    .await;
    // Never detach an expired query or hand an aborted transaction back
    // while starting the other window. SQL/rollback remain owned here.
    match result {
        Ok(source) => {
            tx.commit().await.map_err(|why| why.to_string())?;
            if let Some(budget) = budget {
                budget.check()?;
            }
            Ok(source)
        }
        Err(why) => match tx.rollback().await {
            Ok(()) => Err(why),
            Err(rollback) => Err(format!("{why}; membership source rollback: {rollback}")),
        },
    }
}

/// `snapshot` with the group-read batch size named, so a test can show
/// that a batch boundary never changes a verdict (c195e83e Q5).
pub(crate) async fn snapshot_batched(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
    batch: usize,
) -> Result<ReplayInput, String> {
    snapshot_bounded(pool, since, actor, scope, limit, batch, BYTE_BOUND).await
}

/// `snapshot_batched` with the byte bound named, so a test can hold a
/// read to a bound its fixture can reach without moving 128 MiB.
pub(crate) async fn snapshot_bounded(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
    batch: usize,
    byte_bound: i64,
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
    // Bound each native statement, including lock waits, inside this one
    // snapshot transaction. This is not an overall transaction deadline;
    // rollback restores the pooled connection's original session setting.
    sqlx::query("SET LOCAL statement_timeout = '20s'")
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
    let input = snapshot_on(
        &mut tx,
        since,
        actor,
        scope,
        limit,
        batch,
        byte_bound,
        (until, cutoff),
        None,
        None,
        since,
        false,
        None,
        None,
    )
    .await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(input)
}

#[allow(clippy::too_many_arguments)]
async fn snapshot_on(
    conn: &mut PgConnection,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
    batch: usize,
    byte_bound: i64,
    native: (DateTime<Utc>, i64),
    after: Option<i64>,
    selected: Option<&BTreeSet<String>>,
    retained_since: DateTime<Utc>,
    paged: bool,
    statement_budget: Option<StatementControl<'_>>,
    mut observation: Option<&mut crate::membership_observation::Observation>,
) -> Result<ReplayInput, String> {
    let (until, cutoff) = native;
    let authorized = if let Some(selected) = selected {
        Some(selected.clone())
    } else {
        census_on(conn, scope, limit, statement_budget).await?
    };
    let ids = authorized
        .as_ref()
        .map(|ids| ids.iter().cloned().collect::<Vec<_>>());
    let cut = match after {
        Some(cut) => cut,
        None => {
            window_cut(
                &mut *conn,
                since,
                cutoff,
                statement_budget,
                observation.as_deref_mut(),
            )
            .await?
        }
    };
    let (keys, registry_bytes, logged) = evaluated_keys(
        &mut *conn,
        cutoff,
        until,
        limit,
        byte_bound,
        statement_budget,
    )
    .await?;
    let retention = retention(&mut *conn, logged, actor.as_deref(), statement_budget).await?;
    let metadata = MetadataKeys {
        kinds: PROJECTED_KINDS,
        keys: &keys,
        siblings: Some(IDENTITY_KEYS),
    };
    let (rows, history_bytes) = if paged {
        boss_events::history_snapshot::read_paged_on_with_budget(
            &mut *conn,
            cutoff,
            cut,
            ids.as_deref(),
            PACKET_KINDS,
            GLOBAL_KINDS,
            metadata,
            limit,
            byte_bound - registry_bytes,
            512,
            statement_budget,
        )
        .await?
    } else {
        boss_events::history_snapshot::read_on(
            &mut *conn,
            cutoff,
            cut,
            ids.as_deref(),
            PACKET_KINDS,
            GLOBAL_KINDS,
            metadata,
            limit,
            byte_bound - registry_bytes,
        )
        .await?
    };
    let history_rows = rows.len() as i64;
    // A committed original awaiting relay is evidence of an incomplete
    // audit prefix, even when none of its siblings has reached the log.
    let registry_kinds = GLOBAL_KINDS
        .iter()
        .copied()
        .filter(|kind| *kind != "clock.mapping.recorded")
        .collect::<Vec<_>>();
    refresh(conn, statement_budget).await?;
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
        .fetch_one(&mut *conn).await.map_err(|e|e.to_string())?;
    // Outbox insertion is an original database wall capture preceding
    // commit. Event.timestamp may be business time, including the future;
    // it cannot bound the affected wall-clock interval.
    let source_unavailable = pending
        .into_iter()
        .map(|captured| Unavailable {
            since: captured.max(retained_since),
            until,
            reason: "committed original membership or registry facts await audit relay".into(),
        })
        .collect();
    refresh(conn, statement_budget).await?;
    let head: Option<String> =
        sqlx::query_scalar("SELECT revision::text FROM clock_mapping_head WHERE id=1")
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    let groups = original_groups(&mut *conn, rows, cutoff, batch, statement_budget).await?;
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
        retained_since - chrono::Duration::microseconds(1),
        until,
    );
    let initial = if after.is_some() {
        InitialState::default()
    } else {
        starting_state(
            &mut *conn,
            Cut {
                seq: cut,
                since,
                cutoff,
            },
            ids.as_deref(),
            &groups,
            &clock,
            Budget {
                census: limit,
                rows: limit - history_rows,
                bytes: byte_bound - registry_bytes - history_bytes,
            },
            metadata,
            &retention,
            actor.as_deref(),
            scope,
            until,
            statement_budget,
            observation,
        )
        .await?
    };
    let groups = if let Some(after) = after {
        groups
            .into_iter()
            .filter(|group| group.rows.iter().any(|row| row.seq > after))
            .collect()
    } else {
        groups
    };
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
        // The census now judges each packet at the cut (Q3) and names a
        // missing original on the packet itself; there is no genesis
        // prefix left for a global flag to judge.
        packet_prefix_complete: true,
        source_unavailable,
        initial,
    })
}

async fn census_on(
    conn: &mut PgConnection,
    scope: &JobScope,
    limit: i64,
    statement_budget: Option<StatementControl<'_>>,
) -> Result<Option<BTreeSet<String>>, String> {
    let authorized = if *scope == JobScope::All {
        None
    } else {
        let (owner, owners, accounts) = match scope {
            JobScope::OwnerIs(id) => (Some(id.as_str()), None, None),
            JobScope::OwnerIn(ids) => (None, Some(ids.clone()), None),
            JobScope::AccountIn(ids) => (None, None, Some(ids.clone())),
            _ => return Err("unsupported native membership scope".into()),
        };
        refresh(conn, statement_budget).await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT id::text FROM jobs WHERE ($1::text IS NOT NULL AND owner_id=$1) OR ($2::text[] IS NOT NULL AND owner_id=ANY($2)) OR ($3::text[] IS NOT NULL AND subject_kind IN('account','employee') AND subject_id=ANY($3)) ORDER BY id LIMIT $4")
            .bind(owner).bind(owners).bind(accounts).bind(limit+1).fetch_all(&mut *conn).await.map_err(|e|e.to_string())?;
        if ids.len() as i64 > limit {
            return Err("current authorized packet census exceeds bound".into());
        }
        Some(ids.into_iter().collect::<BTreeSet<_>>())
    };
    Ok(authorized)
}

/// Every metadata key the replay can read off a Job or Step row at this
/// cut (backlog 45b25020): each key a station published at or below the
/// cut names, each key a station derived from a workflow published at or
/// below it names, and a Step's agent block (`agent_spec::resolved`
/// reads it to decide whether to project the workflow's). Every version
/// is read, not only the one in force at `at`, so a station the window
/// republishes is covered; a superset only carries more metadata, it
/// never decides differently. Answers the keys and the registry bytes,
/// judged against the bounds before any payload moves, and every logged
/// station version that declares a terminal window (backlog bbeff5cb).
async fn evaluated_keys(
    conn: &mut PgConnection,
    cutoff: i64,
    at: DateTime<Utc>,
    limit: i64,
    byte_bound: i64,
    statement_budget: Option<StatementControl<'_>>,
) -> Result<(Vec<String>, i64, Vec<StationSpec>), String> {
    refresh(conn, statement_budget).await?;
    let (count, bytes): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*)::bigint,COALESCE(SUM(octet_length(payload::text)),0)::bigint \
         FROM audit_log WHERE id<=$1 AND kind=ANY($2)",
    )
    .bind(cutoff)
    .bind(REGISTRY_KINDS)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    if count > limit {
        return Err("native membership registry history exceeds row bound".into());
    }
    if bytes > byte_bound {
        return Err("native membership registry history exceeds byte bound".into());
    }
    refresh(conn, statement_budget).await?;
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT kind,payload FROM audit_log WHERE id<=$1 AND kind=ANY($2) ORDER BY id LIMIT $3",
    )
    .bind(cutoff)
    .bind(REGISTRY_KINDS)
    .bind(limit + 1)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    let mut stations = Vec::new();
    let mut workflows = Vec::new();
    for (kind, payload) in rows {
        if kind == e::STATION_PUBLISHED || kind == e::STATION_RETIRED {
            stations
                .push(serde_json::from_value::<StationSpec>(payload).map_err(|e| e.to_string())?);
        } else {
            workflows
                .push(serde_json::from_value::<WorkflowSpec>(payload).map_err(|e| e.to_string())?);
        }
    }
    stations.extend(crate::station_projection::derived_stations(
        &workflows,
        &[],
        at,
    ));
    let keys: BTreeSet<String> = stations
        .iter()
        .flat_map(|s| s.predicate.metadata_keys())
        .chain(crate::agent_spec::KEYS.map(String::from))
        .collect();
    let retaining = stations
        .into_iter()
        .filter(|s| s.terminal_window_days.is_some())
        .collect();
    Ok((keys.into_iter().collect(), bytes, retaining))
}

/// One station version that can still count a terminal packet: its
/// terminal window, and its predicate bound to the reader as the replay
/// binds it. `None` is a live row whose predicate cannot be read, which
/// is taken to hold every packet its window can.
struct Retention {
    days: u64,
    predicate: Option<StationPredicate>,
}

/// Q1 per station (backlog bbeff5cb): every station that can hold a
/// terminal packet inside the window. The live registry's active rows
/// are read in this snapshot, never a constant (the decision's own
/// words), and so is every logged version at or below the cutoff, so a
/// station the window publishes or retires is counted too — a superset
/// only carries more, it never decides differently. A self predicate
/// the reader cannot bind has no members for this reader (the replay
/// skips it the same way), so it holds nothing.
async fn retention(
    conn: &mut PgConnection,
    logged: Vec<StationSpec>,
    actor: Option<&str>,
    statement_budget: Option<StatementControl<'_>>,
) -> Result<Vec<Retention>, String> {
    refresh(conn, statement_budget).await?;
    let live: Vec<(serde_json::Value, i32)> = sqlx::query_as(
        "SELECT predicate,terminal_window_days FROM stations \
         WHERE status='active' AND terminal_window_days IS NOT NULL",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    let live = live.into_iter().filter_map(|(predicate, days)| {
        let days = u64::try_from(days).ok()?;
        Some((
            serde_json::from_value::<StationPredicate>(predicate).ok(),
            days,
        ))
    });
    let logged = logged
        .into_iter()
        .filter_map(|s| Some((Some(s.predicate), u64::from(s.terminal_window_days?))));
    Ok(live
        .chain(logged)
        .filter_map(|(predicate, days)| match predicate {
            None => Some(Retention {
                days,
                predicate: None,
            }),
            Some(p) => p.bind_self(actor).map(|bound| Retention {
                days,
                predicate: Some(bound),
            }),
        })
        .collect())
}

const WINDOW_CUT_ORDER_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM audit_log WHERE created_at>=$1 AND id<=$2)";

/// The last audit row inserted before the window opens. `created_at` is
/// taken under the audit insertion lock right after `nextval`
/// (20261016143000-original-event-transaction-groups.sql), so it follows
/// audit order and the rows before `since` are a prefix of the log. That
/// is verified, not assumed: a row at or after `since` at or below the
/// cut would fold an in-window transition into the starting state.
async fn window_cut(
    conn: &mut PgConnection,
    since: DateTime<Utc>,
    cutoff: i64,
    statement_budget: Option<StatementControl<'_>>,
    observation: Option<&mut crate::membership_observation::Observation>,
) -> Result<i64, String> {
    refresh(conn, statement_budget).await?;
    let cut: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT id FROM audit_log WHERE created_at<$1 AND id<=$2 \
         ORDER BY created_at DESC,id DESC LIMIT 1),0)",
    )
    .bind(since)
    .bind(cutoff)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| e.to_string())?;
    let disordered = if let Some(report) = observation {
        // Keep the actual native cut even when planning or the check refuses.
        report.window_cut = Some(cut);
        observation::window_cut_order(conn, since, cut, statement_budget, report).await?
    } else {
        refresh(conn, statement_budget).await?;
        sqlx::query_scalar::<_, bool>(WINDOW_CUT_ORDER_SQL)
            .bind(since)
            .bind(cut)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| e.to_string())?
    };
    if disordered {
        return Err("audit insertion instants do not follow audit order at the window cut".into());
    }
    Ok(cut)
}

/// Group the history's rows by their original transaction and read each
/// group's roster in batches of `batch` ids (Q5).
async fn original_groups(
    conn: &mut PgConnection,
    rows: Vec<OriginalEntry>,
    cutoff: i64,
    batch: usize,
    statement_budget: Option<StatementControl<'_>>,
) -> Result<Vec<ReplayGroup>, String> {
    let mut keyed = BTreeMap::<String, (Option<GroupReference>, Vec<HistoricalEvent>)>::new();
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
        let entry = keyed
            .entry(key)
            .or_insert_with(|| (reference.clone(), Vec::new()));
        if entry.0 != reference {
            return Err(
                "original sibling reference disagrees with its verified audit-anchored group"
                    .into(),
            );
        }
        entry.1.push(HistoricalEvent {
            seq: row.row.seq,
            created_at: Some(row.row.created_at),
            event: boss_core::event::Event {
                id: row.row.event_id,
                timestamp: row.row.timestamp,
                source: row.row.source,
                kind: row.row.kind,
                payload: row.row.payload,
            },
        });
    }
    let references: Vec<GroupReference> = keyed.values().filter_map(|(r, _)| r.clone()).collect();
    let mut read = boss_events::original_groups::read_groups_on_with_budget(
        conn,
        &references,
        cutoff,
        4096,
        batch,
        statement_budget,
    )
    .await?
    .into_iter();
    let mut groups = Vec::with_capacity(keyed.len());
    for (reference, rows) in keyed.into_values() {
        let original = match reference {
            Some(_) => Some(read.next().ok_or("original group read is short")?),
            None => None,
        };
        groups.push(ReplayGroup { original, rows });
    }
    groups.sort_by_key(|g| g.rows.iter().map(|r| r.seq).max());
    Ok(groups)
}

struct Cut {
    seq: i64,
    since: DateTime<Utc>,
    cutoff: i64,
}

/// What the starting state may still spend. An identity census moves no
/// payload, so it is bounded like the authorized census, by `limit`;
/// snapshot rows share the row and byte bounds with the window's history.
struct Budget {
    census: i64,
    rows: i64,
    bytes: i64,
}

/// One current packet's standing at the cut, read from its originals.
#[derive(sqlx::FromRow)]
struct Census {
    id: String,
    /// The packet's latest original row at the snapshot's cutoff.
    last_seq: Option<i64>,
    last_at: Option<DateTime<Utc>>,
    /// Its latest Job snapshot at or below the window cut.
    job_seq: Option<i64>,
    job_status: Option<String>,
    job_closed_on: Option<String>,
    /// What a station's Job-level clauses read off that snapshot, for a
    /// terminal one only (backlog bbeff5cb): its kind, its tags, and its
    /// metadata cut to the keys a retaining station names.
    job_kind: Option<String>,
    job_tags: Option<serde_json::Value>,
    job_metadata: Option<serde_json::Value>,
}

const CENSUS_SQL: &str = concat!(
    "SELECT j.id::text AS id,last.id AS last_seq,last.created_at AS last_at,\
    snap.id AS job_seq,snap.payload->>'status' AS job_status,snap.payload->>'closed_on' AS job_closed_on,\
    CASE WHEN snap.payload->>'status' IN('closed','cancelled') THEN snap.payload->>'kind' END AS job_kind,\
    CASE WHEN snap.payload->>'status' IN('closed','cancelled') THEN snap.payload->'tags' END AS job_tags,\
    CASE WHEN snap.payload->>'status' IN('closed','cancelled') THEN snap.payload->'metadata' END AS job_metadata \
    FROM jobs j \
    LEFT JOIN LATERAL (SELECT a.id,a.created_at FROM audit_log a \
        WHERE original_packet_identity(a.kind,a.payload)=j.id::text AND a.id<=$2 \
        ORDER BY a.id DESC LIMIT 1) last ON TRUE \
    LEFT JOIN LATERAL (SELECT a.id,",
    boss_events::projected_payload!("$5", "$6"),
    " AS payload FROM audit_log a \
        WHERE original_packet_identity(a.kind,a.payload)=j.id::text AND a.id<=$3 \
        AND a.kind IN('jobs.job.created','jobs.job.updated') ORDER BY a.id DESC LIMIT 1) snap ON TRUE \
    WHERE ($1::text[] IS NULL OR j.id::text=ANY($1)) ORDER BY j.id LIMIT $4"
);

// The starting state, spelled once for its bound check and its read: per
// carried packet the latest row of each Job/Step entity at or below the
// cut (through audit_original_packet_identity, never a prefix scan; Q3),
// every claim receipt (a claim's Step is inside its receipt), manifest
// claims whose packet only their chunks name, and those chunks. Rows of
// a group the replay applies itself — one that straddles the cut, or a
// registry/clock group whose transaction also wrote packet rows — are
// left out: that group is replayed whole (Q2).
macro_rules! seed_ctes {
    () => {
        "WITH packet_rows AS MATERIALIZED (SELECT DISTINCT ON (pid,entity) id FROM (\
            SELECT a.id,original_packet_identity(a.kind,a.payload) AS pid,\
            CASE WHEN a.kind IN('jobs.job.created','jobs.job.updated') THEN 'job' \
                 WHEN a.kind IN('jobs.step.created','jobs.step.updated') THEN 'step:'||(a.payload->>'id') \
                 WHEN a.kind='jobs.step.first_recorded' THEN 'step:'||(a.payload#>>'{step,id}') \
                 ELSE 'event:'||a.event_id::text END AS entity \
            FROM audit_log a WHERE original_packet_identity(a.kind,a.payload)=ANY($1) AND a.id<=$2 \
            AND a.kind=ANY($3) AND (a.original_group IS NULL OR NOT (a.original_group->>'id'=ANY($4)))\
         ) candidates ORDER BY pid,entity,id DESC), \
         manifest_claims AS MATERIALIZED (SELECT id,event_id FROM audit_log \
            WHERE kind='jobs.station.claimed' AND id<=$2 AND original_packet_identity(kind,payload) IS NULL \
            AND (original_group IS NULL OR NOT (original_group->>'id'=ANY($4))) \
            AND original_claim_packet_identity(payload,event_id,$2)=ANY($1)), \
         claims AS (SELECT event_id FROM audit_log WHERE id IN(SELECT id FROM packet_rows) \
            AND kind='jobs.station.claimed' UNION SELECT event_id FROM manifest_claims), \
         seed AS (SELECT id FROM packet_rows UNION SELECT id FROM manifest_claims \
            UNION SELECT id FROM audit_log WHERE kind='jobs.station.selection_evidence' AND id<=$2 \
            AND payload->>'claim_event_id' IN(SELECT event_id::text FROM claims))"
    };
}

const SEED_CTES: &str = seed_ctes!();

// Size and retain the same PostgreSQL text in one pass. Parsing JSON first
// would lose PostgreSQL's spaces and undercount the transported payload.
#[derive(sqlx::FromRow)]
struct SeedEntry {
    seq: i64,
    event_id: uuid::Uuid,
    timestamp: DateTime<Utc>,
    created_at: DateTime<Utc>,
    source: String,
    kind: String,
    payload: Option<String>,
}

// Materialization evaluates each accepted projection once. The cumulative
// PostgreSQL text size guards the wire: an over-budget row carries only a
// NULL failure marker, never its oversized payload.
const SEED_SQL: &str = concat!(
    seed_ctes!(),
    ", projected_seed AS MATERIALIZED (SELECT id AS seq,event_id,timestamp,created_at,source,kind,(",
    boss_events::projected_payload!("$6", "$7"),
    ")::text AS payload \
     FROM audit_log WHERE id IN(SELECT id FROM seed) ORDER BY id LIMIT $5), \
     sized_seed AS (SELECT *,SUM(octet_length(payload)) OVER (ORDER BY seq) AS bytes FROM projected_seed) \
     SELECT seq,event_id,timestamp,created_at,source,kind, \
     CASE WHEN bytes <= $8 THEN payload ELSE NULL END AS payload FROM sized_seed ORDER BY seq"
);

async fn starting_state(
    conn: &mut PgConnection,
    cut: Cut,
    authorized: Option<&[String]>,
    groups: &[ReplayGroup],
    clock: &ClockTimeline,
    budget: Budget,
    metadata: MetadataKeys<'_>,
    retention: &[Retention],
    actor: Option<&str>,
    scope: &JobScope,
    until: DateTime<Utc>,
    statement_budget: Option<StatementControl<'_>>,
    mut observation: Option<&mut crate::membership_observation::Observation>,
) -> Result<InitialState, String> {
    // Every row the replay applies itself: the window's rows, the whole
    // of any group that straddles the cut (Q2), and registry or clock
    // groups whose own transaction also wrote packet rows.
    let replayed: Vec<&HistoricalEvent> = groups.iter().flat_map(|g| &g.rows).collect();
    let touched: BTreeSet<String> = replayed
        .iter()
        .filter_map(|row| crate::membership_replay::packet_identity(row))
        .collect();
    let created_in_window: BTreeSet<String> = replayed
        .iter()
        .filter(|row| row.event.kind == e::JOB_CREATED)
        .filter_map(|row| crate::membership_replay::packet_identity(row))
        .collect();
    // A replayed group's rows at or below the cut are the group's, not
    // the starting state's: the replay applies that group whole, so the
    // same row seeded as well would be applied twice (Q2).
    let replayed_below: Vec<&ReplayGroup> = groups
        .iter()
        .filter(|g| g.original.is_some() && g.rows.iter().any(|r| r.seq <= cut.seq))
        .collect();
    let excluded_groups: Vec<String> = replayed_below
        .iter()
        .filter_map(|g| g.original.as_ref().map(|o| o.reference.id.to_string()))
        .collect();
    // The census carries only the metadata a retaining station names:
    // the clauses `could_hold` evaluates, and nothing else of a packet.
    let retained_keys: Vec<String> = retention
        .iter()
        .filter_map(|r| r.predicate.as_ref())
        .flat_map(StationPredicate::metadata_keys)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let census_started = std::time::Instant::now();
    refresh(conn, statement_budget).await?;
    let census: Vec<Census> = sqlx::query_as(CENSUS_SQL)
        .bind(authorized)
        .bind(cut.cutoff)
        .bind(cut.seq)
        .bind(budget.census + 1)
        .bind(metadata.kinds)
        .bind(&retained_keys)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    if census.len() as i64 > budget.census {
        return Err("current packet census exceeds bound".into());
    }
    if let Some(report) = observation.as_deref_mut() {
        report.phases.push(observation::timed(
            "census",
            census_started,
            Some(census.len() as u64),
        ));
    }
    let lowest = lowest_window_date(clock);
    let mut carried = Vec::new();
    let mut unknown = BTreeMap::new();
    let mut pruned_terminal = 0u64;
    let mut pruned_packets = Vec::new();
    // Only an uncertain partition can consume this witness. With a
    // known clock, keep pruning's original zero retained-row/byte cost.
    let needs_pruned_census = clock
        .unknown
        .iter()
        .any(|gap| gap.since < until && cut.since < gap.until);
    for packet in census {
        if packet.job_seq.is_none() {
            if !created_in_window.contains(&packet.id) {
                unknown.insert(
                    packet.id,
                    "no original Job snapshot before the window cut and no creation in the window"
                        .to_string(),
                );
            }
            continue;
        }
        if prunable(&packet, &cut, retention, lowest, &touched) {
            pruned_terminal += 1;
            if needs_pruned_census {
                if pruned_terminal > budget.rows as u64 {
                    return Err("native membership pruned census exceeds row bound".into());
                }
                pruned_packets.push(crate::membership_replay::pruned::PrunedPacket {
                    id: packet.id,
                    job_seq: packet.job_seq,
                    last_seq: packet.last_seq,
                    last_at: packet.last_at,
                    status: packet.job_status,
                    closed_on: packet.job_closed_on,
                    kind: packet.job_kind,
                    tags: packet.job_tags,
                    metadata: packet.job_metadata,
                });
            }
            continue;
        }
        carried.push(packet.id);
    }
    let pruned_census = if pruned_packets.is_empty() {
        None
    } else {
        use crate::membership_replay::{
            projection::{ProjectionBinding, serialized_len},
            pruned::PrunedCensus,
        };
        let used = serialized_len(&pruned_packets).map_err(|e| e.to_string())?;
        // Count the authorization binding before copying it; original
        // census payloads above were moved, never cloned before measuring.
        let binding_bytes =
            serialized_len(&(actor, scope, authorized)).map_err(|e| e.to_string())?;
        if used
            .checked_add(binding_bytes)
            .is_none_or(|n| n as i64 > budget.bytes)
        {
            return Err("native membership pruned census exceeds byte bound".into());
        }
        Some(PrunedCensus {
            schema: 1,
            cut: cut.seq,
            audit_cutoff: cut.cutoff,
            since: cut.since,
            until,
            binding: ProjectionBinding {
                actor: actor.map(str::to_owned),
                scope: scope.clone(),
                currently_authorized: authorized.map(|ids| ids.iter().cloned().collect()),
            },
            metadata_keys: retained_keys,
            packets: pruned_packets,
        })
    };
    let witness_bytes = pruned_census
        .as_ref()
        .map(crate::membership_replay::projection::serialized_len)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(0) as i64;
    let witness_rows = pruned_census.as_ref().map_or(0, |w| w.packets.len()) as i64;
    if witness_bytes > budget.bytes || witness_rows > budget.rows {
        return Err("native membership pruned census exceeds shared row/byte bound".into());
    }
    let seed_rows = budget.rows - witness_rows;
    let seed_bytes = budget.bytes - witness_bytes;
    let mut rows = Vec::new();
    if let Some(report) = observation.as_deref_mut() {
        report.audit_cutoff = Some(cut.cutoff);
        report.window_cut = Some(cut.seq);
        report.carried_packets = carried.len();
        report.excluded_groups = excluded_groups.len();
        observation::plans(
            conn,
            &carried,
            &excluded_groups,
            &cut,
            &budget,
            metadata,
            statement_budget,
            report,
        )
        .await?;
    }
    let stream_started = std::time::Instant::now();
    if !carried.is_empty() {
        refresh(conn, statement_budget).await?;
        let mut entries = sqlx::query_as::<_, SeedEntry>(SEED_SQL)
            .bind(&carried)
            .bind(cut.seq)
            .bind(SNAPSHOT_KINDS)
            .bind(&excluded_groups)
            .bind(seed_rows + 1)
            .bind(metadata.kinds)
            .bind(metadata.keys)
            .bind(seed_bytes)
            .fetch(&mut *conn);
        let mut bytes = 0_i64;
        while let Some(row) = entries.try_next().await.map_err(|e| e.to_string())? {
            if rows.len() as i64 >= seed_rows {
                return Err("native membership starting state exceeds row bound".into());
            }
            let payload = row
                .payload
                .ok_or_else(|| "native membership starting state exceeds byte bound".to_owned())?;
            let row_bytes = i64::try_from(payload.len())
                .map_err(|_| "native membership starting state exceeds byte bound".to_owned())?;
            bytes = bytes
                .checked_add(row_bytes)
                .filter(|total| *total <= seed_bytes)
                .ok_or_else(|| "native membership starting state exceeds byte bound".to_owned())?;
            rows.push(HistoricalEvent {
                seq: row.seq,
                created_at: Some(row.created_at),
                event: boss_core::event::Event {
                    id: row.event_id,
                    timestamp: row.timestamp,
                    source: row.source,
                    kind: row.kind,
                    payload: serde_json::from_str(&payload).map_err(|e| e.to_string())?,
                },
            });
        }
    }
    if let Some(report) = observation {
        report.phases.push(observation::timed(
            "stream_decode",
            stream_started,
            Some(rows.len() as u64),
        ));
    }
    // The replay seeds a snapshot before a replayed group whose first
    // row follows it, and after one wholly below the cut that ends
    // before it; a group reaching past the cut follows every snapshot.
    // A snapshot of the same packet falling inside a group's span has no
    // established order against that group's older row, so the packet
    // is UNKNOWN rather than folded in a guessed order.
    let mut spans = BTreeMap::<String, Vec<(i64, i64)>>::new();
    for group in &replayed_below {
        let first = group.rows.iter().map(|r| r.seq).min().unwrap_or(i64::MAX);
        let last = group.rows.iter().map(|r| r.seq).max().unwrap_or(i64::MAX);
        let upper = if last > cut.seq { i64::MAX } else { last };
        for row in group.rows.iter().filter(|r| r.seq <= cut.seq) {
            if let Some(id) = crate::membership_replay::packet_identity(row) {
                spans.entry(id).or_default().push((first, upper));
            }
        }
    }
    for row in &rows {
        if let Some(id) = crate::membership_replay::packet_identity(row)
            && spans
                .get(&id)
                .is_some_and(|s| s.iter().any(|(lo, hi)| *lo < row.seq && row.seq < *hi))
        {
            unknown.insert(
                id,
                "an original group replayed across the window cut interleaves with a snapshot of its packet"
                    .to_string(),
            );
        }
    }
    // Q3 for Steps: a current Step of a packet the replay will hold must
    // be witnessed by a snapshot at the cut or a creation in the window.
    let held: Vec<String> = carried
        .iter()
        .cloned()
        .chain(created_in_window.iter().cloned())
        .collect();
    if !held.is_empty() {
        refresh(conn, statement_budget).await?;
        let current: Vec<(String, String)> = sqlx::query_as(
            "SELECT s.id::text,s.job_id::text FROM steps s \
             WHERE s.job_id=ANY($1::text[]::uuid[]) LIMIT $2",
        )
        .bind(&held)
        .bind(budget.census + 1)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
        if current.len() as i64 > budget.census {
            return Err("current Step census exceeds bound".into());
        }
        let witnessed: BTreeSet<String> = rows
            .iter()
            .chain(replayed.iter().copied())
            .filter_map(step_witness)
            .collect();
        for (step, job) in current {
            if !witnessed.contains(&step) {
                unknown.entry(job).or_insert_with(|| {
                    "a current Step has no original snapshot before the window cut and no creation in the window"
                        .to_string()
                });
            }
        }
    }
    Ok(InitialState {
        cut: cut.seq,
        rows,
        unknown,
        pruned_terminal,
        pruned_census,
    })
}

/// The lowest business date any KNOWN instant of the window maps to. A
/// mapping only moves forward within one known span, so each span's
/// lowest date is at its start; both ends are read so a span that runs
/// backward cannot hide one. None when no instant of the window is known.
fn lowest_window_date(clock: &ClockTimeline) -> Option<NaiveDate> {
    clock
        .known
        .iter()
        .flat_map(|span| {
            let last = span.until - chrono::Duration::microseconds(1);
            [clock.date_at(span.since).ok(), clock.date_at(last).ok()]
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min()
}

/// Q1, conservatively and per station: terminal at its latest snapshot,
/// nothing of it after the cut, and for EVERY station that can retain a
/// terminal packet, either past that station's window or failing its
/// Job-level clauses — so no station can count it at any known instant
/// of the window. A frozen packet's kind, status, tags and metadata
/// cannot change inside the window, so a clause it fails at the cut it
/// fails throughout (backlog bbeff5cb: one global window carried 38,811
/// packets closed within my-watchlist's 14 days, one of which that
/// station could count). An unknown clock span is left to the replay
/// (`InitialState::pruned_terminal`).
fn prunable(
    packet: &Census,
    cut: &Cut,
    retention: &[Retention],
    lowest: Option<NaiveDate>,
    touched: &BTreeSet<String>,
) -> bool {
    let terminal = matches!(
        packet.job_status.as_deref(),
        Some("closed") | Some("cancelled")
    );
    let quiet = packet.last_seq.is_some_and(|seq| seq <= cut.seq) && !touched.contains(&packet.id);
    terminal
        && quiet
        && retention
            .iter()
            .all(|r| past_window(packet, cut, r.days, lowest) || !could_hold(packet, r))
}

/// The snapshot older than `days` in wall time, AND past that window on
/// every known business date of the window, so `in_window` cannot hold
/// it at any known instant. The business half is what a closed packet's
/// membership is decided by; the wall half is the decision's own words.
fn past_window(packet: &Census, cut: &Cut, days: u64, lowest: Option<NaiveDate>) -> bool {
    let Some(wall_bound) = i64::try_from(days)
        .ok()
        .and_then(chrono::Duration::try_days)
        .and_then(|days| cut.since.checked_sub_signed(days))
    else {
        return false;
    };
    let old = packet.last_at.is_some_and(|at| at < wall_bound);
    let expired = match packet.job_closed_on.as_deref() {
        // Terminal with no close date: `in_window` never holds it and
        // no expiry can fire for it.
        None => true,
        Some(text) => text
            .parse::<NaiveDate>()
            .ok()
            .and_then(|closed| closed.checked_add_days(chrono::Days::new(days.checked_add(1)?)))
            .zip(lowest)
            .is_some_and(|(target, lowest)| lowest >= target),
    };
    old && expired
}

/// Whether the station could count this packet, judged on its snapshot
/// by every clause but the step clause (`job_fields_match`, the match's
/// own clauses). A step clause is never judged here: a packet that
/// meets every Job clause is held whatever its steps. A snapshot whose
/// fields cannot be read is held, as is any packet under a predicate
/// that cannot be read.
fn could_hold(packet: &Census, retention: &Retention) -> bool {
    let Some(predicate) = &retention.predicate else {
        return true;
    };
    let status = packet
        .job_status
        .clone()
        .and_then(|s| serde_json::from_value::<JobStatus>(serde_json::Value::String(s)).ok());
    let tags = packet
        .job_tags
        .clone()
        .and_then(|t| serde_json::from_value::<Vec<String>>(t).ok());
    let (Some(kind), Some(status), Some(tags)) = (packet.job_kind.as_deref(), status, tags) else {
        return true;
    };
    let metadata = packet
        .job_metadata
        .clone()
        .unwrap_or(serde_json::Value::Null);
    predicate.job_fields_match(kind, &status, &tags, &metadata)
}

/// The Step id a row witnesses the existence of, for the Step census.
fn step_witness(row: &HistoricalEvent) -> Option<String> {
    let payload = &row.event.payload;
    match row.event.kind.as_str() {
        e::STEP_CREATED | e::STEP_UPDATED => payload.get("id")?.as_str().map(str::to_owned),
        e::STEP_FIRST_RECORDED => payload.pointer("/step/id")?.as_str().map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
