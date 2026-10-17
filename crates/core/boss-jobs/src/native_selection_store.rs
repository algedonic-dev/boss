//! Constructor binding for native transaction composition; not authorization.
use crate::port::JobsError;
use crate::station_claim::{
    actor_inputs, native_calendar_request, native_user, require_native_claim_constraints,
    require_native_step_authority,
};
use sqlx::{PgPool, Postgres, Transaction};
use std::sync::Arc;

/// Test evidence survives successful-output capture and future cancellation.
#[cfg(test)]
pub(crate) struct SelectionPhase {
    phase: String,
    ingress: std::time::Instant,
    began: std::time::Instant,
    finished: bool,
    sink: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
}

#[cfg(test)]
impl SelectionPhase {
    pub(crate) fn start(phase: String, ingress: std::time::Instant) -> Self {
        Self::start_to(
            phase,
            ingress,
            Arc::new(|record| {
                use std::io::Write;
                // A single bounded write avoids splicing libtest stdout into JSON.
                let line = format!("selection_trace {record}\n");
                let _ = std::io::stderr().lock().write_all(line.as_bytes());
            }),
        )
    }
    fn start_to(
        phase: String,
        ingress: std::time::Instant,
        sink: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    ) -> Self {
        let trace = Self {
            phase,
            ingress,
            began: std::time::Instant::now(),
            finished: false,
            sink,
        };
        trace.write("start");
        trace
    }
    fn write(&self, event: &str) {
        let record = serde_json::json!({
            "phase": self.phase,
            "ingress": format!("{:?}", self.ingress),
            "operation": format!("{}@{:?}", self.phase, self.began),
            "event": event,
            "elapsed_us": self.began.elapsed().as_micros(),
            "ingress_elapsed_us": self.ingress.elapsed().as_micros(),
        });
        (self.sink)(record);
    }
    pub(crate) fn finish(&mut self, outcome: &str) {
        self.write(outcome);
        self.finished = true;
    }
}

#[cfg(test)]
impl Drop for SelectionPhase {
    fn drop(&mut self) {
        if !self.finished {
            self.write("cancelled");
        }
    }
}

/// The shared policy adapter consumes the caller's actual connection and
/// absolute ingress budget; it cannot acquire a second pool or reset time.
struct SelectionStatementRefresh(boss_events::statement_budget::StatementBudget);

#[async_trait::async_trait]
impl boss_policy_client::postgres::StatementRefresh for SelectionStatementRefresh {
    async fn before_statement(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<(), boss_policy_client::PolicyError> {
        boss_events::statement_budget::refresh(conn, Some(self.0))
            .await
            .map_err(boss_policy_client::PolicyError::Storage)
    }
}

/// One monotonic budget, carried from ingress rather than restarted per query.
#[derive(Clone, Copy)]
pub struct SelectionDeadline(tokio::time::Instant);

impl SelectionDeadline {
    pub fn at_ingress() -> Self {
        Self(tokio::time::Instant::now() + crate::station_claim::SELECTION_PRE_COMMIT_BUDGET)
    }
    pub fn from_ingress(ingress: &crate::station_claim::AuthenticatedSelectionIngress) -> Self {
        Self(
            tokio::time::Instant::from_std(ingress.started())
                + crate::station_claim::SELECTION_PRE_COMMIT_BUDGET,
        )
    }
    pub fn remaining(&self) -> Result<std::time::Duration, JobsError> {
        self.0
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                JobsError::SelectionConflict(
                    "selection ingress deadline expired before commit dispatch".into(),
                )
            })
    }
    pub fn verify_before_commit_dispatch(&self) -> Result<(), JobsError> {
        self.remaining().map(|_| ())
    }
    /// Dropping this future cancels pre-dispatch work and drops its transaction;
    /// no detached task retains permission to mutate after expiration.
    #[track_caller]
    pub fn before_dispatch<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, JobsError>>,
    ) -> impl std::future::Future<Output = Result<T, JobsError>> {
        let deadline = *self;
        #[cfg(test)]
        let caller = std::panic::Location::caller();
        async move {
            #[cfg(test)]
            let mut trace = SelectionPhase::start(
                format!("pre_dispatch@{caller}"),
                deadline.0.into_std() - crate::station_claim::SELECTION_PRE_COMMIT_BUDGET,
            );
            deadline.remaining()?;
            let result = tokio::time::timeout_at(deadline.0, work)
                .await
                .map_err(|_| {
                    JobsError::SelectionConflict(
                        "selection ingress deadline expired before commit dispatch".into(),
                    )
                })
                .and_then(|result| result);
            #[cfg(test)]
            trace.finish(if result.is_ok() { "returned" } else { "error" });
            result
        }
    }
    pub fn sql_budget(&self) -> boss_events::statement_budget::StatementBudget {
        boss_events::statement_budget::StatementBudget::until(self.0)
    }
    #[track_caller]
    pub fn constrain_sql<'a, 'c>(
        &self,
        tx: &'a mut Transaction<'c, Postgres>,
    ) -> impl std::future::Future<Output = Result<(), JobsError>> + 'a {
        let deadline = *self;
        #[cfg(test)]
        let caller = std::panic::Location::caller();
        async move {
            #[cfg(test)]
            let mut trace = SelectionPhase::start(
                format!("sql_refresh@{caller}"),
                deadline.0.into_std() - crate::station_claim::SELECTION_PRE_COMMIT_BUDGET,
            );
            let result = deadline
                .sql_budget()
                .refresh(tx)
                .await
                .map_err(JobsError::Storage);
            #[cfg(test)]
            trace.finish(if result.is_ok() { "returned" } else { "error" });
            result?;
            deadline.remaining()?;
            Ok(())
        }
    }
}

