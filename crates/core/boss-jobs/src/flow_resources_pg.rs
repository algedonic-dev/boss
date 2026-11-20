//! Joined FLOW ledger operations. Caller owns the step lock and transaction.
use crate::{
    flow_resources::{self as flow, Amount, Capacity, Claim, Demand, Refusal},
    pools::{PoolCapacity, PoolKind, PoolSpec},
    port::JobsError,
    registry::WorkflowSpec,
};
use boss_core::{
    job::{Step, StepStatus},
    publisher::EventStamp,
};
use boss_events::statement_budget::{StatementBudget, refresh};
use serde_json::{Value, json};

fn storage(e: impl std::fmt::Display) -> JobsError {
    JobsError::Storage(e.to_string())
}

pub(crate) async fn finish_write(
    tx: sqlx::Transaction<'_, sqlx::Postgres>,
    outcome: Result<(), JobsError>,
    label: &str,
) -> Result<(), JobsError> {
    match outcome {
        Ok(()) => tx.commit().await.map_err(|error| {
            storage(format!(
                "{label} COMMIT acknowledgement failed; effect uncertain: {error}"
            ))
        }),
        Err(original) => match tx.rollback().await {
            Ok(()) => Err(original),
            Err(rollback) => Err(storage(format!(
                "{original}; {label} rollback acknowledgement failed: {rollback}"
            ))),
        },
    }
}

/// What every active claim and unreturned stock obligation holds of
/// `pool`, read in the caller's transaction.
async fn held_in(
    tx: &mut sqlx::PgConnection,
    pool: &str,
    budget: Option<StatementBudget>,
) -> Result<Amount, JobsError> {
    refresh(tx, budget).await.map_err(storage)?;
    let held:String=sqlx::query_scalar("SELECT COALESCE(SUM(amount),0)::text FROM (SELECT a.amount FROM step_resource_allocations a JOIN step_resource_claims c USING(step_id) WHERE a.pool=$1 AND c.active UNION ALL SELECT amount FROM resource_stock_obligations WHERE pool=$1 AND returned_by IS NULL) held")
        .bind(pool).fetch_one(&mut *tx).await.map_err(storage)?;
    Amount::parse(&held).map_err(storage)
}

