//! Pool-ledger fixtures, not an activated people Workflow acquisition. T2
//! derives demand from the executor; this car does not invent a second bill.
use super::*;
use crate::flow_resources::{self as flow, Allocation, Amount, Capacity, Claim, Demand};
use boss_core::{
    job::{JobId, StepId},
    roster_capacity::{RosterSnapshot, RosterSnapshotError},
};
use serde_json::json;

enum Backend {
    Memory(InMemoryPools),
    Pg(PgPools, boss_testing::TestDb),
}
impl Backend {
    async fn facts(&self, name: &str) -> Vec<crate::claims_console::SequencedFact> {
        let events = match self {
            Self::Memory(p) => p
                .recorded_events()
                .unwrap()
                .into_iter()
                .filter(|e| e.payload["name"] == name)
                .collect::<Vec<_>>(),
            Self::Pg(_, db) => {
                let rows: Vec<(uuid::Uuid, DateTime<Utc>, String, String, serde_json::Value)> = sqlx::query_as("SELECT event_id,timestamp,source,kind,payload FROM event_outbox WHERE kind='jobs.pool.published' AND payload->>'name'=$1 ORDER BY id")
                    .bind(name).fetch_all(&db.pool).await.unwrap();
                rows.into_iter()
                    .map(|(id, timestamp, source, kind, payload)| Event {
                        id,
                        timestamp,
                        source,
                        kind,
                        payload,
                    })
                    .collect()
            }
        };
        events
            .into_iter()
            .enumerate()
            .map(|(i, e)| crate::claims_console::SequencedFact {
                seq: i as i64 + 1,
                recorded_at: e.timestamp,
                event_id: e.id,
                event_at: e.timestamp,
                source: e.source,
                kind: e.kind,
                payload: serde_json::to_string(&e.payload).unwrap(),
                original_group: None,
            })
            .collect()
    }
    fn registry(&self) -> &dyn PoolRegistry {
        match self {
            Self::Memory(p) => p,
            Self::Pg(p, _) => p,
        }
    }
    async fn fixture_hold(&self, spec: &PoolSpec) -> Claim {
        let demand = Demand::flow(
            &spec.name,
            spec.version,
            Amount::parse("1").unwrap(),
            Capacity::Known(Amount::parse("3").unwrap()),
            Amount::zero(),
        );
        let claim = Claim {
            version: 1,
            id: uuid::Uuid::new_v4(),
            step_id: StepId::new(),
            job_id: JobId::new(),
            generation: 1,
            holder: "fixture-holder".into(),
            workflow_kind: "pool-fixture-only".into(),
            workflow_version: 1,
            bill: ResourceBill::default(),
            observed: vec![demand.clone()],
            allocations: flow::plan(&[demand]).unwrap(),
            claimed_at: None,
            resolved_duration_ms: None,
            lease_until: None,
        };
        // Fixture storage represents one existing charge. It is deliberately
        // NOT claimed as a canonical Workflow-bill acquisition/replay proof.
        match self {
            Self::Memory(p) => {
                p.resource_lock()
                    .unwrap()
                    .claims
                    .insert(claim.step_id.to_string(), (claim.clone(), true));
            }
            Self::Pg(_, db) => {
                let mut tx = db.pool.begin().await.unwrap();
                sqlx::query("INSERT INTO step_resource_claims(step_id,job_id,generation,holder,active,body) VALUES($1,$2,1,$3,true,$4)")
                    .bind(*claim.step_id.inner().as_uuid()).bind(*claim.job_id.inner().as_uuid()).bind(&claim.holder).bind(json!(claim)).execute(&mut *tx).await.unwrap();
                sqlx::query(
                    "INSERT INTO step_resource_allocations(step_id,pool,amount) VALUES($1,$2,1)",
                )
                .bind(*claim.step_id.inner().as_uuid())
                .bind(&spec.name)
                .execute(&mut *tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
        }
        claim
    }
    async fn held(&self, pool: &str) -> Amount {
        match self {
            Self::Memory(p) => Amount::sum(
                p.resource_lock()
                    .unwrap()
                    .claims
                    .values()
                    .filter(|(_, active)| *active)
                    .flat_map(|(c, _)| c.allocations.iter())
                    .filter(|a| a.pool == pool)
                    .map(|a| &a.amount),
            )
            .unwrap(),
            Self::Pg(_, db) => {
                let held: String = sqlx::query_scalar("SELECT COALESCE(SUM(a.amount),0)::text FROM step_resource_allocations a JOIN step_resource_claims c USING(step_id) WHERE a.pool=$1 AND c.active")
                    .bind(pool).fetch_one(&db.pool).await.unwrap();
                Amount::parse(&held).unwrap()
            }
        }
    }
    async fn stored(&self, claim: &Claim) -> serde_json::Value {
        match self {
            Self::Memory(p) => json!(
                p.resource_lock()
                    .unwrap()
                    .claims
                    .get(&claim.step_id.to_string())
                    .unwrap()
            ),
            Self::Pg(_, db) => sqlx::query_scalar(
                "SELECT jsonb_build_array(body,active) FROM step_resource_claims WHERE step_id=$1",
            )
            .bind(*claim.step_id.inner().as_uuid())
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        }
    }
    async fn demand(&self, spec: &PoolSpec) -> Result<Vec<Allocation>, flow::Refusal> {
        let capacity = match &spec.capacity {
            PoolCapacity::Unknown {} => Capacity::Unknown,
            PoolCapacity::Configured { amount } => Capacity::Known(amount.amount().unwrap()),
        };
        flow::plan(&[Demand::flow(
            &spec.name,
            spec.version,
            Amount::parse("1").unwrap(),
            capacity,
            self.held(&spec.name).await,
        )])
    }
}

fn declaration(name: &str, roster: bool) -> PoolSpec {
    let mut row = json!({"name":name,"version":1,"status":"active","unit":"people","kind":"flow","capacity":{"state":"configured","amount":"3"}});
    if roster {
        row["roster"] = json!({"eligibility":[{"attribute":"role","codes":["fixture-role"]}],"basis":{"revision":1,"count":3,"as_of":Utc::now()}});
    }
    serde_json::from_value(row).unwrap()
}
fn known(
    spec: &PoolSpec,
    revision: i64,
    count: u64,
) -> Result<RosterSnapshot, RosterSnapshotError> {
    Ok(RosterSnapshot {
        eligibility: spec.roster.as_ref().unwrap().eligibility.clone(),
        revision: Some(revision),
        count,
    })
}
async fn contract(backend: Backend) {
    let actor = ActorId::automation("roster-pool-fixture");
    let now = Utc::now();
    let initial = backend
        .registry()
        .publish_declared(declaration("people:fixture-role", true), &actor, now)
        .await
        .unwrap();
    let claim = backend.fixture_hold(&initial).await;
    let second_claim = backend.fixture_hold(&initial).await;
    let held = backend.stored(&claim).await;
    let second_held = backend.stored(&second_claim).await;
    let unknown = backend
        .registry()
        .observe_roster(
            &initial,
            Err(RosterSnapshotError::Unavailable(
                "people outbox undrained".into(),
            )),
            &actor,
            now,
        )
        .await;
    assert!(
        unknown.is_ok(),
        "an actual unread derived observation must publish while held: {unknown:?}"
    );
    let unknown = unknown.unwrap();
    assert_eq!(unknown.capacity, PoolCapacity::Unknown {});
    assert_eq!(
        unknown.roster.as_ref().unwrap().basis,
        initial.roster.as_ref().unwrap().basis,
        "Unknown keeps the exact last good witness"
    );
    assert_eq!(
        unknown.roster.as_ref().unwrap().unknown_reason.as_deref(),
        Some("people outbox undrained")
    );
    assert_eq!(backend.stored(&claim).await, held);
    let refusal = backend.demand(&unknown).await.unwrap_err();
    assert_eq!(refusal.pool, initial.name);
    assert_eq!(refusal.reason, "capacity-unknown");
    let restored = backend
        .registry()
        .observe_roster(&unknown, known(&unknown, 2, 3), &actor, now)
        .await
        .unwrap();
    assert!(backend.demand(&restored).await.is_ok());
    let below = backend
        .registry()
        .observe_roster(&restored, known(&restored, 3, 1), &actor, now)
        .await
        .unwrap();
    assert_eq!(
        below.capacity,
        PoolCapacity::Configured {
            amount: Quantity::from(1)
        }
    );
    assert!(backend.demand(&below).await.is_err());
    assert_eq!(backend.stored(&claim).await, held);
    assert_eq!(backend.stored(&second_claim).await, second_held);
    assert_eq!(
        backend.held(&initial.name).await,
        Amount::parse("2").unwrap()
    );
    // Stale or caller-edited declarations cannot smuggle in the exception.
    assert!(
        backend
            .registry()
            .observe_roster(&initial, known(&initial, 4, 3), &actor, now)
            .await
            .is_err()
    );
    let mut changed = below.clone();
    changed.roster.as_mut().unwrap().eligibility[0].codes = vec!["other-role".into()];
    assert!(
        backend
            .registry()
            .observe_roster(&changed, known(&changed, 4, 3), &actor, now)
            .await
            .is_err()
    );
    let mut changed = below.clone();
    changed.unit = "caller-edited-unit".into();
    assert!(
        backend
            .registry()
            .observe_roster(&changed, known(&changed, 4, 3), &actor, now)
            .await
            .is_err()
    );
    let mut changed = below.clone();
    changed.kind = PoolKind::Stock;
    changed.reclaimer = Some("caller-reclaimer".into());
    assert!(
        backend
            .registry()
            .observe_roster(&changed, known(&changed, 4, 3), &actor, now)
            .await
            .is_err()
    );
    assert_eq!(backend.stored(&claim).await, held);
    let static_pool = backend
        .registry()
        .publish_declared(declaration("people:static", false), &actor, now)
        .await
        .unwrap();
    let static_claim = backend.fixture_hold(&static_pool).await;
    let static_held = backend.stored(&static_claim).await;
    let mut spoof = static_pool.clone();
    spoof.version += 1;
    spoof.capacity = PoolCapacity::Unknown {};
    spoof.roster = initial.roster.clone();
    spoof.roster.as_mut().unwrap().eligibility[0].codes = vec!["static".into()];
    spoof.roster.as_mut().unwrap().unknown_reason = Some("caller-added-roster".into());
    assert!(
        spoof.validate().is_ok(),
        "spoof reaches the held lineage fence, not merely schema validation"
    );
    let mut known_spoof = spoof.clone();
    known_spoof.capacity = static_pool.capacity.clone();
    known_spoof.roster.as_mut().unwrap().unknown_reason = None;
    assert!(known_spoof.validate().is_ok());
    assert!(
        backend
            .registry()
            .publish_declared(known_spoof, &actor, now)
            .await
            .is_err()
    );
    assert!(
        backend
            .registry()
            .publish_declared(spoof, &actor, now)
            .await
            .is_err()
    );
    assert!(
        backend
            .registry()
            .observe_roster(
                &static_pool,
                Err(RosterSnapshotError::Unavailable("unread".into())),
                &actor,
                now
            )
            .await
            .is_err()
    );
    assert_eq!(backend.stored(&static_claim).await, static_held);
    let (left, right) = tokio::join!(
        backend
            .registry()
            .observe_roster(&below, known(&below, 4, 3), &actor, now),
        backend
            .registry()
            .observe_roster(&below, known(&below, 5, 3), &actor, now)
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "same expected declaration has exactly one winning writer"
    );
    assert_eq!(
        backend
            .registry()
            .list_versions(&initial.name)
            .await
            .unwrap()
            .len(),
        5
    );
    assert_eq!(backend.stored(&claim).await, held);
    assert_eq!(backend.stored(&second_claim).await, second_held);
    let facts = backend.facts(&initial.name).await;
    assert_eq!(
        facts.len(),
        5,
        "one immutable pool fact per actual new observation, none for refusals"
    );
    let batch = crate::claims_console::HistoryBatch {
        through: 5,
        through_at: now,
        facts,
    };
    use crate::claims_console::ClaimsHistory;
    let first = crate::claims_console::InMemoryClaimsHistory::default();
    first.materialize(&batch).await.unwrap();
    let observed_unknown = first.read(Some(2), 24, now).await.unwrap();
    assert_eq!(
        observed_unknown.pools[0].state,
        crate::regions::RegionState::Attention
    );
    assert_eq!(
        observed_unknown.pools[0].pool.as_ref().unwrap().roster,
        unknown.roster
    );
    assert_eq!(
        observed_unknown.pools[0].pool.as_ref().unwrap().capacity,
        PoolCapacity::Unknown {}
    );
    let before_replay = first.read(None, 24, now).await.unwrap();
    first.materialize(&batch).await.unwrap();
    assert_eq!(first.read(None, 24, now).await.unwrap(), before_replay);
    let rebuilt = crate::claims_console::InMemoryClaimsHistory::default();
    rebuilt.materialize(&batch).await.unwrap();
    assert_eq!(
        rebuilt.read(None, 24, now).await.unwrap(),
        before_replay,
        "pool observation history rebuild is deterministic; fixture claims are deliberately not canonical claim replay"
    );
}
#[tokio::test]
async fn memory_roster_observations_conserve_every_held_charge() {
    contract(Backend::Memory(InMemoryPools::default())).await;
}
#[tokio::test]
async fn pg_roster_observations_conserve_every_held_charge() {
    let db = boss_testing::TestDb::new().await;
    contract(Backend::Pg(PgPools::new(db.pool.clone()), db)).await;
}

#[tokio::test]
async fn a_refused_observation_outbox_rolls_back_capacity_and_version_with_held_unchanged() {
    let db = boss_testing::TestDb::new().await;
    let backend = Backend::Pg(PgPools::new(db.pool.clone()), db);
    let actor = ActorId::automation("roster-rollback-fixture");
    let now = Utc::now();
    let initial = backend
        .registry()
        .publish_declared(declaration("people:fixture-role", true), &actor, now)
        .await
        .unwrap();
    let claim = backend.fixture_hold(&initial).await;
    let before = backend.stored(&claim).await;
    if let Backend::Pg(_, db) = &backend {
        sqlx::raw_sql("CREATE FUNCTION reject_roster_fact() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='jobs.pool.published' THEN RAISE EXCEPTION 'own roster outbox refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_roster_fact BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION reject_roster_fact();").execute(&db.pool).await.unwrap();
    }
    let failure = backend
        .registry()
        .observe_roster(
            &initial,
            Err(RosterSnapshotError::Unavailable("unread".into())),
            &actor,
            now,
        )
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("own roster outbox refusal"));
    assert_eq!(
        backend.registry().list_active().await.unwrap(),
        vec![initial]
    );
    assert_eq!(backend.stored(&claim).await, before);
    assert_eq!(backend.facts("people:fixture-role").await.len(), 1);
}
