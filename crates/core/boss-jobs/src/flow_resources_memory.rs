//! In-memory implementation of the same locked whole-vector ledger contract.
use crate::{
    flow_resources::{self as flow, Amount, Capacity, Claim, Demand, Refusal},
    pools::{PoolCapacity, PoolKind, PoolSpec, State},
    port::JobsError,
    registry::WorkflowSpec,
};
use boss_core::job::{Step, StepStatus};

fn storage(e: impl std::fmt::Display) -> JobsError {
    JobsError::Storage(e.to_string())
}
pub(crate) fn held(state: &State, pool: &str) -> Result<Amount, String> {
    Amount::sum(
        state
            .claims
            .values()
            .filter(|(_, active)| *active)
            .flat_map(|(c, _)| &c.allocations)
            .filter(|a| a.pool == pool)
            .map(|row| &row.amount)
            .chain(
                state
                    .obligations
                    .values()
                    .filter(|o| o.allocation.pool == pool && o.returned_by.is_none())
                    .map(|o| &o.allocation.amount),
            ),
    )
}
pub(crate) fn publication_fence(
    state: &State,
    spec: &PoolSpec,
    roster_observation: bool,
) -> Result<(), String> {
    let held = held(state, &spec.name)?;
    let previous = state
        .rows
        .values()
        .find(|p| p.name == spec.name && p.status == crate::registry::WorkflowStatus::Active);
    let contract_changed = previous.is_some_and(|p| {
        (p.roster.is_some() || spec.roster.is_some())
            && !crate::pools::same_roster_contract(p, spec)
    });
    let qualified_observation = roster_observation
        && previous
            .is_some_and(|p| p.roster.is_some() && crate::pools::same_roster_contract(p, spec));
    if !held.is_zero()
        && (contract_changed
            || state.claims.values().filter(|(_, a)| *a).any(|(c, _)| {
                c.observed.iter().any(|d| {
                    d.pool == spec.name
                        && !d.amount.is_zero()
                        && d.stock != (spec.kind == PoolKind::Stock)
                })
            })
            || state.obligations.values().any(|o| {
                o.allocation.pool == spec.name
                    && o.returned_by.is_none()
                    && (spec.kind != PoolKind::Stock
                        || spec.reclaimer.as_deref() != Some(o.reclaimer.as_str()))
            })
            || (!qualified_observation
                && match &spec.capacity {
                    PoolCapacity::Unknown {} => true,
                    PoolCapacity::Configured { amount } => amount.amount()? < held,
                }))
    {
        return Err("publication cannot invalidate outstanding FLOW allocations".into());
    }
    Ok(())
}
pub(crate) fn prepare(
    state: &State,
    step: &Step,
    protocol: Option<&WorkflowSpec>,
    actor: &str,
    claimant_class: Option<&str>,
    stations: &[crate::stations::StationSpec],
    job: Option<&boss_core::job::Job>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Claim>, JobsError> {
    let Some(workflow) = protocol else {
        return Ok(None);
    };
    let Some(bill) = workflow
        .steps
        .iter()
        .find(|s| Some(s.title.as_str()) == step.spec_slug.as_deref())
        .and_then(|s| s.resources.as_ref())
    else {
        return Ok(None);
    };
    bill.validate().map_err(storage)?;
    let requested = bill.pools();
    if requested
        .iter()
        .all(|(_, amount)| amount.amount().is_ok_and(|a| a.is_zero()))
    {
        return Ok(None);
    }
    if !job.is_some_and(|job| {
        !matches!(
            job.status,
            boss_core::job::JobStatus::Closed | boss_core::job::JobStatus::Cancelled
        )
    }) {
        return Err(JobsError::ResourceRefused(Refusal {
            pool: "acquisition".into(),
            reason: "packet-not-open".into(),
            observed: Vec::new(),
        }));
    }
    let prior = state.claims.get(&step.id.to_string());
    if let Some((claim, true)) = prior {
        claim.validate().map_err(storage)?;
        if claim.holder == actor
            && step.status == StepStatus::Active
            && step.metadata.get(flow::TOKEN_KEY) == Some(&serde_json::json!(claim))
        {
            if claim.lease_until.is_some_and(|until| until <= now) {
                return Err(crate::resource_leases::error("resource-lease-expired"));
            }
            return Ok(Some(claim.clone()));
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
    // The class a from-run bill holds is the claimant's recorded class
    // (backlog 7750d37d), else the one read off this packet (backlog
    // af5dd547), against the pools under this same lock; any other bill
    // is unchanged. The claim records the resolved bill.
    //
    // Its vCPU is placed first, on the host its protocol's owner floats
    // the most room on (backlog 5608c1a2), while the bill still says
    // `from-run`; the slot's class is then named on that placed bill. A
    // run whose profile binds no worker has no vCPU to place (backlog
    // db88f6a9), so the waiver is read before anything else.
    let bill = &flow::waive_workerless(bill, job.map(|job| &job.metadata));
    let shares = if flow::draws_a_host(bill) {
        match crate::station_tree::protocol_owner(workflow) {
            Some(owner) => flow::floating_shares(stations, owner).map_err(|why| {
                JobsError::ResourceRefused(Refusal {
                    pool: "host".into(),
                    reason: format!("host-share-unreadable: {why}"),
                    observed: Vec::new(),
                })
            })?,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let placed = flow::resolve_host(bill, &shares, |pool| held(state, pool))
        .map_err(JobsError::ResourceRefused)?;
    let (bill, _) = flow::resolve_bill(
        &placed,
        job.map(|job| &job.metadata),
        claimant_class,
        state.rows.values(),
    )
    .map_err(JobsError::ResourceRefused)?;
    let requested = bill.pools();
    let mut demands = Vec::new();
    for (name, quantity) in requested {
        let share = flow::share_of(&bill, &name, &shares).map_err(JobsError::ResourceRefused)?;
        let spec = state
            .rows
            .values()
            .find(|p| p.name == name && p.status == crate::registry::WorkflowStatus::Active);
        let (version, capacity, stock) = match spec {
            Some(spec) => (
                spec.version,
                match &spec.capacity {
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
            pool: name.clone(),
            version,
            capacity,
            stock,
            amount: quantity.amount().map_err(storage)?,
            held: held(state, &name).map_err(storage)?,
            share,
            // A version-2 decision records no line (backlog 30c1efe8):
            // readers go first, so this writer still writes version 2.
            levels: Vec::new(),
        });
    }
    let allocations = flow::plan(&demands).map_err(JobsError::ResourceRefused)?;
    let generation = prior
        .map(|(c, _)| c.generation)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| storage("resource generation exhausted"))?;
    demands.sort_by(|a, b| a.pool.cmp(&b.pool));
    let duration = flow::resolved_duration_ms(workflow, step)?;
    let claim = Claim {
        version: flow::WRITTEN_CLAIM_VERSION,
        id: uuid::Uuid::new_v4(),
        step_id: step.id,
        job_id: step.job_id,
        generation,
        holder: actor.into(),
        workflow_kind: workflow.kind.clone(),
        workflow_version: workflow.version,
        bill,
        allocations,
        observed: demands,
        claimed_at: Some(now),
        resolved_duration_ms: Some(duration),
        lease_until: Some(flow::lease_until(now, duration)?),
    };
    claim.validate().map_err(storage)?;
    Ok(Some(claim))
}

pub(crate) fn attach(step: &mut Step, claim: Option<&Claim>) -> Result<bool, JobsError> {
    let Some(claim) = claim else { return Ok(false) };
    let body = serde_json::json!(claim);
    let fresh = step.metadata.get(flow::TOKEN_KEY) != Some(&body);
    let object = step
        .metadata
        .as_object_mut()
        .ok_or_else(|| storage("step metadata is not an object"))?;
    object.insert(flow::TOKEN_KEY.into(), body);
    object.remove("waiting_on");
    Ok(fresh)
}
pub(crate) fn apply(state: &mut State, claim: &Claim) {
    let key = claim.step_id.to_string();
    if let Some((old, _)) = state.claims.get(&key)
        && let Some(until) = old.lease_until
    {
        state.lease_expiry.remove(&(until, key.clone()));
    }
    if let Some(until) = claim.lease_until {
        state.lease_expiry.insert((until, key));
    }
    state
        .claims
        .insert(claim.step_id.to_string(), (claim.clone(), true));
}

pub(crate) type PlannedRelease = (
    Claim,
    boss_core::event::Event,
    Vec<crate::resource_leases::StockObligation>,
);

pub(crate) fn exit_plan(
    state: &State,
    current: &Step,
    next: &Step,
    events: &[boss_core::event::Event],
    cancelled: bool,
) -> Result<Option<PlannedRelease>, JobsError> {
    flow::preserve_token(&current.metadata, &next.metadata)?;
    let Some((claim, active)) = state.claims.get(&current.id.to_string()) else {
        return Ok(None);
    };
    // An inactive claim was already released — by an exit, or by the
    // lease sweep, whose expiry fact is the release (ad1c3af2).
    if !active {
        return Ok(None);
    }
    if !flow::exit_required(claim, current, next, cancelled)? {
        return Ok(None);
    }
    let mut event = flow::exit_event(claim, next, events, cancelled)?;
    flow::note_lapsed_lease(claim, &mut event);
    let obligations = crate::resource_leases::obligations_for(
        claim,
        state.identities.get(&claim.id).map(|b| b.identity.clone()),
        &state.rows.values().cloned().collect::<Vec<_>>(),
    )?;
    event.payload["stock_obligations"] = serde_json::json!(obligations);
    Ok(Some((claim.clone(), event, obligations)))
}
pub(crate) fn release(
    state: &mut State,
    claim: &Claim,
    obligations: &[crate::resource_leases::StockObligation],
) {
    // The plan is fully checked before the first state write. Applying it is
    // infallible, so a failed parse cannot follow a partially stored step.
    for obligation in obligations {
        state.obligations.insert(
            (claim.id, obligation.allocation.pool.clone()),
            obligation.clone(),
        );
    }
    if let Some(until) = claim.lease_until {
        state
            .lease_expiry
            .remove(&(until, claim.step_id.to_string()));
    }
    state
        .claims
        .insert(claim.step_id.to_string(), (claim.clone(), false));
}
