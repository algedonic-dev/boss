//! The reference adapter stores row versions, never a factory checkpoint per fact.
use super::*;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

const MAX_BATCH: usize = 1024;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub(super) const HANDLED_RESOURCE_KINDS: &[&str] = &[
    crate::flow_resources::RESERVED,
    crate::flow_resources::RELEASED,
    crate::flow_resources::REFUSED,
    crate::events::STATION_RESOURCE_REFUSED,
    crate::pools::PUBLISHED,
    crate::pools::RETIRED,
    crate::resource_leases::IDENTITY_BOUND,
    crate::resource_leases::LEASE_EXPIRED,
    crate::resource_leases::STOCK_RETURNED,
];
pub(super) fn unknown_resource(fact: &SequencedFact) -> Result<bool, ClaimsError> {
    if fact.source != "jobs" || HANDLED_RESOURCE_KINDS.contains(&fact.kind.as_str()) {
        return Ok(false);
    }
    if [
        "jobs.step.resource_",
        "jobs.station.resource_",
        "jobs.pool.",
    ]
    .iter()
    .any(|p| fact.kind.starts_with(p))
    {
        return Ok(true);
    }
    let payload: Value = serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
    Ok([
        &payload,
        &payload["metadata"],
        &payload["step"],
        &payload["step"]["metadata"],
    ]
    .iter()
    .any(|value| {
        ["claim", "resource_claim", "reclaimer"]
            .iter()
            .any(|key| value.get(key).is_some())
    }) || payload
        .get("__projection_witness")
        .is_some_and(|v| v == &Value::Bool(true)))
}
pub(super) const RETENTION_HOURS: i64 = crate::regions::MAX_WINDOW_HOURS;

#[cfg(test)]
mod legacy_pool_tests {
    #[test]
    fn legacy_model_pool_publish_and_retire_fold_without_reauthoring() {
        for class in ["opus", "sol"] {
            for (kind, status) in [
                ("jobs.pool.published", "active"),
                ("jobs.pool.retired", "retired"),
            ] {
                let payload = serde_json::json!({
                    "name": format!("model-slot:{class}"), "version": 2,
                    "status": status, "unit": "slot", "kind": "flow",
                    "capacity": {"state": "configured", "amount": "2"},
                    "models": ["historical-model"]
                });
                let at = chrono::Utc::now();
                let fact = crate::claims_console::SequencedFact {
                    seq: 1,
                    recorded_at: at,
                    event_at: at,
                    event_id: uuid::Uuid::new_v4(),
                    source: "jobs".into(),
                    kind: kind.into(),
                    payload: payload.to_string(),
                    original_group: None,
                };
                let rows = super::projected_rows(&fact, None, None, None, None)
                    .unwrap_or_else(|why| panic!("legacy {class} {kind} must fold: {why}"));
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].body["declaration"]["name"], payload["name"]);
                assert_eq!(rows[0].body["declaration"]["models"], payload["models"]);
                assert_eq!(rows[0].body["declaration"]["status"], status);
            }
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Row {
    pub kind: String,
    pub key: String,
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub body: Value,
}
#[derive(Default)]
pub(super) struct State {
    pub generation: i64,
    pub pruned_through: i64,
    pub source: Option<SourceObservation>,
    pub source_failed: Option<DateTime<Utc>>,
    pub head: Option<(i64, DateTime<Utc>)>,
    pub cuts: BTreeMap<i64, DateTime<Utc>>,
    pub fingerprints: BTreeMap<i64, String>,
    pub events: BTreeMap<uuid::Uuid, i64>,
    pub rows: Vec<Row>,
    pub current: BTreeMap<(String, String), Row>,
    pub groups: BTreeMap<uuid::Uuid, (i64, i64)>,
    pub group_evidence: BTreeMap<uuid::Uuid, String>,
}
#[derive(Default)]
pub struct InMemoryClaimsHistory {
    state: Mutex<State>,
}
#[cfg(feature = "postgres")]
impl InMemoryClaimsHistory {
    pub(super) fn from_state(state: State) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }
    pub(super) fn into_state(self) -> Result<State, ClaimsError> {
        self.state
            .into_inner()
            .map_err(|_| unread("materialized owner unavailable"))
    }
}
fn invalid(why: impl Into<String>) -> ClaimsError {
    ClaimsError::Invalid(why.into())
}
fn unread(why: impl Into<String>) -> ClaimsError {
    ClaimsError::Unavailable(why.into())
}

fn obligations(body: &Value) -> Result<Vec<crate::resource_leases::StockObligation>, ClaimsError> {
    body.get("stock_obligations")
        .map(|value| serde_json::from_value(value.clone()).map_err(|e| unread(e.to_string())))
        .transpose()
        .map(Option::unwrap_or_default)
}
pub(super) fn has_stock(body: &Value) -> bool {
    body["stock_obligations"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["returned_by"].is_null()))
}

fn acquisition_key(value: &Value) -> String {
    value["claim"]["id"]
        .as_str()
        .or_else(|| value["obligation"]["claim"]["id"].as_str())
        .unwrap_or("")
        .into()
}

/// An open claim's executor kind and how the reading knows it (factory
/// taxonomy M1, backlog 3ff2ef60): `recorded` when the claim door wrote
/// it on the claim's fact, else `derived-from-id` — the holder's id
/// through `ActorId::executor_kind`, the mapping the door itself uses.
/// The derivation IS the backfill of the claims recorded before the
/// door did: a pure function of the log, rebuilt with the projection,
/// no hand rows. A claim nobody holds has no kind.
fn executor_kind_of(
    body: &Value,
    holder: Option<&str>,
) -> Result<(Option<boss_core::actor::ExecutorKind>, Option<String>), ClaimsError> {
    let recorded: Option<boss_core::actor::ExecutorKind> =
        serde_json::from_value(body["executor_kind"].clone())
            .map_err(|e| unread(format!("materialized executor kind: {e}")))?;
    Ok(match (recorded, holder.filter(|h| !h.trim().is_empty())) {
        (Some(kind), _) => (Some(kind), Some("recorded".into())),
        (None, Some(holder)) => {
            let kind = match holder.parse::<boss_core::actor::ActorId>() {
                Ok(actor) => actor.executor_kind(),
                Err(never) => match never {},
            };
            (Some(kind), Some("derived-from-id".into()))
        }
        (None, None) => (None, None),
    })
}

