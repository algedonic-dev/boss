//! Actual in-process owner composition, not a cached authorization response.

use std::sync::Arc;

use crate::agents::AgentsRegistry;
use crate::port::{JobScope, JobsError};
use crate::station_claim::{AuthenticatedSelectionIngress, NativeAuthorityObservation};

#[derive(Clone)]
pub struct NativeMemorySelectionStore {
    pub(crate) calendar: Option<Arc<boss_calendar::InMemoryCalendar>>,
    pub(crate) workflows: Arc<dyn crate::WorkflowRegistry>,
    pub(crate) stations: Arc<dyn crate::StationRegistry>,
    pub(crate) agents: Arc<dyn AgentsRegistry>,
    pub(crate) policy: Arc<dyn boss_policy_client::PolicyClient>,
}

impl NativeMemorySelectionStore {
    pub fn new(
        workflows: Arc<dyn crate::WorkflowRegistry>,
        stations: Arc<dyn crate::StationRegistry>,
        agents: Arc<dyn AgentsRegistry>,
        policy: Arc<dyn boss_policy_client::PolicyClient>,
    ) -> Self {
        Self {
            calendar: None,
            workflows,
            stations,
            agents,
            policy,
        }
    }

    pub fn with_calendar(mut self, calendar: Arc<boss_calendar::InMemoryCalendar>) -> Self {
        self.calendar = Some(calendar);
        self
    }

    /// The callback is an internal read under held actual actor/policy owners.
    /// None means a writer is busy; no guard escapes into an asynchronous wait.
    pub(crate) fn try_read<T>(
        &self,
        ingress: &AuthenticatedSelectionIngress,
        read: impl FnOnce(boss_policy_client::User, JobScope) -> Result<Option<T>, JobsError>,
    ) -> Result<Option<T>, JobsError> {
        let Some(agents) = self
            .agents
            .try_lock_selection_rows()
            .map_err(|error| JobsError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        let Some(policy) = self
            .policy
            .try_lock_selection_rows()
            .map_err(|error| JobsError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        let (rows, automations) = crate::station_claim::actor_inputs(
            agents.rows().to_vec(),
            agents.automations().to_vec(),
            ingress.actor_id(),
        );
        let (user, has_role) = match crate::station_claim::native_user(ingress, &rows, &automations)
        {
            Ok(user) => (user, true),
            Err(_) => {
                let mut user = boss_policy_client::User::anonymous();
                user.id = ingress.actor_id().to_owned();
                (user, false)
            }
        };
        let rules: Vec<_> = policy
            .rules()
            .iter()
            .filter(|rule| has_role && rule.role == user.role)
            .cloned()
            .collect();
        let scope = read_scope(&user, &rules, policy.overrides())?;
        let result = read(user.clone(), scope.clone())?;
        // Expiry is a live owner input even when a packet read took time.
        if read_scope(&user, &rules, policy.overrides())? != scope {
            return Err(JobsError::SelectionConflict(
                "native read authority expired during receipt lookup".into(),
            ));
        }
        Ok(result)
    }

    pub(crate) fn try_observe(
        &self,
        ingress: &AuthenticatedSelectionIngress,
        request_id: uuid::Uuid,
        station: &str,
        identity: &Arc<()>,
    ) -> Result<Option<NativeAuthorityObservation>, JobsError> {
        let Some(agents) = self
            .agents
            .try_lock_selection_rows()
            .map_err(|error| JobsError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        let Some(policy) = self
            .policy
            .try_lock_selection_rows()
            .map_err(|error| JobsError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        let (rows, automations) = crate::station_claim::actor_inputs(
            agents.rows().to_vec(),
            agents.automations().to_vec(),
            ingress.actor_id(),
        );
        let user = crate::station_claim::native_user(ingress, &rows, &automations)?;
        let rules: Vec<_> = policy
            .rules()
            .iter()
            .filter(|rule| rule.role == user.role)
            .cloned()
            .collect();
        let overrides: Vec<_> = policy
            .overrides()
            .iter()
            .filter(|row| row.user_id == user.id)
            .cloned()
            .collect();
        let scope = read_scope(&user, &rules, &overrides)?;
        let observed_at = boss_policy_client::engine::expiry_now();
        let control = boss_policy_client::controls::UPDATE_STEP;
        let resource = control.resource();
        let action = control.action();
        let id = boss_policy_client::types::rule_id(&user.role, &resource, action);
        let rule = rules.iter().find(|rule| rule.id == id);
        let (decision, _) = boss_policy_client::engine::evaluate_snapshot(
            &user,
            action,
            &resource,
            &overrides,
            rule,
            observed_at,
        );
        if !matches!(decision, boss_policy_client::Decision::Allow { .. }) {
            return Err(JobsError::SelectionConflict(
                "native step update authority refused".into(),
            ));
        }
        Ok(Some(NativeAuthorityObservation {
            store_identity: identity.clone(),
            request_id,
            station_name: station.to_owned(),
            ingress: ingress.clone(),
            user,
            agents: rows,
            automations,
            rules,
            overrides,
            observed_at,
            scope,
            budget_hour: None,
        }))
    }
}

fn read_scope(
    user: &boss_policy_client::User,
    rules: &[boss_policy_client::PolicyRule],
    overrides: &[boss_policy_client::UserOverride],
) -> Result<JobScope, JobsError> {
    let control = boss_policy_client::controls::READ_JOB;
    let resource = control.resource();
    let action = control.action();
    let id = boss_policy_client::types::rule_id(&user.role, &resource, action);
    let rule = rules.iter().find(|rule| rule.id == id);
    let (decision, _) = boss_policy_client::engine::evaluate_snapshot(
        user,
        action,
        &resource,
        overrides,
        rule,
        boss_policy_client::engine::expiry_now(),
    );
    match decision {
        boss_policy_client::Decision::Allow { scope } => Ok(JobScope::from_predicate(
            user,
            &boss_policy_client::predicates::scope_to_predicate(&scope, user),
        )),
        boss_policy_client::Decision::Deny { .. } => Err(JobsError::SelectionConflict(
            "native job read authority refused".into(),
        )),
    }
}
