//! Report-only role comparison through explicit registry, policy and
//! telemetry dependencies (ddf0773e, approved design abf9eeae, car 2).
//! The asserted decision is returned unchanged. A registry error or an
//! absent role is an unknown comparison, never a made-up registry role.
//! Enforcing the record, granting roles and deleting headers are later cars.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use boss_core::role_of_record::RoleOfRecord;
use serde::Serialize;

use crate::{Action, Decision, PolicyClient, PolicyClientError, Predicate, Resource, User};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportMode {
    Off,
    Report,
}

pub trait ReportModeSource: Send + Sync {
    fn mode(&self) -> ReportMode;
}

impl ReportModeSource for ReportMode {
    fn mode(&self) -> ReportMode {
        *self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleObservation {
    pub actor: String,
    pub asserted_role: String,
    pub recorded_actor: Option<String>,
    pub recorded_role: Option<String>,
    pub action: String,
    pub resource: String,
    pub lookup_status: String,
    pub asserted_allowed: Option<bool>,
    /// A registered role's actual policy answer, absent when unjudgeable.
    pub recorded_allowed: Option<bool>,
    /// True only when a currently admitted operation would be denied.
    pub would_deny: Option<bool>,
    pub would_change_scope: Option<bool>,
}

pub trait RoleReportSink: Send + Sync {
    fn record(&self, observation: RoleObservation);
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportRow {
    pub observation: RoleObservation,
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReportSnapshot {
    pub rows: Vec<ReportRow>,
    pub overflow: u64,
    /// This process-local tally cannot certify a durable 72-hour window.
    pub durable_window: bool,
}

#[derive(Default)]
struct Counts {
    rows: BTreeMap<String, ReportRow>,
    overflow: u64,
}

/// Bounded telemetry, owned by the service and explicitly injected into
/// the comparison. It emits no audit event and resets on process restart.
pub struct ReportTally {
    limit: usize,
    counts: Mutex<Counts>,
}

impl ReportTally {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            counts: Mutex::new(Counts::default()),
        }
    }

    pub fn snapshot(&self) -> ReportSnapshot {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        ReportSnapshot {
            rows: counts.rows.values().cloned().collect(),
            overflow: counts.overflow,
            durable_window: false,
        }
    }
}

impl RoleReportSink for ReportTally {
    fn record(&self, observation: RoleObservation) {
        // The row cap also needs a byte cap: a caller-controlled id must
        // not turn 512 rows into unbounded retained memory. An oversized
        // observation is counted as overflow, never shortened into evidence.
        let bytes = [
            observation.actor.len(),
            observation.asserted_role.len(),
            observation.recorded_actor.as_ref().map_or(0, String::len),
            observation.recorded_role.as_ref().map_or(0, String::len),
            observation.action.len(),
            observation.resource.len(),
            observation.lookup_status.len(),
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add);
        if bytes > 4096 {
            let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
            counts.overflow = counts.overflow.saturating_add(1);
            return;
        }
        // JSON is only a deterministic key here, containing finite strings
        // and scalars. Refuse a failed key as visible overflow, never silence.
        let key = serde_json::to_string(&observation);
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let Ok(key) = key else {
            counts.overflow = counts.overflow.saturating_add(1);
            return;
        };
        if let Some(row) = counts.rows.get_mut(&key) {
            row.count = row.count.saturating_add(1);
        } else if counts.rows.len() < self.limit {
            counts.rows.insert(
                key,
                ReportRow {
                    observation,
                    count: 1,
                },
            );
        } else {
            counts.overflow = counts.overflow.saturating_add(1);
        }
    }
}

pub struct ReportingPolicyClient {
    inner: Arc<dyn PolicyClient>,
    roles: Arc<dyn RoleOfRecord>,
    sink: Arc<dyn RoleReportSink>,
    mode: Arc<dyn ReportModeSource>,
}

impl ReportingPolicyClient {
    pub fn new(
        inner: Arc<dyn PolicyClient>,
        roles: Arc<dyn RoleOfRecord>,
        sink: Arc<dyn RoleReportSink>,
        mode: ReportMode,
    ) -> Self {
        Self::with_mode_source(inner, roles, sink, Arc::new(mode))
    }

    pub fn with_mode_source(
        inner: Arc<dyn PolicyClient>,
        roles: Arc<dyn RoleOfRecord>,
        sink: Arc<dyn RoleReportSink>,
        mode: Arc<dyn ReportModeSource>,
    ) -> Self {
        Self {
            inner,
            roles,
            sink,
            mode,
        }
    }

    async fn observe(&self, user: &User, action: Action, resource: Resource, decision: &Decision) {
        if self.mode.mode() == ReportMode::Off {
            return;
        }
        let mut observation = RoleObservation {
            actor: user.id.clone(),
            asserted_role: user.role.clone(),
            recorded_actor: None,
            recorded_role: None,
            action: action.as_str().into(),
            resource: resource.as_str().into(),
            lookup_status: "unregistered".into(),
            asserted_allowed: Some(decision.is_allowed()),
            recorded_allowed: None,
            would_deny: None,
            would_change_scope: None,
        };
        match self.roles.role_for(&user.id).await {
            Ok(Some(record)) => {
                observation.recorded_actor = Some(record.actor_id);
                observation.recorded_role = record.role.filter(|r| !r.trim().is_empty());
                if let Some(role) = &observation.recorded_role {
                    observation.lookup_status = "registered".into();
                    // Identity, access tier, session scope and department are
                    // preserved. This is a role comparison, never elevation.
                    let recorded = User {
                        role: role.clone(),
                        ..user.clone()
                    };
                    match self.inner.check(&recorded, action, resource).await {
                        Ok(candidate) => {
                            observation.recorded_allowed = Some(candidate.is_allowed());
                            observation.would_deny =
                                Some(decision.is_allowed() && !candidate.is_allowed());
                            observation.would_change_scope = Some(match (decision, &candidate) {
                                (Decision::Allow { scope: a }, Decision::Allow { scope: b }) => {
                                    a != b
                                }
                                _ => false,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(actor = %user.id, %error, "role comparison could not ask policy; asserted decision preserved");
                            observation.lookup_status = "policy-unavailable".into();
                        }
                    }
                } else {
                    observation.lookup_status = "missing-role".into();
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(actor = %user.id, %error, "role registry could not answer; asserted decision preserved");
                observation.lookup_status = match error {
                    boss_core::role_of_record::RoleLookupError::Ambiguous(_) => "ambiguous",
                    _ => "unavailable",
                }
                .into();
            }
        }
        self.sink.record(observation);
    }

    fn unavailable(&self, user: &User, action: Action, resource: &Resource) {
        if self.mode.mode() == ReportMode::Report {
            self.sink.record(RoleObservation {
                actor: user.id.clone(),
                asserted_role: user.role.clone(),
                recorded_actor: None,
                recorded_role: None,
                action: action.as_str().into(),
                resource: resource.as_str().into(),
                lookup_status: "asserted-policy-unavailable".into(),
                asserted_allowed: None,
                recorded_allowed: None,
                would_deny: None,
                would_change_scope: None,
            });
        }
    }
}

#[async_trait]
impl PolicyClient for ReportingPolicyClient {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        let decision = match self.inner.check(user, action, resource.clone()).await {
            Ok(decision) => decision,
            Err(error) => {
                self.unavailable(user, action, &resource);
                return Err(error);
            }
        };
        self.observe(user, action, resource, &decision).await;
        Ok(decision)
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        // The original predicate remains the inner client's, including its
        // error and scoped filtering. The extra read is telemetry only.
        let predicate = match self.inner.scope_predicate(user, resource.clone()).await {
            Ok(predicate) => predicate,
            Err(error) => {
                self.unavailable(user, Action::Read, &resource);
                return Err(error);
            }
        };
        if self.mode.mode() == ReportMode::Report {
            match self.inner.check(user, Action::Read, resource.clone()).await {
                Ok(decision) => self.observe(user, Action::Read, resource, &decision).await,
                Err(error) => {
                    self.unavailable(user, Action::Read, &resource);
                    tracing::warn!(%error, "role report read comparison unavailable; original predicate preserved");
                }
            }
        }
        Ok(predicate)
    }
}