pub enum NativeSelectionAcquisition {
    Replayed(crate::station_claim::SelectionReceipt),
    Fresh(Transaction<'static, Postgres>),
}

#[derive(Clone)]
pub struct NativeSelectionStore {
    pool: PgPool,
    identity: Arc<()>,
}
impl NativeSelectionStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            identity: Arc::new(()),
        }
    }
    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }
    pub fn observes(&self, observation: &crate::station_claim::NativeAuthorityObservation) -> bool {
        Arc::ptr_eq(&self.identity, &observation.store_identity)
    }
    pub async fn read_selection(
        &self,
        ingress: &crate::station_claim::AuthenticatedSelectionIngress,
        request_id: uuid::Uuid,
        station: &str,
    ) -> Result<crate::station_claim::NativeSelectionRead, JobsError> {
        let deadline = SelectionDeadline::from_ingress(ingress);
        deadline
            .before_dispatch(async {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                deadline.constrain_sql(&mut tx).await?;
                lock_actor_policy_rows_with_budget_in(&mut tx, Some(deadline.sql_budget())).await?;
                let (agents, automations) = crate::agents::PgAgents::selection_rows_budgeted_in(
                    &mut tx,
                    Some(deadline.sql_budget()),
                )
                .await
                .map_err(|error| JobsError::Storage(error.to_string()))?;
                let (agents, automations) = actor_inputs(agents, automations, ingress.actor_id());
                let (user, has_role) = match native_user(ingress, &agents, &automations) {
                    Ok(user) => (user, true),
                    Err(_) => {
                        // Identity was authenticated at this new ingress. An
                        // explicit current read override can authorize an old
                        // artifact without supplying missing fresh actor data.
                        // The carrier's default role grants nothing: no role
                        // rule is passed to the evaluator in this branch.
                        let mut identity = boss_policy_client::User::anonymous();
                        identity.id = ingress.actor_id().to_string();
                        (identity, false)
                    }
                };
                let (mut rules, overrides) =
                    boss_policy_client::postgres::PgPolicy::selection_rows_budgeted_in(
                        &mut tx,
                        &user.id,
                        &SelectionStatementRefresh(deadline.sql_budget()),
                    )
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                rules.retain(|rule| has_role && rule.role == user.role);
                let scope = require_native_read(
                    &user,
                    &rules,
                    &overrides,
                    boss_policy_client::engine::expiry_now(),
                )?;
                deadline.constrain_sql(&mut tx).await?;
                let original =
                    original_selection_in(&mut tx, request_id, ingress.actor_id(), station).await?;
                if let Some(receipt) = &original {
                    receipt.require_read_scope(&scope)?;
                    let id = receipt
                        .pointer("/selected/job_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
                        .map(boss_core::job::JobId::from_uuid)
                        .ok_or_else(|| {
                            JobsError::Storage(
                                "original selection receipt lacks packet identity".into(),
                            )
                        })?;
                    deadline.constrain_sql(&mut tx).await?;
                    let job = crate::postgres::get_job_on(&mut *tx, &id).await?;
                    if !job.as_ref().is_some_and(|job| scope.admits(job)) {
                        return Err(JobsError::SelectionConflict(
                            "original selection is outside current native read scope".into(),
                        ));
                    }
                }
                if require_native_read(
                    &user,
                    &rules,
                    &overrides,
                    boss_policy_client::engine::expiry_now(),
                )? != scope
                {
                    return Err(JobsError::SelectionConflict(
                        "native read authority expired during original lookup".into(),
                    ));
                }
                deadline.verify_before_commit_dispatch()?;
                tx.rollback()
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                Ok(crate::station_claim::NativeSelectionRead {
                    user,
                    scope,
                    original,
                })
            })
            .await
    }
    pub async fn observe_authority(
        &self,
        ingress: crate::station_claim::AuthenticatedSelectionIngress,
        request_id: uuid::Uuid,
        station_name: &str,
    ) -> Result<crate::station_claim::NativeAuthorityObservation, JobsError> {
        let deadline = SelectionDeadline::from_ingress(&ingress);
        deadline
            .before_dispatch(async {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                deadline.constrain_sql(&mut tx).await?;
                sqlx::query("LOCK TABLE workflows, stations IN SHARE MODE")
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                deadline.constrain_sql(&mut tx).await?;
                lock_actor_policy_rows_with_budget_in(&mut tx, Some(deadline.sql_budget())).await?;
                let (agents, automations) = crate::agents::PgAgents::selection_rows_budgeted_in(
                    &mut tx,
                    Some(deadline.sql_budget()),
                )
                .await
                .map_err(|error| JobsError::Storage(error.to_string()))?;
                let (agents, automations) = actor_inputs(agents, automations, ingress.actor_id());
                let user = native_user(&ingress, &agents, &automations)?;
                let (mut rules, overrides) =
                    boss_policy_client::postgres::PgPolicy::selection_rows_budgeted_in(
                        &mut tx,
                        &user.id,
                        &SelectionStatementRefresh(deadline.sql_budget()),
                    )
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                rules.retain(|rule| rule.role == user.role);
                let observed_at = boss_policy_client::engine::expiry_now();
                let (_, scope) = require_native_policy(&user, &rules, &overrides, observed_at)?;
                tx.rollback()
                    .await
                    .map_err(|error| JobsError::Storage(error.to_string()))?;
                Ok(crate::station_claim::NativeAuthorityObservation {
                    store_identity: self.identity.clone(),
                    request_id,
                    station_name: station_name.to_string(),
                    ingress,
                    user,
                    agents,
                    automations,
                    rules,
                    overrides,
                    observed_at,
                    scope,
                    budget_hour: None,
                })
            })
            .await
    }
    pub async fn consume_authority_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        observation: &crate::station_claim::NativeAuthorityObservation,
        context: &crate::station_claim::SelectionContext,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<serde_json::Value, JobsError> {
        if !self.observes(observation)
            || context.request_id != observation.request_id
            || context.station.name != observation.station_name
            || actor != observation.ingress.actor_id()
            || Some(stamp.actor()) != observation.user.ambient_actor().as_ref()
        {
            return Err(JobsError::SelectionConflict(
                "native selection constructor or authenticated request identity changed".into(),
            ));
        }
        let (agents, automations) = crate::agents::PgAgents::selection_rows_budgeted_in(
            &mut *tx,
            Some(SelectionDeadline::from_ingress(&observation.ingress).sql_budget()),
        )
        .await
        .map_err(|error| JobsError::Storage(error.to_string()))?;
        let (agents, automations) =
            actor_inputs(agents, automations, observation.ingress.actor_id());
        let user = native_user(&observation.ingress, &agents, &automations)?;
        if agents != observation.agents
            || automations != observation.automations
            || user != observation.user
        {
            return Err(JobsError::SelectionConflict(
                "native actor, aliases or role inputs changed before selection commit".into(),
            ));
        }
        let (mut rules, overrides) =
            boss_policy_client::postgres::PgPolicy::selection_rows_budgeted_in(
                &mut *tx,
                &user.id,
                &SelectionStatementRefresh(
                    SelectionDeadline::from_ingress(&observation.ingress).sql_budget(),
                ),
            )
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        rules.retain(|rule| rule.role == user.role);
        if rules != observation.rules || overrides != observation.overrides {
            return Err(JobsError::SelectionConflict(
                "native policy inputs changed before selection commit".into(),
            ));
        }
        require_native_claim_constraints(observation, context)?;
        let at = boss_policy_client::engine::expiry_now();
        let step_authority =
            require_native_step_authority(observation, &context.validated_step, at)?;
        let (decision, scope) = require_native_policy(&user, &rules, &overrides, at)?;
        if scope != context.scope {
            return Err(JobsError::SelectionConflict(
                "native policy read scope disagrees with prepared selection".into(),
            ));
        }
        let boss_policy_client::Decision::Allow {
            scope: update_scope,
        } = &decision
        else {
            return Err(JobsError::SelectionConflict(
                "native policy update authority unavailable".into(),
            ));
        };
        if !crate::policy_glue::scope_matches(&user, update_scope, &context.validated_job)
            && !crate::policy_glue::step_is_callers(
                &user,
                context.validated_job.status == boss_core::job::JobStatus::Open,
                &context.validated_step,
            )
        {
            return Err(JobsError::SelectionConflict(
                "native step update is outside its actual grant scope".into(),
            ));
        }
        SelectionDeadline::from_ingress(&observation.ingress).verify_before_commit_dispatch()?;
        let budget_hour = observation.budget_hour_observation(context);
        Ok(
            serde_json::json!({"version": 1, "method": "same NativeSelectionStore transaction holds actual definition, actor, policy and OPEN population writers",
            "ingress": observation.ingress.observation(), "budget_hour": budget_hour,
            "credential_executor": crate::station_claim::require_native_executor(observation, &context.validated_step, &context.validated_job)?,
            "prepared_inputs": observation.prepared_inputs(),
            "consumed_inputs": {"user": user, "agents": agents, "automations": automations, "rules": rules, "overrides": overrides,
                "update": decision, "read_scope": scope, "step_authority": step_authority, "evaluated_at": at}}),
        )
    }
    /// Held native writers cannot change the consumed rows. Re-evaluate time
    /// after the last SQL write, synchronously before COMMIT dispatch.
    pub(crate) fn verify_policy_at_dispatch(
        &self,
        observation: &crate::station_claim::NativeAuthorityObservation,
        context: &crate::station_claim::SelectionContext,
        stored: &boss_core::job::Step,
    ) -> Result<(), JobsError> {
        if !self.observes(observation) {
            return Err(JobsError::SelectionConflict(
                "native constructor changed before commit dispatch".into(),
            ));
        }
        require_native_claim_constraints(observation, context)?;
        crate::station_claim::require_native_executor(observation, stored, &context.validated_job)?;
        let (decision, scope) = require_native_policy(
            &observation.user,
            &observation.rules,
            &observation.overrides,
            boss_policy_client::engine::expiry_now(),
        )?;
        let boss_policy_client::Decision::Allow {
            scope: update_scope,
        } = decision
        else {
            return Err(JobsError::SelectionConflict(
                "native policy refuses commit dispatch".into(),
            ));
        };
        if scope != context.scope
            || (!crate::policy_glue::scope_matches(
                &observation.user,
                &update_scope,
                &context.validated_job,
            ) && !crate::policy_glue::step_is_callers(
                &observation.user,
                context.validated_job.status == boss_core::job::JobStatus::Open,
                &context.validated_step,
            ))
        {
            return Err(JobsError::SelectionConflict(
                "native scope changed before commit dispatch".into(),
            ));
        }
        require_native_step_authority(
            observation,
            &context.validated_step,
            boss_policy_client::engine::expiry_now(),
        )?;
        native_calendar_request(
            observation,
            stored,
            boss_policy_client::engine::expiry_now(),
        )?;
        SelectionDeadline::from_ingress(&observation.ingress).verify_before_commit_dispatch()
    }

    pub(crate) async fn consume_calendar_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        observation: &crate::station_claim::NativeAuthorityObservation,
        stored: &boss_core::job::Step,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<serde_json::Value, JobsError> {
        if !self.observes(observation)
            || stamp.actor().to_string() != observation.ingress.actor_id()
        {
            return Err(JobsError::SelectionConflict(
                "calendar consumption lacks its actual native constructor/caller".into(),
            ));
        }
        let at = boss_policy_client::engine::expiry_now();
        let Some((request, decision)) = native_calendar_request(observation, stored, at)? else {
            return Ok(
                serde_json::json!({"required": false, "reason": "stored step has no complete scheduling window"}),
            );
        };
        let write = boss_calendar::postgres::PgCalendar::reserve_joined_with_budget_in(
            tx,
            request.clone(),
            stamp,
            Some(SelectionDeadline::from_ingress(&observation.ingress).sql_budget()),
        )
        .await
        .map_err(|error| match error {
            boss_calendar::CalendarError::Conflict { existing } => {
                JobsError::SelectionCalendarConflict { existing }
            }
            other => JobsError::SelectionConflict(format!(
                "native calendar owner refuses selection: {other}"
            )),
        })?;
        Ok(
            serde_json::json!({"required": true, "request": request, "decision": decision, "evaluated_at": at, "consumed": write}),
        )
    }
    pub(crate) fn consume_concurrency(
        &self,
        observation: &crate::station_claim::NativeAuthorityObservation,
        context: &crate::station_claim::SelectionContext,
        population: &[boss_core::job::Job],
    ) -> Result<serde_json::Value, JobsError> {
        if !self.observes(observation) {
            return Err(JobsError::SelectionConflict(
                "native concurrency owner belongs to another constructor".into(),
            ));
        }
        let agent = observation.agents.iter().find(|agent| {
            agent.id == observation.user.id
                || agent
                    .aliases
                    .iter()
                    .any(|alias| alias == &observation.user.id)
        });
        let resolved = crate::agent_spec::resolved(
            &context.validated_step,
            context.active_workflows.get(&context.validated_job.kind),
        );
        let Some(agent) = agent else {
            return Ok(
                serde_json::json!({"required": false, "reason": "authenticated actor has no registered-agent row"}),
            );
        };
        let cap = agent
            .max_concurrent_runs
            .and_then(|cap| u32::try_from(cap).ok());
        if !crate::agent_budget::declares_an_agent_run(&resolved.metadata) || cap.is_none() {
            return Ok(
                serde_json::json!({"required": false, "reason": "step declares no agent run or actor declares no concurrency cap",
                "actor": agent, "declares_agent_run": crate::agent_budget::declares_an_agent_run(&resolved.metadata)}),
            );
        }
        let held_by: Vec<String> = std::iter::once(agent.id.clone())
            .chain(agent.aliases.iter().cloned())
            .collect();
        let open_runs: Vec<_> = population
            .iter()
            .filter(|job| job.kind == crate::agent_budget::RUN_KIND)
            .cloned()
            .collect();
        let count = crate::agent_budget::in_flight_runs(&open_runs, &held_by);
        let reading = crate::agent_budget::Concurrency::measure(&agent.id, cap, count);
        if !reading.decision.is_allowed() {
            return Err(JobsError::SelectionConflict(format!(
                "native concurrency refuses selection: {}",
                reading.refusal_body()
            )));
        }
        // Copy every field the existing counter actually consumes, plus the
        // packet identity. Unrelated packet payloads are not a counter input.
        let rows: Vec<_> = open_runs
            .iter()
            .filter(|job| crate::agent_budget::counted_run(job, &held_by))
            .map(|job| {
                serde_json::json!({"id": job.id, "kind": job.kind, "status": job.status,
                "agent": job.metadata.get(crate::agent_budget::RUN_AGENT_KEY),
                "owner_id":job.owner_id,"subject":job.subject})
            })
            .collect();
        Ok(
            serde_json::json!({"required": true, "actor": agent, "held_by": held_by,
            "rows_predicate":{"agent_in":held_by,"status":"open"},
            "runs": rows, "reading": reading, "method": "complete OPEN jobs population under the actual native jobs writer fence"}),
        )
    }

    /// Join only constructor-related owners. Required authority evidence is separate.
    pub async fn begin_for(
        &self,
        owner: &Self,
    ) -> Result<Transaction<'static, Postgres>, JobsError> {
        if !Arc::ptr_eq(&self.identity, &owner.identity) {
            return Err(JobsError::SelectionConflict(
                "native owner belongs to a different constructor instance".into(),
            ));
        }
        self.pool
            .begin()
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))
    }
    /// Fence actual policy row writers; other required authority owners are separate.
    pub async fn begin_policy_fence_for(
        &self,
        owner: &Self,
    ) -> Result<Transaction<'static, Postgres>, JobsError> {
        let mut tx = self.begin_for(owner).await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        lock_policy_rows_in(&mut tx).await?;
        Ok(tx)
    }
    pub async fn begin_actor_policy_fence_for(
        &self,
        owner: &Self,
    ) -> Result<Transaction<'static, Postgres>, JobsError> {
        let mut tx = self.begin_for(owner).await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        // Actor and alias writers precede policy owners in the native composition.
        lock_actor_policy_rows_in(&mut tx).await?;
        Ok(tx)
    }
    pub async fn begin_definition_actor_policy_fence_for(
        &self,
        owner: &Self,
    ) -> Result<Transaction<'static, Postgres>, JobsError> {
        let mut tx = self.begin_for(owner).await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        sqlx::query("LOCK TABLE workflows, stations IN SHARE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        lock_actor_policy_rows_in(&mut tx).await?;
        Ok(tx)
    }
    /// Original retry precedes fresh-owner checks. Caller read authority remains required.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn acquire_selection_for(
        &self,
        owner: &Self,
        request_id: uuid::Uuid,
        actor: &str,
        station: &str,
    ) -> Result<NativeSelectionAcquisition, JobsError> {
        self.acquire_selection_before(
            owner,
            request_id,
            actor,
            station,
            SelectionDeadline::at_ingress(),
        )
        .await
    }
    pub async fn acquire_selection_before(
        &self,
        owner: &Self,
        request_id: uuid::Uuid,
        actor: &str,
        station: &str,
        deadline: SelectionDeadline,
    ) -> Result<NativeSelectionAcquisition, JobsError> {
        deadline
            .before_dispatch(
                self.acquire_selection_inner(owner, request_id, actor, station, deadline),
            )
            .await
    }
    async fn acquire_selection_inner(
        &self,
        owner: &Self,
        request_id: uuid::Uuid,
        actor: &str,
        station: &str,
        deadline: SelectionDeadline,
    ) -> Result<NativeSelectionAcquisition, JobsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        deadline.constrain_sql(&mut tx).await?;
        if let Some(receipt) = original_selection_in(&mut tx, request_id, actor, station).await? {
            // A replay reads an already committed artifact. It dispatches no
            // commit, so deadline cancellation cannot turn it into a new write.
            tx.rollback()
                .await
                .map_err(|error| JobsError::Storage(error.to_string()))?;
            return Ok(NativeSelectionAcquisition::Replayed(receipt));
        }
        if !Arc::ptr_eq(&self.identity, &owner.identity) {
            return Err(JobsError::SelectionConflict(
                "native owner belongs to a different constructor instance".into(),
            ));
        }
        deadline.constrain_sql(&mut tx).await?;
        sqlx::query("LOCK TABLE workflows, stations IN SHARE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        deadline.constrain_sql(&mut tx).await?;
        lock_actor_policy_rows_with_budget_in(&mut tx, Some(deadline.sql_budget())).await?;
        deadline.constrain_sql(&mut tx).await?;
        boss_calendar::postgres::PgCalendar::lock_selection_rows_with_budget_in(
            &mut tx,
            Some(deadline.sql_budget()),
        )
        .await
        .map_err(|error| JobsError::Storage(error.to_string()))?;
        deadline.constrain_sql(&mut tx).await?;
        sqlx::query("LOCK TABLE jobs, steps, station_claim_receipts IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        deadline.constrain_sql(&mut tx).await?;
        if let Some(receipt) = original_selection_in(&mut tx, request_id, actor, station).await? {
            tx.rollback()
                .await
                .map_err(|error| JobsError::Storage(error.to_string()))?;
            return Ok(NativeSelectionAcquisition::Replayed(receipt));
        }
        deadline.constrain_sql(&mut tx).await?;
        Ok(NativeSelectionAcquisition::Fresh(tx))
    }
    /// Read complete native definition inputs, not an authorization decision.
    pub async fn active_definitions_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<
        (
            Vec<crate::registry::WorkflowSpec>,
            Vec<crate::stations::StationSpec>,
        ),
        JobsError,
    > {
        let workflows = crate::registry::active_workflow_rows_in(&mut **tx, None)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        let stations = crate::stations::active_station_rows_in(&mut **tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        Ok((workflows, stations))
    }
    pub async fn open_population_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<boss_core::job::Job>, JobsError> {
        crate::postgres::open_population_in(tx).await
    }
    pub async fn begin_all_input_fence_for(
        &self,
        owner: &Self,
    ) -> Result<Transaction<'static, Postgres>, JobsError> {
        let mut tx = self.begin_definition_actor_policy_fence_for(owner).await?;
        sqlx::query("LOCK TABLE jobs, steps, station_claim_receipts IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
        Ok(tx)
    }
}
async fn original_selection_in(
    tx: &mut Transaction<'_, Postgres>,
    request_id: uuid::Uuid,
    actor: &str,
    station: &str,
) -> Result<Option<crate::station_claim::SelectionReceipt>, JobsError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT receipt FROM station_claim_receipts WHERE request_id = $1")
            .bind(request_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|error| JobsError::Storage(error.to_string()))?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let receipt = crate::station_claim::SelectionReceipt::from_json(raw)?;
    crate::station_claim::replay(&receipt, actor, station, request_id)?;
    Ok(Some(receipt))
}
async fn lock_actor_policy_rows_in(tx: &mut Transaction<'_, Postgres>) -> Result<(), JobsError> {
    lock_actor_policy_rows_with_budget_in(tx, None).await
}
async fn lock_actor_policy_rows_with_budget_in(
    tx: &mut Transaction<'_, Postgres>,
    budget: Option<boss_events::statement_budget::StatementBudget>,
) -> Result<(), JobsError> {
    boss_events::statement_budget::refresh(tx, budget)
        .await
        .map_err(JobsError::Storage)?;
    sqlx::query("LOCK TABLE agents, actor_aliases, automation_actors IN SHARE MODE")
        .execute(&mut **tx)
        .await
        .map_err(|error| JobsError::Storage(error.to_string()))?;
    boss_events::statement_budget::refresh(tx, budget)
        .await
        .map_err(JobsError::Storage)?;
    lock_policy_rows_in(tx).await
}