pub(crate) async fn reserve_flow_vector_in_tx(
    tx: &mut sqlx::PgConnection,
    step: &Step,
    actor: &str,
    budget: Option<StatementBudget>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Claim>, JobsError> {
    refresh(tx, budget).await.map_err(storage)?;
    let protocol:Option<sqlx::types::Json<Box<serde_json::value::RawValue>>>=sqlx::query_scalar("SELECT to_jsonb(w) FROM workflows w JOIN jobs j ON j.kind=w.kind AND j.workflow_version=w.version WHERE j.id=$1")
        .bind(*step.job_id.inner().as_uuid()).fetch_optional(&mut *tx).await.map_err(storage)?;
    let Some(protocol) = protocol else {
        return Ok(None);
    };
    let workflow: WorkflowSpec =
        crate::resource_history::workflow(protocol.0.get()).map_err(storage)?;
    let Some(bill) = workflow
        .steps
        .iter()
        .find(|s| Some(s.title.as_str()) == step.spec_slug.as_deref())
        .and_then(|s| s.resources.as_ref())
    else {
        return Ok(None);
    };
    bill.validate().map_err(storage)?;
    let mut requested = bill.pools();
    requested.sort_by(|a, b| a.0.cmp(&b.0));
    if requested
        .iter()
        .all(|(_, amount)| amount.amount().is_ok_and(|a| a.is_zero()))
    {
        return Ok(None);
    }
    refresh(tx, budget).await.map_err(storage)?;
    let open: bool =
        sqlx::query_scalar("SELECT status NOT IN ('closed','cancelled') FROM jobs WHERE id=$1")
            .bind(*step.job_id.inner().as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
    if !open {
        return Err(JobsError::ResourceRefused(Refusal {
            pool: "acquisition".into(),
            reason: "packet-not-open".into(),
            observed: Vec::new(),
        }));
    }
    refresh(tx, budget).await.map_err(storage)?;
    let prior: Option<(i64, bool, Value)> =
        sqlx::query_as("SELECT generation,active,body FROM step_resource_claims WHERE step_id=$1")
            .bind(*step.id.inner().as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
    if let Some((_, true, body)) = &prior {
        let claim: Claim = serde_json::from_value(body.clone()).map_err(storage)?;
        claim.validate().map_err(storage)?;
        if claim.holder == actor
            && step.status == StepStatus::Active
            && step.metadata.get(flow::TOKEN_KEY) == Some(body)
        {
            if claim.lease_until.is_some_and(|until| until <= now) {
                return Err(crate::resource_leases::error("resource-lease-expired"));
            }
            return Ok(Some(claim));
        }
        return Err(JobsError::ResourceRefused(Refusal {
            pool: "acquisition".into(),
            reason: "active-generation-mismatch".into(),
            observed: Vec::new(),
        }));
    }
    if step.status == StepStatus::Active {
        return Err(JobsError::ResourceRefused(Refusal {
            pool: "acquisition".into(),
            reason: "active-step-untracked".into(),
            observed: Vec::new(),
        }));
    }
    // THE RUN'S OWN METADATA, READ ONCE (backlogs af5dd547, db88f6a9): a
    // from-run bill reads its `model` below, and a bill naming workerless
    // profiles reads its `profile` HERE, before the host is placed — a run
    // that binds no worker has no vCPU to place.
    let run: Option<Value> = if bill.takes_model_from_run() || !bill.workerless_profiles.is_empty()
    {
        refresh(tx, budget).await.map_err(storage)?;
        Some(
            sqlx::query_scalar("SELECT metadata FROM jobs WHERE id=$1")
                .bind(*step.job_id.inner().as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?,
        )
    } else {
        None
    };
    let bill = &flow::waive_workerless(bill, run.as_ref());
    // THE vCPU IS PLACED ON A HOST FIRST (backlog 5608c1a2): a from-run
    // bill billing vCPU reads its protocol owner's floating share of each
    // box from the station rows, and the held amount on each, and names
    // the box with the most room; the chosen pool is re-judged against
    // that share under its lineage lock below, so a racing claim that
    // filled it is refused there, on the pool, by name.
    let shares = match (
        flow::draws_a_host(bill),
        crate::station_tree::protocol_owner(&workflow),
    ) {
        (true, Some(owner)) => {
            refresh(tx, budget).await.map_err(storage)?;
            let rows = crate::stations::active_station_rows_in(&mut *tx)
                .await
                .map_err(storage)?;
            flow::floating_shares(&rows, owner).map_err(|why| {
                JobsError::ResourceRefused(Refusal {
                    pool: "host".into(),
                    reason: format!("host-share-unreadable: {why}"),
                    observed: Vec::new(),
                })
            })?
        }
        _ => Vec::new(),
    };
    let mut held_on_hosts = std::collections::BTreeMap::new();
    for (pool, _) in &shares {
        held_on_hosts.insert(pool.clone(), held_in(tx, pool, budget).await?);
    }
    let bill = &flow::resolve_host(bill, &shares, |pool| {
        held_on_hosts
            .get(pool)
            .cloned()
            .ok_or_else(|| format!("{pool} was not read"))
    })
    .map_err(JobsError::ResourceRefused)?;
    // THE CLASS IS THE CLAIMANT'S, ELSE READ OFF THE RUN (backlogs
    // 7750d37d, af5dd547): a from-run bill reads the claimant's agents
    // row, then this packet's `model` and the active robot pools that
    // list models; any other bill reads nothing more. The pools are read
    // before their lineage lock, so the one chosen is re-judged under it
    // below.
    let (bill, run_model) = if bill.takes_model_from_run() {
        refresh(tx, budget).await.map_err(storage)?;
        let rows: Vec<sqlx::types::Json<Box<serde_json::value::RawValue>>> = sqlx::query_scalar(
            "SELECT spec FROM resource_pools WHERE status='active' AND starts_with(name,$1)",
        )
        .bind(crate::resources::ROBOTS_PREFIX)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let pools = rows
            .iter()
            .map(|raw| crate::resource_history::pool(raw.0.get()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        // The claimant's recorded class decides before the run's model
        // (backlog 7750d37d): its agents row, by id or by an alias the
        // row carries, as the claim door reads it.
        refresh(tx, budget).await.map_err(storage)?;
        let class: Option<String> = sqlx::query_scalar(
            "SELECT model_class FROM agents WHERE id = $1 \
             OR id = (SELECT actor_id FROM actor_aliases WHERE alias = $1) LIMIT 1",
        )
        .bind(actor)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .flatten();
        flow::resolve_bill(bill, run.as_ref(), class.as_deref(), &pools)
            .map_err(JobsError::ResourceRefused)?
    } else {
        (bill.clone(), None)
    };
    let mut requested = bill.pools();
    requested.sort_by(|a, b| a.0.cmp(&b.0));
    let run_pool = run_model
        .as_ref()
        .and(bill.model.as_deref())
        .map(crate::resources::robot_pool);
    let mut demands = Vec::new();
    for (name, quantity) in requested {
        let amount = quantity.amount().map_err(storage)?;
        // Same lineage fence used by publication; a fresh statement after the
        // lock observes reservations committed by its previous holder.
        crate::declared_version::lock_and_read_newest_with_budget(
            tx,
            "resource_pools",
            "name",
            &name,
            budget,
        )
        .await
        .map_err(storage)?;
        refresh(tx, budget).await.map_err(storage)?;
        let spec: Option<sqlx::types::Json<Box<serde_json::value::RawValue>>> =
            sqlx::query_scalar("SELECT spec FROM resource_pools WHERE name=$1 AND status='active'")
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?;
        let spec: Option<PoolSpec> = spec
            .map(|raw| crate::resource_history::pool(raw.0.get()))
            .transpose()
            .map_err(storage)?;
        // The pool a from-run bill chose must still meter the run's model
        // once its lineage is locked: a publish that dropped the model in
        // between moved the class, and the claim says so rather than
        // holding a slot no declaration now names.
        if let (Some(pool), Some(model), Some(spec)) = (&run_pool, &run_model, &spec)
            && *pool == name
            && !spec.models.iter().any(|served| served == model)
        {
            return Err(JobsError::ResourceRefused(Refusal {
                pool: name,
                reason: "run-model-class-moved".into(),
                observed: Vec::new(),
            }));
        }
        let held = held_in(tx, &name, budget).await?;
        let share = flow::share_of(&bill, &name, &shares).map_err(JobsError::ResourceRefused)?;
        let (version, capacity, stock) = match spec {
            Some(spec) => (
                spec.version,
                match spec.capacity {
                    PoolCapacity::Unknown {} => Capacity::Unknown,
                    PoolCapacity::Configured { amount } => {
                        Capacity::Known(amount.amount().map_err(storage)?)
                    }
                },
                spec.kind == PoolKind::Stock,
            ),
            None => (0, Capacity::Unknown, false),
        };
        demands.push(Demand {
            pool: name,
            version,
            amount,
            capacity,
            held,
            stock,
            share,
            // A version-2 decision records no line (backlog 30c1efe8):
            // readers go first, so this writer still writes version 2.
            levels: Vec::new(),
        });
    }
    let allocations = flow::plan(&demands).map_err(JobsError::ResourceRefused)?;
    let generation = prior
        .map(|p| p.0)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| storage("resource generation exhausted"))?;
    let duration = flow::resolved_duration_ms(&workflow, step)?;
    let claim = Claim {
        version: flow::WRITTEN_CLAIM_VERSION,
        id: uuid::Uuid::new_v4(),
        step_id: step.id,
        job_id: step.job_id,
        generation,
        holder: actor.into(),
        workflow_kind: workflow.kind,
        workflow_version: workflow.version,
        bill,
        allocations,
        observed: demands,
        claimed_at: Some(now),
        resolved_duration_ms: Some(duration),
        lease_until: Some(flow::lease_until(now, duration)?),
    };
    claim.validate().map_err(storage)?;
    refresh(tx, budget).await.map_err(storage)?;
    sqlx::query("INSERT INTO step_resource_claims(step_id,job_id,generation,holder,active,body,lease_until) VALUES($1,$2,$3,$4,true,$5,$6) ON CONFLICT(step_id) DO UPDATE SET generation=EXCLUDED.generation,holder=EXCLUDED.holder,active=true,body=EXCLUDED.body,lease_until=EXCLUDED.lease_until")
        .bind(*step.id.inner().as_uuid()).bind(*step.job_id.inner().as_uuid()).bind(generation).bind(actor).bind(json!(claim)).bind(claim.lease_until).execute(&mut *tx).await.map_err(storage)?;
    refresh(tx, budget).await.map_err(storage)?;
    sqlx::query("DELETE FROM step_resource_allocations WHERE step_id=$1")
        .bind(*step.id.inner().as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    for row in &claim.allocations {
        refresh(tx, budget).await.map_err(storage)?;
        sqlx::query(
            "INSERT INTO step_resource_allocations(step_id,pool,amount) VALUES($1,$2,$3::numeric)",
        )
        .bind(*step.id.inner().as_uuid())
        .bind(&row.pool)
        .bind(row.amount.to_string())
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    }
    Ok(Some(claim))
}

pub(crate) async fn record_flow_resource_refusal_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    step: &Step,
    reason: &Refusal,
    stamp: &EventStamp,
    budget: Option<StatementBudget>,
) -> Result<Step, JobsError> {
    let (refused, events) = flow::refused_step(step, reason, stamp)?;
    refresh(tx, budget).await.map_err(storage)?;
    crate::postgres::preserve_first_records(tx, &step.id, &refused.metadata).await?;
    refresh(tx, budget).await.map_err(storage)?;
    sqlx::query("UPDATE steps SET metadata=$2,updated_at=$3,sign_offs=$4 WHERE id=$1")
        .bind(*step.id.inner().as_uuid())
        .bind(&refused.metadata)
        .bind(stamp.timestamp)
        .bind(json!(refused.sign_offs))
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    for event in events {
        boss_events::outbox::record_event_with_budget_in_tx(tx, &event, budget)
            .await
            .map_err(storage)?;
    }
    Ok(refused)
}

pub(crate) async fn release(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    current: &Step,
    next: &Step,
    events: &[boss_core::event::Event],
    cancelled: bool,
) -> Result<(), JobsError> {
    flow::preserve_token(&current.metadata, &next.metadata)?;
    let body: Option<(Value, bool)> =
        sqlx::query_as("SELECT body,active FROM step_resource_claims WHERE step_id=$1")
            .bind(*current.id.inner().as_uuid())
            .fetch_optional(&mut **tx)
            .await
            .map_err(storage)?;
    let Some((body, active)) = body else {
        return Ok(());
    };
    let claim: Claim = serde_json::from_value(body).map_err(storage)?;
    claim.validate().map_err(storage)?;
    // An inactive claim was already released — by an exit, or by the
    // lease sweep, whose expiry fact is the release (ad1c3af2).
    if !active {
        return Ok(());
    }
    if !flow::exit_required(&claim, current, next, cancelled)? {
        return Ok(());
    }
    let mut event = flow::exit_event(&claim, next, events, cancelled)?;
    flow::note_lapsed_lease(&claim, &mut event);
    for allocation in &claim.allocations {
        crate::declared_version::lock_and_read_newest(
            tx,
            "resource_pools",
            "name",
            &allocation.pool,
        )
        .await
        .map_err(storage)?;
    }
    let obligations =
        crate::resource_leases_pg::collect_stock_obligations_in_tx(tx, &claim, None).await?;
    crate::resource_leases_pg::store_stock_obligations_in_tx(tx, &obligations, None).await?;
    event.payload["stock_obligations"] = json!(obligations);
    sqlx::query("UPDATE step_resource_claims SET active=false WHERE step_id=$1 AND generation=$2 AND holder=$3 AND active")
        .bind(*current.id.inner().as_uuid()).bind(claim.generation).bind(&claim.holder).execute(&mut **tx).await.map_err(storage)?;
    sqlx::query("DELETE FROM step_resource_allocations WHERE step_id=$1")
        .bind(*current.id.inner().as_uuid())
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    boss_events::outbox::record_event_in_tx(tx, &event)
        .await
        .map_err(storage)
}

pub(crate) async fn record_reserved(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claim: &Claim,
    step: &Step,
    stamp: &EventStamp,
    budget: Option<StatementBudget>,
) -> Result<(), JobsError> {
    let event = stamp.event(
        flow::RESERVED,
        json!({"job_id":step.job_id,"step_id":step.id,"claim":claim,"step":step}),
    );
    boss_events::outbox::record_event_with_budget_in_tx(tx, &event, budget)
        .await
        .map_err(storage)
}

/// What `pool` holds right now: every active claim's allocation of it and
/// every stock obligation not yet returned. Read inside the caller's
/// transaction, under the lineage lock its publish or retire took.
pub(crate) async fn held(
    tx: &mut sqlx::PgConnection,
    pool: &str,
) -> Result<Amount, crate::pools::PoolError> {
    let held: String = sqlx::query_scalar("SELECT COALESCE(SUM(amount),0)::text FROM (SELECT a.amount FROM step_resource_allocations a JOIN step_resource_claims c USING(step_id) WHERE a.pool=$1 AND c.active UNION ALL SELECT amount FROM resource_stock_obligations WHERE pool=$1 AND returned_by IS NULL) held")
        .bind(pool)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| crate::pools::PoolError::Storage(e.to_string()))?;
    Amount::parse(&held).map_err(crate::pools::PoolError::Storage)
}

pub(crate) async fn publication_fence(
    tx: &mut sqlx::PgConnection,
    spec: &PoolSpec,
    roster_observation: bool,
) -> Result<(), crate::pools::PoolError> {
    let previous: Option<sqlx::types::Json<Box<serde_json::value::RawValue>>> =
        sqlx::query_scalar("SELECT spec FROM resource_pools WHERE name=$1 AND status='active'")
            .bind(&spec.name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| crate::pools::PoolError::Storage(e.to_string()))?;
    let previous = previous
        .map(|j| crate::resource_history::pool(j.0.get()).map_err(crate::pools::PoolError::Storage))
        .transpose()?;
    let contract_changed = previous.as_ref().is_some_and(|p| {
        (p.roster.is_some() || spec.roster.is_some())
            && !crate::pools::same_roster_contract(p, spec)
    });
    let qualified_observation = roster_observation
        && previous
            .as_ref()
            .is_some_and(|p| p.roster.is_some() && crate::pools::same_roster_contract(p, spec));
    let incompatible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM step_resource_allocations a JOIN step_resource_claims c USING(step_id) CROSS JOIN LATERAL jsonb_array_elements(c.body->'observed') d WHERE a.pool=$1 AND c.active AND d->>'pool'=$1 AND (d->>'stock')::boolean<>$2) OR EXISTS(SELECT 1 FROM resource_stock_obligations WHERE pool=$1 AND returned_by IS NULL AND (NOT $2 OR body->>'reclaimer' IS DISTINCT FROM $3))")
        .bind(&spec.name).bind(spec.kind==PoolKind::Stock).bind(&spec.reclaimer).fetch_one(&mut *tx).await.map_err(|e|crate::pools::PoolError::Storage(e.to_string()))?;
    let held = held(tx, &spec.name).await?;
    if !held.is_zero()
        && (incompatible
            || contract_changed
            || (!qualified_observation
                && match &spec.capacity {
                    PoolCapacity::Unknown {} => true,
                    PoolCapacity::Configured { amount } => {
                        amount.amount().map_err(crate::pools::PoolError::Storage)? < held
                    }
                }))
    {
        return Err(crate::pools::PoolError::Conflict(
            "publication cannot invalidate outstanding FLOW allocations".into(),
        ));
    }
    Ok(())
}
