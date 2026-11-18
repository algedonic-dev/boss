//! Transactional lease projection. The caller's original statement budget is
//! refreshed before each query; COMMIT uncertainty remains explicit.
use crate::{
    flow_resources::Claim,
    port::JobsError,
    resource_leases::{self as leases, IdentityBinding, StockObligation},
};
use boss_core::{job::StepId, publisher::EventStamp};
use boss_events::statement_budget::{StatementBudget, refresh};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

fn storage(e: impl std::fmt::Display) -> JobsError {
    JobsError::Storage(e.to_string())
}
/// The pre-commit statement budget each lease operation (bind, sweep,
/// discharge) spends across ALL its statements, measured from just before
/// it opens its transaction. This is the production latency claim.
pub(crate) const STATEMENT_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// How a lease operation measures its budget: production's
/// [`STATEMENT_BUDGET`] from now, or a fixture's own budget from a clock
/// that has already lost the measured stall. It was a literal 2 s here,
/// so a test judging what a lease operation WRITES lost to a loaded gate
/// drive through PgJobs whatever its claims history had been given
/// (backlog ebbf38c6). `PgJobs::for_fixture` is the fold fixture's.
#[derive(Clone, Copy)]
pub(crate) struct LeaseBudget {
    budget: std::time::Duration,
    clock: fn() -> tokio::time::Instant,
}
impl LeaseBudget {
    pub(crate) const PRODUCTION: Self = Self {
        budget: STATEMENT_BUDGET,
        clock: tokio::time::Instant::now,
    };
    #[cfg(feature = "test-support")]
    pub(crate) const FOLD_FIXTURE: Self = Self {
        budget: crate::fixture_budget::FIXTURE_BUDGET,
        clock: crate::fixture_budget::after_the_measured_stall,
    };
    #[cfg(feature = "test-support")]
    pub(crate) fn new(budget: std::time::Duration, clock: fn() -> tokio::time::Instant) -> Self {
        Self { budget, clock }
    }
    /// Started at the operation's own transaction, not at its call: a
    /// discharge verifies its evidence first, outside the budget.
    fn start(self) -> StatementBudget {
        StatementBudget::until((self.clock)() + self.budget)
    }
}

