//! Incremental owning-service adapter. It reads immutable audit prefixes,
//! never current workflow/job rows and never a second claim authority.
use super::*;

pub(super) fn selected_kinds() -> Vec<&'static str> {
    crate::flow_resources::FACT_KINDS
        .iter()
        .copied()
        .chain([
            crate::events::JOB_CREATED,
            crate::events::JOB_UPDATED,
            crate::events::STEP_CREATED,
            crate::events::STEP_UPDATED,
            crate::events::STATION_CLAIMED,
            crate::events::STATION_SELECTION_EVIDENCE,
        ])
        .collect()
}

/// The top-level payload keys a selected fact keeps on its way into the
/// projection; every other key is cut in SQL before the fact is read.
/// A `jobs.pool.published` payload is the whole serialized `PoolSpec`,
/// and the console serves that declaration back as the pool, so a pool
/// field missing here is silently absent on `/api/claims`: `station`
/// was, and `disk:ci-workspaces` v3 read "not yet bound" on the floor
/// while its published fact named forge-ci (backlog 6a146fbf,
/// 2026-11-06). The pin below holds every `PoolSpec` key to this list.
pub(super) const PAYLOAD_KEYS: &[&str] = &[
    "id",
    "step_id",
    "job_id",
    "status",
    "assignee_id",
    "metadata",
    "name",
    "version",
    "unit",
    "kind",
    "capacity",
    "reclaimer",
    "department",
    "created_at",
    // The rest of a pool's declaration (backlog 6a146fbf). No other
    // selected kind carries these at the top level: a station claim's
    // station rides inside `receipt_json`, a lease's inside `obligation`.
    "station",
    "reclaimer_kind",
    // The models a robot pool meters (backlog af5dd547).
    "models",
    "roster",
    "program",
    "return_verifier",
    "identity_pin_unenforced",
    // A pool's shape — capacity, stock or rate (T5 of design 4b27a65a,
    // backlog 3f58c823).
    "shape",
    // Where a pool's capacity number comes from (T5, backlog 84f570a1).
    "capacity_from",
    // The station that keeps a pool (backlog 42cd9a83).
    "keeper",
    "claim",
    "step",
    "reason",
    "cause_event_id",
    "receipt_json",
    "binding",
    "stock_obligations",
    "obligation",
    "evidence",
    "observed_at",
    "_actor",
    "receipt",
    "receipt_manifest",
    "claim_event_id",
    "request_id",
    "index",
    "text",
    "opened_at",
    // The station the per-step claim door records on its own fact
    // (backlog 2682c418). Absent here, the fold never saw it and every
    // robot claim read `not-recorded` in production while every
    // in-memory test passed (971a6190).
    crate::events::CLAIM_STATION_KEY,
    crate::events::CLAIM_STATION_ID_KEY,
    crate::events::STATION_RESOLUTION_KEY,
    crate::events::STATION_RESOLUTION_REASON_KEY,
    // The executor kind the same door records beside it (backlog
    // 3ff2ef60). Absent here until station tree T1 (050284c2), so every
    // claim read `derived-from-id` on the console though its fact said
    // which kind it was.
    crate::events::EXECUTOR_KIND_KEY,
];

pub async fn run(
    history: std::sync::Arc<dyn ClaimsHistory>,
    pool: sqlx::PgPool,
    clock: std::sync::Arc<dyn boss_clock_client::ClockClient>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *cancel.borrow() {
            break;
        }
        let immediate = match advance_native(
            history.as_ref(),
            &pool,
            boss_events::projection_tail::MAX_FACTS,
            clock.as_ref(),
        )
        .await
        {
            Ok((through, head)) => {
                match maintain(history.as_ref(), clock.as_ref(), Some(&cancel)).await {
                    Ok(true) => through != head,
                    Ok(false) => break,
                    Err(error) => {
                        tracing::warn!(%error,"claims history retention pass refused");
                        false
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error,"claims history source prefix unavailable");
                false
            }
        };
        if immediate {
            tokio::task::yield_now().await;
            continue;
        }
        tokio::select! {
            changed=cancel.changed()=>{if changed.is_err() || *cancel.borrow() {break}},
            _=tokio::time::sleep(std::time::Duration::from_secs(5))=>{},
        }
    }
}

// Catch-up and rebuild share the SAME maintenance boundary. Expired derived
// prefixes cannot accumulate until the end of a long initial replay. Every
// pass keeps its own1024-row/2s bound; shutdown is checked between passes.
async fn maintain(
    history: &dyn ClaimsHistory,
    clock: &dyn boss_clock_client::ClockClient,
    cancel: Option<&tokio::sync::watch::Receiver<bool>>,
) -> Result<bool, ClaimsError> {
    loop {
        if cancel.is_some_and(|c| *c.borrow()) {
            return Ok(false);
        }
        let cursor = history.cursor().await?;
        if history.prune(cursor, clock.now().await.now, 1024).await? < 1024 {
            return Ok(true);
        }
        tokio::task::yield_now().await;
    }
}

