//! Check the whole resource ledger before any rebuild wipe. Current rows are
//! derived from immutable acquisition/release facts, never retained as a cache.
use std::collections::{BTreeMap, HashMap};

use boss_core::job::{Step, StepStatus};
use boss_events::replay::ReplayEvent;

#[cfg(test)]
mod legacy_pool_tests {
    #[test]
    fn legacy_model_pool_history_rebuilds_without_reauthoring() {
        for class in ["opus", "sol"] {
            let payload = serde_json::json!({
                "name": format!("model-slot:{class}"), "version": 2,
                "status": "active", "unit": "slot", "kind": "flow",
                "capacity": {"state": "configured", "amount": "2"},
                "models": ["historical-model"]
            });
            let pool: crate::pools::PoolSpec = serde_json::from_value(payload.clone()).unwrap();
            assert!(pool.validate().is_err(), "new authoring stays strict");
            let event = super::ReplayEvent {
                raw_payload: Some(payload.to_string()),
                audit_id: 1,
                event_id: uuid::Uuid::new_v4(),
                kind: "jobs.pool.published".into(),
                ts: chrono::Utc::now(),
                payload,
            };
            assert!(
                super::reconstruct(&[event]).is_ok(),
                "legacy {class} publish must replay"
            );
        }
    }
}
use serde_json::Value;

use crate::flow_resources::{self as flow, Amount, Capacity, Claim};
use crate::pools::{PoolCapacity, PoolKind, PoolSpec};
use crate::resource_leases::{self as leases, IdentityBinding, StockObligation};

#[derive(Clone)]
pub(crate) struct Entry {
    pub claim: Claim,
    pub active: bool,
    pub obligations: Vec<StockObligation>,
}

#[derive(Clone)]
pub(crate) enum Effect {
    Claim(Entry),
    Binding(IdentityBinding, uuid::Uuid),
    Returned(StockObligation),
}

fn held(
    rows: &BTreeMap<uuid::Uuid, Entry>,
    obligations: &BTreeMap<(uuid::Uuid, String), StockObligation>,
    pool: &str,
) -> Result<Amount, String> {
    Amount::sum(
        rows.values()
            .filter(|row| row.active)
            .flat_map(|row| &row.claim.allocations)
            .filter(|allocation| allocation.pool == pool)
            .map(|row| &row.amount)
            .chain(
                obligations
                    .values()
                    .filter(|o| o.allocation.pool == pool && o.returned_by.is_none())
                    .map(|o| &o.allocation.amount),
            ),
    )
}