fn require_native_read(
    user: &boss_policy_client::User,
    rules: &[boss_policy_client::PolicyRule],
    overrides: &[boss_policy_client::UserOverride],
    at: chrono::DateTime<chrono::Utc>,
) -> Result<crate::port::JobScope, JobsError> {
    use boss_policy_client::Decision;
    let control = boss_policy_client::controls::READ_JOB;
    let read_id =
        boss_policy_client::types::rule_id(&user.role, &control.resource(), control.action());
    let (read, _) = boss_policy_client::engine::evaluate_snapshot(
        user,
        control.action(),
        &control.resource(),
        overrides,
        rules.iter().find(|row| row.id == read_id),
        at,
    );
    let Decision::Allow { scope } = read else {
        return Err(JobsError::SelectionConflict(
            "native policy refuses current job read authority".into(),
        ));
    };
    let predicate = boss_policy_client::predicates::scope_to_predicate(&scope, user);
    Ok(crate::port::JobScope::from_predicate(user, &predicate))
}
fn require_native_policy(
    user: &boss_policy_client::User,
    rules: &[boss_policy_client::PolicyRule],
    overrides: &[boss_policy_client::UserOverride],
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(boss_policy_client::Decision, crate::port::JobScope), JobsError> {
    use boss_policy_client::Decision;
    let scope = require_native_read(user, rules, overrides, at)?;
    let control = boss_policy_client::controls::UPDATE_STEP;
    let update_id =
        boss_policy_client::types::rule_id(&user.role, &control.resource(), control.action());
    let (update, _) = boss_policy_client::engine::evaluate_snapshot(
        user,
        control.action(),
        &control.resource(),
        overrides,
        rules.iter().find(|row| row.id == update_id),
        at,
    );
    if matches!(update, Decision::Deny { .. }) {
        return Err(JobsError::SelectionConflict(
            "native policy refuses current step update authority".into(),
        ));
    }
    Ok((update, scope))
}
async fn lock_policy_rows_in(tx: &mut Transaction<'_, Postgres>) -> Result<(), JobsError> {
    sqlx::query("LOCK TABLE policy_rules, policy_user_overrides IN SHARE MODE")
        .execute(&mut **tx)
        .await
        .map_err(|error| JobsError::Storage(error.to_string()))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn phase_evidence_keeps_original_ingress_and_names_cancelled_work() {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = records.clone();
        let sink: Arc<dyn Fn(serde_json::Value) + Send + Sync> = Arc::new(move |record| {
            captured.lock().unwrap().push(record);
        });
        let ingress = std::time::Instant::now() - std::time::Duration::from_secs(2);
        let pending_sink = sink.clone();
        let mut pending = Box::pin(async move {
            let mut trace = SelectionPhase::start_to("pool.acquire".into(), ingress, pending_sink);
            std::future::pending::<()>().await;
            trace.finish("returned");
        });
        assert!(futures::poll!(pending.as_mut()).is_pending());
        drop(pending);
        let mut returned = SelectionPhase::start_to("station.read".into(), ingress, sink);
        returned.finish("returned");
        drop(returned);
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["phase"], "pool.acquire");
        assert_eq!(records[0]["event"], "start");
        assert_eq!(records[1]["event"], "cancelled");
        assert_eq!(records[0]["operation"], records[1]["operation"]);
        assert_eq!(records[2]["phase"], "station.read");
        assert_eq!(records[3]["event"], "returned");
        for record in records.iter() {
            assert_eq!(record["ingress"], format!("{ingress:?}"));
            assert!(record["ingress_elapsed_us"].as_u64().unwrap() >= 2_000_000);
        }
    }

    struct DelayedPolicyRefresh {
        budget: SelectionStatementRefresh,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl boss_policy_client::postgres::StatementRefresh for DelayedPolicyRefresh {
        async fn before_statement(
            &self,
            conn: &mut sqlx::PgConnection,
        ) -> Result<(), boss_policy_client::PolicyError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
            boss_policy_client::postgres::StatementRefresh::before_statement(&self.budget, conn)
                .await
        }
    }

    #[tokio::test]
    async fn shared_policy_refresh_does_not_restart_the_original_deadline() {
        let db = boss_testing::TestDb::new().await;
        let deadline = SelectionDeadline::at_ingress();
        let mut tx = db.pool.begin().await.unwrap();
        let refresh = DelayedPolicyRefresh {
            budget: SelectionStatementRefresh(deadline.sql_budget()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        boss_policy_client::postgres::PgPolicy::selection_rows_budgeted_in(
            &mut tx,
            "fixture-user",
            &refresh,
        )
        .await
        .unwrap();
        assert_eq!(refresh.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let configured_ms: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM current_setting('statement_timeout')::interval) * 1000)::bigint")
            .fetch_one(&mut *tx).await.unwrap();
        let remaining_ms = deadline.remaining().unwrap().as_millis() as i64;
        assert!(
            configured_ms <= remaining_ms + 100 && configured_ms < 4000,
            "second policy query retained {configured_ms}ms; original ingress has {remaining_ms}ms"
        );
        tx.rollback().await.unwrap();
    }

    async fn resolved_ingress() -> crate::station_claim::AuthenticatedSelectionIngress {
        resolved_ingress_and_gate().await.0
    }
    async fn resolved_ingress_and_gate() -> (
        crate::station_claim::AuthenticatedSelectionIngress,
        Arc<boss_core::machine_gate::MachineGate>,
    ) {
        use tower::ServiceExt;
        let dir = boss_testing::scratch_dir("native-selection-ingress");
        boss_testing::write_file(&dir.join("forge.current"), "native-selection-fixture-key");
        let captured = Arc::new(std::sync::Mutex::new(None));
        let sink = captured.clone();
        let app = axum::Router::new().route(
            "/fixture-ingress",
            axum::routing::get(
                move |proof: axum::Extension<
                    crate::station_claim::AuthenticatedSelectionIngress,
                >| {
                    let sink = sink.clone();
                    async move {
                        *sink.lock().unwrap() = Some(proof.0);
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            ),
        );
        let app = crate::runner_credential::mount(app, dir);
        let gate = Arc::new(boss_core::machine_gate::MachineGate::new(
            "jobs",
            &["/health"],
            boss_core::machine_gate::Reading::new(
                boss_core::machine_gate::Mode::Off,
                Default::default(),
            ),
        ));
        let app = boss_core::machine_gate::gated(app, gate.clone());
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/fixture-ingress")
                    .header(
                        crate::runner_credential::HEADER,
                        "native-selection-fixture-key",
                    )
                    .header(
                        "x-boss-user",
                        serde_json::to_string(&boss_policy_client::User::service("forged-owner"))
                            .unwrap(),
                    )
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        let proof = captured.lock().unwrap().take().unwrap();
        (proof, gate)
    }
    #[tokio::test]
    async fn a_mode_move_changes_new_ingress_without_rewriting_the_admitted_operation() {
        use boss_core::machine_gate::{Mode, Reading, RequestModeObservation};
        use tower::ServiceExt;
        let (proof, gate) = resolved_ingress_and_gate().await;
        let original_epoch = proof.started();
        assert_eq!(proof.observation()["machine_mode"], "off");
        gate.observe(Reading::new(Mode::Enforce, Default::default()));
        assert_eq!(gate.reading().mode, Mode::Enforce);
        assert_eq!(proof.observation()["machine_mode"], "off");
        assert_eq!(
            proof.started(),
            original_epoch,
            "a mode refresh cannot restart the five-second request budget"
        );
        // A subsequent request sees the new reading. A health door lets the
        // mode observation be examined without inventing a shared-token actor.
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(|mode: axum::Extension<RequestModeObservation>| async move {
                mode.mode().name()
            }),
        );
        let app = boss_core::machine_gate::gated(app, gate);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 32)
                .await
                .unwrap(),
            "enforce"
        );
    }
    struct NativeClaimFixture {
        db: boss_testing::TestDb,
        store: NativeSelectionStore,
        repo: crate::PgJobs,
        stamp: boss_core::publisher::EventStamp,
        policy: boss_policy_client::postgres::PgPolicy,
        context: crate::station_claim::SelectionContext,
    }
    async fn native_claim_fixture() -> NativeClaimFixture {
        native_claim_fixture_with_preparation_delay(std::time::Duration::ZERO).await
    }
    async fn native_claim_fixture_with_preparation_delay(
        preparation_delay: std::time::Duration,
    ) -> NativeClaimFixture {
        use crate::agents::{AgentsRegistry, PgAgents};
        use crate::{JobsRepository, StationRegistry, WorkflowRegistry};
        use boss_policy_client::{Action, PolicyRepository, PolicyRule, Resource, Scope};
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let repo = crate::PgJobs::for_native_selection(store.clone());
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("ops-runner"),
        );
        let registry = PgAgents::new(db.pool.clone());
        registry
            .declare_automations(
                &[crate::agents::automations::AutomationActor {
                    id: crate::runner_credential::ACTOR.into(),
                    role: "platform-admin".into(),
                    description: "native claim owning fixture".into(),
                    signs_for: None,
                }],
                &stamp,
            )
            .await
            .unwrap();
        let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy
                .upsert_rule(
                    &PolicyRule {
                        id: boss_policy_client::types::rule_id("platform-admin", &resource, action),
                        role: "platform-admin".into(),
                        resource,
                        action,
                        scope: Scope::All,
                        active: true,
                    },
                    "native-owner-fixture",
                )
                .await
                .unwrap();
        }
        let mut job = boss_core::job::Job::new(
            "fixture",
            boss_core::job::Subject::new("custom", "fixture"),
            "Native",
            "emp-owner",
            boss_core::job::Priority::Standard,
            stamp.timestamp.date_naive(),
        );
        job.status = boss_core::job::JobStatus::Open;
        let mut step = boss_core::job::Step::new(job.id, "task", "Work", 0);
        step.status = boss_core::job::StepStatus::Ready;
        step.spec_slug = Some("work".into());
        repo.create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(
                crate::events::JOB_CREATED,
                serde_json::to_value(&job).unwrap(),
            )],
        )
        .await
        .unwrap();
        repo.add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(
                crate::events::STEP_CREATED,
                crate::events::step_state_payload(&step),
            )],
        )
        .await
        .unwrap();
        let stations = crate::PgStations::new(db.pool.clone());
        let station = crate::StationSpec::draft(
            "fixture",
            "Fixture",
            crate::StationKind::Batch,
            crate::station_queue::StationPredicate {
                kind: Some("fixture".into()),
                step: Some(crate::station_queue::StepMatch {
                    slug: Some("work".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            stamp.timestamp,
        );
        stations
            .create_draft(station, stamp.actor(), stamp.timestamp)
            .await
            .unwrap();
        let station = stations
            .publish("fixture", stamp.actor(), stamp.timestamp)
            .await
            .unwrap();
        let request_id = uuid::Uuid::new_v4();
        tokio::time::sleep(preparation_delay).await;
        let active_workflows = crate::PgWorkflows::new(db.pool.clone())
            .list_active(None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.kind.clone(), row))
            .collect();
        let validated_job = repo.get_job(&job.id).await.unwrap().unwrap();
        let validated_step = repo.get_step(&step.id).await.unwrap().unwrap();
        // Test-row preparation is not part of the request under test. Begin
        // its unchanged ingress budget after the fixture's reads complete.
        let observation = store
            .observe_authority(resolved_ingress().await, request_id, "fixture")
            .await
            .unwrap();
        let context = crate::station_claim::SelectionContext {
            native_authority: Some(Arc::new(observation)),
            actor_observation: None,
            prepared_policy: None,
            definition_fence: None,
            request_id,
            station,
            scope: crate::port::JobScope::All,
            active_workflows,
            validated_job,
            validated_step,
        };
        NativeClaimFixture {
            db,
            store,
            repo,
            stamp,
            policy,
            context,
        }
    }
    #[tokio::test]
    async fn fixture_preparation_does_not_spend_the_native_claim_ingress_budget() {
        use crate::JobsRepository;
        let NativeClaimFixture {
            db: _db,
            repo,
            stamp,
            context,
            ..
        } = native_claim_fixture_with_preparation_delay(
            crate::station_claim::SELECTION_PRE_COMMIT_BUDGET + std::time::Duration::from_secs(1),
        )
        .await;
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        assert!(matches!(
            result,
            crate::station_claim::SelectionResult::Claimed { .. }
        ));
    }
    #[tokio::test]
    async fn direct_native_original_replay_requires_current_read_authority() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            policy,
            context,
            ..
        } = native_claim_fixture().await;
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap()
        else {
            panic!("fresh claim");
        };
        let original = receipt.json().to_owned();
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::job(),
                    Action::Read,
                ),
                "retire-current-read-after-original",
            )
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        assert!(
            matches!(result, Err(crate::JobsError::SelectionConflict(_))),
            "a direct native mutation port cannot expose original replay after current Read retirement"
        );
        assert_eq!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .unwrap()
                .json(),
            original
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn reviewer_native_claim_retains_credential_executor_host_guard() {
        use crate::JobsRepository;
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            mut context,
            ..
        } = native_claim_fixture().await;
        context.validated_step.metadata[crate::credential_executor::KEY] =
            serde_json::json!(crate::runner_credential::PRINCIPAL);
        context.validated_job.metadata[crate::field_writer::HOST_KEY] =
            serde_json::json!("boss-gcp");
        repo.update_job(&context.validated_job).await.unwrap();
        repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
            .await
            .unwrap();
        context.validated_job = repo
            .get_job(&context.validated_job.id)
            .await
            .unwrap()
            .unwrap();
        context.validated_step = repo
            .get_step(&context.validated_step.id)
            .await
            .unwrap()
            .unwrap();
        let proof = context
            .native_authority
            .as_ref()
            .unwrap()
            .ingress
            .observation();
        assert_eq!(proof["host"], "forge");
        assert_eq!(proof["principal"], crate::runner_credential::PRINCIPAL);
        let caller = crate::field_writer::CredentialedCaller {
            principal: proof["principal"].as_str().unwrap().to_owned(),
            actor_id: proof["actor_id"].as_str().unwrap().to_owned(),
            host: proof["host"].as_str().map(str::to_owned),
        };
        assert!(
            crate::credential_executor::judge(
                &context.validated_step.metadata,
                Some(&caller),
                Some("boss-gcp")
            )
            .is_err(),
            "existing HTTP guard must reject these exact resolved facts"
        );
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        assert!(
            matches!(result, Err(JobsError::SelectionConflict(_))),
            "forge-resolved executor must not acquire a boss-gcp packet through the native port"
        );
        assert_eq!(
            repo.get_step(&context.validated_step.id)
                .await
                .unwrap()
                .unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            before,
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn native_pg_executor_guard_preserves_matching_and_ordinary_flows() {
        use crate::JobsRepository;
        for (declaration, host, allowed) in [
            (
                Some(serde_json::json!(crate::runner_credential::PRINCIPAL)),
                Some("forge"),
                true,
            ),
            (
                Some(serde_json::json!("runner:other")),
                Some("forge"),
                false,
            ),
            (Some(serde_json::Value::Null), Some("forge"), false),
            (
                Some(serde_json::json!(crate::runner_credential::PRINCIPAL)),
                None,
                false,
            ),
            (None, Some("boss-gcp"), true),
        ] {
            let NativeClaimFixture {
                db,
                repo,
                stamp,
                mut context,
                ..
            } = native_claim_fixture().await;
            if let Some(declaration) = declaration {
                context.validated_step.metadata[crate::credential_executor::KEY] = declaration;
            }
            if let Some(host) = host {
                context.validated_job.metadata[crate::field_writer::HOST_KEY] =
                    serde_json::json!(host);
            }
            repo.update_job(&context.validated_job).await.unwrap();
            repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
                .await
                .unwrap();
            context.validated_job = repo
                .get_job(&context.validated_job.id)
                .await
                .unwrap()
                .unwrap();
            context.validated_step = repo
                .get_step(&context.validated_step.id)
                .await
                .unwrap()
                .unwrap();
            let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            let result = repo
                .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
                .await;
            if allowed {
                assert!(
                    matches!(
                        result,
                        Ok(crate::station_claim::SelectionResult::Claimed { .. })
                    ),
                    "matching or undeclared executor must retain ordinary claim behavior"
                );
            } else {
                assert!(
                    matches!(result, Err(JobsError::SelectionConflict(_))),
                    "wrong principal/malformed/missing host must refuse"
                );
                assert_eq!(
                    repo.get_step(&context.validated_step.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    context.validated_step
                );
                assert!(
                    repo.station_claim_receipt(context.request_id)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    before,
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                        .fetch_one(&db.pool)
                        .await
                        .unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn configured_native_claim_conserves_the_actual_consumed_owners() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            policy,
            context,
            ..
        } = native_claim_fixture().await;
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { step, receipt } = result else {
            panic!("fixture is a fresh claim")
        };
        assert_eq!(step.status, boss_core::job::StepStatus::Active);
        assert!(
            receipt.get("owner_consumption").is_some(),
            "a native claim must prove which real owners it consumed"
        );
        assert_eq!(
            receipt
                .pointer("/owner_consumption/ingress/actor_id")
                .unwrap(),
            crate::runner_credential::ACTOR
        );
        assert!(
            receipt
                .pointer("/owner_consumption/consumed_inputs/evaluated_at")
                .is_some(),
            "pre-write evaluation time must be named as an evaluation, not claimed as COMMIT linearization"
        );
        assert!(
            receipt
                .pointer("/owner_consumption/consumed_inputs/linearized_at")
                .is_none()
        );
        let original = receipt.json().to_string();
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::step(),
                    Action::Update,
                ),
                "retire-after-original-commit",
            )
            .await
            .unwrap();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Replayed(receipt) = result else {
            panic!("original replay must precede fresh owner retirement")
        };
        assert_eq!(receipt.json(), original);
        // Even the replay read spends the original request's pool budget.
        // Occupy every actual connection; a repository read must not silently
        // add six seconds before it reaches the owner/deadline checks.
        let mut held = Vec::new();
        for _ in 0..db.pool.options().get_max_connections() {
            held.push(db.pool.acquire().await.unwrap());
        }
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            drop(held);
        });
        let began = tokio::time::Instant::now();
        let blocked = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        let elapsed = began.elapsed();
        release.await.unwrap();
        assert!(
            blocked.is_err(),
            "pool exhaustion must refuse within the ingress budget"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(5800),
            "receipt lookup exceeded the ingress budget: {elapsed:?}"
        );
    }
    #[tokio::test]
    async fn commit_completion_is_not_canceled_by_the_pre_dispatch_sql_budget() {
        use crate::JobsRepository;
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            context,
            ..
        } = native_claim_fixture().await;
        sqlx::raw_sql("CREATE FUNCTION slow_native_receipt_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(6); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER slow_native_receipt_commit AFTER INSERT ON station_claim_receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION slow_native_receipt_commit()")
            .execute(&db.pool).await.unwrap();
        let began = tokio::time::Instant::now();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } = result.unwrap() else {
            panic!("fresh native commit must return its original receipt")
        };
        assert!(
            began.elapsed() >= std::time::Duration::from_secs(6),
            "the actual COMMIT must exercise its deferred owning trigger"
        );
        assert_eq!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .unwrap(),
            receipt
        );
        assert_eq!(
            repo.get_step(&context.validated_step.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            boss_core::job::StepStatus::Active
        );
    }
    #[tokio::test]
    async fn joined_claim_holds_the_real_policy_writer_until_receipt_commit() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            policy,
            context,
            store,
        } = native_claim_fixture().await;
        assert!(store.observes(context.native_authority.as_ref().unwrap()));
        sqlx::raw_sql("CREATE FUNCTION pause_native_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(hashtext(current_database()), 1419); RETURN NEW; END $$; CREATE TRIGGER pause_native_receipt BEFORE INSERT ON station_claim_receipts FOR EACH ROW EXECUTE FUNCTION pause_native_receipt()")
            .execute(&db.pool).await.unwrap();
        let mut barrier = db.pool.acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock(hashtext(current_database()), 1419)")
            .execute(&mut *barrier)
            .await
            .unwrap();
        let repo = Arc::new(repo);
        let claimant = repo.clone();
        let request = context.clone();
        let claim_stamp = stamp.clone();
        let claim = tokio::spawn(async move {
            claimant
                .claim_station_head_at(
                    &request,
                    crate::runner_credential::ACTOR,
                    &[],
                    &claim_stamp,
                    &[],
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event = 'advisory' AND query LIKE 'INSERT INTO station_claim_receipts%')").fetch_one(&db.pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let rule_id =
            boss_policy_client::types::rule_id("platform-admin", &Resource::step(), Action::Update);
        let mut writer = tokio::spawn(async move {
            policy
                .deactivate_rule(&rule_id, "retire-during-native-claim")
                .await
        });
        wait_for_actual_writer(&db.pool, "policy_rules", &mut writer).await;
        assert!(
            !writer.is_finished(),
            "real policy writer must conflict with the joined claim, not copied observations"
        );
        sqlx::query("SELECT pg_advisory_unlock(hashtext(current_database()), 1419)")
            .execute(&mut *barrier)
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } =
            claim.await.unwrap().unwrap()
        else {
            panic!("fresh native claim expected")
        };
        writer.await.unwrap().unwrap();
        let crate::station_claim::SelectionResult::Replayed(original) = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap()
        else {
            panic!("original must survive subsequent retirement")
        };
        assert_eq!(receipt.json(), original.json());
    }
    #[tokio::test]
    async fn policy_expiring_during_joined_sql_rolls_back_step_receipt_and_outbox() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource, Scope, UserOverride};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            policy,
            store,
            mut context,
        } = native_claim_fixture().await;
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::step(),
                    Action::Update,
                ),
                "expiry-fixture",
            )
            .await
            .unwrap();
        policy
            .upsert_user_override(
                &UserOverride {
                    id: "native-expiring-update".into(),
                    user_id: crate::runner_credential::ACTOR.into(),
                    resource: Resource::step(),
                    action: Action::Update,
                    scope: Scope::All,
                    reason: "actual timed grant owning control".into(),
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::milliseconds(800)),
                },
                "expiry-fixture",
            )
            .await
            .unwrap();
        context.native_authority = Some(Arc::new(
            store
                .observe_authority(resolved_ingress().await, context.request_id, "fixture")
                .await
                .unwrap(),
        ));
        sqlx::raw_sql("CREATE FUNCTION delay_native_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1.2); RETURN NEW; END $$; CREATE TRIGGER delay_native_receipt BEFORE INSERT ON station_claim_receipts FOR EACH ROW EXECUTE FUNCTION delay_native_receipt()")
            .execute(&db.pool).await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let began = tokio::time::Instant::now();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        assert!(
            began.elapsed() >= std::time::Duration::from_secs(1),
            "the refusal must follow the actual SQL delay, not fail at preparation"
        );
        assert!(
            matches!(result, Err(JobsError::SelectionConflict(ref reason)) if reason == "native policy refuses current step update authority"),
            "a grant that expires during real SQL must refuse at the final policy check"
        );
        assert_eq!(
            repo.get_step(&context.validated_step.id)
                .await
                .unwrap()
                .unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            before, after,
            "expired authority must roll back every state-event outbox write"
        );
    }
    #[tokio::test]
    async fn step_authority_expiring_during_joined_sql_cannot_commit_the_claim() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource, Scope, UserOverride};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            policy,
            store,
            mut context,
        } = native_claim_fixture().await;
        context.validated_step.metadata["authority_role"] = serde_json::json!("native-signature");
        repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
            .await
            .unwrap();
        policy
            .upsert_user_override(
                &UserOverride {
                    id: "native-expiring-step-signature".into(),
                    user_id: crate::runner_credential::ACTOR.into(),
                    resource: Resource::new("step-signoff:native-signature"),
                    action: Action::SignOff,
                    scope: Scope::All,
                    reason: "actual step authority expiry".into(),
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::milliseconds(800)),
                },
                "step-authority-fixture",
            )
            .await
            .unwrap();
        context.native_authority = Some(Arc::new(
            store
                .observe_authority(resolved_ingress().await, context.request_id, "fixture")
                .await
                .unwrap(),
        ));
        sqlx::raw_sql("CREATE FUNCTION delay_native_step_authority() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1.2); RETURN NEW; END $$; CREATE TRIGGER delay_native_step_authority BEFORE INSERT ON station_claim_receipts FOR EACH ROW EXECUTE FUNCTION delay_native_step_authority()")
            .execute(&db.pool).await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let began = tokio::time::Instant::now();
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        assert!(began.elapsed() >= std::time::Duration::from_secs(1));
        assert!(
            matches!(result, Err(JobsError::SelectionConflict(ref reason)) if reason.contains("step authority")),
            "expired step authority must refuse at actual commit dispatch"
        );
        assert_eq!(
            repo.get_step(&context.validated_step.id)
                .await
                .unwrap()
                .unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            before,
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn direct_native_claim_cannot_skip_station_capability_or_human_only_constraints() {
        use crate::JobsRepository;
        use crate::stations::StationRegistry;
        for constraint in ["role", "model", "human"] {
            let NativeClaimFixture {
                db,
                repo,
                stamp,
                mut context,
                ..
            } = native_claim_fixture().await;
            if constraint == "human" {
                context.validated_step.metadata[crate::human_only::KEY] = serde_json::json!(true);
                repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
                    .await
                    .unwrap();
            } else {
                let stations = crate::PgStations::new(db.pool.clone());
                let mut changed = context.station.clone();
                changed.capability = Some(crate::stations::StationCapability {
                    roles: if constraint == "role" {
                        vec!["another-role".into()]
                    } else {
                        vec![]
                    },
                    models: if constraint == "model" {
                        vec!["a-required-model".into()]
                    } else {
                        vec![]
                    },
                });
                stations
                    .create_draft(changed, stamp.actor(), stamp.timestamp)
                    .await
                    .unwrap();
                context.station = stations
                    .publish("fixture", stamp.actor(), stamp.timestamp)
                    .await
                    .unwrap();
            }
            let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            let result = repo
                .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
                .await;
            if constraint == "model" {
                assert!(
                    matches!(
                        result,
                        Ok(crate::station_claim::SelectionResult::Claimed { .. })
                    ),
                    "existing allows_model contract admits a non-agent with no model; do not invent a new constraint"
                );
                continue;
            }
            assert!(
                matches!(result, Err(JobsError::SelectionConflict(_))),
                "actual native commit must enforce {constraint} below HTTP"
            );
            assert_eq!(
                repo.get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                context.validated_step
            );
            assert!(
                repo.station_claim_receipt(context.request_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                before,
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                    .fetch_one(&db.pool)
                    .await
                    .unwrap()
            );
        }
    }
    #[tokio::test]
    async fn native_receipt_copies_consumed_inputs_without_unrelated_owner_rows() {
        use crate::JobsRepository;
        use crate::agents::AgentsRegistry;
        use boss_policy_client::{Action, PolicyRepository, PolicyRule, Resource, Scope};
        let NativeClaimFixture {
            repo,
            stamp,
            policy,
            store,
            mut context,
            db,
        } = native_claim_fixture().await;
        let unrelated = "unrelated-private-owner-control";
        crate::agents::PgAgents::new(db.pool.clone())
            .declare_automations(
                &[crate::agents::automations::AutomationActor {
                    id: format!("automation:{unrelated}"),
                    role: unrelated.into(),
                    description: unrelated.into(),
                    signs_for: None,
                }],
                &stamp,
            )
            .await
            .unwrap();
        policy
            .upsert_rule(
                &PolicyRule {
                    id: boss_policy_client::types::rule_id(
                        unrelated,
                        &Resource::new(unrelated),
                        Action::Read,
                    ),
                    role: unrelated.into(),
                    resource: Resource::new(unrelated),
                    action: Action::Read,
                    scope: Scope::All,
                    active: true,
                },
                "owner-copy-fixture",
            )
            .await
            .unwrap();
        context.native_authority = Some(Arc::new(
            store
                .observe_authority(resolved_ingress().await, context.request_id, "fixture")
                .await
                .unwrap(),
        ));
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } = result else {
            panic!("fresh claim");
        };
        assert!(
            !receipt.json().contains(unrelated),
            "Job-readable receipt must conserve the full inputs actually used without exposing unrelated authority rows"
        );
        assert_eq!(
            receipt
                .pointer("/owner_consumption/consumed_inputs/user/id")
                .unwrap(),
            crate::runner_credential::ACTOR
        );
        assert!(
            receipt
                .pointer("/owner_consumption/consumed_inputs/step_authority")
                .is_some(),
            "step-specific authority result is copied, never inferred from a generic Update pass"
        );
    }
    async fn declared_native_agent_alias_fixture() -> NativeClaimFixture {
        use crate::agents::AgentsRegistry;
        let mut fixture = native_claim_fixture().await;
        sqlx::query("INSERT INTO agent_rate_card (model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, note) VALUES ('native-alias-model', 1, 1, 'synthetic owning fixture, not measured provider pricing')")
            .execute(&fixture.db.pool).await.unwrap();
        crate::agents::PgAgents::new(fixture.db.pool.clone())
            .publish(
                &[crate::agents::AgentInput {
                    id: "agent-native-alias".into(),
                    display_name: "Native alias fixture".into(),
                    default_model: "native-alias-model".into(),
                    aliases: vec![crate::runner_credential::ACTOR.into()],
                    role: Some("platform-admin".into()),
                    department: None,
                    hourly_budget_usd_micros: Some(1),
                    max_concurrent_runs: Some(1),
                }],
                boss_core::publish::PublishMode::InsertIfAbsent,
                &fixture.stamp,
            )
            .await
            .unwrap();
        fixture.context.native_authority = Some(Arc::new(
            fixture
                .store
                .observe_authority(
                    resolved_ingress().await,
                    fixture.context.request_id,
                    "fixture",
                )
                .await
                .unwrap(),
        ));
        fixture
    }
    #[tokio::test]
    async fn native_declared_alias_consumes_its_actual_model_in_station_constraints() {
        use crate::JobsRepository;
        use crate::stations::StationRegistry;
        for allowed in [true, false] {
            let NativeClaimFixture {
                db,
                repo,
                stamp,
                mut context,
                ..
            } = declared_native_agent_alias_fixture().await;
            let stations = crate::PgStations::new(db.pool.clone());
            let mut changed = context.station.clone();
            changed.capability = Some(crate::stations::StationCapability {
                roles: vec!["platform-admin".into()],
                models: vec![
                    if allowed {
                        "native-alias-model"
                    } else {
                        "a-different-model"
                    }
                    .into(),
                ],
            });
            stations
                .create_draft(changed, stamp.actor(), stamp.timestamp)
                .await
                .unwrap();
            context.station = stations
                .publish("fixture", stamp.actor(), stamp.timestamp)
                .await
                .unwrap();
            let result = repo
                .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
                .await;
            if allowed {
                let crate::station_claim::SelectionResult::Claimed { receipt, .. } =
                    result.unwrap()
                else {
                    panic!("fresh alias claim");
                };
                assert_eq!(
                    receipt
                        .pointer("/owner_consumption/consumed_inputs/agents/0/default_model")
                        .unwrap(),
                    "native-alias-model"
                );
                assert_eq!(
                    receipt
                        .pointer("/owner_consumption/ingress/actor_id")
                        .unwrap(),
                    crate::runner_credential::ACTOR
                );
            } else {
                assert!(
                    matches!(result, Err(JobsError::SelectionConflict(ref reason)) if reason.contains("model")),
                    "declared alias must not be treated as an unregistered no-model actor"
                );
                assert_eq!(
                    repo.get_step(&context.validated_step.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    context.validated_step
                );
                assert!(
                    repo.station_claim_receipt(context.request_id)
                        .await
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
    #[tokio::test]
    async fn native_alias_cap_uses_complete_real_open_run_population_at_commit() {
        use crate::JobsRepository;
        for in_flight in [true, false] {
            let NativeClaimFixture {
                db,
                repo,
                stamp,
                mut context,
                ..
            } = declared_native_agent_alias_fixture().await;
            context.validated_step.metadata[crate::agent_spec::MODEL_KEY] =
                serde_json::json!("native-alias-model");
            repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
                .await
                .unwrap();
            if in_flight {
                let mut run = boss_core::job::Job::new(
                    crate::agent_budget::RUN_KIND,
                    boss_core::job::Subject::new("custom", "native-cap"),
                    "Counted actual native run",
                    "agent-native-alias",
                    boss_core::job::Priority::Standard,
                    stamp.timestamp.date_naive(),
                );
                run.status = boss_core::job::JobStatus::Open;
                run.metadata[crate::agent_budget::RUN_AGENT_KEY] =
                    serde_json::json!("agent-native-alias");
                repo.create_job_at(
                    &run,
                    stamp.timestamp,
                    &[stamp.event(
                        crate::events::JOB_CREATED,
                        serde_json::to_value(&run).unwrap(),
                    )],
                )
                .await
                .unwrap();
            }
            let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            let result = repo
                .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
                .await;
            if in_flight {
                assert!(
                    matches!(result, Err(JobsError::SelectionConflict(ref reason)) if reason.contains("concurrency")),
                    "real native cap must count the committed OPEN run under its actual jobs writer fence"
                );
                assert_eq!(
                    repo.get_step(&context.validated_step.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    context.validated_step
                );
                assert!(
                    repo.station_claim_receipt(context.request_id)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    before,
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                        .fetch_one(&db.pool)
                        .await
                        .unwrap()
                );
            } else {
                let crate::station_claim::SelectionResult::Claimed { receipt, .. } =
                    result.unwrap()
                else {
                    panic!("unchanged free capacity");
                };
                assert_eq!(
                    receipt
                        .pointer("/owner_consumption/concurrency/reading/in_flight")
                        .unwrap(),
                    0
                );
                assert_eq!(
                    receipt
                        .pointer("/owner_consumption/concurrency/reading/max_concurrent_runs")
                        .unwrap(),
                    1
                );
                assert_eq!(
                    receipt
                        .pointer("/owner_consumption/concurrency/runs")
                        .unwrap(),
                    &serde_json::json!([])
                );
            }
        }
    }
    #[tokio::test]
    async fn joined_native_claim_audit_rebuild_conserves_exact_receipt_step_and_calendar() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            context,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let request = crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
            .unwrap()
            .unwrap();
        let result = repo
            .claim_station_head_at(
                &context,
                crate::runner_credential::ACTOR,
                &[],
                &stamp,
                &[stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&post),
                )],
            )
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { step, receipt } = result else {
            panic!("fresh claim");
        };
        let calendar = boss_calendar::PgCalendar::new(db.pool.clone());
        let rows = calendar
            .list(&request.subject, request.window)
            .await
            .unwrap();
        let bus: Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
        let drained = boss_events::outbox::drain_outbox_once(&db.pool, &bus, 1000)
            .await
            .unwrap();
        assert!(drained.delivered >= 3);
        assert_eq!(
            boss_events::outbox::pending_count(&db.pool).await.unwrap(),
            0
        );
        let audit: serde_json::Value =
            sqlx::query_scalar("SELECT payload FROM audit_log WHERE kind = $1")
                .bind(crate::events::STATION_CLAIMED)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let parts: Vec<(i64,uuid::Uuid,String,chrono::DateTime<chrono::Utc>,serde_json::Value)> = sqlx::query_as(
            "SELECT id,event_id,kind,timestamp,payload FROM audit_log WHERE kind IN ('jobs.station.claimed','jobs.station.selection_evidence') ORDER BY id")
            .fetch_all(&db.pool).await.unwrap();
        let parts = parts
            .into_iter()
            .map(
                |(audit_id, event_id, kind, ts, payload)| boss_events::replay::ReplayEvent {
                    audit_id,
                    event_id,
                    kind,
                    ts,
                    payload,
                },
            )
            .collect::<Vec<_>>();
        let reconstructed = crate::selection_evidence::reconstruct(&parts).unwrap();
        assert_eq!(
            reconstructed.last().unwrap().payload["receipt_json"].as_str(),
            Some(receipt.json())
        );
        assert!(audit.get("receipt_manifest").is_some());
        assert!(
            audit.get("receipt").is_none(),
            "the exact JSON is the only receipt copy"
        );
        for _ in 0..2 {
            crate::rebuild::rebuild_jobs_and_steps(&db.pool)
                .await
                .unwrap();
            boss_calendar::rebuild::rebuild_calendar(&db.pool)
                .await
                .unwrap();
            assert_eq!(
                repo.get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                *step
            );
            assert_eq!(
                repo.station_claim_receipt(context.request_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .json(),
                receipt.json()
            );
            assert_eq!(
                calendar
                    .list(&request.subject, request.window)
                    .await
                    .unwrap(),
                rows
            );
            let fresh_reader = NativeSelectionStore::new(db.pool.clone());
            let original = fresh_reader
                .read_selection(&resolved_ingress().await, context.request_id, "fixture")
                .await
                .unwrap()
                .original
                .expect("current authorized rebuilt original read");
            assert_eq!(original.json(), receipt.json());
        }
    }
    #[tokio::test]
    async fn current_explicit_read_override_can_replay_original_before_retired_fresh_actor_owner() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource, Scope, UserOverride};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            context,
            policy,
            store,
        } = native_claim_fixture().await;
        let result = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } = result else {
            panic!("fresh original");
        };
        policy
            .upsert_user_override(
                &UserOverride {
                    id: "native-original-current-read".into(),
                    user_id: crate::runner_credential::ACTOR.into(),
                    resource: Resource::job(),
                    action: Action::Read,
                    scope: Scope::All,
                    reason: "current immutable original read; not fresh claim authority".into(),
                    expires_at: None,
                },
                "original-read-fixture",
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM automation_actors WHERE id = $1")
            .bind(crate::runner_credential::ACTOR)
            .execute(&db.pool)
            .await
            .unwrap();
        let read = store
            .read_selection(&resolved_ingress().await, context.request_id, "fixture")
            .await
            .expect("current read override is independent of unavailable fresh actor authority");
        assert_eq!(read.original.unwrap().json(), receipt.json());
        policy
            .deactivate_user_override("native-original-current-read", "retire-original-read")
            .await
            .unwrap();
        assert!(
            store
                .read_selection(&resolved_ingress().await, context.request_id, "fixture")
                .await
                .is_err(),
            "no actor role and no current override must not borrow an anonymous or asserted role grant"
        );
        assert!(
            store
                .observe_authority(resolved_ingress().await, uuid::Uuid::new_v4(), "fixture")
                .await
                .is_err(),
            "historical read must not supply fresh actor authority"
        );
        assert_eq!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .unwrap()
                .json(),
            receipt.json()
        );
    }
    #[tokio::test]
    async fn reviewer_read_expiry_while_original_receipt_sql_waits_refuses_artifact() {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, Resource, Scope, UserOverride};
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            context,
            policy,
            store,
        } = native_claim_fixture().await;
        let original = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap();
        assert!(matches!(
            original,
            crate::station_claim::SelectionResult::Claimed { .. }
        ));
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::job(),
                    Action::Read,
                ),
                "reviewer-expiry",
            )
            .await
            .unwrap();
        let expires = chrono::Utc::now() + chrono::Duration::seconds(2);
        policy
            .upsert_user_override(
                &UserOverride {
                    id: "reviewer-short-read".into(),
                    user_id: crate::runner_credential::ACTOR.into(),
                    resource: Resource::job(),
                    action: Action::Read,
                    scope: Scope::All,
                    reason: "reviewer actual read expiry control".into(),
                    expires_at: Some(expires),
                },
                "reviewer-expiry",
            )
            .await
            .unwrap();
        let mut blocker = db.pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE station_claim_receipts IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let ingress = resolved_ingress().await;
        let started = std::time::Instant::now();
        let request_id = context.request_id;
        let reader =
            tokio::spawn(
                async move { store.read_selection(&ingress, request_id, "fixture").await },
            );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT receipt FROM station_claim_receipts%'")
                    .fetch_one(&db.pool).await.unwrap();
                if waiting > 0 { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("actual receipt SELECT must reach its lock wait before expiry");
        assert!(
            chrono::Utc::now() < expires,
            "must observe granted read before expiry"
        );
        while chrono::Utc::now() <= expires + chrono::Duration::milliseconds(100) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        blocker.rollback().await.unwrap();
        let result = reader.await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "exercise read expiry, not ingress timeout"
        );
        assert!(
            result.is_err(),
            "current read expired during actual receipt SQL, but artifact was returned: {:?}",
            result.map(|r| r.original.map(|v| v.json().to_owned()))
        );
    }

    struct SlowNativeStations {
        inner: crate::PgStations,
        slow: bool,
        entered: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl crate::stations::StationRegistry for SlowNativeStations {
        async fn get_active(
            &self,
            name: &str,
        ) -> Result<crate::stations::StationSpec, crate::stations::StationError> {
            if self.slow {
                self.entered.notify_one();
                tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            }
            self.inner.get_active(name).await
        }
        async fn get_version(
            &self,
            name: &str,
            version: i32,
        ) -> Result<crate::stations::StationSpec, crate::stations::StationError> {
            self.inner.get_version(name, version).await
        }
        async fn list_active(
            &self,
        ) -> Result<Vec<crate::stations::StationSpec>, crate::stations::StationError> {
            self.inner.list_active().await
        }
        async fn list_versions(
            &self,
            name: &str,
        ) -> Result<Vec<crate::stations::StationSpec>, crate::stations::StationError> {
            self.inner.list_versions(name).await
        }
        async fn create_draft(
            &self,
            spec: crate::stations::StationSpec,
            actor: &boss_core::actor::ActorId,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<crate::stations::StationSpec, crate::stations::StationError> {
            self.inner.create_draft(spec, actor, now).await
        }
        async fn publish(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<crate::stations::StationSpec, crate::stations::StationError> {
            self.inner.publish(name, actor, now).await
        }
        async fn retire(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), crate::stations::StationError> {
            self.inner.retire(name, actor, now).await
        }
        async fn publish_declared(
            &self,
            spec: crate::stations::StationSpec,
            actor: &boss_core::actor::ActorId,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<crate::stations::StationSpec, crate::stations::StationError> {
            self.inner.publish_declared(spec, actor, now).await
        }
    }
    #[tokio::test]
    async fn authenticated_http_selection_uses_the_configured_native_store() {
        native_http_control(true, false, false, false).await;
    }
    #[tokio::test]
    async fn native_http_authority_does_not_depend_on_a_separate_cached_policy_answer() {
        native_http_control(false, false, false, false).await;
    }
    #[tokio::test]
    async fn native_http_preparation_cancels_a_slow_station_read_at_ingress_deadline() {
        native_http_control(true, true, false, false).await;
    }
    #[tokio::test]
    async fn native_http_slow_read_keeps_its_ingress_cut_after_observer_descheduling() {
        native_http_control_with_pause_delay(
            true,
            true,
            false,
            false,
            NativeHttpAdversity::ObserverPause(std::time::Duration::from_millis(900)),
        )
        .await;
    }
    #[tokio::test]
    async fn native_http_copies_actual_budget_query_inputs_and_keeps_over_budget_advisory() {
        native_http_control(true, false, true, false).await;
    }
    #[tokio::test]
    async fn native_http_unavailable_budget_is_explicit_unknown_without_fake_zero_or_cash_refusal()
    {
        native_http_control(true, false, true, true).await;
    }
    #[tokio::test]
    async fn native_http_budget_fixture_wait_before_admission_does_not_spend_ingress() {
        native_http_control_with_pause_delay(
            true,
            false,
            true,
            false,
            NativeHttpAdversity::FixturePreparation(std::time::Duration::from_secs(6)),
        )
        .await;
    }
    #[tokio::test]
    async fn native_http_budget_actual_sql_wait_expires_without_a_claim() {
        native_http_control_with_pause_delay(
            true,
            false,
            true,
            false,
            NativeHttpAdversity::BudgetInputWait,
        )
        .await;
    }
    enum NativeHttpAdversity {
        None,
        ObserverPause(std::time::Duration),
        FixturePreparation(std::time::Duration),
        BudgetInputWait,
    }
    async fn native_http_control(
        external_allow: bool,
        slow_station: bool,
        with_budget: bool,
        budget_unavailable: bool,
    ) {
        native_http_control_with_pause_delay(
            external_allow,
            slow_station,
            with_budget,
            budget_unavailable,
            NativeHttpAdversity::None,
        )
        .await;
    }
    async fn native_http_control_with_pause_delay(
        external_allow: bool,
        slow_station: bool,
        with_budget: bool,
        budget_unavailable: bool,
        adversity: NativeHttpAdversity,
    ) {
        use crate::JobsRepository;
        use boss_core::{port::EventBus, publisher::DomainPublisher};
        use boss_policy_client::{Action, FakePolicyClient, PolicyRepository, Resource, Scope};
        use tower::ServiceExt;
        let fixture = if with_budget {
            declared_native_agent_alias_fixture().await
        } else {
            native_claim_fixture().await
        };
        let NativeClaimFixture {
            db,
            repo,
            mut context,
            stamp,
            ..
        } = fixture;
        if with_budget {
            context.validated_step.metadata[crate::agent_spec::MODEL_KEY] =
                serde_json::json!("native-alias-model");
            context.validated_step.metadata[crate::agent_spec::BUDGET_KEY] = serde_json::json!(1.0);
            repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
                .await
                .unwrap();
        }
        if budget_unavailable {
            sqlx::query("DROP VIEW agent_runs_read")
                .execute(&db.pool)
                .await
                .unwrap();
        }
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        let builder = FakePolicyClient::builder().allow(
            "platform-admin",
            Action::Read,
            Resource::job(),
            Scope::All,
        );
        let policy = Arc::new(if external_allow {
            builder
                .allow(
                    "platform-admin",
                    Action::Update,
                    Resource::step(),
                    Scope::All,
                )
                .build()
        } else {
            builder.build()
        });
        let slow_station_entered = Arc::new(tokio::sync::Notify::new());
        let station_registry: Arc<dyn crate::stations::StationRegistry> =
            Arc::new(SlowNativeStations {
                inner: crate::PgStations::new(db.pool.clone()),
                slow: slow_station,
                entered: slow_station_entered.clone(),
            });
        let app = crate::http::router(crate::http::JobsApiState {
            stations: Some(station_registry),
            kind_registry: Some(Arc::new(crate::PgWorkflows::new(db.pool.clone()))),
            agent_budget: Some(Arc::new(crate::agent_budget::BudgetDoor {
                agents: Arc::new(crate::agents::PgAgents::new(db.pool.clone())),
                runs: Arc::new(crate::agent_runs::PgAgentRuns::new(db.pool.clone())),
            })),
            ..crate::http::JobsApiState::minimal(
                Arc::new(repo),
                bus,
                DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        });
        let dir = boss_testing::scratch_dir("native-http-selection-ingress");
        let app = if slow_station {
            // Make an actual earlier preparation wait visible: the slow port
            // must inherit ingress's remaining budget, not receive five more.
            app.layer(axum::middleware::from_fn(
                |request: axum::extract::Request, next: axum::middleware::Next| async move {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    next.run(request).await
                },
            ))
        } else {
            app
        };
        boss_testing::write_file(&dir.join("forge.current"), "native-http-fixture-key");
        let app = crate::runner_credential::mount(app, dir);
        let gate = Arc::new(boss_core::machine_gate::MachineGate::new(
            "jobs",
            &[],
            boss_core::machine_gate::Reading::new(
                boss_core::machine_gate::Mode::Off,
                Default::default(),
            ),
        ));
        let app = boss_core::machine_gate::gated(app, gate);
        let asserted = context.native_authority.as_ref().unwrap().user.clone();
        for credential in [None, Some("foreign-http-fixture-key")] {
            let request_id = uuid::Uuid::new_v4();
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/stations/fixture/claim-next")
                .header("x-boss-user", serde_json::to_string(&asserted).unwrap())
                .header("content-type", "application/json");
            if let Some(credential) = credential {
                request = request.header(crate::runner_credential::HEADER, credential);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(axum::body::Body::from(
                            serde_json::json!({"request_id":request_id}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::UNAUTHORIZED,
                "unsigned/foreign transport must not reach native selection"
            );
            let repo = crate::PgJobs::new(db.pool.clone());
            assert_eq!(
                repo.get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                context.validated_step
            );
            assert!(
                repo.station_claim_receipt(request_id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let user = boss_policy_client::User::service("forged-header-owner");
        if let NativeHttpAdversity::FixturePreparation(delay) = adversity {
            tokio::time::sleep(delay).await;
        }
        let mut budget_blocker = if matches!(adversity, NativeHttpAdversity::BudgetInputWait) {
            let mut tx = db.pool.begin().await.unwrap();
            sqlx::query("LOCK TABLE agent_runs IN ACCESS EXCLUSIVE MODE")
                .execute(&mut *tx)
                .await
                .unwrap();
            Some(tx)
        } else {
            None
        };
        let began = tokio::time::Instant::now();
        let request = app.clone().oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/stations/fixture/claim-next")
                .header(crate::runner_credential::HEADER, "native-http-fixture-key")
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({"request_id": context.request_id}).to_string(),
                ))
                .unwrap(),
        );
        tokio::pin!(request);
        let response = if slow_station {
            tokio::select! {
                result = &mut request => panic!(
                    "native HTTP fixture ended before its slow port was exercised: {}",
                    result.unwrap().status()
                ),
                () = slow_station_entered.notified() => {}
            }
            // Preparation has already spent real ingress time. Advancing
            // another five seconds would test a freshly restarted budget.
            if let NativeHttpAdversity::ObserverPause(delay) = adversity {
                std::thread::sleep(delay);
            }
            tokio::time::pause();
            // Freeze the observer first: a host deschedule between reading
            // remaining and pausing must not be counted a second time when
            // the virtual clock advances to the original ingress boundary.
            let remaining =
                crate::station_claim::SELECTION_PRE_COMMIT_BUDGET.saturating_sub(began.elapsed());
            tokio::time::advance(remaining).await;
            request.await.unwrap()
        } else if budget_blocker.is_some() {
            // Both the real HTTP request and PostgreSQL must be running;
            // observing a pending task alone does not establish its SQL wait.
            loop {
                tokio::select! {
                    result = &mut request => panic!(
                        "native budget request ended before its SQL wait: {}",
                        result.unwrap().status()
                    ),
                    waiting = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = 'agent_runs'::regclass AND NOT granted)").fetch_one(&db.pool) => {
                        if waiting.unwrap() { break; }
                    }
                }
                tokio::task::yield_now().await;
            }
            request.await.unwrap()
        } else {
            request.await.unwrap()
        };
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        if slow_station || budget_blocker.is_some() {
            let elapsed = began.elapsed();
            if slow_station {
                tokio::time::resume();
                assert!(
                    elapsed < std::time::Duration::from_millis(5800),
                    "native preparation must cancel its real port read within original five seconds, elapsed {:?}",
                    elapsed
                );
            }
            if let Some(blocker) = budget_blocker.take() {
                blocker.rollback().await.unwrap();
            }
            assert_eq!(status, axum::http::StatusCode::CONFLICT);
            assert!(String::from_utf8_lossy(&body).contains("deadline"));
            let repo = crate::PgJobs::new(db.pool.clone());
            assert_eq!(
                repo.get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                context.validated_step
            );
            assert!(
                repo.station_claim_receipt(context.request_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            return;
        }
        assert_eq!(
            status,
            axum::http::StatusCode::CREATED,
            "actual authenticated HTTP selection must reach the configured native transaction; ingress elapsed {:?}: {}",
            began.elapsed(),
            String::from_utf8_lossy(&body)
        );
        if with_budget {
            let receipt: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let reading = receipt
                .pointer("/owner_consumption/budget_hour")
                .expect("actual advisory input observation must be copied");
            if budget_unavailable {
                assert_eq!(reading.get("status").unwrap(), "unknown");
                assert_eq!(reading.get("inputs").unwrap(), &serde_json::Value::Null);
                assert!(
                    reading.get("reading").is_none(),
                    "unknown query cannot report measured zero"
                );
                assert!(
                    reading
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|error| error.contains("agent_runs_read"))
                );
            } else {
                assert_eq!(reading.get("status").unwrap(), "measured");
                assert_eq!(reading.get("inputs").unwrap(), &serde_json::json!([]));
                assert_eq!(
                    reading.pointer("/reading/actor").unwrap(),
                    "agent-native-alias"
                );
                assert_eq!(reading.pointer("/reading/spent_usd_micros").unwrap(), 0);
                assert_eq!(
                    reading.pointer("/reading/budget_usd_micros").unwrap(),
                    1_000_000
                );
                assert_eq!(
                    reading
                        .pointer("/reading/hourly_budget_usd_micros")
                        .unwrap(),
                    1
                );
                assert!(reading.get("observed_at").is_some());
                assert!(
                    reading.pointer("/reading/decision/deny").is_some()
                        || reading.pointer("/reading/decision/reason").is_some(),
                    "over-cap reading must remain a signal, not a claim refusal"
                );
                let facts: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
                        .bind(crate::agent_budget::CLAIM_OVER_BUDGET)
                        .fetch_one(&db.pool)
                        .await
                        .unwrap();
                assert_eq!(
                    facts, 1,
                    "actual over-budget fact commits beside successful native claim"
                );
            }
        }
        for credential in [None, Some("foreign-http-fixture-key")] {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/stations/fixture/claim-next")
                .header("x-boss-user", serde_json::to_string(&asserted).unwrap())
                .header("content-type", "application/json");
            if let Some(credential) = credential {
                request = request.header(crate::runner_credential::HEADER, credential);
            }
            let replay = app
                .clone()
                .oneshot(
                    request
                        .body(axum::body::Body::from(
                            serde_json::json!({"request_id":context.request_id}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(
                replay.status(),
                axum::http::StatusCode::CREATED,
                "unsigned asserted actor must not read an authentic original receipt"
            );
        }
        let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::step(),
                    Action::Update,
                ),
                "retire-http-update",
            )
            .await
            .unwrap();
        let replay = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/stations/fixture/claim-next")
                    .header(crate::runner_credential::HEADER, "native-http-fixture-key")
                    .header("x-boss-user", serde_json::to_string(&user).unwrap())
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"request_id":context.request_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.status(),
            axum::http::StatusCode::CREATED,
            "original replay precedes retired update authority"
        );
        assert_eq!(
            axum::body::to_bytes(replay.into_body(), 65536)
                .await
                .unwrap(),
            body,
            "HTTP replay must conserve the original bytes"
        );
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::job(),
                    Action::Read,
                ),
                "retire-http-read",
            )
            .await
            .unwrap();
        let refused = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/stations/fixture/claim-next")
                    .header(crate::runner_credential::HEADER, "native-http-fixture-key")
                    .header("x-boss-user", serde_json::to_string(&asserted).unwrap())
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"request_id":context.request_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            refused.status(),
            axum::http::StatusCode::CONFLICT,
            "even an original receipt needs current real native read authority"
        );
    }
    #[tokio::test]
    async fn native_scheduled_claim_cannot_skip_calendar_authority_and_ownership() {
        use crate::JobsRepository;
        let NativeClaimFixture {
            db,
            repo,
            stamp,
            mut context,
            ..
        } = native_claim_fixture().await;
        let mut scheduled = context.validated_step.clone();
        scheduled.metadata = serde_json::json!({
            "scheduled_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "duration_minutes": 15,
            "assignee_id": "emp-calendar-fixture"
        });
        repo.update_step_at(
            &scheduled,
            stamp.timestamp,
            &[stamp.event(
                crate::events::STEP_UPDATED,
                crate::events::step_state_payload(&scheduled),
            )],
        )
        .await
        .unwrap();
        context.validated_step = repo.get_step(&scheduled.id).await.unwrap().unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let selected = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await;
        assert!(
            selected.is_err(),
            "UpdateStep alone cannot skip required calendar reservation authority/owner and commit a scheduled claim"
        );
        assert_eq!(
            repo.get_step(&scheduled.id).await.unwrap().unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(before, after);
    }
    async fn calendar_claim_fixture() -> NativeClaimFixture {
        use crate::JobsRepository;
        use boss_policy_client::{Action, PolicyRepository, PolicyRule, Resource, Scope};
        let mut fixture = native_claim_fixture().await;
        fixture
            .policy
            .upsert_rule(
                &PolicyRule {
                    id: boss_policy_client::types::rule_id(
                        "platform-admin",
                        &Resource::schedule(),
                        Action::Create,
                    ),
                    role: "platform-admin".into(),
                    resource: Resource::schedule(),
                    action: Action::Create,
                    scope: Scope::All,
                    active: true,
                },
                "native-calendar-fixture",
            )
            .await
            .unwrap();
        let mut scheduled = fixture.context.validated_step.clone();
        scheduled.metadata = serde_json::json!({"scheduled_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(), "duration_minutes": 15});
        fixture
            .repo
            .update_step_at(
                &scheduled,
                fixture.stamp.timestamp,
                &[fixture.stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&scheduled),
                )],
            )
            .await
            .unwrap();
        fixture.context.validated_step =
            fixture.repo.get_step(&scheduled.id).await.unwrap().unwrap();
        fixture.context.native_authority = Some(Arc::new(
            fixture
                .store
                .observe_authority(
                    resolved_ingress().await,
                    fixture.context.request_id,
                    "fixture",
                )
                .await
                .unwrap(),
        ));
        fixture
    }
    fn calendar_post_step(
        context: &crate::station_claim::SelectionContext,
    ) -> boss_core::job::Step {
        let mut post = context.validated_step.clone();
        post.status = boss_core::job::StepStatus::Active;
        post.assignee_id = Some(crate::runner_credential::ACTOR.into());
        post
    }
    #[tokio::test]
    async fn actual_native_calendar_row_event_and_receipt_commit_and_replay_together() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        use boss_policy_client::{Action, PolicyRepository, Resource};
        let NativeClaimFixture {
            db,
            repo,
            context,
            stamp,
            policy,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let request = crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
            .unwrap()
            .unwrap();
        let selected = repo
            .claim_station_head_at(
                &context,
                crate::runner_credential::ACTOR,
                &[],
                &stamp,
                &[stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&post),
                )],
            )
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { step, receipt } = selected else {
            panic!("fresh calendar claim")
        };
        let rows = boss_calendar::PgCalendar::new(db.pool.clone())
            .list(&request.subject, request.window)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject.id, step.assignee_id.unwrap());
        assert_eq!(rows[0].reason_ref_id, step.id.to_string());
        assert_eq!(
            receipt
                .pointer("/owner_consumption/calendar/consumed/reservation/id")
                .and_then(serde_json::Value::as_str),
            Some(rows[0].id.to_string().as_str())
        );
        let payload: serde_json::Value =
            sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
                .bind(boss_calendar::events::RESERVATION_RESERVED)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let event_row: boss_core::calendar::Reservation = serde_json::from_value(payload).unwrap();
        assert_eq!(
            event_row, rows[0],
            "the stored reservation and full outbox fact are one value"
        );
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::schedule(),
                    Action::Create,
                ),
                "retire-after-calendar-commit",
            )
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Replayed(original) = repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
            .unwrap()
        else {
            panic!("original calendar receipt precedes fresh authority")
        };
        assert_eq!(original.json(), receipt.json());
        assert_eq!(
            boss_calendar::PgCalendar::new(db.pool.clone())
                .list(&request.subject, request.window)
                .await
                .unwrap(),
            rows
        );
    }
    #[tokio::test]
    async fn foreign_hard_calendar_hold_refuses_with_its_full_identity_and_no_job_effects() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        let NativeClaimFixture {
            db,
            repo,
            context,
            stamp,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let mut request =
            crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
                .unwrap()
                .unwrap();
        request.reason_ref_id = "foreign-calendar-owner".into();
        let calendar = boss_calendar::PgCalendar::new(db.pool.clone());
        let id = calendar
            .reserve_at(request.clone(), stamp.timestamp, &stamp)
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let error = match repo
            .claim_station_head_at(&context, crate::runner_credential::ACTOR, &[], &stamp, &[])
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("foreign hard reservation must refuse the claim"),
        };
        assert!(
            error.to_string().contains(&id.to_string()),
            "calendar conflict must conserve actual owner identity: {error}"
        );
        assert_eq!(
            repo.get_step(&post.id).await.unwrap().unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            before
        );
        assert_eq!(
            calendar
                .list(&request.subject, request.window)
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn calendar_same_prior_hold_is_consumed_without_a_duplicate_row_or_fact() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        let NativeClaimFixture {
            db,
            repo,
            context,
            stamp,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let request = crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
            .unwrap()
            .unwrap();
        let calendar = boss_calendar::PgCalendar::new(db.pool.clone());
        let id = calendar
            .reserve_at(request.clone(), stamp.timestamp, &stamp)
            .await
            .unwrap();
        let before = calendar
            .list(&request.subject, request.window)
            .await
            .unwrap();
        let selected = repo
            .claim_station_head_at(
                &context,
                crate::runner_credential::ACTOR,
                &[],
                &stamp,
                &[stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&post),
                )],
            )
            .await
            .unwrap();
        let crate::station_claim::SelectionResult::Claimed { receipt, .. } = selected else {
            panic!("fresh claim consuming prior hold")
        };
        assert_eq!(
            receipt.pointer("/owner_consumption/calendar/consumed/inserted"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            receipt
                .pointer("/owner_consumption/calendar/consumed/reservation/id")
                .and_then(serde_json::Value::as_str),
            Some(id.to_string().as_str())
        );
        assert_eq!(
            calendar
                .list(&request.subject, request.window)
                .await
                .unwrap(),
            before
        );
        let facts: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
            .bind(boss_calendar::events::RESERVATION_RESERVED)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            facts, 1,
            "consuming a real existing hold must not fabricate a second reserved fact"
        );
    }
    #[tokio::test]
    async fn calendar_and_job_effects_roll_back_together_when_receipt_write_refuses() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        let NativeClaimFixture {
            db,
            repo,
            context,
            stamp,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let request = crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
            .unwrap()
            .unwrap();
        sqlx::raw_sql("CREATE FUNCTION refuse_calendar_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'joined-calendar-receipt-refusal'; END $$; CREATE TRIGGER refuse_calendar_receipt BEFORE INSERT ON station_claim_receipts FOR EACH ROW EXECUTE FUNCTION refuse_calendar_receipt()")
            .execute(&db.pool).await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let result = repo
            .claim_station_head_at(
                &context,
                crate::runner_credential::ACTOR,
                &[],
                &stamp,
                &[stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&post),
                )],
            )
            .await;
        assert!(
            matches!(result, Err(JobsError::Storage(ref reason)) if reason.contains("joined-calendar-receipt-refusal")),
            "the injected refusal must occur after the calendar write in the same transaction"
        );
        assert_eq!(
            repo.get_step(&post.id).await.unwrap().unwrap(),
            context.validated_step
        );
        assert!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            boss_calendar::PgCalendar::new(db.pool.clone())
                .list(&request.subject, request.window)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            before
        );
    }
    #[tokio::test]
    async fn calendar_real_reservation_writer_waits_for_the_joined_claim_commit() {
        use crate::JobsRepository;
        use boss_calendar::CalendarClient;
        let NativeClaimFixture {
            db,
            repo,
            context,
            stamp,
            ..
        } = calendar_claim_fixture().await;
        let post = calendar_post_step(&context);
        let mut request =
            crate::calendar_hook::start_request(&post, crate::runner_credential::ACTOR)
                .unwrap()
                .unwrap();
        request.window = boss_core::calendar::TimeWindow::new(
            request.window.start + chrono::Duration::hours(2),
            request.window.end + chrono::Duration::hours(2),
        )
        .unwrap();
        request.reason_ref_id = "real-calendar-writer".into();
        sqlx::raw_sql("CREATE FUNCTION pause_calendar_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(hashtext(current_database()), 1420); RETURN NEW; END $$; CREATE TRIGGER pause_calendar_receipt BEFORE INSERT ON station_claim_receipts FOR EACH ROW EXECUTE FUNCTION pause_calendar_receipt()")
            .execute(&db.pool).await.unwrap();
        let mut barrier = db.pool.acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock(hashtext(current_database()), 1420)")
            .execute(&mut *barrier)
            .await
            .unwrap();
        let claim = tokio::spawn(async move {
            repo.claim_station_head_at(
                &context,
                crate::runner_credential::ACTOR,
                &[],
                &stamp,
                &[stamp.event(
                    crate::events::STEP_UPDATED,
                    crate::events::step_state_payload(&post),
                )],
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event = 'advisory' AND query LIKE 'INSERT INTO station_claim_receipts%')").fetch_one(&db.pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let calendar = boss_calendar::PgCalendar::new(db.pool.clone());
        let writer_stamp = boss_core::publisher::EventStamp::new(
            "calendar",
            boss_core::actor::ActorId::automation("calendar-fixture"),
        );
        let mut writer = tokio::spawn(async move {
            calendar
                .reserve_at(request, writer_stamp.timestamp, &writer_stamp)
                .await
        });
        wait_for_actual_writer(&db.pool, "calendar_reservations", &mut writer).await;
        assert!(
            !writer.is_finished(),
            "real calendar write must conflict with the selector's actual owner"
        );
        sqlx::query("SELECT pg_advisory_unlock(hashtext(current_database()), 1420)")
            .execute(&mut *barrier)
            .await
            .unwrap();
        claim.await.unwrap().unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM calendar_reservations")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            2
        );
    }
    #[tokio::test]
    async fn real_resolver_and_native_rows_prepare_and_consume_the_same_store_authority() {
        use crate::agents::{AgentsRegistry, PgAgents};
        use boss_policy_client::{Action, PolicyRepository, PolicyRule, Resource, Scope};
        let db = boss_testing::TestDb::new().await;
        let registry = PgAgents::new(db.pool.clone());
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("ops-runner"),
        );
        registry
            .declare_automations(
                &[crate::agents::automations::AutomationActor {
                    id: crate::runner_credential::ACTOR.into(),
                    role: "platform-admin".into(),
                    description: "native authority owning fixture".into(),
                    signs_for: None,
                }],
                &stamp,
            )
            .await
            .unwrap();
        let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy
                .upsert_rule(
                    &PolicyRule {
                        id: boss_policy_client::types::rule_id("platform-admin", &resource, action),
                        role: "platform-admin".into(),
                        resource,
                        action,
                        scope: Scope::All,
                        active: true,
                    },
                    "native-owner-fixture",
                )
                .await
                .unwrap();
        }
        let store = NativeSelectionStore::new(db.pool.clone());
        let request_id = uuid::Uuid::new_v4();
        let observation = store
            .observe_authority(resolved_ingress().await, request_id, "fixture")
            .await
            .unwrap();
        assert_eq!(observation.user.id, crate::runner_credential::ACTOR);
        assert_eq!(observation.user.role, "platform-admin");
        let job = boss_core::job::Job::new(
            "fixture",
            boss_core::job::Subject::new("custom", "fixture"),
            "Native",
            "emp-owner",
            boss_core::job::Priority::Standard,
            stamp.timestamp.date_naive(),
        );
        let step = boss_core::job::Step::new(job.id, "task", "Work", 0);
        let context = crate::station_claim::SelectionContext {
            native_authority: Some(Arc::new(observation.clone())),
            actor_observation: None,
            prepared_policy: None,
            definition_fence: None,
            request_id,
            station: crate::stations::StationSpec::draft(
                "fixture",
                "Fixture",
                crate::stations::StationKind::Batch,
                Default::default(),
                stamp.timestamp,
            ),
            scope: crate::port::JobScope::All,
            active_workflows: Default::default(),
            validated_job: job,
            validated_step: step,
        };
        let mut tx = match store
            .acquire_selection_before(
                &store,
                context.request_id,
                crate::runner_credential::ACTOR,
                "fixture",
                SelectionDeadline::from_ingress(&observation.ingress),
            )
            .await
            .unwrap()
        {
            NativeSelectionAcquisition::Fresh(tx) => tx,
            NativeSelectionAcquisition::Replayed(_) => panic!("fixture has no original receipt"),
        };
        let consumed = store
            .consume_authority_in(
                &mut tx,
                &observation,
                &context,
                crate::runner_credential::ACTOR,
                &stamp,
            )
            .await
            .unwrap();
        assert_eq!(
            consumed.pointer("/consumed_inputs/user/id").unwrap(),
            crate::runner_credential::ACTOR
        );
        let mut other_request = context.clone();
        other_request.request_id = uuid::Uuid::new_v4();
        assert!(
            store
                .consume_authority_in(
                    &mut tx,
                    &observation,
                    &other_request,
                    crate::runner_credential::ACTOR,
                    &stamp
                )
                .await
                .is_err(),
            "one prepared ingress observation must not authorize a different selection request"
        );
        let mut other_station = context.clone();
        other_station.station.name = "different-station".into();
        assert!(
            store
                .consume_authority_in(
                    &mut tx,
                    &observation,
                    &other_station,
                    crate::runner_credential::ACTOR,
                    &stamp
                )
                .await
                .is_err()
        );
        let foreign = NativeSelectionStore::new(db.pool.clone());
        assert!(
            foreign
                .consume_authority_in(
                    &mut tx,
                    &observation,
                    &context,
                    crate::runner_credential::ACTOR,
                    &stamp
                )
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        policy
            .deactivate_rule(
                &boss_policy_client::types::rule_id(
                    "platform-admin",
                    &Resource::step(),
                    Action::Update,
                ),
                "native-owner-retirement",
            )
            .await
            .unwrap();
        let mut tx = store.begin_all_input_fence_for(&store).await.unwrap();
        assert!(
            store
                .consume_authority_in(
                    &mut tx,
                    &observation,
                    &context,
                    crate::runner_credential::ACTOR,
                    &stamp
                )
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn original_receipt_bytes_precede_unavailable_fresh_owners_without_new_effects() {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let request_id = uuid::Uuid::new_v4();
        let original = format!(
            "{{\n  \"version\": 1, \"request_id\": \"{request_id}\", \"actor\": \"emp-worker\", \"station\": {{\"name\": \"fixture\"}}, \"historical_number\": 9.0\n}}\n"
        );
        // A historical receipt fixture, copied exactly; not fresh authority.
        sqlx::query("INSERT INTO station_claim_receipts(request_id, receipt) VALUES ($1, $2)")
            .bind(request_id)
            .bind(&original)
            .execute(&db.pool)
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let mut retired_owner = db.pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE workflows, agents, policy_rules IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *retired_owner)
            .await
            .unwrap();
        let foreign_db = boss_testing::TestDb::new().await;
        let unavailable = NativeSelectionStore::new(foreign_db.pool.clone());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.acquire_selection_for(&unavailable, request_id, "emp-worker", "fixture"),
        )
        .await
        .unwrap()
        .unwrap();
        let NativeSelectionAcquisition::Replayed(receipt) = result else {
            panic!("original receipt must precede new owner acquisition")
        };
        assert_eq!(receipt.json(), original);
        assert!(
            store
                .acquire_selection_for(&unavailable, request_id, "different-actor", "fixture")
                .await
                .is_err()
        );
        retired_owner.rollback().await.unwrap();
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(after, before);
    }
    #[tokio::test]
    async fn pool_acquisition_uses_the_one_five_second_selection_budget() {
        let db = boss_testing::TestDb::new().await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect_with((*db.pool.connect_options()).clone())
            .await
            .unwrap();
        let held = pool.acquire().await.unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            drop(held);
        });
        let store = NativeSelectionStore::new(pool);
        let started = std::time::Instant::now();
        let result = store
            .acquire_selection_for(&store, uuid::Uuid::new_v4(), "emp-worker", "fixture")
            .await;
        let elapsed = started.elapsed();
        release.await.unwrap();
        assert!(
            result.is_err(),
            "an expired pool wait must never dispatch selection work"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(5500),
            "pool waiting is inside the total deadline: {elapsed:?}"
        );
    }
    #[tokio::test]
    async fn prior_ingress_wait_is_not_a_new_repository_budget() {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let mut writer = db.pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE workflows IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *writer)
            .await
            .unwrap();
        // Keep the paused timer from auto-advancing while real PostgreSQL
        // answers. Arrival at its blocked owner lock is observed explicitly.
        let keep_clock = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        tokio::time::pause();
        let deadline = SelectionDeadline::at_ingress();
        tokio::time::advance(std::time::Duration::from_millis(4500)).await;
        let started = tokio::time::Instant::now();
        let mut acquiring = tokio::spawn(async move {
            store
                .acquire_selection_before(
                    &store,
                    uuid::Uuid::new_v4(),
                    "emp-worker",
                    "fixture",
                    deadline,
                )
                .await
        });
        wait_for_actual_writer(&db.pool, "workflows", &mut acquiring).await;
        tokio::time::advance(std::time::Duration::from_millis(501)).await;
        keep_clock.abort();
        assert!(keep_clock.await.unwrap_err().is_cancelled());
        let result = acquiring.await.unwrap();
        let elapsed = started.elapsed();
        tokio::time::resume();
        writer.rollback().await.unwrap();
        assert!(result.is_err());
        assert!(elapsed < std::time::Duration::from_millis(1200));
    }
    #[tokio::test]
    async fn real_automation_declaration_waits_for_the_native_actor_fence() {
        use crate::agents::{AgentsRegistry, PgAgents};
        let db = boss_testing::TestDb::new().await;
        let registry = Arc::new(PgAgents::new(db.pool.clone()));
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store.begin_actor_policy_fence_for(&store).await.unwrap();
        let declared = crate::agents::automations::AutomationActor {
            id: "automation:selector-fixture".into(),
            role: "platform-admin".into(),
            description: "owning native selector control".into(),
            signs_for: None,
        };
        let writer_registry = registry.clone();
        let mut writer = tokio::spawn(async move {
            let stamp = boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("native-owner-control"),
            );
            writer_registry
                .declare_automations(&[declared], &stamp)
                .await
        });
        wait_for_actual_writer(&db.pool, "automation_actors", &mut writer).await;
        assert!(registry.list_automations().await.unwrap().is_empty());
        tx.rollback().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.inserted, vec!["automation:selector-fixture"]);
        assert_eq!(registry.list_automations().await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn owning_agent_reads_refresh_the_budget_after_their_real_wait() {
        let db = boss_testing::TestDb::new().await;
        let deadline = SelectionDeadline::at_ingress();
        let mut blocker = db.pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE agents IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let reading = tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            deadline.constrain_sql(&mut tx).await.unwrap();
            crate::agents::PgAgents::selection_rows_budgeted_in(
                &mut tx,
                Some(deadline.sql_budget()),
            )
            .await
            .unwrap();
            // The observer may be descheduled after the owner refreshes SQL.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let configured_ms: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM current_setting('statement_timeout')::interval) * 1000)::bigint")
                .fetch_one(&mut *tx).await.unwrap();
            tx.rollback().await.unwrap();
            configured_ms
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = 'agents'::regclass AND NOT granted)")
                    .fetch_one(&db.pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        // The blocked owner cannot refresh until this lock is released. Sample
        // that causal upper bound, not a later observer's scheduling instant.
        let release_budget_ms = deadline.remaining().unwrap().as_millis() as i64;
        blocker.rollback().await.unwrap();
        let configured_ms = reading.await.unwrap();
        assert!(
            configured_ms <= release_budget_ms,
            "owning adapter retained {configured_ms}ms after its agents wait; ingress had {release_budget_ms}ms before release"
        );
    }
    #[tokio::test]
    async fn later_owner_statements_use_the_remaining_ingress_budget() {
        later_owner_statements_control(std::time::Duration::ZERO).await;
    }
    #[tokio::test]
    async fn later_owner_statements_observe_a_descheduled_actual_owner() {
        later_owner_statements_control(std::time::Duration::from_millis(2100)).await;
    }
    async fn later_owner_statements_control(arrival_delay: std::time::Duration) {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let mut blocker = db.pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE workflows IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        // Installing the test's blocker is fixture setup, before admission.
        // The target's real owner wait still consumes its original budget.
        let deadline = SelectionDeadline::at_ingress();
        let acquiring_store = store.clone();
        let mut acquiring = tokio::spawn(async move {
            tokio::time::sleep(arrival_delay).await;
            acquiring_store
                .acquire_selection_before(
                    &acquiring_store,
                    uuid::Uuid::new_v4(),
                    "automation:budget-control",
                    "fixture",
                    deadline,
                )
                .await
        });
        wait_for_actual_writer(&db.pool, "workflows", &mut acquiring).await;
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let release_budget_ms = deadline.remaining().unwrap().as_millis() as i64;
        blocker.rollback().await.unwrap();
        let mut tx = match acquiring.await.unwrap().unwrap() {
            NativeSelectionAcquisition::Fresh(tx) => tx,
            NativeSelectionAcquisition::Replayed(_) => panic!("no original receipt"),
        };
        // Deliberately exercise a descheduled observer after the owner refreshed.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let configured_ms: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM current_setting('statement_timeout')::interval) * 1000)::bigint")
            .fetch_one(&mut *tx).await.unwrap();
        assert!(
            configured_ms <= release_budget_ms,
            "later statement retained {configured_ms}ms after a real owner wait; ingress had {release_budget_ms}ms before release"
        );
        tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn expired_sql_wait_drops_uncommitted_effects() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query("CREATE TABLE selector_deadline_control (n integer)")
            .execute(&db.pool)
            .await
            .unwrap();
        let deadline = SelectionDeadline::at_ingress();
        let result: Result<(), JobsError> = deadline
            .before_dispatch(async {
                let mut tx = db
                    .pool
                    .begin()
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                deadline.constrain_sql(&mut tx).await?;
                sqlx::query("INSERT INTO selector_deadline_control VALUES (7)")
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                sqlx::query("SELECT pg_sleep(6)")
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                deadline.verify_before_commit_dispatch()?;
                tx.rollback()
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                Ok(())
            })
            .await;
        assert!(result.is_err());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM selector_deadline_control")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            count, 0,
            "deadline cancellation must not leave transaction effects"
        );
    }
    #[tokio::test]
    async fn actual_shared_constructor_joins_one_transaction_but_same_pool_new_handle_refuses() {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let owner = store.clone();
        let mut tx = store.begin_for(&owner).await.unwrap();
        sqlx::query("CREATE TEMP TABLE constructor_binding_control (n integer)")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO constructor_binding_control VALUES (7)")
            .execute(&mut *tx)
            .await
            .unwrap();
        let n: i32 = sqlx::query_scalar("SELECT n FROM constructor_binding_control")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 7);
        tx.rollback().await.unwrap();
        let foreign = NativeSelectionStore::new(db.pool.clone());
        assert!(matches!(
            store.begin_for(&foreign).await,
            Err(JobsError::SelectionConflict(_))
        ));
    }
    #[tokio::test]
    async fn distinct_actual_database_owner_refuses_before_join() {
        let first = boss_testing::TestDb::new().await;
        let second = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(first.pool.clone());
        let foreign = NativeSelectionStore::new(second.pool.clone());
        assert!(matches!(
            store.begin_for(&foreign).await,
            Err(JobsError::SelectionConflict(_))
        ));
    }
    #[tokio::test]
    async fn actual_policy_writer_cannot_change_inputs_until_native_transaction_ends() {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let owner = store.clone();
        let tx = store.begin_policy_fence_for(&owner).await.unwrap();
        let mut writer = db.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(&mut *writer)
            .await
            .unwrap();
        let changed = sqlx::query("UPDATE policy_rules SET active = active")
            .execute(&mut *writer)
            .await;
        assert!(
            changed
                .as_ref()
                .err()
                .and_then(|e| e.as_database_error())
                .is_some_and(|e| e.code().as_deref() == Some("55P03")),
            "actual policy SQL writer entered while native owner transaction was held: {changed:?}"
        );
        writer.rollback().await.unwrap();
        tx.rollback().await.unwrap();
        sqlx::query("UPDATE policy_rules SET active = active")
            .execute(&db.pool)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn real_policy_port_retirement_waits_for_native_transaction_then_records_its_audit() {
        use boss_policy_client::port::PolicyRepository;
        use boss_policy_client::types::{Action, PolicyRule, Resource, Scope};
        let db = boss_testing::TestDb::new().await;
        let policy = Arc::new(boss_policy_client::postgres::PgPolicy::new(db.pool.clone()));
        let rule = PolicyRule {
            id: "worker:step:update".into(),
            role: "worker".into(),
            resource: Resource::step(),
            action: Action::Update,
            scope: Scope::All,
            active: true,
        };
        policy
            .upsert_rule(&rule, "native-owner-fixture")
            .await
            .unwrap();
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store.begin_policy_fence_for(&store.clone()).await.unwrap();
        let writer_policy = policy.clone();
        let id = rule.id.clone();
        let mut writer = tokio::spawn(async move {
            writer_policy
                .deactivate_rule(&id, "native-owner-writer")
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = 'policy_rules'::regclass AND NOT granted)").fetch_one(&db.pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut writer)
                .await
                .is_err(),
            "actual policy retirement escaped the native transaction fence"
        );
        assert!(policy.rule_for(&rule.id).await.unwrap().unwrap().active);
        tx.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!policy.rule_for(&rule.id).await.unwrap().unwrap().active);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM policy_rule_audit WHERE target_id = $1 AND kind = 'rule.deactivate' AND changed_by = 'native-owner-writer'").bind(&rule.id).fetch_one(&db.pool).await.unwrap();
        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn real_agent_registry_writer_waits_for_the_native_actor_fence() {
        use crate::agents::{AgentInput, AgentsRegistry, PgAgents};
        use boss_core::publish::PublishMode;
        let db = boss_testing::TestDb::new().await;
        let registry = Arc::new(PgAgents::new(db.pool.clone()));
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("native-owner-control"),
        );
        let input = AgentInput {
            id: "agent-native-fence".into(),
            display_name: "Original".into(),
            default_model: "opus-5[1m]".into(),
            aliases: vec!["native-fence@example.test".into()],
            role: None,
            department: None,
            hourly_budget_usd_micros: Some(10),
            max_concurrent_runs: Some(1),
        };
        registry
            .publish(std::slice::from_ref(&input), PublishMode::Take, &stamp)
            .await
            .unwrap();
        let store = NativeSelectionStore::new(db.pool.clone());
        let mut tx = store
            .begin_actor_policy_fence_for(&store.clone())
            .await
            .unwrap();
        let observed = crate::agents::postgres::row_in_tx(&mut tx, &input.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observed.aliases, input.aliases);
        assert_eq!(observed.default_model, input.default_model);
        assert_eq!(
            observed.hourly_budget_usd_micros,
            input.hourly_budget_usd_micros
        );
        assert_eq!(observed.max_concurrent_runs, input.max_concurrent_runs);
        let changed = AgentInput {
            display_name: "Changed".into(),
            max_concurrent_runs: Some(2),
            ..input.clone()
        };
        let writer_registry = registry.clone();
        let mut writer = tokio::spawn(async move {
            writer_registry
                .publish(&[changed], PublishMode::Take, &stamp)
                .await
        });
        wait_for_actual_writer(&db.pool, "agents", &mut writer).await;
        assert_eq!(
            registry
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.id == input.id)
                .unwrap()
                .display_name,
            "Original"
        );
        tx.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            registry
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.id == input.id)
                .unwrap()
                .max_concurrent_runs,
            Some(2)
        );
    }
    #[tokio::test]
    async fn real_workflow_writer_cannot_publish_a_draft_during_native_definition_fence() {
        use crate::registry::{PgWorkflows, WorkflowRegistry, WorkflowSpec};
        let db = boss_testing::TestDb::new().await;
        let registry = Arc::new(PgWorkflows::new(db.pool.clone()));
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store
            .begin_definition_actor_policy_fence_for(&store.clone())
            .await
            .unwrap();
        let writer_registry = registry.clone();
        let mut writer = tokio::spawn(async move {
            writer_registry
                .create_draft(
                    WorkflowSpec::platform_seed(
                        "native-definition-control",
                        "Native definition",
                        "platform",
                        vec!["custom".into()],
                        vec![],
                    ),
                    &boss_core::actor::ActorId::automation("native-owner-control"),
                    chrono::Utc::now(),
                )
                .await
        });
        wait_for_actual_writer(&db.pool, "workflows", &mut writer).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut writer)
                .await
                .is_err(),
            "real workflow writer entered native definition fence"
        );
        assert!(
            registry
                .list_versions("native-definition-control")
                .await
                .unwrap()
                .is_empty()
        );
        tx.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            registry
                .list_versions("native-definition-control")
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn real_station_writer_cannot_publish_a_draft_during_native_definition_fence() {
        use crate::{PgStations, StationKind, StationRegistry, StationSpec};
        let db = boss_testing::TestDb::new().await;
        let registry = Arc::new(PgStations::new(db.pool.clone()));
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store
            .begin_definition_actor_policy_fence_for(&store.clone())
            .await
            .unwrap();
        let writer_registry = registry.clone();
        let mut writer = tokio::spawn(async move {
            writer_registry
                .create_draft(
                    StationSpec::draft(
                        "native-station-control",
                        "Native station",
                        StationKind::Batch,
                        crate::station_queue::StationPredicate::default(),
                        chrono::Utc::now(),
                    ),
                    &boss_core::actor::ActorId::automation("native-owner-control"),
                    chrono::Utc::now(),
                )
                .await
        });
        wait_for_actual_writer(&db.pool, "stations", &mut writer).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut writer)
                .await
                .is_err(),
            "real station writer entered native definition fence"
        );
        assert!(
            registry
                .list_versions("native-station-control")
                .await
                .unwrap()
                .is_empty()
        );
        tx.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            registry
                .list_versions("native-station-control")
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn a_delayed_owning_writer_is_observed_at_its_lock_not_a_scheduler_guess() {
        let db = boss_testing::TestDb::new().await;
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store
            .begin_definition_actor_policy_fence_for(&store.clone())
            .await
            .unwrap();
        let pool = db.pool.clone();
        let mut writer = tokio::spawn(async move {
            // The gate's competing binaries can delay arrival past the old
            // two-second observation window without weakening a single lock.
            tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
            sqlx::query("UPDATE stations SET name = name")
                .execute(&pool)
                .await
                .unwrap();
        });
        wait_for_actual_writer(&db.pool, "stations", &mut writer).await;
        assert!(!writer.is_finished());
        tx.rollback().await.unwrap();
        writer.await.unwrap();
    }
    #[tokio::test]
    #[should_panic(expected = "writer finished before its native fence was observed")]
    async fn an_escaped_writer_fails_the_arrival_barrier_instead_of_waiting_forever() {
        let db = boss_testing::TestDb::new().await;
        let pool = db.pool.clone();
        let mut writer = tokio::spawn(async move {
            sqlx::query("UPDATE stations SET name = name")
                .execute(&pool)
                .await
                .unwrap();
        });
        wait_for_actual_writer(&db.pool, "stations", &mut writer).await;
    }
    async fn wait_for_actual_writer<T>(
        pool: &PgPool,
        table: &str,
        writer: &mut tokio::task::JoinHandle<T>,
    ) {
        // Arrival is a PostgreSQL fact, not a deadline on the test runner's
        // scheduler. A writer that escapes or fails before reaching its lock
        // fails this barrier immediately instead of becoming a vacuous pass.
        loop {
            tokio::select! {
                _ = &mut *writer => panic!("actual {table} writer finished before its native fence was observed"),
                waiting = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = $1::regclass AND NOT granted)").bind(table).fetch_one(pool) => {
                    if waiting.unwrap() {
                        assert!(!writer.is_finished(), "actual {table} writer escaped its observed fence");
                        return;
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    }
    #[tokio::test]
    async fn native_transaction_reads_the_same_complete_active_definitions_as_the_owning_ports() {
        use crate::{PgStations, PgWorkflows, StationRegistry, WorkflowRegistry};
        let db = boss_testing::TestDb::new().await;
        let workflows = PgWorkflows::new(db.pool.clone())
            .list_active(None)
            .await
            .unwrap();
        let stations = PgStations::new(db.pool.clone())
            .list_active()
            .await
            .unwrap();
        assert!(!workflows.is_empty() && !stations.is_empty());
        let store = NativeSelectionStore::new(db.pool.clone());
        let mut tx = store
            .begin_definition_actor_policy_fence_for(&store.clone())
            .await
            .unwrap();
        let actual = store.active_definitions_in(&mut tx).await.unwrap();
        assert_eq!(actual, (workflows, stations));
        tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn real_run_admission_waits_for_the_native_population_fence() {
        use crate::{JobsRepository, PgJobs};
        use boss_core::job::{Job, JobStatus, Priority, Subject};
        let db = boss_testing::TestDb::new().await;
        let jobs = Arc::new(PgJobs::new(db.pool.clone()));
        let store = NativeSelectionStore::new(db.pool.clone());
        let tx = store
            .begin_all_input_fence_for(&store.clone())
            .await
            .unwrap();
        let mut run = Job::new(
            crate::agent_budget::RUN_KIND,
            Subject::new("custom", "native-population"),
            "Actual native run",
            "agent-native-fence",
            Priority::Standard,
            chrono::Utc::now().date_naive(),
        );
        run.status = JobStatus::Open;
        let id = run.id;
        let writer_jobs = jobs.clone();
        let mut writer = tokio::spawn(async move {
            let stamp = boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::automation("native-owner-control"),
            );
            writer_jobs
                .create_job_at(
                    &run,
                    stamp.timestamp,
                    &[stamp.event(
                        crate::events::JOB_CREATED,
                        serde_json::to_value(&run).unwrap(),
                    )],
                )
                .await
        });
        wait_for_actual_writer(&db.pool, "jobs", &mut writer).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut writer)
                .await
                .is_err(),
            "actual run admission entered while native population was held"
        );
        assert!(jobs.get_job(&id).await.unwrap().is_none());
        tx.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            jobs.get_job(&id).await.unwrap().unwrap().status,
            JobStatus::Open
        );
        let original = jobs.get_job(&id).await.unwrap().unwrap();
        let mut population_tx = store
            .begin_all_input_fence_for(&store.clone())
            .await
            .unwrap();
        let population = store.open_population_in(&mut population_tx).await.unwrap();
        assert_eq!(
            population.iter().find(|job| job.id == id),
            Some(&original),
            "native population must conserve every field of the actual OPEN admission"
        );
        population_tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_before_commit_rolls_back_but_after_commit_keeps_the_original_effect() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query("CREATE TABLE native_deadline_control (n integer PRIMARY KEY)")
            .execute(&db.pool)
            .await
            .unwrap();
        for committed in [false, true] {
            let pool = db.pool.clone();
            let (sent, received) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut tx = pool.begin().await.unwrap();
                sqlx::query("INSERT INTO native_deadline_control VALUES ($1)")
                    .bind(if committed { 2 } else { 1 })
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                if committed {
                    tx.commit().await.unwrap();
                } else {
                    sent.send(()).unwrap();
                    std::future::pending::<()>().await;
                    return;
                }
                sent.send(()).unwrap();
                std::future::pending::<()>().await;
            });
            received.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM native_deadline_control WHERE n = $1")
                    .bind(if committed { 2 } else { 1 })
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
            assert_eq!(count, if committed { 1 } else { 0 });
        }
    }
    async fn postgres_proxy_frame(
        read: &mut tokio::net::tcp::OwnedReadHalf,
    ) -> std::io::Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;
        let kind = read.read_u8().await?;
        let size = read.read_u32().await?;
        if !(4..=4 * 1024 * 1024).contains(&size) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut frame = vec![0; size as usize + 1];
        frame[0] = kind;
        frame[1..5].copy_from_slice(&size.to_be_bytes());
        read.read_exact(&mut frame[5..]).await?;
        Ok(frame)
    }
    async fn lose_one_commit_reply(
        client: tokio::net::TcpStream,
        server: tokio::net::TcpStream,
        armed: Arc<std::sync::atomic::AtomicBool>,
        refused: Arc<std::sync::atomic::AtomicBool>,
        lost: Arc<std::sync::atomic::AtomicUsize>,
        refuse_reconciliation: bool,
    ) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = server.into_split();
        // TestDb speaks plain local PostgreSQL. No wire bytes are printed or
        // retained; only the command tag COMMIT is observed for this fault.
        let size = client_read.read_u32().await?;
        if !(8..=4 * 1024 * 1024).contains(&size) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut startup = vec![0; size as usize - 4];
        client_read.read_exact(&mut startup).await?;
        server_write.write_all(&size.to_be_bytes()).await?;
        server_write.write_all(&startup).await?;
        let to_server = async {
            loop {
                let frame = postgres_proxy_frame(&mut client_read).await?;
                server_write.write_all(&frame).await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), std::io::Error>(())
        };
        let to_client = async {
            loop {
                let frame = postgres_proxy_frame(&mut server_read).await?;
                if frame[0] == b'C'
                    && &frame[5..] == b"COMMIT\0"
                    && armed.swap(false, Ordering::SeqCst)
                {
                    // The server has actually committed. Lose its reply,
                    // optionally denying fresh reads so uncertainty is real.
                    lost.fetch_add(1, Ordering::SeqCst);
                    refused.store(refuse_reconciliation, Ordering::SeqCst);
                    return Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
                }
                client_write.write_all(&frame).await?;
            }
        };
        tokio::select! {
            result = to_server => result,
            result = to_client => result,
        }
    }
    #[tokio::test]
    async fn actual_lost_commit_reply_reconciles_original_or_names_uncertainty_without_second_effect()
     {
        use crate::JobsRepository;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for refuse_reconciliation in [false, true] {
            let NativeClaimFixture {
                db,
                stamp,
                mut context,
                ..
            } = native_claim_fixture().await;
            let options = db.pool.connect_options();
            assert!(
                options.get_socket().is_none(),
                "fault fixture requires the disposable TestDb TCP endpoint"
            );
            let target = (options.get_host().to_string(), options.get_port());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy_port = listener.local_addr().unwrap().port();
            let armed = Arc::new(AtomicBool::new(false));
            let refused = Arc::new(AtomicBool::new(false));
            let lost = Arc::new(AtomicUsize::new(0));
            let worker_armed = armed.clone();
            let worker_refused = refused.clone();
            let worker_lost = lost.clone();
            let proxy = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (client, _) = accepted.unwrap();
                            client.set_nodelay(true).unwrap();
                            if worker_refused.load(Ordering::SeqCst) { drop(client); continue; }
                            let server = tokio::net::TcpStream::connect((target.0.as_str(), target.1)).await.unwrap();
                            server.set_nodelay(true).unwrap();
                            connections.spawn(lose_one_commit_reply(client, server,
                                worker_armed.clone(), worker_refused.clone(), worker_lost.clone(), refuse_reconciliation));
                        },
                        _ = connections.join_next(), if !connections.is_empty() => {},
                    }
                }
            });
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(30))
                .connect_with(
                    options
                        .as_ref()
                        .clone()
                        .host("127.0.0.1")
                        .port(proxy_port)
                        .ssl_mode(sqlx::postgres::PgSslMode::Disable),
                )
                .await
                .unwrap();
            let store = NativeSelectionStore::new(pool.clone());
            context.native_authority = Some(Arc::new(
                store
                    .observe_authority(resolved_ingress().await, context.request_id, "fixture")
                    .await
                    .unwrap(),
            ));
            let repo = crate::PgJobs::for_native_selection(store);
            let mut claimed = context.validated_step.clone();
            claimed.assignee_id = Some(crate::runner_credential::ACTOR.into());
            claimed.status = boss_core::job::StepStatus::Active;
            let events = [stamp.event(
                crate::events::STEP_UPDATED,
                serde_json::to_value(&claimed).unwrap(),
            )];
            let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            armed.store(true, Ordering::SeqCst);
            let result = repo
                .claim_station_head_at(
                    &context,
                    crate::runner_credential::ACTOR,
                    &[],
                    &stamp,
                    &events,
                )
                .await;
            assert_eq!(
                lost.load(Ordering::SeqCst),
                1,
                "fault must occur after actual PostgreSQL COMMIT, result: {}",
                match &result {
                    Ok(_) => "returned a receipt".into(),
                    Err(error) => error.to_string(),
                }
            );
            let direct = crate::PgJobs::new(db.pool.clone());
            let original = direct
                .station_claim_receipt(context.request_id)
                .await
                .unwrap()
                .unwrap();
            if refuse_reconciliation {
                assert!(
                    matches!(result, Err(JobsError::Storage(ref reason)) if reason.contains("uncertain")),
                    "unavailable read cannot claim rollback or success after lost commit reply"
                );
            } else {
                let crate::station_claim::SelectionResult::Replayed(receipt) = result.unwrap()
                else {
                    panic!("lost commit acknowledgment must answer the actual original receipt")
                };
                assert_eq!(receipt.json(), original.json());
            }
            // A new request reads the dispatched operation's original through
            // current native Read authority; it is not an unconfigured mutation.
            // The earlier uncertain-COMMIT outcome reconciliation is unchanged.
            let fresh_reader = NativeSelectionStore::new(db.pool.clone());
            let receipt = fresh_reader
                .read_selection(&resolved_ingress().await, context.request_id, "fixture")
                .await
                .unwrap()
                .original
                .expect("current authorized original read");
            assert_eq!(receipt.json(), original.json());
            assert_eq!(
                direct
                    .get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                claimed
            );
            let chunks:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind='jobs.station.selection_evidence' AND payload->>'request_id'=$1")
                .bind(context.request_id.to_string()).fetch_one(&db.pool).await.unwrap();
            assert!(
                chunks > 0,
                "lost COMMIT must retain its complete evidence batch"
            );
            let claims:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind='jobs.station.claimed' AND payload->'receipt_manifest'->>'request_id'=$1")
                .bind(context.request_id.to_string()).fetch_one(&db.pool).await.unwrap();
            assert_eq!(claims, 1, "reconciliation must not produce a second claim");
            assert_eq!(
                before + i64::try_from(events.len()).unwrap() + chunks + 1,
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                    .fetch_one(&db.pool)
                    .await
                    .unwrap()
            );
            pool.close().await;
            proxy.abort();
            assert!(proxy.await.unwrap_err().is_cancelled());
        }
    }
}