pub(super) fn projected_rows(
    fact: &SequencedFact,
    old_step: Option<&Row>,
    old_claim: Option<&Row>,
    old_pool: Option<&Row>,
    old_unknown: Option<&Row>,
) -> Result<Vec<Row>, ClaimsError> {
    if fact.source != "jobs" {
        return Ok(vec![]);
    }
    let make = |kind: &str, key: String, body: Value| Row {
        kind: kind.into(),
        key,
        seq: fact.seq,
        at: fact.recorded_at,
        body,
    };
    match fact.kind.as_str() {
        // A retire's payload is the row it retired, so it folds exactly
        // like a publish: the pool's row now reads `retired` and the
        // reading below serves only active pools (backlog f399458f).
        "jobs.pool.published" | "jobs.pool.retired" => {
            let pool = crate::resource_history::pool(&fact.payload).map_err(unread)?;
            pool.validate_history().map_err(|e| unread(e.to_string()))?;
            let mut capacity_since = fact.recorded_at;
            if let Some(old) = old_pool {
                let previous: PoolSpec = serde_json::from_value(old.body["declaration"].clone())
                    .map_err(|e| unread(e.to_string()))?;
                let same_capacity = match (&previous.capacity, &pool.capacity) {
                    (
                        crate::pools::PoolCapacity::Unknown {},
                        crate::pools::PoolCapacity::Unknown {},
                    ) => true,
                    (
                        crate::pools::PoolCapacity::Configured { amount: a },
                        crate::pools::PoolCapacity::Configured { amount: b },
                    ) => a.amount().map_err(unread)? == b.amount().map_err(unread)?,
                    _ => false,
                };
                if same_capacity
                    && previous.status == pool.status
                    && previous.unit == pool.unit
                    && previous.kind == pool.kind
                {
                    capacity_since = serde_json::from_value(old.body["capacity_since"].clone())
                        .map_err(|e| unread(e.to_string()))?;
                }
            }
            Ok(vec![make(
                "pool",
                pool.name.clone(),
                serde_json::json!({"declaration":pool,"capacity_since":capacity_since}),
            )])
        }
        "jobs.job.created" | "jobs.job.updated" => {
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let id = value["id"]
                .as_str()
                .ok_or_else(|| unread("job fact has no identity"))?;
            let status = value["status"]
                .as_str()
                .ok_or_else(|| unread("job fact has no status"))?;
            Ok(vec![make(
                "job",
                id.into(),
                serde_json::json!({"status":status,"packet_opened_at":value["opened_at"]}),
            )])
        }
        "jobs.step.created" | "jobs.step.updated" => {
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let id = value["id"]
                .as_str()
                .ok_or_else(|| unread("step fact has no identity"))?;
            if value
                .get("step_id")
                .is_some_and(|other| other != &value["id"])
            {
                return Err(unread("step identity keys disagree"));
            }
            let job = value["job_id"]
                .as_str()
                .ok_or_else(|| unread("step fact has no packet identity"))?;
            let status = value["status"]
                .as_str()
                .ok_or_else(|| unread("step fact has no status"))?;
            let holder = value.get("assignee_id").cloned().unwrap_or(Value::Null);
            let since = old_step
                .filter(|old| old.body["status"] == "active" && old.body["holder"] == holder)
                .map(|old| old.body["since"].clone())
                .unwrap_or_else(|| serde_json::json!(fact.event_at));
            // Keep the original acquisition transaction across metadata-only
            // updates; a fresh activation must never inherit its predecessor.
            let activation_group = old_step
                .filter(|old| old.body["status"] == "active" && old.body["holder"] == holder)
                .map(|old| old.body["activation_group"].clone())
                .unwrap_or_else(|| {
                    serde_json::json!(fact.original_group.as_ref().map(|g| g.reference.id))
                });
            // The executor kind the claim door RECORDED on this fact
            // (backlog 3ff2ef60), else the one it recorded for this same
            // holder's claim, kept across the holder's later writes — a
            // new holder never inherits it. A word that is not one of the
            // three is refused, not read as the nearest kind. Absent both,
            // the reading derives it from the holder's id (`open_claims`).
            let executor_kind = match value.get(crate::events::EXECUTOR_KIND_KEY) {
                Some(word) if !word.is_null() => serde_json::json!(
                    serde_json::from_value::<boss_core::actor::ExecutorKind>(word.clone())
                        .map_err(|e| unread(format!("recorded executor kind: {e}")))?
                ),
                _ => old_step
                    .filter(|old| old.body["status"] == "active" && old.body["holder"] == holder)
                    .map(|old| old.body["executor_kind"].clone())
                    .unwrap_or(Value::Null),
            };
            let continuing = old_step
                .is_some_and(|old| old.body["status"] == "active" && old.body["holder"] == holder);
            // The audit sequence of the fact that STARTED this activation,
            // kept across the holder's later writes (backlog 971a6190). A
            // claim with neither an original transaction nor a resource
            // claim — every unbilled per-step claim — has nothing else to
            // bind its station receipt to, and a receipt is written only by
            // the fact that starts an activation, so the two sequences are
            // equal exactly when the receipt belongs to this activation.
            let activation_seq = if status != "active" {
                Value::Null
            } else if continuing {
                old_step.map_or(Value::Null, |old| old.body["activation_seq"].clone())
            } else {
                serde_json::json!(fact.seq)
            };
            let mut rows = vec![make(
                "step",
                id.into(),
                serde_json::json!({"job_id":job,"status":status,"holder":holder,
                "since":since,"run":value["metadata"][crate::agent_runs::EDGE_KEY],
                "executor_kind":executor_kind,
                "activation_group":activation_group,
                "activation_seq":activation_seq,
                "waiting_on":value["metadata"]["waiting_on"],"resource_claim":value["metadata"]["resource_claim"]}),
            )];
            // The station the PER-STEP claim door recorded on this fact
            // (backlog 2682c418), read into the same `station-claim` row a
            // `jobs.station.claimed` receipt makes and bound the same way,
            // to this activation's transaction — so the reading below has
            // one definition of a claim's station whichever door took it.
            // Only a fresh activation records one: an idempotent re-claim
            // by the holder changes nothing, as the claim door says.
            match value.get(crate::events::CLAIM_STATION_KEY) {
                None | Some(Value::Null) => {}
                Some(_) if continuing => {}
                Some(name) => {
                    let station = name
                        .as_str()
                        .filter(|name| !name.trim().is_empty() && name.len() <= 256)
                        .ok_or_else(|| unread("claim station missing or unbounded"))?;
                    if status != "active" || !holder.is_string() {
                        return Err(unread("claim station recorded on a step that is not held"));
                    }
                    rows.push(make(
                        "station-claim",
                        id.into(),
                        serde_json::json!({
                            "station":station,"job_id":job,"holder":holder,
                            "station_id":value[crate::events::CLAIM_STATION_ID_KEY],
                            "station_resolution":value[crate::events::STATION_RESOLUTION_KEY],
                            "station_resolution_reason":value[crate::events::STATION_RESOLUTION_REASON_KEY],
                            "claim":value["metadata"]["resource_claim"],
                            "activation_group":activation_group,
                            // This fact IS the activation's start, so the
                            // receipt names it (backlog 971a6190). A
                            // `jobs.station.claimed` receipt names none: an
                            // ungrouped one cannot prove its activation.
                            "activation_seq":activation_seq,
                        }),
                    ));
                }
            }
            Ok(rows)
        }
        crate::events::STATION_CLAIMED => {
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let raw = value["receipt_json"]
                .as_str()
                .ok_or_else(|| unread("station claim receipt missing"))?;
            let receipt = crate::station_claim::SelectionReceipt::from_json(raw.to_owned())
                .map_err(|e| unread(e.to_string()))?;
            let step = &receipt["claimed_step"];
            let id = step["id"]
                .as_str()
                .ok_or_else(|| unread("station receipt step missing"))?;
            let job = step["job_id"]
                .as_str()
                .ok_or_else(|| unread("station receipt packet missing"))?;
            let station = receipt["claim_station"]["name"]
                .as_str()
                .or_else(|| receipt["station"]["name"].as_str())
                .filter(|name| !name.trim().is_empty() && name.len() <= 256)
                .ok_or_else(|| unread("station receipt name missing or unbounded"))?;
            if receipt["selected"]["step_id"] != id
                || receipt["selected"]["job_id"] != job
                || receipt["event_id"] != serde_json::json!(fact.event_id)
                || step["status"] != "active"
                || !step["assignee_id"].is_string()
            {
                return Err(unread(
                    "station receipt original event/step/packet/holder differs",
                ));
            }
            // The station fact itself is the authoritative claimed-step state;
            // its producer need not emit a second jobs.step.updated fact.
            let mut claimed = fact.clone();
            claimed.kind = crate::events::STEP_UPDATED.into();
            claimed.payload = serde_json::to_string(step).map_err(|e| unread(e.to_string()))?;
            let mut rows = projected_rows(&claimed, old_step, old_claim, old_pool, old_unknown)?;
            rows.push(make(
                "station-claim",
                id.into(),
                serde_json::json!({
                    "station":station,"job_id":job,"holder":step["assignee_id"],
                    "station_id":receipt["claim_station"]["id"],
                    "station_resolution":receipt["station_resolution"],
                    "station_resolution_reason":receipt["station_resolution_reason"],
                    "claim":step["metadata"]["resource_claim"],
                    "activation_group":fact.original_group.as_ref().map(|g|g.reference.id),
                }),
            ));
            Ok(rows)
        }
        crate::flow_resources::RESERVED | crate::flow_resources::RELEASED => {
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let claim: Claim = serde_json::from_value(value["claim"].clone())
                .map_err(|e| unread(e.to_string()))?;
            claim.validate().map_err(unread)?;
            if value["step_id"] != serde_json::json!(claim.step_id)
                || value["job_id"] != serde_json::json!(claim.job_id)
                || value["step"]["id"] != value["step_id"]
                || value["step"]["job_id"] != value["job_id"]
                || value["step"]["metadata"]["resource_claim"] != serde_json::json!(claim)
            {
                return Err(unread(
                    "resource fact identities or stored acquisition disagree",
                ));
            }
            let active = fact.kind == crate::flow_resources::RESERVED;
            if active {
                if old_claim.is_some()
                    || value["step"]["status"] != "active"
                    || value["step"]["assignee_id"] != claim.holder
                {
                    return Err(unread(
                        "reservation repeats or differs from its active holder",
                    ));
                }
            } else {
                let old =
                    old_claim.ok_or_else(|| unread("release has no materialized reservation"))?;
                if old.body["active"] != true || old.body["claim"] != serde_json::json!(claim) {
                    return Err(unread("release differs from its reserved generation"));
                }
                if value["cause_event_id"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .is_none()
                {
                    return Err(unread("release has no immutable cause identity"));
                }
            }
            let mut rows = vec![make(
                "acquisition",
                claim.id.to_string(),
                serde_json::json!({"claim":claim,"active":active,
                    "reserved_at":old_claim.map(|r|r.body.get("reserved_at").cloned().unwrap_or_else(||serde_json::json!(r.at))).unwrap_or_else(||serde_json::json!(fact.recorded_at)),
                    "binding":old_claim.map(|r|r.body["binding"].clone()).unwrap_or(Value::Null),
                    "stock_obligations":value.get("stock_obligations").cloned().unwrap_or_else(||serde_json::json!([]))}),
            )];
            if !active {
                let released: Vec<_> = claim
                    .allocations
                    .iter()
                    .filter(|a| !claim.observed.iter().any(|d| d.pool == a.pool && d.stock))
                    .cloned()
                    .collect();
                rows.push(make(
                    "release",
                    fact.event_id.to_string(),
                    serde_json::json!({"allocations":released}),
                ));
                for allocation in &released {
                    rows.push(make(
                        "motion",
                        allocation.pool.clone(),
                        serde_json::json!({"last_release":fact.recorded_at}),
                    ));
                }
            }
            Ok(rows)
        }
        crate::resource_leases::IDENTITY_BOUND
        | crate::resource_leases::LEASE_EXPIRED
        | crate::resource_leases::STOCK_RETURNED => {
            use crate::resource_leases as leases;
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let old = old_claim.ok_or_else(|| unread("lease fact has no acquisition basis"))?;
            let claim: Claim = serde_json::from_value(old.body["claim"].clone())
                .map_err(|e| unread(e.to_string()))?;
            claim.validate().map_err(unread)?;
            let _actor = value["_actor"]
                .as_str()
                .ok_or_else(|| unread("lease fact lacks attribution"))?;
            if value["job_id"] != serde_json::json!(claim.job_id)
                || value["step_id"] != serde_json::json!(claim.step_id)
            {
                return Err(unread("lease fact changed packet/step identity"));
            }
            let mut body = old.body.clone();
            // An existing pre-lifecycle row's version time is still its
            // reservation time. Capture it before making the first version.
            if body.get("reserved_at").is_none() {
                body["reserved_at"] = serde_json::json!(old.at);
            }
            let mut extra = Vec::new();
            if fact.kind == leases::STOCK_RETURNED {
                let after: leases::StockObligation =
                    serde_json::from_value(value["obligation"].clone())
                        .map_err(|e| unread(e.to_string()))?;
                let evidence: uuid::Uuid = serde_json::from_value(value["evidence"].clone())
                    .map_err(|e| unread(e.to_string()))?;
                let mut stock = obligations(&body)?;
                let before = stock
                    .iter_mut()
                    .find(|o| o.allocation.pool == after.allocation.pool)
                    .ok_or_else(|| unread("return lacks its retained obligation"))?;
                let mut expected = before.clone();
                expected.returned_by = Some(evidence);
                if before.returned_by.is_some()
                    || before.identity.is_none()
                    || evidence.is_nil()
                    || after != expected
                    || after.claim != claim
                {
                    return Err(unread(
                        "return changed its identity/quantity/evidence or repeats discharge",
                    ));
                }
                *before = after.clone();
                body["stock_obligations"] = serde_json::json!(stock);
                extra.push(make(
                    "release",
                    fact.event_id.to_string(),
                    serde_json::json!({"allocations":[after.allocation]}),
                ));
                extra.push(make(
                    "motion",
                    after.allocation.pool.clone(),
                    serde_json::json!({"last_release":fact.recorded_at}),
                ));
            } else {
                if old.body["active"] != true || value["claim"] != serde_json::json!(claim) {
                    return Err(unread("lease fact changed its active acquisition"));
                }
                let observed: DateTime<Utc> = serde_json::from_value(value["observed_at"].clone())
                    .map_err(|e| unread(e.to_string()))?;
                if observed.timestamp_micros() != fact.event_at.timestamp_micros() {
                    return Err(unread("lease observation clock changed"));
                }
                if fact.kind == leases::IDENTITY_BOUND {
                    let binding: leases::IdentityBinding =
                        serde_json::from_value(value["binding"].clone())
                            .map_err(|e| unread(e.to_string()))?;
                    // Match the authoritative replay: the command already
                    // validated its holder; attribution may name its delegate.
                    leases::validate_binding(&claim, true, &claim.holder, &binding, observed)
                        .map_err(|e| unread(e.to_string()))?;
                    if !body["binding"].is_null() {
                        return Err(unread("acquisition identity bound twice"));
                    }
                    body["binding"] = serde_json::json!(binding);
                } else {
                    if claim.lease_until.is_none_or(|until| until > observed) {
                        return Err(unread("expiry precedes frozen lease deadline"));
                    }
                    let stock: Vec<leases::StockObligation> =
                        serde_json::from_value(value["stock_obligations"].clone())
                            .map_err(|e| unread(e.to_string()))?;
                    let binding: Option<leases::IdentityBinding> =
                        serde_json::from_value(body["binding"].clone())
                            .map_err(|e| unread(e.to_string()))?;
                    let expected: Vec<_> = claim
                        .allocations
                        .iter()
                        .filter(|a| claim.observed.iter().any(|d| d.pool == a.pool && d.stock))
                        .cloned()
                        .collect();
                    if stock
                        .iter()
                        .map(|o| o.allocation.clone())
                        .collect::<Vec<_>>()
                        != expected
                        || stock.iter().any(|o| {
                            o.claim != claim
                                || o.returned_by.is_some()
                                || o.identity != binding.as_ref().map(|b| b.identity.clone())
                                || o.reclaimer.trim().is_empty()
                        })
                    {
                        return Err(unread("expiry did not conserve its physical stock"));
                    }
                    body["active"] = serde_json::json!(false);
                    body["expired"] = serde_json::json!(true);
                    body["stock_obligations"] = serde_json::json!(stock);
                    let flow: Vec<_> = claim
                        .allocations
                        .iter()
                        .filter(|a| !expected.contains(a))
                        .cloned()
                        .collect();
                    extra.push(make(
                        "release",
                        fact.event_id.to_string(),
                        serde_json::json!({"allocations":flow}),
                    ));
                    for allocation in flow {
                        extra.push(make(
                            "motion",
                            allocation.pool,
                            serde_json::json!({"last_release":fact.recorded_at}),
                        ));
                    }
                }
            }
            extra.push(make("acquisition", claim.id.to_string(), body));
            Ok(extra)
        }
        crate::flow_resources::REFUSED | crate::events::STATION_RESOURCE_REFUSED => {
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let (step, reason) = if fact.kind == crate::events::STATION_RESOURCE_REFUSED {
                let receipt: Value = serde_json::from_str(
                    value["receipt_json"]
                        .as_str()
                        .ok_or_else(|| unread("station refusal has no canonical receipt"))?,
                )
                .map_err(|e| unread(e.to_string()))?;
                if receipt["outcome"] != "resource-refused" {
                    return Err(unread("station refusal outcome differs"));
                }
                (receipt["refused_step"].clone(), receipt["refusal"].clone())
            } else {
                if value["job_id"] != value["step"]["job_id"]
                    || value["step_id"] != value["step"]["id"]
                {
                    return Err(unread("refusal packet/step identity differs"));
                }
                (value["step"].clone(), value["reason"].clone())
            };
            let reason: crate::flow_resources::Refusal =
                serde_json::from_value(reason).map_err(|e| unread(e.to_string()))?;
            if crate::flow_resources::plan(&reason.observed).err().as_ref() != Some(&reason) {
                return Err(unread("refusal differs from its complete decision vector"));
            }
            let step_id = step["id"]
                .as_str()
                .ok_or_else(|| unread("refusal has no step identity"))?;
            let job = step["job_id"]
                .as_str()
                .ok_or_else(|| unread("refusal has no packet identity"))?;
            if step["status"] != "ready"
                || step["metadata"]["waiting_on"]["resource"] != reason.pool
                || step["metadata"]["waiting_on"]["reason"] != reason.reason
            {
                return Err(unread("refusal differs from its waiting step"));
            }
            let group = fact.original_group.as_ref().ok_or_else(|| {
                unread("refusal attempt identity is unproven without its original transaction")
            })?;
            Ok(vec![make(
                "refusal",
                format!("{}:{step_id}", group.reference.id),
                serde_json::json!({"job_id":job,"step_id":step_id,"reason":reason}),
            )])
        }
        _ if unknown_resource(fact)? => {
            let previous: Option<UnknownKind> = old_unknown
                .map(|r| serde_json::from_value(r.body.clone()).map_err(|e| unread(e.to_string())))
                .transpose()?;
            let summary = UnknownKind {
                kind: fact.kind.clone(),
                first_seq: previous.as_ref().map_or(fact.seq, |p| p.first_seq),
                count: previous.map_or(Ok(1), |p| {
                    p.count
                        .checked_add(1)
                        .ok_or_else(|| unread("unknown resource count overflow"))
                })?,
            };
            Ok(vec![make(
                "unknown-resource",
                fact.kind.clone(),
                serde_json::to_value(summary).map_err(|e| unread(e.to_string()))?,
            )])
        }
        _ => Ok(vec![]),
    }
}

pub(super) fn reading(
    rows: Vec<Row>,
    cut: i64,
    at: DateTime<Utc>,
    hours: i64,
    now: DateTime<Utc>,
    mut projection: ProjectionBasis,
) -> Result<ClaimsReading, ClaimsError> {
    projection.unknown_kinds = rows
        .iter()
        .filter(|r| r.kind == "unknown-resource")
        .map(|r| serde_json::from_value(r.body.clone()).map_err(|e| unread(e.to_string())))
        .collect::<Result<Vec<UnknownKind>, _>>()?;
    projection.unknown_kinds.sort_by(|a, b| a.kind.cmp(&b.kind));
    let mut pools = Vec::new();
    let mut claims = Vec::new();
    let mut waiting = Vec::new();
    let mut other_waits = BTreeMap::<&str, u64>::new();
    let terminal: BTreeMap<_, _> = rows
        .iter()
        .filter(|r| r.kind == "job")
        .map(|r| {
            (
                r.key.clone(),
                (
                    matches!(r.body["status"].as_str(), Some("closed" | "cancelled")),
                    &r.body,
                ),
            )
        })
        .collect();
    for row in &rows {
        if row.kind == "pool" {
            let pool: PoolSpec = serde_json::from_value(row.body["declaration"].clone())
                .map_err(|e| unread(e.to_string()))?;
            if pool.status != crate::registry::WorkflowStatus::Active {
                continue;
            }
            let unknown = matches!(pool.capacity, crate::pools::PoolCapacity::Unknown {});
            pools.push(PoolReading {
                pool_key: pool.name.clone(),
                pool: Some(pool),
                reserved: Amount::zero(),
                failed_reservations: 0,
                releases: 0,
                state: if unknown {
                    RegionState::Attention
                } else {
                    RegionState::Clear
                },
                band: unknown.then(|| "capacity-unknown".into()),
            });
        }
        if row.kind != "step" {
            continue;
        }
        let job = row.body["job_id"]
            .as_str()
            .ok_or_else(|| unread("materialized step has no job"))?;
        let (closed, packet) = terminal
            .get(job)
            .ok_or_else(|| unread("materialized step has no packet basis"))?;
        if *closed {
            continue;
        }
        if row.body["status"] == "active" {
            let acquisition = if row.body["resource_claim"].is_null() {
                None
            } else {
                let claim: Claim = serde_json::from_value(row.body["resource_claim"].clone())
                    .map_err(|e| unread(e.to_string()))?;
                claim.validate().map_err(unread)?;
                let reserved = rows
                    .iter()
                    .find(|r| r.kind == "acquisition" && r.key == claim.id.to_string())
                    .ok_or_else(|| unread("active step has no reservation basis"))?;
                if (reserved.body["active"] != true && reserved.body["expired"] != true)
                    || reserved.body["claim"] != serde_json::json!(claim)
                    || row.key != claim.step_id.to_string()
                    || job != claim.job_id.to_string()
                    || row.body["holder"] != claim.holder
                {
                    return Err(unread(
                        "active step differs from reserved generation/holder",
                    ));
                }
                Some(claim)
            };
            let reserved = acquisition.as_ref().and_then(|c| {
                rows.iter()
                    .find(|r| r.kind == "acquisition" && r.key == c.id.to_string())
            });
            let expired = reserved.is_some_and(|r| r.body["expired"] == true);
            let binding = reserved
                .map(|r| {
                    serde_json::from_value(r.body["binding"].clone())
                        .map_err(|e| unread(e.to_string()))
                })
                .transpose()?
                .flatten();
            let stock = reserved
                .map(|r| obligations(&r.body))
                .transpose()?
                .unwrap_or_default();
            let remaining = acquisition
                .as_ref()
                .and_then(|c| c.lease_until)
                .map(|until| {
                    if expired {
                        0
                    } else {
                        until.signed_duration_since(at).num_seconds().max(0)
                    }
                });
            let run: Option<String> = serde_json::from_value(row.body["run"].clone())
                .map_err(|e| unread(format!("recorded run edge: {e}")))?;
            let packet_opened_at: Option<DateTime<Utc>> =
                serde_json::from_value(packet["packet_opened_at"].clone())
                    .map_err(|e| unread(format!("packet opening: {e}")))?;
            let station_receipt = rows
                .iter()
                .find(|r| r.kind == "station-claim" && r.key == row.key);
            // A grouped or billed claim binds its receipt by the original
            // transaction or the resource claim. A claim with NEITHER, on
            // either side — every unbilled per-step claim (backlog
            // 971a6190) — binds it by the audit sequence of the fact that
            // started this activation. Only the per-step door's own claim
            // fact writes a receipt naming that sequence, and it names its
            // own, so a receipt left by an earlier activation of the same
            // step by the same holder names an earlier sequence and can
            // never attach to a later claim; a `jobs.station.claimed`
            // receipt names none and keeps the rule above.
            let unbound = |body: &Value, claim: &str| {
                body["activation_group"].is_null() && body[claim].is_null()
            };
            let matched_station = station_receipt.filter(|receipt| {
                receipt.body["job_id"] == row.body["job_id"]
                    && receipt.body["holder"] == row.body["holder"]
                    && ((!row.body["activation_group"].is_null()
                        && receipt.body["activation_group"] == row.body["activation_group"])
                        || (!row.body["resource_claim"].is_null()
                            && receipt.body["claim"] == row.body["resource_claim"])
                        || (unbound(&row.body, "resource_claim")
                            && unbound(&receipt.body, "claim")
                            && !row.body["activation_seq"].is_null()
                            && receipt.body["activation_seq"] == row.body["activation_seq"]))
            });
            let station = matched_station
                .map(|receipt| {
                    serde_json::from_value::<String>(receipt.body["station"].clone())
                        .map_err(|e| unread(e.to_string()))
                })
                .transpose()?;
            let missing = |body: &Value, key: &str| {
                if body.get(key).is_some() {
                    "not-recorded"
                } else {
                    "history-not-rebuilt"
                }
                .to_owned()
            };
            let holder = row.body["holder"].as_str().map(str::to_owned);
            let (executor_kind, executor_kind_basis) =
                executor_kind_of(&row.body, holder.as_deref())?;
            claims.push(OpenClaim {
                step_id: row.key.clone(),
                job_id: job.into(),
                holder,
                executor_kind,
                executor_kind_basis,
                run_unavailable: run.is_none().then(|| missing(&row.body, "run")),
                run,
                station_unavailable: station.is_none().then(|| {
                    if station_receipt.is_some() {
                        "unmatched-claim-receipt".to_owned()
                    } else {
                        missing(&row.body, "run")
                    }
                }),
                station,
                station_id: matched_station
                    .and_then(|receipt| receipt.body["station_id"].as_str().map(str::to_owned)),
                station_resolution: matched_station.and_then(|receipt| {
                    receipt.body["station_resolution"]
                        .as_str()
                        .map(str::to_owned)
                }),
                station_resolution_reason: matched_station.and_then(|receipt| {
                    receipt.body["station_resolution_reason"]
                        .as_str()
                        .map(str::to_owned)
                }),
                packet_opened_at_unavailable: packet_opened_at
                    .is_none()
                    .then(|| missing(packet, "packet_opened_at")),
                packet_opened_at,
                allocations: acquisition
                    .as_ref()
                    .filter(|_| !expired)
                    .map(|c| c.allocations.clone())
                    .unwrap_or_default(),
                resource_acquisition: acquisition,
                since: serde_json::from_value(row.body["since"].clone())
                    .map_err(|e| unread(e.to_string()))?,
                lease_remaining: remaining,
                lease_unavailable: if remaining.is_some() {
                    String::new()
                } else {
                    LEASES_UNAVAILABLE.into()
                },
                identity_binding: binding,
                lease_expired: expired,
                stock_obligations: stock,
            });
        }
        if row.body["status"] == "ready" && !row.body["waiting_on"].is_null() {
            let wait = &row.body["waiting_on"];
            if let (Some(pool), Some(reason)) = (
                wait["resource"].as_str().filter(|s| !s.trim().is_empty()),
                wait["reason"].as_str().filter(|s| !s.trim().is_empty()),
            ) {
                waiting.push(WaitingOn {
                    step_id: row.key.clone(),
                    job_id: job.into(),
                    pool: pool.into(),
                    reason: reason.into(),
                });
            } else if let Some(wait) = crate::step_wait::Declaration::parse(wait) {
                let kind = match wait.on {
                    crate::step_wait::Target::Packet => "packet",
                    crate::step_wait::Target::Person => "person",
                };
                let count = other_waits.entry(kind).or_default();
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| unread("wait count overflow"))?;
            } else {
                // A malformed wait is evidence about ONE row, not an inability
                // to answer every other row in this bounded historical basis.
                projection.wait_attention.push(WaitAttention {
                    kind: "malformed-waiting-on".into(),
                    job_id: job.into(),
                    step_id: row.key.clone(),
                    reason:
                        "expected resource/reason or the structured packet/person wait declaration"
                            .into(),
                });
            }
        }
    }
    for reserved in rows
        .iter()
        .filter(|r| r.kind == "acquisition" && r.body["active"] == true)
    {
        if !claims.iter().any(|c| {
            c.resource_acquisition
                .as_ref()
                .is_some_and(|claim| claim.id.to_string() == reserved.key)
        }) {
            return Err(unread(
                "outstanding reservation has no matching active step/packet basis",
            ));
        }
    }
    let mut demanded: BTreeSet<String> = claims
        .iter()
        .flat_map(|c| c.allocations.iter().map(|a| a.pool.clone()))
        .collect();
    let retained_stock: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == "acquisition")
        .map(|r| obligations(&r.body))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter(|o| o.returned_by.is_none())
        .collect();
    demanded.extend(retained_stock.iter().map(|o| o.allocation.pool.clone()));
    for metric in &rows {
        if metric.kind == "metrics" {
            demanded.insert(metric.key.clone());
        }
        if metric.kind == "refusal" && metric.at >= at - chrono::Duration::hours(hours) {
            demanded.insert(
                metric.body["reason"]["pool"]
                    .as_str()
                    .ok_or_else(|| unread("refusal pool missing"))?
                    .into(),
            );
        }
    }
    for key in demanded {
        if !pools.iter().any(|p| p.pool_key == key) {
            pools.push(PoolReading {
                pool_key: key,
                pool: None,
                reserved: Amount::zero(),
                failed_reservations: 0,
                releases: 0,
                state: RegionState::Attention,
                band: Some("pool-declaration-unavailable".into()),
            });
        }
    }
    for pool in &mut pools {
        pool.reserved = Amount::sum(
            claims
                .iter()
                .flat_map(|c| &c.allocations)
                .filter(|a| a.pool == pool.pool_key)
                .map(|a| &a.amount)
                .chain(
                    retained_stock
                        .iter()
                        .filter(|o| o.allocation.pool == pool.pool_key)
                        .map(|o| &o.allocation.amount),
                ),
        )
        .map_err(unread)?;
        pool.releases = rows
            .iter()
            .filter(|r| {
                r.kind == "release"
                    && r.at >= at - chrono::Duration::hours(hours)
                    && r.body["allocations"]
                        .as_array()
                        .is_some_and(|xs| xs.iter().any(|x| x["pool"] == pool.pool_key))
            })
            .count() as u64;
        pool.failed_reservations = rows
            .iter()
            .filter(|r| {
                r.kind == "refusal"
                    && r.at >= at - chrono::Duration::hours(hours)
                    && r.body["reason"]["pool"] == pool.pool_key
            })
            .count() as u64;
        for metric in rows
            .iter()
            .filter(|r| r.kind == "metrics" && r.key == pool.pool_key)
        {
            pool.releases = pool
                .releases
                .checked_add(
                    metric.body["releases"]
                        .as_u64()
                        .ok_or_else(|| unread("invalid release count"))?,
                )
                .ok_or_else(|| unread("release count overflow"))?;
            pool.failed_reservations = pool
                .failed_reservations
                .checked_add(
                    metric.body["refusals"]
                        .as_u64()
                        .ok_or_else(|| unread("invalid refusal count"))?,
                )
                .ok_or_else(|| unread("refusal count overflow"))?;
        }
        if let Some(PoolSpec {
            capacity: crate::pools::PoolCapacity::Configured { amount },
            ..
        }) = &pool.pool
        {
            let capacity = amount.amount().map_err(unread)?;
            let what = format!(
                "{} of {} reserved in {}",
                pool.reserved, capacity, pool.pool_key
            );
            let finding = if pool.reserved > capacity {
                Some(crate::region_states::Finding::new(
                    crate::region_states::OVER_CAPACITY,
                    None,
                    what.clone(),
                    format!("{what}: the declared capacity did not hold"),
                ))
            } else if !pool.reserved.is_zero() && pool.reserved == capacity {
                let declaration = rows
                    .iter()
                    .find(|r| r.kind == "pool" && r.key == pool.pool_key)
                    .ok_or_else(|| unread("pool basis disappeared"))?;
                let capacity_since: DateTime<Utc> =
                    serde_json::from_value(declaration.body["capacity_since"].clone())
                        .map_err(|e| unread(e.to_string()))?;
                // The audit cut/retention/motion basis is recorded wall time.
                // A domain event timestamp may be simulation time; keep that
                // public claim.since fact, but never use it as a wall timer.
                let mut filled = capacity_since;
                for row in rows.iter().filter(|r| r.kind == "acquisition") {
                    let active = row.body["active"] == true
                        && row.body["claim"]["allocations"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|a| a["pool"] == pool.pool_key));
                    let charged_stock = obligations(&row.body)?
                        .iter()
                        .any(|o| o.returned_by.is_none() && o.allocation.pool == pool.pool_key);
                    if active || charged_stock {
                        // Older rows predate lifecycle support and their at is
                        // still the original reservation. New versions carry
                        // that clock explicitly; at remains the version cut.
                        let reserved_at = row.body.get("reserved_at").map_or(Ok(row.at), |v| {
                            serde_json::from_value(v.clone()).map_err(|e| unread(e.to_string()))
                        })?;
                        filled = filled.max(reserved_at);
                    }
                }
                let last = rows
                    .iter()
                    .find(|r| r.kind == "motion" && r.key == pool.pool_key)
                    .map(|r| {
                        serde_json::from_value::<DateTime<Utc>>(r.body["last_release"].clone())
                            .map_err(|e| unread(e.to_string()))
                    })
                    .transpose()?;
                let crossings = usize::try_from(pool.releases)
                    .map_err(|_| unread("release count exceeds native capacity"))?;
                crate::region_states::capacity_motion(
                    &what,
                    Some(filled),
                    &[crate::region_states::OutRail {
                        name: format!("{} releases", pool.pool_key),
                        crossings: Some(crossings),
                        last,
                    }],
                    hours,
                    at,
                )
            } else if pool.failed_reservations > 0
                && pool.releases == 0
                && waiting.iter().any(|w| w.pool == pool.pool_key)
            {
                Some(crate::region_states::Finding::new(
                    crate::region_states::RESOURCE_NO_PROGRESS,
                    None,
                    format!(
                        "{} failed reservations in{hours}h, zero releases",
                        pool.failed_reservations
                    ),
                    format!(
                        "{} has a waiting reservation and no observed release",
                        pool.pool_key
                    ),
                ))
            } else {
                None
            };
            if let Some(finding) = finding {
                pool.state = finding.band.state;
                pool.band = Some(finding.band.id.into());
            }
        }
    }
    if !projection.unknown_kinds.is_empty() {
        for pool in &mut pools {
            pool.state = RegionState::Attention;
            pool.band = Some("unknown-resource-kind".into());
        }
    }
    pools.sort_by(|a, b| a.pool_key.cmp(&b.pool_key));
    projection.other_waits = other_waits
        .into_iter()
        .map(|(kind, count)| OtherWait {
            kind: kind.into(),
            count,
        })
        .collect();
    projection.wait_attention.sort_by(|a, b| {
        a.job_id
            .cmp(&b.job_id)
            .then_with(|| a.step_id.cmp(&b.step_id))
    });
    claims.sort_by(|a, b| a.step_id.cmp(&b.step_id));
    waiting.sort_by(|a, b| a.step_id.cmp(&b.step_id));
    let binding = super::binding(&pools).map(|p| p.pool_key.clone());
    let result = ClaimsReading {
        projection,
        as_of: cut,
        as_of_at: at,
        retained_from: now - chrono::Duration::hours(RETENTION_HOURS),
        retention_hours: RETENTION_HOURS,
        window_hours: hours,
        claims,
        waiting_on: waiting,
        pools,
        binding,
    };
    check_bytes(&result)?;
    Ok(result)
}

