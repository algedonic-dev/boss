//! A station pull is an owned READY claim and its conserved selection receipt.
//!
//! HTTP reads may prepare authorization, but only the repository transaction
//! decides whether that prepared packet is still the eligible head.

use std::collections::BTreeMap;

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde_json::Value;

use crate::{
    port::{JobScope, JobsError},
    registry::WorkflowSpec,
    stations::StationSpec,
};

pub const SELECTION_PRE_COMMIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn native_calendar_request(
    observation: &crate::station_claim::NativeAuthorityObservation,
    stored: &boss_core::job::Step,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<
    Option<(
        boss_core::calendar::ReservationRequest,
        boss_policy_client::Decision,
    )>,
    JobsError,
> {
    let request = crate::calendar_hook::start_request(stored, observation.ingress.actor_id())
        .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
    let Some(request) = request else {
        return Ok(None);
    };
    let control = boss_policy_client::controls::CREATE_SCHEDULE;
    let id = boss_policy_client::types::rule_id(
        &observation.user.role,
        &control.resource(),
        control.action(),
    );
    let (decision, _) = boss_policy_client::engine::evaluate_snapshot(
        &observation.user,
        control.action(),
        &control.resource(),
        &observation.overrides,
        observation.rules.iter().find(|row| row.id == id),
        at,
    );
    let boss_policy_client::Decision::Allow { ref scope } = decision else {
        return Err(JobsError::SelectionConflict(
            "native policy refuses current schedule-create authority".into(),
        ));
    };
    if !boss_policy_client::writes::scope_reaches(
        scope,
        &observation.user,
        Some(&request.subject.id),
    ) {
        return Err(JobsError::SelectionConflict(
            "native schedule-create scope does not reach the stored assignee".into(),
        ));
    }
    Ok(Some((request, decision)))
}

pub(crate) fn require_native_claim_constraints(
    observation: &crate::station_claim::NativeAuthorityObservation,
    context: &crate::station_claim::SelectionContext,
) -> Result<(), JobsError> {
    require_native_executor(observation, &context.validated_step, &context.validated_job)?;
    if let Some(tree) = &context.tree_selection
        && tree.executor != observation.pull_executor()
    {
        return Err(JobsError::SelectionConflict(
            "tree executor inputs disagree with the consumed native actor".into(),
        ));
    }
    let agent = observation.agents.iter().find(|agent| {
        agent.id == observation.user.id
            || agent
                .aliases
                .iter()
                .any(|alias| alias == &observation.user.id)
    });
    if let Some(capability) = &context.station.capability {
        let roles: Vec<&str> = std::iter::once(observation.user.role.as_str())
            .chain(agent.and_then(|agent| agent.role.as_deref()))
            .collect();
        if !capability.admits_roles(&roles) {
            return Err(JobsError::SelectionConflict(
                "native actor role is not admitted by station capability".into(),
            ));
        }
        let models: Vec<String> = agent
            .iter()
            .map(|agent| agent.default_model.clone())
            .collect();
        if !capability.models.is_empty() && !capability.allows_model(&models) {
            return Err(JobsError::SelectionConflict(
                "native actor model is not admitted by station capability".into(),
            ));
        }
    }
    if crate::human_only::declared(&context.validated_step.metadata)
        && crate::owner_resolution::is_automation_shaped(&observation.user.id)
    {
        return Err(JobsError::SelectionConflict(
            "native machine actor cannot hold a human-only step".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_native_executor(
    observation: &NativeAuthorityObservation,
    step: &Step,
    job: &Job,
) -> Result<Value, JobsError> {
    let caller = crate::field_writer::CredentialedCaller {
        actor_id: observation.ingress.actor_id.clone(),
        principal: observation.ingress.principal.clone(),
        host: observation.ingress.host.clone(),
    };
    let host = job
        .metadata
        .get(crate::field_writer::HOST_KEY)
        .and_then(Value::as_str);
    let required = crate::credential_executor::judge(&step.metadata, Some(&caller), host)
        .map_err(JobsError::SelectionConflict)?
        .is_some();
    Ok(
        serde_json::json!({"required":required,"declaration":step.metadata.get(crate::credential_executor::KEY),
        "packet_host":host,"resolved_principal":caller.principal,"resolved_host":caller.host,
        "method":"existing credential executor judge consumes sealed authenticated ingress"}),
    )
}

pub(crate) fn require_native_step_authority(
    observation: &crate::station_claim::NativeAuthorityObservation,
    before: &boss_core::job::Step,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<serde_json::Value, JobsError> {
    let Some(authority) = before
        .metadata
        .get("authority_role")
        .and_then(serde_json::Value::as_str)
        .filter(|role| !role.is_empty())
    else {
        return Ok(
            serde_json::json!({"required": false, "reason": "step declares no authority role"}),
        );
    };
    let agent = observation.agents.iter().find(|agent| {
        agent.id == observation.user.id
            || agent
                .aliases
                .iter()
                .any(|alias| alias == &observation.user.id)
    });
    let handed = before.assignee_id.as_deref().is_some_and(|holder| {
        holder == observation.user.id
            || agent.is_some_and(|agent| {
                agent.id == holder || agent.aliases.iter().any(|alias| alias == holder)
            })
    });
    let role_matches = observation.user.role == authority
        || agent.and_then(|agent| agent.role.as_deref()) == Some(authority);
    if handed || role_matches {
        return Ok(
            serde_json::json!({"required": true, "authority_role": authority,
            "actor": observation.user.id, "handed_to_actor": handed, "role_matches": role_matches,
            "evaluated_at": at, "permission_checks": []}),
        );
    }
    let assign = boss_policy_client::controls::UPDATE_STEP_ASSIGN;
    let mut checks = Vec::new();
    for (action, resource) in [
        (
            boss_policy_client::Action::SignOff,
            boss_policy_client::Resource::new(format!("step-signoff:{authority}")),
        ),
        (assign.action(), assign.resource()),
    ] {
        let id = boss_policy_client::types::rule_id(&observation.user.role, &resource, action);
        let (decision, expiry) = boss_policy_client::engine::evaluate_snapshot(
            &observation.user,
            action,
            &resource,
            &observation.overrides,
            observation.rules.iter().find(|rule| rule.id == id),
            at,
        );
        let allowed = matches!(decision, boss_policy_client::Decision::Allow { .. });
        checks.push(serde_json::json!({"action": action, "resource": resource,
            "user": observation.user, "decision": decision, "expiry": expiry}));
        if allowed {
            return Ok(
                serde_json::json!({"required": true, "authority_role": authority,
                "actor": observation.user.id, "handed_to_actor": false, "role_matches": false,
                "evaluated_at": at, "permission_checks": checks}),
            );
        }
    }
    Err(JobsError::SelectionConflict(
        "native policy refuses current step authority".into(),
    ))
}

pub enum SelectionResult {
    Claimed {
        step: Box<Step>,
        receipt: SelectionReceipt,
    },
    Refused {
        receipt: SelectionReceipt,
    },
    Replayed(SelectionReceipt),
}

/// Only a server credential resolver constructs this identity. A mode
/// observation by itself authenticates nobody, and no header deserializes it.
#[derive(Clone, Debug)]
pub struct AuthenticatedSelectionIngress {
    actor_id: String,
    principal: String,
    host: Option<String>,
    mode: boss_core::machine_gate::RequestModeObservation,
    /// The pre-commit budget this request runs under, carried with its
    /// ingress so every reader of the deadline takes the same one. A
    /// credential-resolved request always carries
    /// `SELECTION_PRE_COMMIT_BUDGET`; only a test can state another
    /// (`with_budget`), because 37 native-selection tests that judged an
    /// answer were measuring the gate's database contention against the
    /// production five seconds instead (f10419f9, gate bb1fbbde), and the
    /// integration tests did the same on gate 7563df44 (backlog d7e5b016).
    budget: std::time::Duration,
    /// Time the request had already spent when the machine gate observed
    /// it: zero for every credential-resolved request. A fixture states the
    /// io.full stall a gate drive measured, so every reader of the deadline
    /// judges a request that has already lost it, rather than the test
    /// waiting for a stall or a stalled drive deciding it (backlog d7e5b016).
    spent: std::time::Duration,
}
impl AuthenticatedSelectionIngress {
    pub(crate) fn from_credential(
        caller: &crate::field_writer::CredentialedCaller,
        mode: boss_core::machine_gate::RequestModeObservation,
    ) -> Self {
        Self {
            actor_id: caller.actor_id.clone(),
            principal: caller.principal.clone(),
            host: caller.host.clone(),
            mode,
            budget: SELECTION_PRE_COMMIT_BUDGET,
            spent: std::time::Duration::ZERO,
        }
    }
    /// Robot claims use the estate door and their own named identity (1f04d9c6,
    /// conductor decision 2026-11-14). Only the machine gate can provide this
    /// witness: admission in off/report mode alone authenticates nobody.
    /// The header supplies no grants; the native owner reads the actor's row
    /// and policy. This transport proof never becomes a runner writer grant.
    pub(crate) fn from_machine(
        user: &boss_policy_client::User,
        _estate: &boss_core::machine_gate::VerifiedEstate,
        mode: boss_core::machine_gate::RequestModeObservation,
    ) -> Option<Self> {
        if user.id.trim().is_empty() || user.id == boss_policy_client::User::ANONYMOUS_ID {
            return None;
        }
        Some(Self {
            actor_id: user.id.clone(),
            principal: "machine:estate".into(),
            host: None,
            mode,
            budget: SELECTION_PRE_COMMIT_BUDGET,
            spent: std::time::Duration::ZERO,
        })
    }
    /// A fixture's ingress clock: the admitted request reports `spent`
    /// already gone before the gate observed it, so `started` and
    /// `deadline` (and every deadline read off them) move back together
    /// (backlog d7e5b016). Test-support only; production keeps zero.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_time_already_spent(mut self, spent: std::time::Duration) -> Self {
        self.spent = spent;
        self
    }
    /// A test's own budget for the same admitted request: its origin stays
    /// the original ingress, only the length is the test's (f10419f9).
    /// Test-support only, so the integration tests reach it (d7e5b016).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_budget(mut self, budget: std::time::Duration) -> Self {
        self.budget = budget;
        self
    }
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }
    pub fn started(&self) -> std::time::Instant {
        let observed = self.mode.started();
        observed.checked_sub(self.spent).unwrap_or(observed)
    }
    /// The one absolute pre-commit deadline: ingress plus its budget, on
    /// tokio's clock. Every reader of the deadline compares it with
    /// `tokio::time::Instant::now()`, so all of them read ONE clock. Review
    /// a0fd6902 measured why that matters: as a `std::time::Instant`, a
    /// post-COMMIT check against the std clock survived the commit test,
    /// which can expire only the clock it moves (f10419f9). In production
    /// the two clocks are the same clock.
    pub fn deadline(&self) -> tokio::time::Instant {
        tokio::time::Instant::from_std(self.started()) + self.budget
    }
    /// Cancel only read-only HTTP preparation, never mutation or COMMIT.
    pub(crate) async fn prepare<T>(
        &self,
        read: impl std::future::Future<Output = T>,
    ) -> Result<T, JobsError> {
        let deadline = self.deadline();
        if tokio::time::Instant::now() >= deadline {
            return Err(JobsError::SelectionConflict(
                "selection ingress deadline expired during preparation".into(),
            ));
        }
        tokio::time::timeout_at(deadline, read).await.map_err(|_| {
            JobsError::SelectionConflict(
                "selection ingress deadline expired during preparation".into(),
            )
        })
    }
    pub fn observation(&self) -> Value {
        serde_json::json!({"actor_id": self.actor_id, "principal": self.principal, "host": self.host,
            "machine_mode": self.mode.mode().name(), "pre_dispatch_budget_ms": self.budget.as_millis(),
            "source": "server-resolved credential and machine gate ingress observation"})
    }
}

/// The exact server artifact and a parsed view derived only from that text.
/// Serving the text avoids reserializing a historical receipt on replay.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionReceipt {
    json: String,
    value: Value,
}

impl SelectionReceipt {
    pub fn from_value(value: Value) -> Result<Self, JobsError> {
        Self::from_value_before(value, None)
    }
    fn from_value_before(
        value: Value,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Self, JobsError> {
        validate_owner_copy_before(&value, deadline)?;
        let json = crate::selection_serialization::json(&value, deadline)?;
        Ok(Self { json, value })
    }
    pub fn from_json(json: String) -> Result<Self, JobsError> {
        let value =
            serde_json::from_str(&json).map_err(|error| JobsError::Storage(error.to_string()))?;
        validate_owner_copy(&value)?;
        Ok(Self { json, value })
    }
    pub fn json(&self) -> &str {
        &self.json
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn into_json(self) -> String {
        self.json
    }

    /// Return the ORIGINAL artifact only when current policy reaches every
    /// captured job binding. Never redact it or reinterpret it using today's rows.
    pub(crate) fn require_read_scope(&self, scope: &JobScope) -> Result<(), JobsError> {
        let check = |job: &Value| -> Result<(), JobsError> {
            let owner = job
                .get("owner_id")
                .and_then(Value::as_str)
                .ok_or_else(|| JobsError::Storage("copied job lacks owner binding".into()))?;
            let kind = job
                .pointer("/subject/subject_kind")
                .and_then(Value::as_str)
                .ok_or_else(|| JobsError::Storage("copied job lacks subject kind".into()))?;
            let id = job
                .pointer("/subject/id")
                .and_then(Value::as_str)
                .ok_or_else(|| JobsError::Storage("copied job lacks subject identity".into()))?;
            if !scope.admits_binding(owner, kind, id) {
                return Err(JobsError::SelectionConflict(
                    "original selection evidence is outside current read scope".into(),
                ));
            }
            Ok(())
        };
        let selected = self
            .pointer("/validated/job")
            .ok_or_else(|| JobsError::Storage("original selection lacks captured job".into()))?;
        check(selected)?;
        // Receipts before f999363f copied every open packet; receipts until
        // a8e91ac5 copied the packets that competed. Either copy is checked
        // binding by binding. Later receipts keep the head packet whole and
        // digest the rest, so they are checked by the scope every
        // competitor was admitted under.
        let population = self
            .pointer("/owner_consumption/competing_population")
            .or_else(|| self.pointer("/owner_consumption/open_population"));
        let digest = self.pointer("/owner_consumption/competitor_digest");
        match (population, digest) {
            (Some(Value::Array(rows)), None) => {
                for row in rows {
                    check(row.get(0).ok_or_else(|| {
                        JobsError::Storage("copied population row lacks job".into())
                    })?)?;
                }
            }
            (None, Some(digest)) => {
                check(
                    self.pointer("/owner_consumption/competing_head/0")
                        .ok_or_else(|| {
                            JobsError::Storage("digested selection lacks its head packet".into())
                        })?,
                )?;
                let count = digest.get("count").and_then(Value::as_u64).ok_or_else(|| {
                    JobsError::Storage("competitor digest lacks its count".into())
                })?;
                // The head always competes. Beyond it, every competitor
                // passed `admitted_by` when it was read; a reader whose
                // scope does not cover that one could be shown a packet it
                // cannot read, so the replay refuses rather than redacts.
                if count > 1 {
                    let admitted_by: JobScope =
                        serde_json::from_value(digest.get("admitted_by").cloned().ok_or_else(
                            || JobsError::Storage("competitor digest lacks its admission".into()),
                        )?)
                        .map_err(|error| JobsError::Storage(error.to_string()))?;
                    if !scope.covers(&admitted_by) {
                        return Err(JobsError::SelectionConflict(
                            "original competitors were admitted under a read scope the current one does not cover".into(),
                        ));
                    }
                }
            }
            (_, None) if self["version"] == 1 => {} // Historical inline artifacts may lack this later observation.
            _ => {
                return Err(JobsError::Storage(
                    "v2 selection lacks complete population evidence".into(),
                ));
            }
        }
        // Counter inputs are copied outside the candidate's scoped population.
        // Older originals lack their bindings: only All can read them without
        // inventing those historical bindings from today's mutable job rows.
        // Only originals written before backlog 3e927996 retired the per-actor
        // run cap carry either path; a fresh receipt carries neither, and the
        // replay of an old one is still held to the reader's scope here.
        if *scope != JobScope::All {
            for path in [
                "/owner_consumption/run_population/rows",
                "/owner_consumption/concurrency/runs",
            ] {
                if let Some(rows) = self.pointer(path) {
                    let rows = rows.as_array().ok_or_else(|| {
                        JobsError::SelectionConflict(
                            "original counter population lacks readable job bindings".into(),
                        )
                    })?;
                    for row in rows {
                        check(row).map_err(|_| JobsError::SelectionConflict(
                            "original counter evidence is outside current read scope or lacks its historical job binding".into()))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Verify only conservation of the copied artifact, never present authority.
/// Historical receipts without either field remain historical receipts.
fn validate_owner_copy(value: &Value) -> Result<(), JobsError> {
    validate_owner_copy_before(value, None)
}
fn validate_owner_copy_before(
    value: &Value,
    deadline: Option<tokio::time::Instant>,
) -> Result<(), JobsError> {
    match (
        value.get("owner_consumption"),
        value.get("owner_consumption_hash"),
    ) {
        (None, None) => Ok(()),
        (Some(artifact), Some(digest)) => {
            let actual = crate::selection_serialization::canonical_hash(artifact, deadline)?;
            if artifact.get("version").and_then(Value::as_u64) != Some(1)
                || digest.as_str() != Some(actual.as_str())
            {
                return Err(JobsError::Storage(
                    "station owner consumption disagrees with its copied artifact".into(),
                ));
            }
            Ok(())
        }
        _ => Err(JobsError::Storage(
            "station owner consumption copy or digest unavailable".into(),
        )),
    }
}

impl std::ops::Deref for SelectionReceipt {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}

impl serde::Serialize for SelectionReceipt {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

#[derive(Clone)]
pub struct SelectionContext {
    pub native_authority: Option<std::sync::Arc<NativeAuthorityObservation>>,
    pub actor_observation: Option<ActorSelectionObservation>,
    pub prepared_policy: Option<std::sync::Arc<PolicySelectionObservation>>,
    pub definition_fence: Option<std::sync::Arc<dyn SelectionFence>>,
    pub request_id: uuid::Uuid,
    pub station: StationSpec,
    pub scope: JobScope,
    pub active_workflows: BTreeMap<String, WorkflowSpec>,
    pub tree_selection: Option<TreeSelection>,
    pub validated_job: Job,
    pub validated_step: Step,
}

/// Copied preparation inputs, bound to the opaque constructor that actually
/// read them. These rows are observations; only a fresh transaction consumes
/// their real writers and re-evaluates authority at commit dispatch.
#[derive(Clone)]
pub struct NativeAuthorityObservation {
    pub(crate) store_identity: std::sync::Arc<()>,
    pub(crate) request_id: uuid::Uuid,
    pub(crate) station_name: String,
    pub(crate) ingress: AuthenticatedSelectionIngress,
    pub(crate) user: boss_policy_client::User,
    pub(crate) agents: Vec<crate::agents::AgentRow>,
    pub(crate) automations: Vec<crate::agents::automations::AutomationActor>,
    pub(crate) rules: Vec<boss_policy_client::PolicyRule>,
    pub(crate) overrides: Vec<boss_policy_client::UserOverride>,
    pub(crate) observed_at: chrono::DateTime<chrono::Utc>,
    pub(crate) scope: JobScope,
    pub(crate) budget_hour: Option<Value>,
}
impl NativeAuthorityObservation {
    pub(crate) fn pull_executor(&self) -> PullExecutor {
        PullExecutor::from_user(
            &self.user,
            self.agents
                .iter()
                .find(|row| row.id == self.user.id || row.aliases.contains(&self.user.id)),
        )
    }
    pub(crate) fn prepared_inputs(&self) -> Value {
        serde_json::json!({"request_id":self.request_id,"station":self.station_name,
            "user":self.user,"agents":self.agents,"automations":self.automations,
            "rules":self.rules,"overrides":self.overrides,"observed_at":self.observed_at})
    }
    pub(crate) fn budget_hour_observation(&self, context: &SelectionContext) -> Value {
        let resolved = crate::agent_spec::resolved(
            &context.validated_step,
            context.active_workflows.get(&context.validated_job.kind),
        );
        let required = !self.agents.is_empty()
            && resolved
                .metadata
                .get(crate::agent_spec::BUDGET_KEY)
                .and_then(Value::as_f64)
                .is_some();
        self.budget_hour.clone().unwrap_or_else(|| {
            if required {
                serde_json::json!({"status":"unknown", "reason":"native preflight supplied no historical advisory reading",
                    "actor_inputs":self.agents,"observed_at":null,"inputs":null})
            } else {
                serde_json::json!({"status":"not_required","reason":"no registered agent or declared step budget",
                    "actor_inputs":self.agents,"observed_at":self.observed_at})
            }
        })
    }
    /// A prepared native decision is only preflight. The same actual rows and
    /// expiry are consumed again under their writer locks before COMMIT.
    pub(crate) fn policy_decision(
        &self,
        action: boss_policy_client::Action,
        resource: &boss_policy_client::Resource,
    ) -> boss_policy_client::Decision {
        let id = boss_policy_client::types::rule_id(&self.user.role, resource, action);
        boss_policy_client::engine::evaluate_snapshot(
            &self.user,
            action,
            resource,
            &self.overrides,
            self.rules.iter().find(|row| row.id == id),
            boss_policy_client::engine::expiry_now(),
        )
        .0
    }
}

/// A native read consumes current registry/policy owners, independently of
/// fresh mutation authority so an original receipt survives Update retirement.
pub struct NativeSelectionRead {
    pub user: boss_policy_client::User,
    pub scope: JobScope,
    pub original: Option<SelectionReceipt>,
}

pub(crate) fn actor_inputs(
    agents: Vec<crate::agents::AgentRow>,
    automations: Vec<crate::agents::automations::AutomationActor>,
    actor: &str,
) -> (
    Vec<crate::agents::AgentRow>,
    Vec<crate::agents::automations::AutomationActor>,
) {
    let matching: Vec<_> = agents
        .into_iter()
        .filter(|row| row.id == actor || row.aliases.iter().any(|alias| alias == actor))
        .collect();
    let automation = if matching.is_empty() {
        crate::agents::automations::row_of_record(&automations, actor)
            .cloned()
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    (matching, automation)
}

pub(crate) fn native_user(
    ingress: &AuthenticatedSelectionIngress,
    agents: &[crate::agents::AgentRow],
    automations: &[crate::agents::automations::AutomationActor],
) -> Result<boss_policy_client::User, JobsError> {
    let mut user = boss_policy_client::User::anonymous();
    user.id = ingress.actor_id().to_string();
    if let Some(row) = agents
        .iter()
        .find(|row| row.id == user.id || row.aliases.contains(&user.id))
    {
        user.role = row.role.clone().ok_or_else(|| {
            JobsError::SelectionConflict("native authenticated actor holds no role".into())
        })?;
        user.department = row.department.clone();
    } else if let Some(row) = crate::agents::automations::row_of_record(automations, &user.id) {
        user.role = row.role.clone();
    } else {
        return Err(JobsError::SelectionConflict(
            "native authenticated actor registry row unavailable".into(),
        ));
    }
    // Only the authenticated ingress identity and current owner rows supply grants.
    Ok(user)
}

#[derive(Clone)]
pub struct ActorSelectionObservation {
    pub owner: std::sync::Arc<dyn crate::agents::AgentsRegistry>,
    pub holder: String,
    pub row: Option<crate::agents::AgentRow>,
    pub requested_user: Value,
}

impl ActorSelectionObservation {
    fn lock_verified(
        &self,
    ) -> Result<Box<dyn crate::agents::port::AgentSelectionRows + '_>, JobsError> {
        let guard = self
            .owner
            .lock_selection_rows()
            .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
        let current = guard
            .rows()
            .iter()
            .find(|row| row.id == self.holder || row.aliases.contains(&self.holder));
        if current != self.row.as_ref() {
            return Err(JobsError::SelectionConflict(
                "agent capability observation changed before selection commit".into(),
            ));
        }
        Ok(guard)
    }
}

/// An acquired local owner boundary. Acquisition refuses before effects;
/// guard destruction cannot run a second commit or turn a commit into failure.
pub trait SelectionGuard {
    /// Validate expiring authority under held owners at the actual commit door.
    fn verify_at_commit(&self) -> Result<Value, JobsError>;
}

pub trait SelectionFence: Send + Sync {
    fn try_lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Box<dyn SelectionGuard + 'a>>, JobsError> {
        let _ = (context, actor, at);
        Err(JobsError::Storage(
            "selection owner has no nonblocking acquisition".into(),
        ))
    }
    fn lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Box<dyn SelectionGuard + 'a>, JobsError>;
}

#[derive(Clone)]
pub struct PolicySelectionObservation {
    owner: std::sync::Arc<dyn boss_policy_client::PolicyClient>,
    user: boss_policy_client::User,
    expected: boss_policy_client::Decision,
    request_id: uuid::Uuid,
    rules: Vec<boss_policy_client::PolicyRule>,
    overrides: Vec<boss_policy_client::UserOverride>,
    observed_at: chrono::DateTime<chrono::Utc>,
    read_decision: boss_policy_client::Decision,
    read_predicate: boss_policy_client::Predicate,
}

impl PolicySelectionObservation {
    pub(crate) fn from_native(
        owner: std::sync::Arc<dyn boss_policy_client::PolicyClient>,
        native: &NativeAuthorityObservation,
    ) -> Result<Self, JobsError> {
        let control = boss_policy_client::controls::UPDATE_STEP;
        let resource = control.resource();
        let action = control.action();
        let id = boss_policy_client::types::rule_id(&native.user.role, &resource, action);
        let (expected, _) = boss_policy_client::engine::evaluate_snapshot(
            &native.user,
            action,
            &resource,
            &native.overrides,
            native.rules.iter().find(|rule| rule.id == id),
            native.observed_at,
        );
        let control = boss_policy_client::controls::READ_JOB;
        let resource = control.resource();
        let action = control.action();
        let id = boss_policy_client::types::rule_id(&native.user.role, &resource, action);
        let (read_decision, _) = boss_policy_client::engine::evaluate_snapshot(
            &native.user,
            action,
            &resource,
            &native.overrides,
            native.rules.iter().find(|rule| rule.id == id),
            native.observed_at,
        );
        let read_predicate = match &read_decision {
            boss_policy_client::Decision::Allow { scope } => {
                boss_policy_client::predicates::scope_to_predicate(scope, &native.user)
            }
            boss_policy_client::Decision::Deny { .. } => boss_policy_client::Predicate::None,
        };
        if JobScope::from_predicate(&native.user, &read_predicate) != native.scope {
            return Err(JobsError::SelectionConflict(
                "native preparation disagrees with its copied read scope".into(),
            ));
        }
        Ok(Self {
            owner,
            user: native.user.clone(),
            expected,
            request_id: native.request_id,
            rules: native.rules.clone(),
            overrides: native.overrides.clone(),
            observed_at: native.observed_at,
            read_decision,
            read_predicate,
        })
    }
    pub fn matches_preflight(
        &self,
        owner: &std::sync::Arc<dyn boss_policy_client::PolicyClient>,
        user: &boss_policy_client::User,
        expected: &boss_policy_client::Decision,
        request_id: uuid::Uuid,
    ) -> bool {
        std::sync::Arc::ptr_eq(&self.owner, owner)
            && &self.user == user
            && &self.expected == expected
            && self.request_id == request_id
    }

    /// Capture actual complete inputs under the SAME native writer owner;
    /// this is not a cached transport response or a digest authority.
    pub fn prepare(
        owner: std::sync::Arc<dyn boss_policy_client::PolicyClient>,
        user: boss_policy_client::User,
        request_id: uuid::Uuid,
    ) -> Result<Self, JobsError> {
        let rows = owner
            .lock_selection_rows()
            .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
        let at = boss_policy_client::engine::expiry_now();
        let control = boss_policy_client::controls::UPDATE_STEP;
        let resource = control.resource();
        let action = control.action();
        let id = boss_policy_client::types::rule_id(&user.role, &resource, action);
        let rule = rows.rules().iter().find(|rule| rule.id == id);
        let (expected, _) = boss_policy_client::engine::evaluate_snapshot(
            &user,
            action,
            &resource,
            rows.overrides(),
            rule,
            at,
        );
        let read_control = boss_policy_client::controls::READ_JOB;
        let read_resource = read_control.resource();
        let read_action = read_control.action();
        let read_id = boss_policy_client::types::rule_id(&user.role, &read_resource, read_action);
        let read_rule = rows.rules().iter().find(|rule| rule.id == read_id);
        let (read_decision, _) = boss_policy_client::engine::evaluate_snapshot(
            &user,
            read_action,
            &read_resource,
            rows.overrides(),
            read_rule,
            at,
        );
        let read_predicate = match &read_decision {
            boss_policy_client::Decision::Allow { scope } => {
                boss_policy_client::predicates::scope_to_predicate(scope, &user)
            }
            boss_policy_client::Decision::Deny { .. } => boss_policy_client::Predicate::None,
        };
        let rules = rows
            .rules()
            .iter()
            .filter(|rule| rule.role == user.role)
            .cloned()
            .collect();
        let overrides = rows
            .overrides()
            .iter()
            .filter(|row| row.user_id == user.id)
            .cloned()
            .collect();
        drop(rows);
        Ok(Self {
            owner,
            user,
            expected,
            request_id,
            rules,
            overrides,
            observed_at: at,
            read_decision,
            read_predicate,
        })
    }
}

pub struct RegistrySelectionFence {
    pub workflows: std::sync::Arc<dyn crate::registry::WorkflowRegistry>,
    pub stations: std::sync::Arc<dyn crate::stations::StationRegistry>,
    pub policy: std::sync::Arc<PolicySelectionObservation>,
}

struct RegistrySelectionGuard<'a> {
    _workflow: Box<dyn crate::registry::WorkflowSelectionRows + 'a>,
    _station: Box<dyn crate::stations::StationSelectionRows + 'a>,
    _actor: Box<dyn crate::agents::port::AgentSelectionRows + 'a>,
    _policy: Box<dyn boss_policy_client::port::PolicySelectionRows + 'a>,
    observation: &'a PolicySelectionObservation,
    scope: &'a JobScope,
    actor_observation: &'a ActorSelectionObservation,
    native_actor: Option<&'a NativeAuthorityObservation>,
    tree: Option<&'a TreeSelection>,
}
impl SelectionGuard for RegistrySelectionGuard<'_> {
    fn verify_at_commit(&self) -> Result<Value, JobsError> {
        verify_native_actor(self.native_actor, &*self._actor)?;
        let policy = validate_policy(
            self.observation,
            &*self._policy,
            self.scope,
            boss_policy_client::engine::expiry_now(),
        )?;
        let actor = self._actor.rows().iter().find(|row| {
            row.id == self.actor_observation.holder
                || row.aliases.contains(&self.actor_observation.holder)
        });
        let pinned = self
            .tree
            .iter()
            .flat_map(|tree| &tree.inputs.protocols)
            .map(|row| {
                self._workflow
                    .version(&row.kind, row.version)
                    .ok_or_else(|| {
                        JobsError::SelectionConflict(
                            "pinned protocol owner disappeared before commit".into(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({
            "version": 1,
            "method": "native owner writer guards held through repository commit",
            "actor": {"holder": self.actor_observation.holder, "row": actor, "automation": crate::agents::automations::row_of_record(self._actor.automations(), &self.actor_observation.holder), "requested_user": self.actor_observation.requested_user},
            "definitions": {"workflows": self._workflow.rows(), "stations": self._station.rows(), "pinned_protocols": pinned},
            "policy": policy,
        }))
    }
}

impl SelectionFence for RegistrySelectionFence {
    fn try_lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Box<dyn SelectionGuard + 'a>>, JobsError> {
        if self.policy.request_id != context.request_id {
            return Err(JobsError::SelectionConflict(
                "prepared policy request identity changed".into(),
            ));
        }
        let Some(workflow) = self.workflows.try_lock_active_selection_rows()? else {
            return Ok(None);
        };
        let Some(station) = self.stations.try_lock_active_selection_rows()? else {
            return Ok(None);
        };
        validate_definitions(context, workflow.rows(), station.rows(), actor, at)?;
        validate_guarded_tree_protocols(context, &*workflow)?;
        let observation = context.actor_observation.as_ref().ok_or_else(|| {
            JobsError::SelectionConflict("selection actor owner witness unavailable".into())
        })?;
        let Some(actor) = observation
            .owner
            .try_lock_selection_rows()
            .map_err(|error| JobsError::SelectionConflict(error.to_string()))?
        else {
            return Ok(None);
        };
        let current = actor
            .rows()
            .iter()
            .find(|row| row.id == observation.holder || row.aliases.contains(&observation.holder));
        if current != observation.row.as_ref() {
            return Err(JobsError::SelectionConflict(
                "agent capability observation changed before selection commit".into(),
            ));
        }
        if let Some(tree) = &context.tree_selection
            && tree.executor != PullExecutor::from_user(&self.policy.user, current)
        {
            return Err(JobsError::SelectionConflict(
                "tree executor inputs changed before selection commit".into(),
            ));
        }
        verify_native_actor(context.native_authority.as_deref(), &*actor)?;
        let Some(policy) = self
            .policy
            .owner
            .try_lock_selection_rows()
            .map_err(|error| JobsError::SelectionConflict(error.to_string()))?
        else {
            return Ok(None);
        };
        validate_policy(
            &self.policy,
            &*policy,
            &context.scope,
            boss_policy_client::engine::expiry_now(),
        )?;
        Ok(Some(Box::new(RegistrySelectionGuard {
            _workflow: workflow,
            _station: station,
            _actor: actor,
            _policy: policy,
            observation: &self.policy,
            scope: &context.scope,
            actor_observation: observation,
            native_actor: context.native_authority.as_deref(),
            tree: context.tree_selection.as_ref(),
        })))
    }
    fn lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Box<dyn SelectionGuard + 'a>, JobsError> {
        if self.policy.request_id != context.request_id {
            return Err(JobsError::SelectionConflict(
                "prepared policy request identity changed".into(),
            ));
        }
        let workflow = self.workflows.lock_active_selection_rows()?;
        let station = self.stations.lock_active_selection_rows()?;
        validate_definitions(context, workflow.rows(), station.rows(), actor, at)?;
        validate_guarded_tree_protocols(context, &*workflow)?;
        let observation = context.actor_observation.as_ref().ok_or_else(|| {
            JobsError::SelectionConflict("selection actor owner witness unavailable".into())
        })?;
        let actor = observation.lock_verified()?;
        let current = actor
            .rows()
            .iter()
            .find(|row| row.id == observation.holder || row.aliases.contains(&observation.holder));
        if let Some(tree) = &context.tree_selection
            && tree.executor != PullExecutor::from_user(&self.policy.user, current)
        {
            return Err(JobsError::SelectionConflict(
                "tree executor inputs changed before selection commit".into(),
            ));
        }
        verify_native_actor(context.native_authority.as_deref(), &*actor)?;
        let policy = self
            .policy
            .owner
            .lock_selection_rows()
            .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
        validate_policy(
            &self.policy,
            &*policy,
            &context.scope,
            boss_policy_client::engine::expiry_now(),
        )?;
        Ok(Box::new(RegistrySelectionGuard {
            _workflow: workflow,
            _station: station,
            _actor: actor,
            _policy: policy,
            observation: &self.policy,
            scope: &context.scope,
            actor_observation: observation,
            native_actor: context.native_authority.as_deref(),
            tree: context.tree_selection.as_ref(),
        }))
    }
}

fn verify_native_actor(
    observation: Option<&NativeAuthorityObservation>,
    owner: &dyn crate::agents::port::AgentSelectionRows,
) -> Result<(), JobsError> {
    if let Some(observation) = observation {
        let (agents, automations) = actor_inputs(
            owner.rows().to_vec(),
            owner.automations().to_vec(),
            observation.ingress.actor_id(),
        );
        if agents != observation.agents
            || automations != observation.automations
            || native_user(&observation.ingress, &agents, &automations)? != observation.user
        {
            return Err(JobsError::SelectionConflict(
                "native actor owner observation changed before selection commit".into(),
            ));
        }
    }
    Ok(())
}

fn validate_policy(
    observation: &PolicySelectionObservation,
    rows: &dyn boss_policy_client::port::PolicySelectionRows,
    scope: &JobScope,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value, JobsError> {
    if JobScope::from_predicate(&observation.user, &observation.read_predicate) != *scope {
        return Err(JobsError::SelectionConflict(
            "prepared native read scope differs from selection input".into(),
        ));
    }
    let rules: Vec<_> = rows
        .rules()
        .iter()
        .filter(|rule| rule.role == observation.user.role)
        .cloned()
        .collect();
    let overrides: Vec<_> = rows
        .overrides()
        .iter()
        .filter(|row| row.user_id == observation.user.id)
        .cloned()
        .collect();
    if rules != observation.rules || overrides != observation.overrides {
        return Err(JobsError::SelectionConflict(
            "prepared policy inputs changed before native commit".into(),
        ));
    }
    let control = boss_policy_client::controls::UPDATE_STEP;
    let resource = control.resource();
    let action = control.action();
    let id = boss_policy_client::types::rule_id(&observation.user.role, &resource, action);
    let rule = rows.rules().iter().find(|rule| rule.id == id);
    let (decision, update_expiry) = boss_policy_client::engine::evaluate_snapshot(
        &observation.user,
        action,
        &resource,
        rows.overrides(),
        rule,
        now,
    );
    if decision != observation.expected {
        return Err(JobsError::SelectionConflict(
            "policy authorization changed before selection commit".into(),
        ));
    }
    let read_control = boss_policy_client::controls::READ_JOB;
    let read_resource = read_control.resource();
    let read_action = read_control.action();
    let read_id =
        boss_policy_client::types::rule_id(&observation.user.role, &read_resource, read_action);
    let read_rule = rows.rules().iter().find(|rule| rule.id == read_id);
    let (read_decision, read_expiry) = boss_policy_client::engine::evaluate_snapshot(
        &observation.user,
        read_action,
        &read_resource,
        rows.overrides(),
        read_rule,
        now,
    );
    let predicate = match &read_decision {
        boss_policy_client::Decision::Allow { scope } => {
            boss_policy_client::predicates::scope_to_predicate(scope, &observation.user)
        }
        boss_policy_client::Decision::Deny { .. } => boss_policy_client::Predicate::None,
    };
    if crate::port::JobScope::from_predicate(&observation.user, &predicate) != *scope {
        return Err(JobsError::SelectionConflict(
            "policy read scope changed before selection commit".into(),
        ));
    }
    Ok(serde_json::json!({
        "prepared_inputs": {
            "request_id": observation.request_id, "user": observation.user,
            "rules": observation.rules, "overrides": observation.overrides, "observed_at": observation.observed_at,
            "source_binding": "same native PolicyClient owner instance; actual writer guard",
            "queries": [
                {"action": "read", "resource": "job", "decision": observation.read_decision, "predicate": observation.read_predicate},
                {"action": "update", "resource": "step", "decision": observation.expected},
            ],
        },
        "user": observation.user,
        "prepared_update": observation.expected,
        "prepared_read_scope": scope,
        "rules": rules, "overrides": overrides,
        "linearized_at": now,
        "consumed_update": decision, "update_expires_at": update_expiry,
        "consumed_read": read_decision, "read_expires_at": read_expiry,
        "consumed_read_predicate": predicate,
    }))
}

pub fn validate_definitions(
    context: &SelectionContext,
    workflows: &[WorkflowSpec],
    stations: &[StationSpec],
    actor: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), JobsError> {
    if let Some(tree) = &context.tree_selection {
        let mut expected = tree.inputs.stations.clone();
        let mut consumed = stations.to_vec();
        expected.sort_by(|a, b| a.name.cmp(&b.name));
        consumed.sort_by(|a, b| a.name.cmp(&b.name));
        if expected.len() != consumed.len()
            || !expected
                .iter()
                .zip(&consumed)
                .all(|(a, b)| same_definition(a, b))
        {
            return Err(JobsError::SelectionConflict(
                "station tree definitions changed after selection preparation".into(),
            ));
        }
    }
    let mut active = BTreeMap::new();
    for workflow in workflows {
        if active
            .insert(workflow.kind.clone(), workflow.clone())
            .is_some()
        {
            return Err(JobsError::SelectionConflict(
                "duplicate active workflow definition".into(),
            ));
        }
    }
    if active != context.active_workflows {
        return Err(JobsError::SelectionConflict(
            "workflow definitions changed after claim validation".into(),
        ));
    }
    let mut authored = BTreeMap::new();
    for station in stations {
        if authored
            .insert(station.name.clone(), station.clone())
            .is_some()
        {
            return Err(JobsError::SelectionConflict(
                "duplicate active station definition".into(),
            ));
        }
    }
    if let Some(station) = authored.get(&context.station.name) {
        return if station.bind_self(Some(actor)).as_ref() == Some(&context.station) {
            Ok(())
        } else {
            Err(JobsError::SelectionConflict(
                "authored station observation changed after claim validation".into(),
            ))
        };
    }
    let names: Vec<_> = authored.keys().cloned().collect();
    for station in crate::station_projection::derived_stations(workflows, &names, at) {
        authored.insert(station.name.clone(), station);
    }
    let effective = authored
        .get(&context.station.name)
        .and_then(|station| station.bind_self(Some(actor)));
    if !effective
        .as_ref()
        .is_some_and(|station| same_definition(station, &context.station))
    {
        return Err(JobsError::SelectionConflict(
            "station definition changed after claim validation".into(),
        ));
    }
    Ok(())
}

/// Definition observations needed to route steps through a declared tree.
/// Pinned protocols locate work; active protocols resolve unprojected agent
/// blocks. They are different inputs and neither substitutes for the other.
pub(crate) fn validate_tree_protocols(
    context: &SelectionContext,
    consumed: &[WorkflowSpec],
) -> Result<(), JobsError> {
    let Some(tree) = &context.tree_selection else {
        return Ok(());
    };
    let mut expected: Vec<_> = tree.inputs.protocols.iter().collect();
    let mut consumed: Vec<_> = consumed.iter().collect();
    expected.sort_by(|a, b| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
    consumed.sort_by(|a, b| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
    if expected != consumed {
        return Err(JobsError::SelectionConflict(
            "pinned protocol inputs changed after tree selection preparation".into(),
        ));
    }
    Ok(())
}

fn validate_guarded_tree_protocols(
    context: &SelectionContext,
    guard: &dyn crate::registry::WorkflowSelectionRows,
) -> Result<(), JobsError> {
    let Some(tree) = &context.tree_selection else {
        return Ok(());
    };
    let consumed = tree
        .inputs
        .protocols
        .iter()
        .map(|row| {
            guard
                .version(&row.kind, row.version)
                .cloned()
                .ok_or_else(|| {
                    JobsError::SelectionConflict(format!(
                        "pinned protocol {}@v{} has no consumed writer owner",
                        row.kind, row.version,
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_tree_protocols(context, &consumed)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct TreePullInputs {
    pub stations: Vec<StationSpec>,
    pub protocols: Vec<WorkflowSpec>,
}

/// Every additional input the tree selector uses must be consumed again
/// under the same owners as the legacy selector, before any claim commits.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct TreeSelection {
    pub inputs: TreePullInputs,
    pub executor: PullExecutor,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct PullExecutor {
    pub ids: Vec<String>,
    pub roles: Vec<String>,
    pub model: Option<String>,
    pub model_class: Option<String>,
}

impl PullExecutor {
    pub(crate) fn from_user(
        user: &boss_policy_client::User,
        agent: Option<&crate::agents::AgentRow>,
    ) -> Self {
        let mut ids = vec![user.id.clone()];
        let mut roles = vec![user.role.clone()];
        if let Some(agent) = agent {
            ids.push(agent.id.clone());
            ids.extend(agent.aliases.iter().cloned());
            roles.extend(agent.role.iter().cloned());
        }
        ids.sort();
        ids.dedup();
        // The acting spelling is the first id consumed by judge_holder.
        ids.retain(|id| id != &user.id);
        ids.insert(0, user.id.clone());
        roles.sort();
        roles.dedup();
        Self {
            ids,
            roles,
            model: agent.map(|row| row.default_model.clone()),
            model_class: agent.and_then(|row| row.model_class.clone()),
        }
    }
    pub fn is(&self, holder: &str) -> bool {
        self.ids.iter().any(|id| id == holder)
    }

    /// The pull filter's authority and executor judgement. The claim door
    /// still consumes its own writer-fenced authority before committing.
    pub fn refusal(
        &self,
        metadata: &Value,
        assignee: Option<&str>,
        launch_model: &str,
        demand: &Result<Option<crate::step_executor::ExecutorDemand>, String>,
    ) -> Option<String> {
        let handed = assignee.is_some_and(|holder| self.is(holder));
        if let Some(role) = metadata
            .get(crate::audience::AUTHORITY_ROLE_KEY)
            .and_then(Value::as_str)
            .filter(|role| !role.is_empty())
            && !handed
            && !self.roles.iter().any(|held| held == role)
        {
            let held = if self.roles.is_empty() {
                "no role".into()
            } else {
                self.roles.join(", ")
            };
            return Some(format!(
                "refused: needs role {role}, and the executor holds {held}"
            ));
        }
        match demand {
            Err(why) => Some(format!(
                "refused: its pinned executor declaration admits nobody ({why})"
            )),
            Ok(Some(demand)) => {
                use crate::step_executor::{Capability, ExecutorDemand, judge_holder};
                let holder = self.ids.first().map(String::as_str).unwrap_or_default();
                let verdict = judge_holder(demand, holder).and_then(|admitted| {
                    let found = match demand {
                        ExecutorDemand::Role(role) => Capability::Role {
                            holds: self.roles.iter().any(|held| held == role),
                        },
                        ExecutorDemand::Model(_) => Capability::Model {
                            class: self.model_class.clone(),
                            known: self.model_class.is_some(),
                        },
                        ExecutorDemand::Equipment(_) => Capability::Equipment(None),
                    };
                    admitted.judge(&found)
                });
                verdict.err().map(|refused| {
                    format!("refused: needs {}, and {}", refused.demanded, refused.found)
                })
            }
            Ok(None) => self
                .model
                .as_ref()
                .filter(|model| model.as_str() != launch_model)
                .map(|model| {
                    format!("refused: runs on {launch_model}, and the executor runs {model}")
                }),
        }
    }

    fn takes(&self, job: &Job, step: &Step, protocol: &WorkflowSpec) -> bool {
        if step
            .assignee_id
            .as_deref()
            .is_some_and(|holder| !self.is(holder))
        {
            return false;
        }
        let Some(settings) = crate::agent_spec::projected(&step.metadata) else {
            return false;
        };
        if reviews_its_own_build(job, step, |id| self.is(id)).is_some() {
            return false;
        }
        self.refusal(
            &step.metadata,
            step.assignee_id.as_deref(),
            &settings.model,
            &crate::step_executor::demanded_by(protocol, step.spec_slug.as_deref()),
        )
        .is_none()
    }
}

/// PURE: the builder `job` records, when `step` asks for an independent
/// review (its agent block's `reviewer` profile) and `is` names that
/// builder — `None` for every other claim.
///
/// ONE PREDICATE, TWO DOORS (backlog 2b927025, design d32b58b7 step 3).
/// A review profile requires a judgement independent of the actor
/// recorded as the builder; this applies by the declared profile,
/// including a landed review, without naming a Workflow kind. The
/// station pull's filter ([`PullExecutor`]) skipped such a step, but the
/// claim door itself never asked, so a review reached its builder by
/// any claim that did not come through the pull: measured 2026-11-17,
/// about 20 reviews were nominated by hand to keep a car's builder off
/// its own review. The door now asks the same question of the holder,
/// so any seat may take a ready review except the one that built it.
///
/// `step` is read as resolved ([`crate::agent_spec::resolved`]): the
/// queue and the door agree on which steps are reviews.
pub fn reviews_its_own_build<'a>(
    job: &'a Job,
    step: &Step,
    is: impl Fn(&str) -> bool,
) -> Option<&'a str> {
    let reviewer = step
        .metadata
        .get(crate::agent_spec::PROFILE_KEY)
        .and_then(Value::as_str)
        == Some("reviewer");
    if !reviewer {
        return None;
    }
    job.metadata
        .get(crate::car::BUILT_BY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|builder| !builder.is_empty() && is(builder))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn head_in_tree(
    station: &StationSpec,
    scope: &JobScope,
    active: &BTreeMap<String, WorkflowSpec>,
    packets: &[(Job, Vec<Step>)],
    inputs: &TreePullInputs,
    executor: &PullExecutor,
) -> Result<Option<(Job, Step)>, JobsError> {
    competitors_in_tree(station, scope, active, packets, inputs, executor)
        .map(|competitors| competitors.into_iter().next())
}

/// Every packet that competed for the head of a tree pull, in queue
/// order, each with the step it would have claimed. A packet absent from
/// this list yielded no candidate, so it cannot change which one heads it.
fn competitors_in_tree(
    station: &StationSpec,
    scope: &JobScope,
    active: &BTreeMap<String, WorkflowSpec>,
    packets: &[(Job, Vec<Step>)],
    inputs: &TreePullInputs,
    executor: &PullExecutor,
) -> Result<Vec<(Job, Step)>, JobsError> {
    let id = station.id.as_deref().ok_or_else(|| {
        JobsError::SelectionConflict("tree selection requires a declared station id".into())
    })?;
    let inside = crate::station_tree::subtree(&inputs.stations, id)
        .map_err(JobsError::SelectionConflict)?
        .into_iter()
        .flat_map(|node| node.station.id.into_iter().chain([node.station.name]))
        .collect();
    let mut candidates = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (job, steps) in packets {
        if !seen.insert(job.id.to_string()) {
            return Err(JobsError::SelectionConflict(
                "duplicate selection packet".into(),
            ));
        }
        if job.status != JobStatus::Open || !scope.admits(job) {
            continue;
        }
        let protocol = inputs
            .protocols
            .iter()
            .find(|row| row.kind == job.kind && row.version == job.workflow_version)
            .ok_or_else(|| {
                JobsError::SelectionConflict(format!(
                    "pinned protocol {}@v{} unavailable for tree selection",
                    job.kind, job.workflow_version
                ))
            })?;
        let mut resolved = crate::agent_spec::resolved_steps(steps, active.get(&job.kind));
        resolved.sort_by_key(|step| (step.sort_order, step.id.to_string()));
        let mut step_ids = std::collections::BTreeSet::new();
        for step in &resolved {
            if !step_ids.insert(step.id.to_string()) {
                return Err(JobsError::SelectionConflict(
                    "duplicate selection step".into(),
                ));
            }
        }
        if let Some(selected) = resolved.iter().find(|step| {
            step.status == StepStatus::Ready
                && !crate::flow_resources::holds_its_place(&step.metadata)
                && executor.takes(job, step, protocol)
                && (station.predicate.matches(job, std::slice::from_ref(*step))
                    || crate::station_queue::addressed_here(
                        &inside,
                        protocol,
                        std::slice::from_ref(*step),
                    )
                    || crate::station_tree::resolve_claim_station(
                        &inputs.stations,
                        Some(protocol),
                        job,
                        step,
                    )
                    .is_some_and(|resolution| {
                        resolution
                            .station
                            .id
                            .as_ref()
                            .is_some_and(|id| inside.contains(id))
                    }))
        }) {
            let raw = steps
                .iter()
                .find(|step| step.id == selected.id)
                .ok_or_else(|| {
                    JobsError::SelectionConflict("selected step identity disappeared".into())
                })?;
            candidates.push((job.clone(), raw.clone()));
        }
    }
    Ok(in_queue_order(station, candidates))
}

/// The candidates ordered by the station's discipline — the one ordering
/// the queue reader uses, so the head is always the first of them.
fn in_queue_order(station: &StationSpec, candidates: Vec<(Job, Step)>) -> Vec<(Job, Step)> {
    let mut ordered: Vec<_> = candidates.iter().map(|(job, _)| job.clone()).collect();
    // The step each would claim is the one the queue reader judges a
    // returned repair by (backlog 205ce78a).
    let returned: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter(|(_, step)| crate::station_queue::returned_repair(step))
        .map(|(job, _)| job.id.to_string())
        .collect();
    crate::station_queue::order_returns_first(&station.discipline, &mut ordered, &returned);
    let mut by_id: BTreeMap<String, (Job, Step)> = candidates
        .into_iter()
        .map(|candidate| (candidate.0.id.to_string(), candidate))
        .collect();
    ordered
        .into_iter()
        .filter_map(|job| by_id.remove(&job.id.to_string()))
        .collect()
}

/// Complete, policy-scoped station membership, ordered by the same function
/// as the queue reader. An ACTIVE step is never a fresh acquisition.
pub fn head(
    station: &StationSpec,
    scope: &JobScope,
    active: &BTreeMap<String, WorkflowSpec>,
    packets: &[(Job, Vec<Step>)],
) -> Result<Option<(Job, Step)>, JobsError> {
    competitors(station, scope, active, packets).map(|competitors| competitors.into_iter().next())
}

/// Every packet that competed for the head of a station pull, in queue
/// order, each with the step it would have claimed.
fn competitors(
    station: &StationSpec,
    scope: &JobScope,
    active: &BTreeMap<String, WorkflowSpec>,
    packets: &[(Job, Vec<Step>)],
) -> Result<Vec<(Job, Step)>, JobsError> {
    let clause = station.predicate.step.as_ref();
    if clause.is_none() && station.id.is_none() {
        return Err(JobsError::SelectionConflict(
            "station declares neither a tree identity nor a step to claim".into(),
        ));
    }
    if station.version <= 0 || station.terminal_window_days.is_some() {
        return Err(JobsError::SelectionConflict(
            "selection requires an exact in-flight station version".into(),
        ));
    }
    let mut candidates = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for (job, steps) in packets {
        if !ids.insert(job.id.to_string()) {
            return Err(JobsError::SelectionConflict(
                "duplicate selection packet".into(),
            ));
        }
        if job.status != JobStatus::Open || !scope.admits(job) {
            continue;
        }
        let resolved = crate::agent_spec::resolved_steps(steps, active.get(&job.kind));
        if clause.is_some() && !station.predicate.matches(job, &resolved) {
            continue;
        }
        let mut matching: Vec<_> = resolved
            .iter()
            .filter(|step| {
                step.status == StepStatus::Ready
                    && match clause {
                        Some(clause) => clause.matches(step),
                        None => step
                            .metadata
                            .get(crate::station_tree::AT_KEY)
                            .and_then(Value::as_str)
                            .is_some_and(|address| station.id.as_deref() == Some(address)),
                    }
            })
            .collect();
        if matching.is_empty() {
            continue;
        }
        if clause.is_some() && matching.len() != 1 {
            return Err(JobsError::SelectionConflict(
                "station packet has an ambiguous claim step".into(),
            ));
        }
        matching.sort_by_key(|step| (step.sort_order, step.id.to_string()));
        // A step HOLDING ITS PLACE in an exhausted pool's line is not
        // offered again: claim-next took only the head, so the next caller
        // met the same refused step and was refused again (backlog
        // 587f395a, measured on 3a7be357). It stays queued at this
        // station, and heads it again once a release re-offers it
        // (`jobs.reoffer_waiting`).
        if crate::flow_resources::holds_its_place(&matching[0].metadata) {
            continue;
        }
        let raw: Vec<_> = steps
            .iter()
            .filter(|step| step.id == matching[0].id)
            .collect();
        if raw.len() != 1 {
            return Err(JobsError::SelectionConflict(
                "selected step identity is not unique in its packet".into(),
            ));
        }
        candidates.push((job.clone(), raw[0].clone()));
    }
    Ok(in_queue_order(station, candidates))
}

/// Derived stations receive a reader timestamp on every resolution. Compare
/// their operational definition rather than mistaking that timestamp for a
/// changed predicate. The observed timestamp remains in the receipt.
pub fn same_definition(left: &StationSpec, right: &StationSpec) -> bool {
    let mut normalized = right.clone();
    normalized.created_at = left.created_at;
    left == &normalized
}

/// What a receipt's `competitor_digest` records, carried in every receipt
/// beside it as `of`.
///
/// VERSIONED, because a receipt keeps the definition it was written
/// under, and the replay check (`require_read_scope`) reads only `count`
/// and `admitted_by`, so every stored shape still replays. Receipts from
/// a8e91ac5 until a710eec5 carry an unversioned text and a sha256 over
/// each competitor's content; receipts from a710eec5 until 16a9f940
/// carry "v2: …" and a sha256 over each competitor's stored row version
/// (PostgreSQL `xmin`). Neither sha256 was read by anything, and the v2
/// one could be recomputed by nobody: an `xmin` is in no event and moves
/// under a truncate-and-reinsert restore, so the same operations on two
/// clusters gave two digests. A field claiming recomputability it does
/// not have is a worse record than none, so new receipts carry no sha256
/// (backlog 16a9f940).
pub const COMPETITOR_DIGEST_OF: &str = "v3: count is how many packets competed for the head, head included; admitted_by is the read scope every one of them passed when it was read. No digest of their ids, revisions or order is kept: no stored revision can be recomputed from the log";

impl SelectionContext {
    pub fn revalidate(&self, packets: &[(Job, Vec<Step>)]) -> Result<(), JobsError> {
        self.revalidated_competitors(packets).map(|_| ())
    }

    /// Revalidate the head over the population the transaction read, and
    /// return the evidence a receipt keeps of it, as the keys to merge
    /// into its owner consumption: the head packet whole
    /// (`competing_head`), the count of every packet that COMPETED for
    /// it with the scope each one passed (`competitor_digest`), and the
    /// counts (`population_read`).
    ///
    /// The receipt copied every scope-visible open packet until 2026-11-18,
    /// competitor or not, and spent the whole five-second ingress budget
    /// serializing that copy at production's 1,501 open packets (backlog
    /// f999363f). It then copied every competitor whole, which failed the
    /// same way once 1,000+ READY steps competed: 1,101 competitors were
    /// refused in 5.051 s "during evidence serialization" (backlog
    /// a8e91ac5). The receipt now grows with neither: the head is whole,
    /// the rest are counted, and the scope every competitor passed is
    /// recorded beside the count so a replay's read-scope check stays
    /// sound without their bindings. No sha256 of the rest since
    /// 16a9f940: nothing read it and nobody could recompute it.
    pub fn revalidated_evidence(
        &self,
        packets: &[(Job, Vec<Step>)],
    ) -> Result<serde_json::Map<String, Value>, JobsError> {
        let competing = self.revalidated_competitors(packets)?;
        let head = packets
            .iter()
            .find(|(job, _)| job.id == self.validated_job.id)
            .ok_or_else(|| {
                JobsError::SelectionConflict("selected packet left the population read".into())
            })?;
        let evidence = serde_json::json!({
            "competing_head": head,
            "competitor_digest": {
                "count": competing.len(),
                "admitted_by": self.scope,
                "of": COMPETITOR_DIGEST_OF,
            },
            "population_read": {"open_packets": packets.len(), "competing": competing.len()},
        });
        match evidence {
            Value::Object(keys) => Ok(keys),
            _ => Err(JobsError::Storage(
                "selection evidence is not an object".into(),
            )),
        }
    }

    /// Every packet that competed for the head, in queue order, refused
    /// unless the validated claim still heads it.
    fn revalidated_competitors(
        &self,
        packets: &[(Job, Vec<Step>)],
    ) -> Result<Vec<(Job, Step)>, JobsError> {
        let competing = match &self.tree_selection {
            Some(tree) => competitors_in_tree(
                &self.station,
                &self.scope,
                &self.active_workflows,
                packets,
                &tree.inputs,
                &tree.executor,
            )?,
            None => competitors(&self.station, &self.scope, &self.active_workflows, packets)?,
        };
        if competing.first() != Some(&(self.validated_job.clone(), self.validated_step.clone())) {
            return Err(JobsError::SelectionConflict(
                "station head or validated claim shape changed; re-read before claiming".into(),
            ));
        }
        Ok(competing)
    }

    pub fn record_consumed(
        &self,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
        claimed: &Step,
        consumption: Value,
    ) -> Result<(SelectionReceipt, Vec<boss_core::event::Event>), JobsError> {
        self.record_attempt_consumed(actor, stamp, claimed, consumption, None)
    }

    pub(crate) fn record_attempt_consumed(
        &self,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
        step: &Step,
        consumption: Value,
        refusal: Option<&crate::flow_resources::Refusal>,
    ) -> Result<(SelectionReceipt, Vec<boss_core::event::Event>), JobsError> {
        let deadline = self
            .native_authority
            .as_ref()
            .map(|observation| observation.ingress.deadline());
        let (original, mut event) = self.record(actor, stamp, step)?;
        let mut value = original.value().clone();
        if let Some(refusal) = refusal {
            event.kind = crate::events::STATION_RESOURCE_REFUSED.into();
            let object = value
                .as_object_mut()
                .ok_or_else(|| JobsError::Storage("selection receipt is not an object".into()))?;
            object.remove("claimed_step");
            object.insert("refused_step".into(), serde_json::json!(step));
            object.insert("outcome".into(), serde_json::json!("resource-refused"));
            object.insert("refusal".into(), serde_json::json!(refusal));
        }
        value["owner_consumption_hash"] = serde_json::json!(
            crate::selection_serialization::canonical_hash(&consumption, deadline)?
        );
        value["owner_consumption"] = consumption;
        let receipt = SelectionReceipt::from_value_before(value, deadline)?;
        if self.native_authority.is_some() {
            receipt.require_read_scope(&self.scope)?;
        }
        event.payload["receipt_json"] = serde_json::json!(receipt.json());
        let events = crate::selection_evidence::conserve_before(&receipt, event, stamp, deadline)?;
        Ok((receipt, events))
    }

    pub fn record(
        &self,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
        claimed: &Step,
    ) -> Result<(SelectionReceipt, boss_core::event::Event), JobsError> {
        let mut event = stamp.event(crate::events::STATION_CLAIMED, serde_json::json!({}));
        use sha2::Digest;
        let station = serde_json::json!(self.station);
        let station_hash = hex::encode(sha2::Sha256::digest(boss_core::job::canonical_json_bytes(
            &station,
        )));
        let mut receipt = serde_json::json!({
            "version":2,
            "event_id":event.id,
            "event_actor":event.payload.get("_actor"),
            "request_id":self.request_id,
            "actor":actor,
            "station":station,
            "station_hash":station_hash,
            "selected":{"job_id":self.validated_job.id,"workflow_kind":self.validated_job.kind,
                "workflow_version":self.validated_job.workflow_version,"step_id":claimed.id},
            "validated":{"job":self.validated_job,"step":self.validated_step,
                "workflows":self.active_workflows,"scope":self.scope,
                "actor_observation":self.actor_observation.as_ref().map(|observation| serde_json::json!({
                    "holder":observation.holder,"row":observation.row,
                    "requested_user":observation.requested_user,
                    "method":"actual agent and alias owner writer lock",
                }))},
            "claimed_step":claimed,
            "selected_at":stamp.timestamp,
        });
        if let Some(tree) = &self.tree_selection {
            let protocol = tree.inputs.protocols.iter().find(|row| {
                row.kind == self.validated_job.kind
                    && row.version == self.validated_job.workflow_version
            });
            let resolved = crate::agent_spec::resolved(
                &self.validated_step,
                self.active_workflows.get(&self.validated_job.kind),
            );
            let resolution = crate::station_tree::resolve_claim_station(
                &tree.inputs.stations,
                protocol,
                &self.validated_job,
                &resolved,
            )
            .ok_or_else(|| {
                JobsError::SelectionConflict(
                    "selected tree step has no resolvable claim station".into(),
                )
            })?;
            receipt["claim_station"] = serde_json::json!({
                "id":resolution.station.id,
                "name":resolution.station.name,
            });
            receipt["station_resolution"] = serde_json::json!(resolution.method);
            if let Some(reason) = resolution.reason {
                receipt["station_resolution_reason"] = serde_json::json!(reason);
            }
        }
        let deadline = self
            .native_authority
            .as_ref()
            .map(|observation| observation.ingress.deadline());
        let receipt = SelectionReceipt::from_value_before(receipt, deadline)?;
        event.payload["receipt_json"] = serde_json::json!(receipt.json());
        Ok((receipt, event))
    }
}

pub fn replay(
    receipt: &Value,
    actor: &str,
    station: &str,
    request_id: uuid::Uuid,
) -> Result<(), JobsError> {
    if receipt.get("actor").and_then(Value::as_str) != Some(actor)
        || receipt.pointer("/station/name").and_then(Value::as_str) != Some(station)
        || receipt.get("request_id").and_then(Value::as_str)
            != Some(request_id.to_string().as_str())
    {
        return Err(JobsError::SelectionConflict(
            "selection request identity was reused with a different actor or station".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tree_pull_tests {
    use super::*;

    fn fixture() -> (StationSpec, Job, Step) {
        let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
        let mut station = StationSpec::draft(
            "company",
            "Company",
            crate::StationKind::Group,
            crate::station_queue::StationPredicate {
                tags_any: vec!["station:company".into()],
                ..Default::default()
            },
            now,
        );
        station.status = crate::registry::WorkflowStatus::Active;
        station.version = 1;
        station.id = Some(uuid::Uuid::new_v4().to_string());
        let mut job = Job::new(
            "fixture",
            boss_core::job::Subject::new("custom", "fixture"),
            "A tree packet",
            "emp-owner",
            boss_core::job::Priority::Standard,
            now.date_naive(),
        );
        job.status = JobStatus::Open;
        let mut step = Step::new(job.id, "task", "Work", 0);
        step.spec_slug = Some("work".into());
        step.status = StepStatus::Ready;
        step.metadata[crate::station_tree::AT_KEY] = serde_json::json!(station.id);
        (station, job, step)
    }

    #[test]
    fn a_tree_root_pulls_an_addressed_step_without_a_predicate_step_clause() {
        let (station, job, step) = fixture();
        let selected = head(
            &station,
            &JobScope::All,
            &BTreeMap::new(),
            &[(job, vec![step.clone()])],
        );
        assert_eq!(
            selected
                .as_ref()
                .ok()
                .and_then(|row| row.as_ref())
                .map(|(_, step)| step.id),
            Some(step.id),
            "a declared tree root has tag predicates and step addresses, not a projected predicate.step: {selected:?}",
        );
    }

    /// The pull agrees with the queue (backlog 205ce78a): a build a
    /// CHANGES verdict returned to its station is the head ahead of an
    /// OLDER fresh build of the same priority, and an urgent fresh build
    /// is still the head ahead of it.
    #[test]
    fn a_pull_takes_a_returned_repair_before_older_fresh_work_of_its_priority() {
        let (station, fresh, fresh_step) = fixture();
        let packet = |priority, opened: chrono::NaiveDate, note: bool| {
            let mut job = fresh.clone();
            job.id = boss_core::job::JobId::new();
            job.priority = priority;
            job.opened_on = opened;
            let mut step = fresh_step.clone();
            step.id = boss_core::job::StepId::new();
            step.job_id = job.id;
            if note {
                step.metadata[crate::station_queue::RETURNED_FOR_CHANGES] =
                    serde_json::json!({"review": "5eed5eed"});
            }
            (job, vec![step])
        };
        let later = fresh.opened_on + chrono::Days::new(3);
        let returned = packet(boss_core::job::Priority::Standard, later, true);
        let pulled = |packets: &[(Job, Vec<Step>)]| {
            head(&station, &JobScope::All, &BTreeMap::new(), packets)
                .unwrap()
                .map(|(job, _)| job.id)
        };
        let both = [(fresh.clone(), vec![fresh_step.clone()]), returned.clone()];
        assert_eq!(pulled(&both), Some(returned.0.id), "the repair first");
        let urgent = packet(boss_core::job::Priority::Urgent, later, false);
        let all = [both[0].clone(), returned, urgent.clone()];
        assert_eq!(pulled(&all), Some(urgent.0.id), "priority still first");
    }

    #[test]
    fn a_tree_packet_with_several_ready_steps_pulls_the_first_step_in_order() {
        let (station, job, mut first) = fixture();
        first.sort_order = 1;
        let mut second = Step::new(job.id, "task", "Second", 2);
        second.status = StepStatus::Ready;
        second.metadata = first.metadata.clone();
        let selected = head(
            &station,
            &JobScope::All,
            &BTreeMap::new(),
            &[(job, vec![second, first.clone()])],
        )
        .expect("a tree station routes steps, not an ambiguous projected clause");
        assert_eq!(selected.map(|(_, step)| step.id), Some(first.id));
    }

    fn tree_fixture() -> (StationSpec, Job, Step, TreePullInputs, PullExecutor) {
        let (root, job, mut step) = fixture();
        step.metadata = serde_json::json!({
            "agent_profile":"builder", "agent_model":"launch-model",
            "agent_budget_usd":5.0, "agent_effort":"high",
        });
        let mut child = root.clone();
        child.id = Some(uuid::Uuid::new_v4().to_string());
        child.name = "engineering".into();
        child.parent = root.id.clone();
        let protocol = crate::registry::WorkflowSpec::platform_seed(
            "fixture",
            "Fixture",
            "test",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "work".into(),
                title_template: "Work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                executor: Some("model:sol".into()),
                metadata_defaults: serde_json::json!({"at":child.id}),
                ..Default::default()
            }],
        );
        let inputs = TreePullInputs {
            stations: vec![root.clone(), child],
            protocols: vec![protocol],
        };
        let executor = PullExecutor {
            ids: vec!["agent-test".into()],
            roles: vec!["engineering-agent".into()],
            model: Some("actual-sol-model".into()),
            model_class: Some("sol".into()),
        };
        (root, job, step, inputs, executor)
    }

    #[test]
    fn a_root_pulls_a_descendant_address_from_the_pinned_protocol() {
        let (root, job, step, inputs, executor) = tree_fixture();
        let selected = head_in_tree(
            &root,
            &JobScope::All,
            &BTreeMap::new(),
            &[(job, vec![step.clone()])],
            &inputs,
            &executor,
        )
        .expect("declared tree and pinned address are readable");
        assert_eq!(selected.map(|(_, step)| step.id), Some(step.id));
    }

    #[test]
    fn a_robot_can_pull_unaddressed_legacy_work_only_from_the_root() {
        let (root, job, step, mut inputs, executor) = tree_fixture();
        inputs.protocols[0].steps[0].metadata_defaults = serde_json::json!({});
        inputs.protocols[0].metadata = serde_json::json!({});
        for (station, expected) in [(&root, Some(step.id)), (&inputs.stations[1], None)] {
            let picked = head_in_tree(
                station,
                &JobScope::All,
                &BTreeMap::new(),
                &[(job.clone(), vec![step.clone()])],
                &inputs,
                &executor,
            )
            .expect("root fallback uses the declared tree");
            assert_eq!(picked.map(|(_, step)| step.id), expected);
        }
        inputs.protocols[0].steps[0].metadata_defaults = serde_json::json!({"at":"missing"});
        assert!(
            head_in_tree(
                &root,
                &JobScope::All,
                &BTreeMap::new(),
                &[(job, vec![step])],
                &inputs,
                &executor,
            )
            .unwrap()
            .is_none(),
            "an invalid explicit address never falls back"
        );
    }

    #[test]
    fn tree_selection_skips_steps_the_executor_cannot_dispatch() {
        let mut wrongly_selected = Vec::new();
        for reason in [
            "another-holder",
            "role",
            "model-class",
            "no-agent-block",
            "legacy-model",
            "own-review",
        ] {
            let (root, mut job, mut step, mut inputs, mut executor) = tree_fixture();
            match reason {
                "another-holder" => step.assignee_id = Some("agent-other".into()),
                "role" => step.metadata["authority_role"] = serde_json::json!("another-role"),
                "model-class" => executor.model_class = Some("another-class".into()),
                "no-agent-block" => step.metadata = serde_json::json!({}),
                "legacy-model" => inputs.protocols[0].steps[0].executor = None,
                "own-review" => {
                    job.kind = "ship-a-change".into();
                    job.metadata["built_by"] = serde_json::json!(executor.ids[0]);
                    inputs.protocols[0].kind = job.kind.clone();
                    inputs.protocols[0].steps[0].title = "review".into();
                    step.spec_slug = Some("review".into());
                    step.metadata["agent_profile"] = serde_json::json!("reviewer");
                }
                _ => unreachable!(),
            }
            let selected = head_in_tree(
                &root,
                &JobScope::All,
                &BTreeMap::new(),
                &[(job, vec![step])],
                &inputs,
                &executor,
            )
            .expect("ineligible work is not an unreadable queue");
            if selected.is_some() {
                wrongly_selected.push(reason);
            }
        }
        assert!(
            wrongly_selected.is_empty(),
            "tree pull selected ineligible work: {wrongly_selected:?}"
        );
    }
}

#[cfg(test)]
mod counter_scope_tests {
    use super::*;
    fn receipt(path: &str, owner: Option<&str>) -> SelectionReceipt {
        let job = serde_json::json!({"owner_id":"claimant","subject":{"subject_kind":"custom","id":"selected"}});
        let mut row = serde_json::json!({"id":"private-run","kind":"agent-run","status":"open","agent":"claimant"});
        if let Some(owner) = owner {
            row["owner_id"] = serde_json::json!(owner);
            row["subject"] = serde_json::json!({"subject_kind":"custom","id":"counted"});
        }
        let mut copied = serde_json::json!({"version":1,"open_population":[[job.clone(),[]]]});
        copied[path] = if path == "run_population" {
            serde_json::json!({"rows":[row]})
        } else {
            serde_json::json!({"runs":[row]})
        };
        let hash = crate::selection_serialization::canonical_hash(&copied, None).unwrap();
        SelectionReceipt::from_value(serde_json::json!({"version":2,"validated":{"job":job},
            "owner_consumption":copied,"owner_consumption_hash":hash}))
        .unwrap()
    }
    /// The bounded evidence is held to the reader's scope exactly as the
    /// full copy was: a replay may not show a competitor the reader cannot
    /// read (f999363f).
    #[test]
    fn a_competing_packet_outside_current_read_scope_refuses_the_replay() {
        let selected = serde_json::json!({"owner_id":"claimant","subject":{"subject_kind":"custom","id":"selected"}});
        let rival = serde_json::json!({"owner_id":"private-owner","subject":{"subject_kind":"custom","id":"rival"}});
        let copied = serde_json::json!({"version":1,
            "competing_population":[[selected.clone(),[]],[rival,[]]],
            "population_read":{"open_packets":9,"competing":2}});
        let hash = crate::selection_serialization::canonical_hash(&copied, None).unwrap();
        let original = SelectionReceipt::from_value(serde_json::json!({"version":2,
            "validated":{"job":selected},"owner_consumption":copied,"owner_consumption_hash":hash}))
        .unwrap();
        assert!(original.require_read_scope(&JobScope::All).is_ok());
        assert!(
            original
                .require_read_scope(&JobScope::OwnerIs("claimant".into()))
                .is_err(),
            "a competitor outside the reader's scope must refuse the replay"
        );
    }

    /// A receipt from a8e91ac5 on keeps the head packet whole and counts
    /// the rest; every counted competitor passed `admitted_by` when it
    /// was read. A replay is admitted only by a scope that admits every
    /// binding `admitted_by` admits — or, when the head was the only
    /// competitor, by the head's own copied binding. Shaped as a receipt
    /// is written since 16a9f940, with no sha256; the stored shapes that
    /// carry one are pinned below.
    fn digested(count: u64, admitted_by: JobScope) -> SelectionReceipt {
        let selected = serde_json::json!({"owner_id":"claimant","subject":{"subject_kind":"custom","id":"selected"}});
        let copied = serde_json::json!({"version":1,
            "competing_head":[selected.clone(),[]],
            "competitor_digest":{"count":count,"admitted_by":admitted_by,"of":COMPETITOR_DIGEST_OF},
            "population_read":{"open_packets":9,"competing":count}});
        let hash = crate::selection_serialization::canonical_hash(&copied, None).unwrap();
        SelectionReceipt::from_value(serde_json::json!({"version":2,
            "validated":{"job":selected},"owner_consumption":copied,"owner_consumption_hash":hash}))
        .unwrap()
    }

    #[test]
    fn a_digested_competitor_set_replays_only_inside_a_scope_covering_its_admission() {
        let claimant = || JobScope::OwnerIs("claimant".into());
        let wider = JobScope::OwnerIn(vec!["claimant".into(), "peer".into()]);
        for (count, admitted_by, current, admitted) in [
            (1101, JobScope::All, JobScope::All, true),
            // Competitors admitted by All may belong to anyone: a narrower
            // reader cannot be shown they competed.
            (1101, JobScope::All, claimant(), false),
            (1101, claimant(), claimant(), true),
            (1101, claimant(), wider.clone(), true),
            (1101, wider.clone(), claimant(), false),
            (
                1101,
                JobScope::AccountIn(vec!["a".into()]),
                claimant(),
                false,
            ),
            // Only the head competed, and its own binding is checked.
            (1, JobScope::All, claimant(), true),
            (1, JobScope::All, JobScope::OwnerIs("other".into()), false),
        ] {
            let receipt = digested(count, admitted_by.clone());
            assert_eq!(
                receipt.require_read_scope(&current).is_ok(),
                admitted,
                "{count} competitors admitted by {admitted_by:?}, replayed under {current:?}"
            );
        }
    }

    /// The unversioned content-hash definition, carried by receipts
    /// written between a8e91ac5 and a710eec5.
    const CONTENT_HASH_DIGEST_OF: &str = "sha256 of the canonical JSON of [[job id, step id, revision], ...] in queue order, head first; a revision is the sha256 of the canonical JSON of [job, claimed step] exactly as the selection read them";
    /// The v2 row-version definition, carried by receipts written between
    /// a710eec5 and 16a9f940.
    const ROW_VERSION_DIGEST_OF: &str = "v2: sha256 of the canonical JSON of [[job id, step id, job row version, step row version], ...] in queue order, head first; a row version is the store's own write version of that row, read in the same statement as the row (PostgreSQL: xmin; in-memory: a write counter), moved by every write and by no read";

    /// A stored receipt that carries a `competitor_digest.sha256` under
    /// the definition `of`: it validates against its own hash, replays
    /// under the read-scope rule that reads only `count` and
    /// `admitted_by`, and is served as stored, never rewritten.
    fn a_stored_sha256_digest_receipt_still_verifies(of: &str) {
        let selected = serde_json::json!({"owner_id":"claimant","subject":{"subject_kind":"custom","id":"selected"}});
        let copied = serde_json::json!({"version":1,
            "competing_head":[selected.clone(),[]],
            "competitor_digest":{"count":1101,"sha256":"1".repeat(64),"admitted_by":JobScope::OwnerIs("claimant".into()),"of":of},
            "population_read":{"open_packets":1101,"competing":1101}});
        let hash = crate::selection_serialization::canonical_hash(&copied, None).unwrap();
        let exact = serde_json::to_string(&serde_json::json!({"version":2,
            "validated":{"job":selected},"owner_consumption":copied,"owner_consumption_hash":hash}))
        .unwrap();
        let original = SelectionReceipt::from_json(exact.clone()).unwrap();
        assert!(
            original
                .require_read_scope(&JobScope::OwnerIs("claimant".into()))
                .is_ok(),
            "{of}"
        );
        assert!(
            original
                .require_read_scope(&JobScope::OwnerIs("other".into()))
                .is_err(),
            "{of}"
        );
        assert_eq!(original.json(), exact, "served as stored, never rewritten");
    }

    /// A receipt keeps the definition it was written under. One written
    /// between a8e91ac5 and a710eec5 carries the unversioned content-hash
    /// text; it still validates against its own hash and replays under
    /// the same read-scope rule.
    #[test]
    fn a_historical_content_hash_digest_receipt_still_verifies() {
        a_stored_sha256_digest_receipt_still_verifies(CONTENT_HASH_DIGEST_OF);
    }

    /// One written between a710eec5 and 16a9f940 carries a v2 sha256 over
    /// PostgreSQL `xmin` values that no one can recompute; retiring the
    /// field from new receipts leaves it readable and replayable as stored.
    #[test]
    fn a_stored_v2_row_version_digest_receipt_still_verifies() {
        a_stored_sha256_digest_receipt_still_verifies(ROW_VERSION_DIGEST_OF);
    }

    /// The current definition is told apart from every stored one by its
    /// version, and names no digest: since 16a9f940 a receipt counts its
    /// competitors and records their admission, and hashes nothing.
    #[test]
    fn the_current_competitor_definition_is_v3_and_names_no_digest() {
        assert!(COMPETITOR_DIGEST_OF.starts_with("v3: "));
        for stored in [CONTENT_HASH_DIGEST_OF, ROW_VERSION_DIGEST_OF] {
            assert_ne!(COMPETITOR_DIGEST_OF, stored);
        }
        assert!(
            !COMPETITOR_DIGEST_OF.contains("sha256"),
            "{COMPETITOR_DIGEST_OF}"
        );
    }

    #[test]
    fn original_counter_rows_require_current_read_scope_in_both_native_shapes() {
        for path in ["run_population", "concurrency"] {
            let original = receipt(path, Some("private-owner"));
            assert!(original.require_read_scope(&JobScope::All).is_ok());
            assert!(
                original
                    .require_read_scope(&JobScope::OwnerIs("claimant".into()))
                    .is_err()
            );
            assert!(original.require_read_scope(&JobScope::All).is_ok());
            let own = receipt(path, Some("claimant"));
            assert!(
                own.require_read_scope(&JobScope::OwnerIs("claimant".into()))
                    .is_ok()
            );
        }
    }
    #[test]
    fn historical_unbound_counter_rows_are_not_reinterpreted_as_current_private_jobs() {
        for path in ["run_population", "concurrency"] {
            let original = receipt(path, None);
            let exact = original.json().to_owned();
            assert!(original.require_read_scope(&JobScope::All).is_ok());
            assert!(
                original
                    .require_read_scope(&JobScope::OwnerIs("claimant".into()))
                    .is_err()
            );
            assert_eq!(original.json(), exact);
        }
    }
}