pub async fn rebuild_native(pool: &sqlx::PgPool) -> Result<(), ClaimsError> {
    rebuild(&PgClaimsHistory::new(pool.clone()), pool).await
}

/// [`rebuild_native`] over a history the fixture built
/// ([`PgClaimsHistory::for_fixture`]), so the rebuild spends the fixture's
/// statement budget rather than a second adapter's production one
/// (backlog 21217d8a).
#[cfg(feature = "test-support")]
pub async fn rebuild_native_with(
    history: &PgClaimsHistory,
    pool: &sqlx::PgPool,
) -> Result<(), ClaimsError> {
    rebuild(history, pool).await
}

async fn rebuild(history: &PgClaimsHistory, pool: &sqlx::PgPool) -> Result<(), ClaimsError> {
    let clock = boss_clock_client::WallClockClient;
    let mut lock = pool
        .begin()
        .await
        .map_err(|e| ClaimsError::Unavailable(e.to_string()))?;
    let result = async {
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *lock)
            .await
            .map_err(|e| ClaimsError::Unavailable(e.to_string()))?;
        // Serializes only two rebuilders of this new derived projection.
        // Producers and ordinary claims do not take this key.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(boss_core::rebuild::lock_key("claims-console"))
            .execute(&mut *lock)
            .await
            .map_err(|e| ClaimsError::Unavailable(e.to_string()))?;
        let cursor = history.cursor().await?;
        history.begin_rebuild(cursor).await?;
        let mut target = None;
        loop {
            let (through, head) = advance_native(
                history,
                pool,
                boss_events::projection_tail::MAX_FACTS,
                &clock,
            )
            .await?;
            maintain(history, &clock, None).await?;
            let target = *target.get_or_insert(head);
            if through >= target {
                break;
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => lock.commit().await.map_err(|e| {
            ClaimsError::Unavailable(format!(
                "rebuild completion acknowledgement unavailable: {e}"
            ))
        }),
        Err(error) => match lock.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(ClaimsError::Unavailable(format!(
                "{error}; rebuild-lock rollback acknowledgement: {rollback}"
            ))),
        },
    }
}