// Match the Pg selection/aggregation on the same port. Historical metrics are
// counted from immutable attempt identities, not copied wholesale into output.
fn reference_rows(
    rows: Vec<&Row>,
    cut: i64,
    at: DateTime<Utc>,
    hours: i64,
) -> Result<Vec<Row>, ClaimsError> {
    let visible: BTreeSet<_> = rows
        .iter()
        .filter(|r| {
            r.kind == "step"
                && (r.body["status"] == "active"
                    || (r.body["status"] == "ready" && !r.body["waiting_on"].is_null()))
        })
        .map(|r| r.key.as_str())
        .collect();
    if visible.len() > 4096 {
        return Err(unread("claims projection read exceeds4096row bound"));
    }
    let jobs: BTreeSet<_> = rows
        .iter()
        .filter(|r| r.kind == "step" && visible.contains(r.key.as_str()))
        .filter_map(|r| r.body["job_id"].as_str())
        .collect();
    let acquisitions: BTreeSet<_> = rows
        .iter()
        .filter(|r| r.kind == "step" && visible.contains(r.key.as_str()))
        .filter_map(|r| r.body["resource_claim"]["id"].as_str())
        .collect();
    let selected: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| {
            matches!(r.kind.as_str(), "pool" | "motion" | "unknown-resource")
                || (r.kind == "step" && visible.contains(r.key.as_str()))
                || (r.kind == "station-claim" && visible.contains(r.key.as_str()))
                || (r.kind == "job" && jobs.contains(r.key.as_str()))
                || (r.kind == "acquisition"
                    && (r.body["active"] == true
                        || has_stock(&r.body)
                        || acquisitions.contains(r.key.as_str())))
        })
        .collect();
    let mut metrics: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r.at >= at - chrono::Duration::hours(hours))
    {
        if row.kind == "refusal" {
            let key = row.body["reason"]["pool"]
                .as_str()
                .ok_or_else(|| unread("refusal pool missing"))?;
            let count = &mut metrics.entry(key).or_default().1;
            *count = count
                .checked_add(1)
                .ok_or_else(|| unread("refusal count overflow"))?;
        }
        if row.kind == "release" {
            for allocation in row.body["allocations"]
                .as_array()
                .ok_or_else(|| unread("release vector missing"))?
            {
                let key = allocation["pool"]
                    .as_str()
                    .ok_or_else(|| unread("release pool missing"))?;
                let count = &mut metrics.entry(key).or_default().0;
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| unread("release count overflow"))?;
            }
        }
    }
    if metrics.len() > 4096
        || metrics
            .keys()
            .try_fold(0usize, |n, key| n.checked_add(key.len()))
            .is_none_or(|n| n > MAX_READING_BYTES)
    {
        return Err(unread("materialized byte bound or pool row bound exceeded"));
    }
    let metrics: Vec<_> = metrics
        .into_iter()
        .map(|(key, (releases, refusals))| Row {
            kind: "metrics".into(),
            key: key.into(),
            seq: cut,
            at,
            body: serde_json::json!({"releases":releases,"refusals":refusals}),
        })
        .collect();
    check_rows(selected.iter().copied().chain(metrics.iter()))?;
    Ok(selected.into_iter().cloned().chain(metrics).collect())
}