pub(crate) fn reconstruct(events: &[ReplayEvent]) -> Result<HashMap<uuid::Uuid, Effect>, String> {
    let causes: HashMap<_, _> = events.iter().map(|event| (event.event_id, event)).collect();
    let mut originals: HashMap<uuid::Uuid, &ReplayEvent> = HashMap::new();
    let mut rows: BTreeMap<uuid::Uuid, Entry> = BTreeMap::new();
    let mut pools: BTreeMap<String, PoolSpec> = BTreeMap::new();
    let mut versions = BTreeMap::new();
    let mut identities: BTreeMap<uuid::Uuid, IdentityBinding> = BTreeMap::new();
    let mut obligations: BTreeMap<(uuid::Uuid, String), StockObligation> = BTreeMap::new();
    let mut returned_evidence = std::collections::BTreeSet::new();
    let mut states: BTreeMap<uuid::Uuid, Step> = BTreeMap::new();
    let mut checked = HashMap::new();
    for event in events {
        if let Some(original) = originals.insert(event.event_id, event) {
            if original != event {
                return Err("one event identity carries different resource facts".into());
            }
            continue;
        }
        if matches!(
            event.kind.as_str(),
            crate::events::STEP_CREATED | crate::events::STEP_UPDATED
        ) {
            if let Ok(step) = serde_json::from_value::<Step>(event.payload.clone()) {
                states.insert(*step.id.inner().as_uuid(), step);
            }
            continue;
        }
        if event.kind == "jobs.pool.published" {
            let pool:PoolSpec=match event.raw_payload.as_deref() {
                Some(raw)=>crate::resource_history::pool(raw)?,
                None=>serde_json::from_value(event.payload.clone()).map_err(|error|format!("historical pool original raw amount unavailable; strict decode refused: {error}"))?,
            };
            pool.validate_history().map_err(|error| error.to_string())?;
            if let Some(previous) = pools.get(&pool.name)
                && (pool.version < previous.version
                    || pool.version == previous.version && pool != *previous)
            {
                return Err("resource pool version changed during replay".into());
            }
            versions.insert((pool.name.clone(), pool.version), pool.clone());
            pools.insert(pool.name.clone(), pool);
            continue;
        }
        if matches!(
            event.kind.as_str(),
            leases::IDENTITY_BOUND | leases::LEASE_EXPIRED | leases::STOCK_RETURNED
        ) {
            if event
                .payload
                .get("_actor")
                .and_then(Value::as_str)
                .is_none()
            {
                return Err("resource lifecycle fact lacks attribution".into());
            }
            if event.kind == leases::STOCK_RETURNED {
                let after: StockObligation =
                    serde_json::from_value(event.payload["obligation"].clone())
                        .map_err(|e| e.to_string())?;
                let evidence: uuid::Uuid =
                    serde_json::from_value(event.payload["evidence"].clone())
                        .map_err(|e| e.to_string())?;
                let key = (after.claim.id, after.allocation.pool.clone());
                let before = obligations
                    .get(&key)
                    .ok_or("stock return has no outstanding obligation")?;
                let mut expected = before.clone();
                expected.returned_by = Some(evidence);
                if before.returned_by.is_some()
                    || before.identity.is_none()
                    || after != expected
                    || evidence.is_nil()
                    || !returned_evidence.insert(evidence)
                    || event.payload["job_id"] != serde_json::json!(after.claim.job_id)
                    || event.payload["step_id"] != serde_json::json!(after.claim.step_id)
                {
                    return Err(
                        "stock return changed its identity/amount/contract or reused evidence"
                            .into(),
                    );
                }
                obligations.insert(key, after.clone());
                checked.insert(event.event_id, Effect::Returned(after));
                continue;
            }
            let claim: Claim = serde_json::from_value(event.payload["claim"].clone())
                .map_err(|e| e.to_string())?;
            claim.validate()?;
            let previous = rows
                .get(claim.step_id.inner().as_uuid())
                .ok_or("resource lifecycle fact lacks its acquisition")?;
            if !previous.active
                || previous.claim != claim
                || event.payload["job_id"] != serde_json::json!(claim.job_id)
                || event.payload["step_id"] != serde_json::json!(claim.step_id)
            {
                return Err("resource lifecycle fact changed its current acquisition".into());
            }
            if event.kind == leases::IDENTITY_BOUND {
                let binding: IdentityBinding =
                    serde_json::from_value(event.payload["binding"].clone())
                        .map_err(|e| e.to_string())?;
                let observed: chrono::DateTime<chrono::Utc> =
                    serde_json::from_value(event.payload["observed_at"].clone())
                        .map_err(|e| e.to_string())?;
                if observed.timestamp_micros() != event.ts.timestamp_micros() {
                    return Err("binding clock differs from its audit timestamp".into());
                }
                leases::validate_binding(&claim, true, &claim.holder, &binding, observed)
                    .map_err(|e| e.to_string())?;
                if identities.insert(claim.id, binding.clone()).is_some() {
                    return Err("resource acquisition was bound twice".into());
                }
                checked.insert(
                    event.event_id,
                    Effect::Binding(binding, *claim.step_id.inner().as_uuid()),
                );
                continue;
            }
            let observed: chrono::DateTime<chrono::Utc> =
                serde_json::from_value(event.payload["observed_at"].clone())
                    .map_err(|e| e.to_string())?;
            if claim.lease_until.is_none_or(|until| until > observed)
                || observed.timestamp_micros() != event.ts.timestamp_micros()
            {
                return Err("resource expiry predates its frozen deadline".into());
            }
            let expected = leases::obligations_for(
                &claim,
                identities.get(&claim.id).map(|b| b.identity.clone()),
                &versions.values().cloned().collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())?;
            let recorded: Vec<StockObligation> =
                serde_json::from_value(event.payload["stock_obligations"].clone())
                    .map_err(|e| e.to_string())?;
            if expected != recorded {
                return Err("resource expiry did not conserve its physical stock".into());
            }
            for o in &recorded {
                if obligations
                    .insert((claim.id, o.allocation.pool.clone()), o.clone())
                    .is_some()
                {
                    return Err("stock obligation was created twice".into());
                }
            }
            let entry = Entry {
                claim,
                active: false,
                obligations: recorded,
            };
            rows.insert(*entry.claim.step_id.inner().as_uuid(), entry.clone());
            checked.insert(event.event_id, Effect::Claim(entry));
            continue;
        }
        if !matches!(event.kind.as_str(), flow::RESERVED | flow::RELEASED) {
            continue;
        }
        let claim: Claim = serde_json::from_value(event.payload["claim"].clone())
            .map_err(|error| error.to_string())?;
        claim.validate()?;
        let step: Step = serde_json::from_value(event.payload["step"].clone())
            .map_err(|error| error.to_string())?;
        if event
            .payload
            .get("_actor")
            .and_then(Value::as_str)
            .is_none()
            || event.payload["job_id"] != serde_json::json!(claim.job_id)
            || event.payload["step_id"] != serde_json::json!(claim.step_id)
            || step.job_id != claim.job_id
            || step.id != claim.step_id
            || step.metadata.get(flow::TOKEN_KEY) != Some(&serde_json::json!(claim))
        {
            return Err("resource fact identity, actor, step or acquisition differs".into());
        }
        let active = event.kind == flow::RESERVED;
        let mut new_obligations = Vec::new();
        if active {
            // audit_log.ts is PostgreSQL microseconds; canonical claim and
            // observation fields retain the original clock's finer precision.
            if claim.version >= 2
                && claim.claimed_at.map(|at| at.timestamp_micros())
                    != Some(event.ts.timestamp_micros())
            {
                return Err("resource lease claim clock differs from its original fact".into());
            }
            if states.get(step.id.inner().as_uuid()) != Some(&step)
                || step.status != StepStatus::Active
                || step.assignee_id.as_deref() != Some(&claim.holder)
            {
                return Err("resource reservation lacks its exact active state fact".into());
            }
            let generation = match rows.get(step.id.inner().as_uuid()) {
                Some(previous) if previous.active => {
                    return Err("a second reservation replaced an active acquisition".into());
                }
                Some(previous) => previous
                    .claim
                    .generation
                    .checked_add(1)
                    .ok_or("resource generation overflow")?,
                None => 1,
            };
            if claim.generation != generation {
                return Err("resource generation is not the next acquisition".into());
            }
            for observed in &claim.observed {
                if observed.held != held(&rows, &obligations, &observed.pool)? {
                    return Err(
                        "resource decision held amount differs from prior immutable allocations"
                            .into(),
                    );
                }
                if observed.amount.is_zero() {
                    continue;
                }
                let pool = pools
                    .get(&observed.pool)
                    .ok_or("resource reservation lacks its published pool fact")?;
                let capacity = match &pool.capacity {
                    PoolCapacity::Unknown {} => Capacity::Unknown,
                    PoolCapacity::Configured { amount } => Capacity::Known(amount.amount()?),
                };
                if pool.status != crate::registry::WorkflowStatus::Active
                    || pool.version != observed.version
                    || capacity != observed.capacity
                    || (pool.kind == PoolKind::Stock) != observed.stock
                {
                    return Err("resource decision differs from its published pool version".into());
                }
            }
        } else {
            let previous = rows
                .get(step.id.inner().as_uuid())
                .ok_or("release has no prior acquisition")?;
            if !previous.active || previous.claim != claim {
                return Err("release changed an acquisition generation or holder".into());
            }
            let cause_id: uuid::Uuid =
                serde_json::from_value(event.payload["cause_event_id"].clone())
                    .map_err(|error| error.to_string())?;
            let cause = causes
                .get(&cause_id)
                .ok_or("resource release lost its state cause")?;
            if cause.ts != event.ts || cause.payload.get("_actor") != event.payload.get("_actor") {
                return Err("resource release provenance differs from its state cause".into());
            }
            match event.payload["reason"].as_str() {
                Some("job-cancelled")
                    if cause.kind == crate::events::JOB_UPDATED
                        && cause.payload["id"] == serde_json::json!(claim.job_id)
                        && cause.payload["status"] == "cancelled" => {}
                Some("job-closed")
                    if cause.kind == crate::events::JOB_UPDATED
                        && cause.payload["id"] == serde_json::json!(claim.job_id)
                        && cause.payload["status"] == "closed" => {}
                Some(reason)
                    if cause.kind == crate::events::STEP_UPDATED
                        && serde_json::from_value::<Step>(cause.payload.clone())
                            .is_ok_and(|actual| actual == step)
                        && match reason {
                            "completed" => step.status == StepStatus::Completed,
                            "skipped" => step.status == StepStatus::Skipped,
                            "released" => step.status != StepStatus::Active,
                            _ => false,
                        } => {}
                _ => return Err("resource release does not conserve its actual state cause".into()),
            }
            let expected = leases::obligations_for(
                &claim,
                identities.get(&claim.id).map(|b| b.identity.clone()),
                &versions.values().cloned().collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())?;
            new_obligations = serde_json::from_value(
                event
                    .payload
                    .get("stock_obligations")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            )
            .map_err(|e| e.to_string())?;
            if expected != new_obligations {
                return Err("resource exit did not conserve its physical stock".into());
            }
            for o in &new_obligations {
                if obligations
                    .insert((claim.id, o.allocation.pool.clone()), o.clone())
                    .is_some()
                {
                    return Err("stock obligation was created twice".into());
                }
            }
        }
        let entry = Entry {
            claim,
            active,
            obligations: new_obligations,
        };
        rows.insert(*step.id.inner().as_uuid(), entry.clone());
        checked.insert(event.event_id, Effect::Claim(entry));
    }
    Ok(checked)
}