pub async fn advance_native_once(
    history: &dyn ClaimsHistory,
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<bool, ClaimsError> {
    let (through, head) =
        advance_native(history, pool, limit, &boss_clock_client::WallClockClient).await?;
    Ok(through == head)
}
async fn advance_native(
    history: &dyn ClaimsHistory,
    pool: &sqlx::PgPool,
    limit: i64,
    clock: &dyn boss_clock_client::ClockClient,
) -> Result<(i64, i64), ClaimsError> {
    let expected = history.cursor().await?;
    let tail = boss_events::projection_tail::read(
        pool,
        expected.through,
        limit,
        boss_events::projection_tail::Selection {
            source: "jobs",
            kinds: &selected_kinds(),
            payload_keys: PAYLOAD_KEYS,
            metadata_keys: &["waiting_on", "resource_claim", crate::agent_runs::EDGE_KEY],
            witness_keys: &["claim", "resource_claim", "reclaimer"],
        },
        boss_events::statement_budget::StatementBudget::until(history.source_deadline()),
    )
    .await;
    let tail = match tail {
        Ok(tail) => tail,
        Err(error) => {
            let acknowledgement = history.source_failed(expected, clock.now().await.now).await;
            return Err(ClaimsError::Unavailable(format!(
                "audit prefix unavailable: {error}; failure observation acknowledgement: {acknowledgement:?}"
            )));
        }
    };
    let progress = (tail.through, tail.observed_head);
    let result = async {
        let observation = SourceObservation {
            audit_head: tail.observed_head,
            observed_at: tail.observed_at,
        };
        // Exact producer artifacts, not current receipt rows. The tail's existing
        // byte/row bounds and complete original-group boundary still apply.
        let artifacts = tail
            .facts
            .iter()
            .filter(|fact| fact.source == "jobs")
            .map(|fact| {
                Ok(boss_events::replay::ReplayEvent {
                    raw_payload: Some(fact.payload.clone()),
                    audit_id: fact.seq,
                    event_id: fact.event_id,
                    kind: fact.kind.clone(),
                    ts: fact.event_at,
                    payload: serde_json::from_str(&fact.payload).map_err(|e| {
                        ClaimsError::Unavailable(format!("station artifact JSON: {e}"))
                    })?,
                })
            })
            .collect::<Result<Vec<_>, ClaimsError>>()?;
        let artifacts = crate::selection_evidence::reconstruct(&artifacts)
            .map_err(|e| ClaimsError::Unavailable(format!("station artifact: {e}")))?;
        for event in artifacts
            .iter()
            .filter(|event| event.kind == crate::events::STATION_CLAIMED)
        {
            let receipt = event.payload["receipt_json"].as_str().ok_or_else(|| {
                ClaimsError::Unavailable("station artifact lacks exact receipt JSON".into())
            })?;
            crate::state_receipt::station_claim(event, receipt)
                .map_err(|e| ClaimsError::Unavailable(format!("station original receipt: {e}")))?;
        }
        let receipts = artifacts
            .into_iter()
            .filter(|event| event.kind == crate::events::STATION_CLAIMED)
            .map(|event| (event.event_id, event.payload))
            .collect::<std::collections::HashMap<_, _>>();
        let batch = HistoryBatch {
            through: tail.through,
            through_at: tail.through_at,
            facts: tail
                .facts
                .into_iter()
                .map(|e| {
                    Ok(SequencedFact {
                        seq: e.seq,
                        recorded_at: e.recorded_at,
                        event_id: e.event_id,
                        event_at: e.event_at,
                        source: e.source,
                        kind: e.kind,
                        payload: match receipts.get(&e.event_id) {
                            Some(payload) => serde_json::to_string(payload).map_err(|e| {
                                ClaimsError::Unavailable(format!("station artifact encoding: {e}"))
                            })?,
                            None => e.payload,
                        },
                        original_group: e.original_group,
                    })
                })
                .collect::<Result<Vec<_>, ClaimsError>>()?,
        };
        history.advance(expected, &batch, observation).await?;
        Ok::<(), ClaimsError>(())
    }
    .await;
    match result {
        Ok(()) => Ok(progress),
        Err(error) => match history.source_failed(expected, clock.now().await.now).await {
            Ok(()) => Err(error),
            Err(acknowledgement) => Err(ClaimsError::Unavailable(format!(
                "{error}; failure observation acknowledgement: {acknowledgement}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::PAYLOAD_KEYS;
    use crate::pools::{PoolCapacity, PoolKind, PoolSpec, RosterDeclaration};

    /// Backlog 6a146fbf: every key a published pool can carry survives the
    /// selection, so the console serves the declaration that was published.
    /// The literal names every field, so a new `PoolSpec` field does not
    /// compile here until its author has answered for its key.
    #[test]
    fn every_key_a_published_pool_carries_survives_the_projection() {
        let spec = PoolSpec {
            name: "disk:ci-workspaces".into(),
            version: 4,
            status: crate::registry::WorkflowStatus::Active,
            unit: "GiB".into(),
            kind: PoolKind::Stock,
            shape: Some(crate::pools::PoolShape::Stock),
            capacity: PoolCapacity::Unknown {},
            roster: Some(RosterDeclaration {
                eligibility: vec![],
                basis: None,
                unknown_reason: Some("unread".into()),
            }),
            reclaimer: Some("infra/forge/reap-dead-ci-jobs.sh".into()),
            reclaimer_kind: Some("maintenance-reap-ci-jobs".into()),
            program: Some("infra/forge/program.sh".into()),
            return_verifier: Some("infra/forge/verify.sh".into()),
            identity_pin_unenforced: Some("pin".into()),
            department: Some("it".into()),
            station: Some("forge-ci".into()),
            keeper: Some("operations".into()),
            models: vec!["opus-5-5".into()],
            capacity_from: Some("the agents registry".into()),
            created_at: chrono::Utc::now(),
        };
        let wire = serde_json::to_value(&spec).expect("a pool serializes");
        let dropped: Vec<&String> = wire
            .as_object()
            .expect("a pool is an object")
            .keys()
            .filter(|key| !PAYLOAD_KEYS.contains(&key.as_str()))
            .collect();
        assert!(
            dropped.is_empty(),
            "the claims projection drops these published pool keys: {dropped:?}"
        );
    }

    /// Station tree T1 (backlog 050284c2): THE CLAIM RECORDS THE KIND, and
    /// the console reads what it recorded. The per-step claim door (which
    /// claim-next also goes through) writes `_executor_kind` and `_station`
    /// on its own `jobs.step.updated` fact, and this list cuts every other
    /// key in SQL before the fold sees it. `_station` was cut until
    /// 971a6190. `_executor_kind` was still cut on 2026-11-06: claim
    /// 67b5cd9b's fact held `"_executor_kind":"robot"`, and `/api/claims`
    /// read it `derived-from-id`, as it read 93 of 95 open claims that
    /// evening. Every key the door adds to its fact is held here.
    #[test]
    fn every_key_the_claim_door_adds_to_its_fact_survives_the_projection() {
        let dropped: Vec<&str> = [
            crate::events::EXECUTOR_KIND_KEY,
            crate::events::CLAIM_STATION_KEY,
            crate::events::CLAIM_STATION_ID_KEY,
            crate::events::STATION_RESOLUTION_KEY,
            crate::events::STATION_RESOLUTION_REASON_KEY,
        ]
        .into_iter()
        .filter(|key| !PAYLOAD_KEYS.contains(key))
        .collect();
        assert!(
            dropped.is_empty(),
            "the claims projection cuts these keys the claim door records: {dropped:?}"
        );
    }
}