impl InMemoryClaimsHistory {
    pub(super) fn apply_batch(
        &self,
        expected: Option<ProjectionCursor>,
        batch: &HistoryBatch,
        source: Option<SourceObservation>,
    ) -> Result<(), ClaimsError> {
        if batch.through < 0 || batch.facts.len() > MAX_BATCH {
            return Err(invalid("negative cut or batch exceeds1024 facts"));
        }
        let bytes = batch.facts.iter().try_fold(0usize, |n, f| {
            n.checked_add(f.payload.len())
                .ok_or_else(|| invalid("batch bytes overflow"))
        })?;
        if bytes > MAX_BATCH_BYTES {
            return Err(invalid("batch exceeds8MiB"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        let cursor = ProjectionCursor {
            generation: state.generation,
            through: state.head.map_or(0, |(seq, _)| seq),
        };
        if expected.is_some_and(|expected| expected != cursor) {
            return Err(invalid("prepared projection cursor/generation changed"));
        }
        if source.is_some_and(|source| {
            source.audit_head < batch.through || source.observed_at < batch.through_at
        }) {
            return Err(invalid(
                "source observation differs from its projection prefix",
            ));
        }
        let mut staged = Vec::new();
        let mut overrides: BTreeMap<(String, String), Row> = BTreeMap::new();
        let mut previous = 0;
        let mut encoded = Vec::new();
        let mut groups = BTreeMap::new();
        let mut event_ids = BTreeSet::new();
        if state
            .head
            .is_some_and(|(head, at)| batch.through < head || (head > 0 && batch.through_at < at))
        {
            return Err(invalid("projection horizon cannot move backwards"));
        }
        for fact in &batch.facts {
            if fact.seq <= previous
                || fact.seq > batch.through
                || fact.recorded_at > batch.through_at
                || fact.event_id.is_nil()
            {
                return Err(invalid("unordered fact or inconsistent horizon"));
            }
            previous = fact.seq;
            if !event_ids.insert(fact.event_id) {
                return Err(invalid(
                    "one event identity appears at multiple batch sequences",
                ));
            }
            let fingerprint = serde_json::to_string(fact).map_err(|e| invalid(e.to_string()))?;
            if let Some(known) = state.fingerprints.get(&fact.seq) {
                if *known != fingerprint {
                    return Err(invalid("an audit sequence changed identity or payload"));
                }
                continue;
            }
            if state.head.is_some_and(|(head, _)| fact.seq <= head)
                || state.events.contains_key(&fact.event_id)
            {
                return Err(invalid(
                    "late fact or duplicate event identity at another sequence",
                ));
            }
            if let Some(group) = &fact.original_group {
                if !group.validate(batch.through).map_err(invalid)? {
                    return Err(unread("incomplete original transaction"));
                }
                if !group
                    .event_ids
                    .iter()
                    .zip(&group.audit_sequences)
                    .any(|(id, seq)| *id == fact.event_id && *seq == Some(fact.seq))
                {
                    return Err(invalid("fact differs from its original transaction roster"));
                }
                let lo = group
                    .audit_sequences
                    .iter()
                    .flatten()
                    .copied()
                    .min()
                    .ok_or_else(|| invalid("empty original group"))?;
                let hi = group
                    .audit_sequences
                    .iter()
                    .flatten()
                    .copied()
                    .max()
                    .ok_or_else(|| invalid("empty original group"))?;
                let evidence = serde_json::to_string(group).map_err(|e| invalid(e.to_string()))?;
                if state
                    .group_evidence
                    .get(&group.reference.id)
                    .is_some_and(|old| *old != evidence)
                    || groups
                        .get(&group.reference.id)
                        .is_some_and(|(_, old): &((i64, i64), String)| *old != evidence)
                {
                    return Err(invalid("original transaction evidence changed"));
                }
                groups.insert(group.reference.id, ((lo, hi), evidence));
            }
            let value: Value =
                serde_json::from_str(&fact.payload).map_err(|e| unread(e.to_string()))?;
            let key = (
                "step".to_owned(),
                value["id"].as_str().unwrap_or("").to_owned(),
            );
            let old = overrides.get(&key).or_else(|| state.current.get(&key));
            let claim_key = ("acquisition".to_owned(), acquisition_key(&value));
            let old_claim = overrides
                .get(&claim_key)
                .or_else(|| state.current.get(&claim_key));
            let pool_key = (
                "pool".to_owned(),
                value["name"].as_str().unwrap_or("").to_owned(),
            );
            let old_pool = overrides
                .get(&pool_key)
                .or_else(|| state.current.get(&pool_key));
            let unknown_key = ("unknown-resource".to_owned(), fact.kind.clone());
            let old_unknown = overrides
                .get(&unknown_key)
                .or_else(|| state.current.get(&unknown_key));
            for row in projected_rows(fact, old, old_claim, old_pool, old_unknown)? {
                let key = (row.kind.clone(), row.key.clone());
                if row.kind == "refusal"
                    && overrides
                        .get(&key)
                        .or_else(|| state.current.get(&key))
                        .is_some_and(|old| old.body != row.body)
                {
                    return Err(unread(
                        "the two refusal doors disagree on one original attempt",
                    ));
                }
                overrides.insert(key, row.clone());
                staged.push(row)
            }
            encoded.push((fact.seq, fact.event_id, fingerprint, fact.recorded_at));
        }
        for (seq, id, hash, at) in encoded {
            state.fingerprints.insert(seq, hash);
            state.events.insert(id, seq);
            state.cuts.insert(seq, at);
        }
        for row in staged {
            state
                .current
                .insert((row.kind.clone(), row.key.clone()), row.clone());
            state.rows.push(row)
        }
        for (id, (bounds, evidence)) in groups {
            state.groups.insert(id, bounds);
            state.group_evidence.insert(id, evidence);
        }
        state.cuts.insert(batch.through, batch.through_at);
        state.head = Some((batch.through, batch.through_at));
        state.source = source.or(state.source);
        if source.is_some() {
            state.source_failed = None;
        }
        Ok(())
    }
}
#[async_trait]
impl ClaimsHistory for InMemoryClaimsHistory {
    /// Advanced from the audit log, an in-memory history is a fold fixture
    /// beside a Postgres one (flow_resource_claims.rs compares the two), so
    /// its tail read takes the same fixture budget from the same stated
    /// stall; production reaches it only as the Postgres adapter's reducer,
    /// which reads no source (backlog ebbf38c6).
    #[cfg(feature = "test-support")]
    fn source_deadline(&self) -> tokio::time::Instant {
        crate::fixture_budget::after_the_measured_stall() + crate::fixture_budget::FIXTURE_BUDGET
    }
    async fn prune(
        &self,
        expected: ProjectionCursor,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, ClaimsError> {
        if !(1..=MAX_BATCH).contains(&limit) {
            return Err(invalid("retention pass must delete1..1024rows"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        if expected
            != (ProjectionCursor {
                generation: state.generation,
                through: state.head.map_or(0, |(s, _)| s),
            })
        {
            return Err(invalid("prune cursor changed"));
        }
        let floor = now - chrono::Duration::hours(RETENTION_HOURS);
        let metrics_floor = floor - chrono::Duration::hours(crate::regions::MAX_WINDOW_HOURS);
        let mut deleted = 0;
        while deleted < limit {
            let old_fact = state
                .fingerprints
                .keys()
                .copied()
                .find(|seq| state.cuts.get(seq).is_some_and(|at| *at < floor));
            if let Some(seq) = old_fact {
                state.fingerprints.remove(&seq);
                state.events.retain(|_, s| *s != seq);
                deleted += 1;
                continue;
            }
            let old_cut = state
                .cuts
                .iter()
                .find(|(_, at)| **at < floor)
                .map(|(s, _)| *s);
            if let Some(seq) = old_cut {
                state.cuts.remove(&seq);
                state.pruned_through = state.pruned_through.max(seq);
                deleted += 1;
                continue;
            }
            let old_group = state
                .groups
                .iter()
                .find(|(_, (_, hi))| *hi <= state.pruned_through)
                .map(|(id, _)| *id);
            if let Some(id) = old_group {
                state.groups.remove(&id);
                state.group_evidence.remove(&id);
                deleted += 1;
                continue;
            }
            let obsolete = |r: &Row, current: &BTreeMap<(String, String), Row>| -> bool {
                match r.kind.as_str() {
                    "step" | "station-claim" => r.body["job_id"].as_str().is_some_and(|id| {
                        current.get(&("job".into(), id.into())).is_some_and(|j| {
                            j.at < floor
                                && matches!(j.body["status"].as_str(), Some("closed" | "cancelled"))
                        })
                    }),
                    "job" => {
                        current
                            .get(&(r.kind.clone(), r.key.clone()))
                            .is_some_and(|j| {
                                j.at < floor
                                    && matches!(
                                        j.body["status"].as_str(),
                                        Some("closed" | "cancelled")
                                    )
                            })
                            && !current.values().any(|s| {
                                matches!(s.kind.as_str(), "step" | "station-claim")
                                    && s.body["job_id"] == r.key
                            })
                    }
                    "acquisition" => {
                        current
                            .get(&(r.kind.clone(), r.key.clone()))
                            .is_some_and(|a| {
                                a.at < floor && a.body["active"] == false && !has_stock(&a.body)
                            })
                    }
                    _ => false,
                }
            };
            let old_row = state.rows.iter().position(|r| {
                if matches!(r.kind.as_str(), "release" | "refusal") {
                    r.at < metrics_floor
                } else {
                    r.at < floor
                        && (obsolete(r, &state.current)
                            || state.rows.iter().any(|new| {
                                new.kind == r.kind
                                    && new.key == r.key
                                    && new.seq > r.seq
                                    && new.at <= floor
                            }))
                }
            });
            if let Some(index) = old_row {
                state.rows.remove(index);
                deleted += 1;
                continue;
            }
            let old_current = state
                .current
                .iter()
                .find(|(_, r)| {
                    if matches!(r.kind.as_str(), "release" | "refusal") {
                        r.at < metrics_floor
                    } else {
                        r.at < floor
                            && obsolete(r, &state.current)
                            && !state
                                .rows
                                .iter()
                                .any(|row| row.kind == r.kind && row.key == r.key)
                    }
                })
                .map(|(key, _)| key.clone());
            if let Some(key) = old_current {
                state.current.remove(&key);
                deleted += 1;
                continue;
            }
            break;
        }
        Ok(deleted)
    }
    async fn cuts(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        buckets: usize,
        now: DateTime<Utc>,
    ) -> Result<CutsReading, ClaimsError> {
        let (from, to) = cut_range(from, to, buckets, now)?;
        let state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        let (head, head_at) = state
            .head
            .ok_or_else(|| unread("no observed materialized horizon"))?;
        let floor = now - chrono::Duration::hours(RETENTION_HOURS);
        // What `read(Some(seq))` answers: recorded, after the pruned horizon
        // (the head always), inside retention, not bisecting a transaction.
        let answerable = |seq: i64, at: DateTime<Utc>| {
            (seq > state.pruned_through || seq == head)
                && seq <= head
                && at >= floor
                && !state
                    .groups
                    .values()
                    .any(|(lo, hi)| *lo <= seq && seq < *hi)
        };
        let first_retained = state
            .cuts
            .iter()
            .find(|(seq, at)| answerable(**seq, **at))
            .map(|(seq, at)| Cut { seq: *seq, at: *at })
            .ok_or_else(|| {
                ClaimsError::OutsideRetention("no retained cut inside 168 hours".into())
            })?;
        let in_range: Vec<Cut> = state
            .cuts
            .iter()
            .filter(|(seq, at)| answerable(**seq, **at) && **at >= from && **at <= to)
            .map(|(seq, at)| Cut { seq: *seq, at: *at })
            .collect();
        let (cuts, complete) = sample_cuts(&in_range, from, to, buckets);
        let reading = CutsReading {
            projection: projection_basis(
                ProjectionCursor {
                    generation: state.generation,
                    through: head,
                },
                state.source,
                state.source_failed,
            ),
            head: Cut {
                seq: head,
                at: head_at,
            },
            first_retained,
            pruned_through: state.pruned_through,
            retained_from: floor,
            retention_hours: RETENTION_HOURS,
            from,
            to,
            buckets,
            complete,
            cuts,
            read_basis: Some(CutsReadBasis {
                range_count: in_range.len().min(buckets + 1),
                range_count_exact: in_range.len() <= buckets,
                query_ms: Vec::new(),
            }),
        };
        check_bytes(&reading)?;
        Ok(reading)
    }
    async fn cursor(&self) -> Result<ProjectionCursor, ClaimsError> {
        let state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        Ok(ProjectionCursor {
            generation: state.generation,
            through: state.head.map_or(0, |(seq, _)| seq),
        })
    }
    async fn begin_rebuild(
        &self,
        expected: ProjectionCursor,
    ) -> Result<ProjectionCursor, ClaimsError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        let cursor = ProjectionCursor {
            generation: state.generation,
            through: state.head.map_or(0, |(seq, _)| seq),
        };
        if cursor != expected {
            return Err(invalid("rebuild cursor changed"));
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("projection generation exhausted"))?;
        *state = State {
            generation,
            ..State::default()
        };
        Ok(ProjectionCursor {
            generation,
            through: 0,
        })
    }
    async fn advance(
        &self,
        expected: ProjectionCursor,
        batch: &HistoryBatch,
        source: SourceObservation,
    ) -> Result<(), ClaimsError> {
        self.apply_batch(Some(expected), batch, Some(source))
    }
    async fn source_failed(
        &self,
        expected: ProjectionCursor,
        at: DateTime<Utc>,
    ) -> Result<(), ClaimsError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        if expected
            != (ProjectionCursor {
                generation: state.generation,
                through: state.head.map_or(0, |(seq, _)| seq),
            })
        {
            return Err(invalid("source-failure cursor changed"));
        }
        state.source_failed = Some(at);
        Ok(())
    }
    #[cfg(feature = "test-support")]
    async fn materialize(&self, batch: &HistoryBatch) -> Result<(), ClaimsError> {
        self.apply_batch(None, batch, None)
    }
    async fn read(
        &self,
        as_of: Option<i64>,
        hours: i64,
        now: DateTime<Utc>,
    ) -> Result<ClaimsReading, ClaimsError> {
        if !(1..=crate::regions::MAX_WINDOW_HOURS).contains(&hours) {
            return Err(invalid("window must be1..168hours"));
        }
        let state = self
            .state
            .lock()
            .map_err(|_| unread("materialized owner unavailable"))?;
        let (head, head_at) = state
            .head
            .ok_or_else(|| unread("no observed materialized horizon"))?;
        if as_of.is_none() {
            if let Some(at) = state.source_failed {
                return Err(unread(format!(
                    "native source unavailable at{at}; materialized through{head}"
                )));
            }
            if let Some(source) = state.source.filter(|s| s.audit_head > head) {
                return Err(unread(format!(
                    "catch-up pending: materialized through{head}, observed audit head{}",
                    source.audit_head
                )));
            }
        }
        let cut = as_of.unwrap_or(head);
        if cut < 0 || cut > head {
            return Err(invalid("requested sequence exceeds materialized horizon"));
        }
        if as_of.is_some() && cut <= state.pruned_through && cut != head {
            return Err(ClaimsError::OutsideRetention(
                "requested cut was pruned outside the supported window".into(),
            ));
        }
        if state
            .groups
            .values()
            .any(|(lo, hi)| *lo <= cut && cut < *hi)
        {
            return Err(invalid(
                "requested sequence bisects an original transaction",
            ));
        }
        let at = if cut == head {
            head_at
        } else {
            *state
                .cuts
                .get(&cut)
                .ok_or_else(|| invalid("requested sequence has no recorded time basis"))?
        };
        if as_of.is_some() && at < now - chrono::Duration::hours(RETENTION_HOURS) {
            return Err(ClaimsError::OutsideRetention(format!(
                "supported history begins{}",
                now - chrono::Duration::hours(RETENTION_HOURS)
            )));
        }
        let rows = if cut == head {
            state.current.values().collect()
        } else {
            let mut selected = BTreeMap::new();
            for row in state.rows.iter().filter(|r| r.seq <= cut) {
                selected.insert((row.kind.clone(), row.key.clone()), row);
            }
            selected.into_values().collect()
        };
        reading(
            reference_rows(rows, cut, at, hours)?,
            cut,
            at,
            hours,
            now,
            projection_basis(
                ProjectionCursor {
                    generation: state.generation,
                    through: head,
                },
                state.source,
                state.source_failed,
            ),
        )
    }
}
