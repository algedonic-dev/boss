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
        }
    }
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }
    pub fn started(&self) -> std::time::Instant {
        self.mode.started()
    }
    /// Cancel only read-only HTTP preparation, never mutation or COMMIT.
    pub(crate) async fn prepare<T>(
        &self,
        read: impl std::future::Future<Output = T>,
    ) -> Result<T, JobsError> {
        let deadline = tokio::time::Instant::from_std(self.started()) + SELECTION_PRE_COMMIT_BUDGET;
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
            "machine_mode": self.mode.mode().name(), "pre_dispatch_budget_ms": SELECTION_PRE_COMMIT_BUDGET.as_millis(),
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
        deadline: Option<std::time::Instant>,
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
        match self.pointer("/owner_consumption/open_population") {
            Some(Value::Array(rows)) => {
                for row in rows {
                    check(row.get(0).ok_or_else(|| {
                        JobsError::Storage("copied population row lacks job".into())
                    })?)?;
                }
            }
            _ if self["version"] == 1 => {} // Historical inline artifacts may lack this later observation.
            _ => {
                return Err(JobsError::Storage(
                    "v2 selection lacks complete population evidence".into(),
                ));
            }
        }
        // Counter inputs are copied outside the candidate's scoped population.
        // Older originals lack their bindings: only All can read them without
        // inventing those historical bindings from today's mutable job rows.
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
    deadline: Option<std::time::Instant>,
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
        Ok(serde_json::json!({
            "version": 1,
            "method": "native owner writer guards held through repository commit",
            "actor": {"holder": self.actor_observation.holder, "row": actor, "automation": crate::agents::automations::row_of_record(self._actor.automations(), &self.actor_observation.holder), "requested_user": self.actor_observation.requested_user},
            "definitions": {"workflows": self._workflow.rows(), "stations": self._station.rows()},
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
        let observation = context.actor_observation.as_ref().ok_or_else(|| {
            JobsError::SelectionConflict("selection actor owner witness unavailable".into())
        })?;
        let actor = observation.lock_verified()?;
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

/// Complete, policy-scoped station membership, ordered by the same function
/// as the queue reader. An ACTIVE step is never a fresh acquisition.
pub fn head(
    station: &StationSpec,
    scope: &JobScope,
    active: &BTreeMap<String, WorkflowSpec>,
    packets: &[(Job, Vec<Step>)],
) -> Result<Option<(Job, Step)>, JobsError> {
    let clause =
        station.predicate.step.as_ref().ok_or_else(|| {
            JobsError::SelectionConflict("station declares no step to claim".into())
        })?;
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
        if !station.predicate.matches(job, &resolved) {
            continue;
        }
        let matching: Vec<_> = resolved
            .iter()
            .filter(|step| step.status == StepStatus::Ready && clause.matches(step))
            .collect();
        if matching.is_empty() {
            continue;
        }
        if matching.len() != 1 {
            return Err(JobsError::SelectionConflict(
                "station packet has an ambiguous claim step".into(),
            ));
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
    let mut ordered: Vec<_> = candidates.iter().map(|(job, _)| job.clone()).collect();
    crate::station_queue::order_by_discipline(&station.discipline, &mut ordered);
    Ok(ordered.first().and_then(|job| {
        candidates
            .into_iter()
            .find(|(candidate, _)| candidate.id == job.id)
    }))
}

/// Derived stations receive a reader timestamp on every resolution. Compare
/// their operational definition rather than mistaking that timestamp for a
/// changed predicate. The observed timestamp remains in the receipt.
pub fn same_definition(left: &StationSpec, right: &StationSpec) -> bool {
    let mut normalized = right.clone();
    normalized.created_at = left.created_at;
    left == &normalized
}

impl SelectionContext {
    pub fn revalidate(&self, packets: &[(Job, Vec<Step>)]) -> Result<(), JobsError> {
        let current = head(&self.station, &self.scope, &self.active_workflows, packets)?;
        if current.as_ref() != Some(&(self.validated_job.clone(), self.validated_step.clone())) {
            return Err(JobsError::SelectionConflict(
                "station head or validated claim shape changed; re-read before claiming".into(),
            ));
        }
        Ok(())
    }

    pub fn record_consumed(
        &self,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
        claimed: &Step,
        consumption: Value,
    ) -> Result<(SelectionReceipt, Vec<boss_core::event::Event>), JobsError> {
        let deadline = self
            .native_authority
            .as_ref()
            .map(|observation| observation.ingress.started() + SELECTION_PRE_COMMIT_BUDGET);
        let (original, mut event) = self.record(actor, stamp, claimed)?;
        let mut value = original.value().clone();
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
        let receipt = serde_json::json!({
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
        let deadline = self
            .native_authority
            .as_ref()
            .map(|observation| observation.ingress.started() + SELECTION_PRE_COMMIT_BUDGET);
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