pub(crate) async fn restore_flow_ledger_in_tx(
    conn: &mut sqlx::PgConnection,
    effect: &Effect,
) -> Result<(), String> {
    let entry = match effect {
        Effect::Claim(entry) => entry,
        Effect::Binding(binding, step) => {
            sqlx::query(
                "INSERT INTO resource_identity_bindings(acquisition,step_id,body) VALUES($1,$2,$3)",
            )
            .bind(binding.acquisition)
            .bind(step)
            .bind(serde_json::json!(binding))
            .execute(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        Effect::Returned(o) => {
            sqlx::query("UPDATE resource_stock_obligations SET returned_by=$3,body=$4 WHERE acquisition=$1 AND pool=$2").bind(o.claim.id).bind(&o.allocation.pool).bind(o.returned_by).bind(serde_json::json!(o)).execute(&mut *conn).await.map_err(|e|e.to_string())?;
            return Ok(());
        }
    };
    let claim = &entry.claim;
    sqlx::query("INSERT INTO step_resource_claims(step_id,job_id,generation,holder,active,body,lease_until) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(step_id) DO UPDATE SET job_id=EXCLUDED.job_id,generation=EXCLUDED.generation,holder=EXCLUDED.holder,active=EXCLUDED.active,body=EXCLUDED.body,lease_until=EXCLUDED.lease_until")
        .bind(*claim.step_id.inner().as_uuid()).bind(*claim.job_id.inner().as_uuid()).bind(claim.generation).bind(&claim.holder).bind(entry.active).bind(serde_json::json!(claim)).bind(claim.lease_until).execute(&mut *conn).await.map_err(|error|error.to_string())?;
    sqlx::query("DELETE FROM step_resource_allocations WHERE step_id=$1")
        .bind(*claim.step_id.inner().as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(|error| error.to_string())?;
    if entry.active {
        for allocation in &claim.allocations {
            sqlx::query("INSERT INTO step_resource_allocations(step_id,pool,amount) VALUES($1,$2,$3::numeric)")
            .bind(*claim.step_id.inner().as_uuid()).bind(&allocation.pool).bind(allocation.amount.to_string()).execute(&mut *conn).await.map_err(|error|error.to_string())?;
        }
    }
    crate::resource_leases_pg::store_stock_obligations_in_tx(conn, &entry.obligations, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