pub(crate) async fn collect_stock_obligations_in_tx(
    tx: &mut PgConnection,
    claim: &Claim,
    budget: Option<StatementBudget>,
) -> Result<Vec<StockObligation>, JobsError> {
    if !claim
        .observed
        .iter()
        .any(|d| d.stock && !d.amount.is_zero())
    {
        return Ok(Vec::new());
    }
    refresh(tx, budget).await.map_err(storage)?;
    let binding: Option<Value> =
        sqlx::query_scalar("SELECT body FROM resource_identity_bindings WHERE acquisition=$1")
            .bind(claim.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
    let identity = binding
        .map(serde_json::from_value::<IdentityBinding>)
        .transpose()
        .map_err(storage)?
        .map(|b| b.identity);
    let mut pools = Vec::new();
    for a in &claim.allocations {
        refresh(tx, budget).await.map_err(storage)?;
        let raw: Value =
            sqlx::query_scalar("SELECT spec FROM resource_pools WHERE name=$1 AND version=$2")
                .bind(&a.pool)
                .bind(a.version)
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?;
        pools.push(serde_json::from_value(raw).map_err(storage)?);
    }
    leases::obligations_for(claim, identity, &pools)
}

pub(crate) async fn store_stock_obligations_in_tx(
    tx: &mut PgConnection,
    obligations: &[StockObligation],
    budget: Option<StatementBudget>,
) -> Result<(), JobsError> {
    for o in obligations {
        refresh(tx, budget).await.map_err(storage)?;
        sqlx::query("INSERT INTO resource_stock_obligations(acquisition,pool,pool_version,amount,returned_by,body) VALUES($1,$2,$3,$4::numeric,$5,$6)")
            .bind(o.claim.id).bind(&o.allocation.pool).bind(o.allocation.version).bind(o.allocation.amount.to_string()).bind(o.returned_by).bind(json!(o)).execute(&mut *tx).await.map_err(storage)?;
    }
    Ok(())
}

pub(crate) async fn bind_identity(
    pool: &PgPool,
    step: &StepId,
    actor: &str,
    binding: &IdentityBinding,
    stamp: &EventStamp,
    lease: LeaseBudget,
) -> Result<(), JobsError> {
    let budget = Some(lease.start());
    let mut tx = pool.begin().await.map_err(storage)?;
    let outcome=async {
        refresh(&mut tx,budget).await.map_err(storage)?;
        let row:Option<(String,Option<String>)>=sqlx::query_as("SELECT status,assignee_id FROM steps WHERE id=$1 FOR UPDATE").bind(*step.inner().as_uuid()).fetch_optional(&mut *tx).await.map_err(storage)?;
        let (status,holder)=row.ok_or(JobsError::StepNotFound(*step))?;
        refresh(&mut tx,budget).await.map_err(storage)?;
        let row:Option<(bool,Value)>=sqlx::query_as("SELECT active,body FROM step_resource_claims WHERE step_id=$1 FOR UPDATE").bind(*step.inner().as_uuid()).fetch_optional(&mut *tx).await.map_err(storage)?;
        let (active,body)=row.ok_or_else(||leases::error("resource-acquisition-unknown"))?;
        let claim:Claim=serde_json::from_value(body).map_err(storage)?;claim.validate().map_err(storage)?;
        leases::validate_binding(&claim,active,actor,binding,stamp.timestamp)?;
        if status!="active" || holder.as_deref()!=Some(actor) {return Err(leases::error("resource-binding-holder-mismatch"));}
        refresh(&mut tx,budget).await.map_err(storage)?;
        let inserted=sqlx::query("INSERT INTO resource_identity_bindings(acquisition,step_id,body) VALUES($1,$2,$3) ON CONFLICT(acquisition) DO NOTHING")
            .bind(claim.id).bind(*step.inner().as_uuid()).bind(json!(binding)).execute(&mut *tx).await.map_err(storage)?;
        if inserted.rows_affected()!=1 {return Err(leases::error("resource-identity-already-bound"));}
        let event=stamp.event(leases::IDENTITY_BOUND,json!({"job_id":claim.job_id,"step_id":claim.step_id,"claim":claim,"binding":binding,"observed_at":stamp.timestamp}));
        boss_events::outbox::record_event_with_budget_in_tx(&mut tx,&event,budget).await.map_err(storage)?;
        boss_events::statement_budget::release_before_commit(&mut tx).await.map_err(storage)
    }.await;
    crate::flow_resources_pg::finish_write(tx, outcome, "resource identity bind").await
}

pub(crate) async fn sweep(
    pool: &PgPool,
    stamp: &EventStamp,
    limit: usize,
    lease: LeaseBudget,
) -> Result<leases::SweepResult, JobsError> {
    if limit == 0 || limit > leases::MAX_SWEEP {
        return Err(leases::error("resource-sweep-bound-invalid"));
    }
    let budget = Some(lease.start());
    let mut tx = pool.begin().await.map_err(storage)?;
    let mut expired = 0;
    let outcome=async {
        refresh(&mut tx,budget).await.map_err(storage)?;
        let ids:Vec<uuid::Uuid>=sqlx::query_scalar("SELECT step_id FROM step_resource_claims WHERE active AND lease_until<=$1 ORDER BY lease_until,step_id LIMIT $2")
            .bind(stamp.timestamp).bind(limit as i64).fetch_all(&mut *tx).await.map_err(storage)?;
        for id in ids {
            refresh(&mut tx,budget).await.map_err(storage)?;
            // The step's row lock is the fence, and the same statement reads
            // its packet's partition: fixed at admission and never updated,
            // so the expiry is the packet's fact in the packet's partition,
            // whoever fired the sweep (Codex review e7a414ae, R1 P2). `OF s`
            // keeps the lock, and SKIP LOCKED, on the step row alone.
            let locked:Option<String>=sqlx::query_scalar("SELECT j.partition FROM steps s JOIN jobs j ON j.id=s.job_id WHERE s.id=$1 FOR UPDATE OF s SKIP LOCKED").bind(id).fetch_optional(&mut *tx).await.map_err(storage)?;
            let Some(partition)=locked else {continue;};
            refresh(&mut tx,budget).await.map_err(storage)?;
            let body:Option<Value>=sqlx::query_scalar("SELECT body FROM step_resource_claims WHERE step_id=$1 AND active AND lease_until<=$2 FOR UPDATE").bind(id).bind(stamp.timestamp).fetch_optional(&mut *tx).await.map_err(storage)?;
            let Some(body)=body else {continue;};let claim:Claim=serde_json::from_value(body).map_err(storage)?;claim.validate().map_err(storage)?;
            // PostgreSQL timestamps carry microseconds; the immutable fact
            // retains the injected clock's full precision. Index candidates
            // still need this canonical comparison before any mutation.
            if claim.lease_until.is_none_or(|until|until>stamp.timestamp) {continue;}
            for a in &claim.allocations {crate::declared_version::lock_and_read_newest_with_budget(&mut tx,"resource_pools","name",&a.pool,budget).await.map_err(storage)?;}
            let obligations=collect_stock_obligations_in_tx(&mut tx,&claim,budget).await?;
            store_stock_obligations_in_tx(&mut tx,&obligations,budget).await?;
            refresh(&mut tx,budget).await.map_err(storage)?;
            sqlx::query("UPDATE step_resource_claims SET active=false WHERE step_id=$1").bind(id).execute(&mut *tx).await.map_err(storage)?;
            refresh(&mut tx,budget).await.map_err(storage)?;
            sqlx::query("DELETE FROM step_resource_allocations WHERE step_id=$1").bind(id).execute(&mut *tx).await.map_err(storage)?;
            let event=stamp.clone().with_partition(crate::postgres::parse_partition(&partition)).event(leases::LEASE_EXPIRED,json!({"job_id":claim.job_id,"step_id":claim.step_id,"claim":claim,"stock_obligations":obligations,"observed_at":stamp.timestamp}));
            boss_events::outbox::record_event_with_budget_in_tx(&mut tx,&event,budget).await.map_err(storage)?;expired+=1;
        }
        boss_events::statement_budget::release_before_commit(&mut tx).await.map_err(storage)
    }.await;
    crate::flow_resources_pg::finish_write(tx, outcome, "resource lease sweep").await?;
    Ok(leases::SweepResult {
        observed_at: stamp.timestamp,
        expired,
    })
}

pub(crate) async fn obligations(
    pool: &PgPool,
    acquisition: uuid::Uuid,
) -> Result<Vec<StockObligation>, JobsError> {
    let bodies: Vec<Value> = sqlx::query_scalar(
        "SELECT body FROM resource_stock_obligations WHERE acquisition=$1 ORDER BY pool LIMIT 4",
    )
    .bind(acquisition)
    .fetch_all(pool)
    .await
    .map_err(storage)?;
    bodies
        .into_iter()
        .map(|body| serde_json::from_value(body).map_err(storage))
        .collect()
}

pub(crate) async fn discharge(
    pool: &PgPool,
    acquisition: uuid::Uuid,
    name: &str,
    actor: &str,
    evidence: uuid::Uuid,
    verifier: &dyn leases::ResourceReturnVerifier,
    stamp: &EventStamp,
    lease: LeaseBudget,
) -> Result<(), JobsError> {
    let before = obligations(pool, acquisition)
        .await?
        .into_iter()
        .find(|o| o.allocation.pool == name)
        .ok_or_else(|| leases::error("stock-obligation-unknown"))?;
    if before.reclaimer != actor {
        return Err(leases::error("stock-reclaimer-mismatch"));
    }
    if before.returned_by == Some(evidence) {
        return Ok(());
    }
    let verified = verifier.verify(&before, evidence).await?;
    leases::validate_return(&before, actor, evidence, &verified)?;
    let budget = Some(lease.start());
    let mut tx = pool.begin().await.map_err(storage)?;
    let outcome=async {
        crate::declared_version::lock_and_read_newest_with_budget(&mut tx,"resource_pools","name",name,budget).await.map_err(storage)?;
        refresh(&mut tx,budget).await.map_err(storage)?;
        let body:Value=sqlx::query_scalar("SELECT body FROM resource_stock_obligations WHERE acquisition=$1 AND pool=$2 FOR UPDATE").bind(acquisition).bind(name).fetch_one(&mut *tx).await.map_err(storage)?;
        let current:StockObligation=serde_json::from_value(body).map_err(storage)?;
        if current.returned_by==Some(evidence){return boss_events::statement_budget::release_before_commit(&mut tx).await.map_err(storage);}
        if current!=before || current.returned_by.is_some(){return Err(leases::error("stock-return-evidence-already-consumed"));}
        let mut after=current;after.returned_by=Some(evidence);
        refresh(&mut tx,budget).await.map_err(storage)?;
        sqlx::query("UPDATE resource_stock_obligations SET returned_by=$3,body=$4 WHERE acquisition=$1 AND pool=$2").bind(acquisition).bind(name).bind(evidence).bind(json!(after)).execute(&mut *tx).await.map_err(storage)?;
        let event=stamp.event(leases::STOCK_RETURNED,json!({"job_id":after.claim.job_id,"step_id":after.claim.step_id,"obligation":after,"evidence":evidence}));
        boss_events::outbox::record_event_with_budget_in_tx(&mut tx,&event,budget).await.map_err(storage)?;
        boss_events::statement_budget::release_before_commit(&mut tx).await.map_err(storage)
    }.await;
    crate::flow_resources_pg::finish_write(tx, outcome, "stock discharge").await
}
