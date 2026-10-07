//! Compare a pure role guard against the service-owned complete snapshot.
//! This records predicate observations, not request totals or policy grants.
use std::sync::Arc;

use crate::User;
use crate::role_reader::SnapshotRoleReader;
use crate::role_reporting::{ReportMode, ReportModeSource, RoleObservation, RoleReportSink};

pub struct RoleGuardReporter {
    roles: Arc<SnapshotRoleReader>,
    sink: Arc<dyn RoleReportSink>,
    mode: Arc<dyn ReportModeSource>,
}

impl RoleGuardReporter {
    pub fn new(
        roles: Arc<SnapshotRoleReader>,
        sink: Arc<dyn RoleReportSink>,
        mode: Arc<dyn ReportModeSource>,
    ) -> Self {
        Self { roles, sink, mode }
    }

    /// `predicate` must be pure over the caller and already-read context.
    /// Do not pass handlers, policy calls, mutations or registry readers.
    /// Role substitution preserves every other caller field. The returned
    /// answer is always the original guard's answer, including in off mode.
    pub fn evaluate(
        &self,
        guard: &str,
        purpose: &str,
        user: &User,
        predicate: impl Fn(&User) -> bool,
    ) -> bool {
        let original = predicate(user);
        self.observe_captured(guard, purpose, user, original, |candidate| {
            Some(predicate(candidate))
        })
    }

    /// Compare against context already captured by the original operation.
    /// Missing candidate context is unknown, never a denial or clean reading.
    pub fn observe_captured(
        &self,
        guard: &str,
        purpose: &str,
        user: &User,
        original: bool,
        candidate: impl Fn(&User) -> Option<bool>,
    ) -> bool {
        if self.mode.mode() == ReportMode::Off {
            return original;
        }
        let (mut observation, recorded_user) =
            self.comparison_base(guard, purpose, user, Some(original));
        if let Some(recorded_user) = recorded_user {
            let answer = candidate(&recorded_user);
            if answer.is_none() {
                observation.lookup_status = "context-unavailable".into();
            }
            observation.recorded_allowed = answer;
            if purpose == "admission" {
                observation.would_deny = answer.map(|allowed| original && !allowed);
            }
        }
        self.sink.record(observation);
        original
    }

    /// Compare a captured selector or scope value, never an admission.
    /// Both values must derive solely from the same already-read context.
    pub fn observe_selection<T: PartialEq>(
        &self,
        guard: &str,
        user: &User,
        original: &T,
        candidate: impl Fn(&User) -> Option<T>,
    ) {
        if self.mode.mode() == ReportMode::Off {
            return;
        }
        let (mut observation, recorded_user) = self.comparison_base(guard, "selection", user, None);
        if let Some(recorded_user) = recorded_user {
            match candidate(&recorded_user) {
                Some(selection) => observation.would_change_scope = Some(original != &selection),
                None => observation.lookup_status = "context-unavailable".into(),
            }
        }
        self.sink.record(observation);
    }

    /// Observe a captured role-only domain context without inventing a
    /// policy caller's access tier, territories or other authority fields.
    pub fn observe_role_context(
        &self,
        guard: &str,
        purpose: &str,
        actor_and_role: (&str, &str),
        original: bool,
        candidate: impl Fn(&str) -> Option<bool>,
    ) -> bool {
        if self.mode.mode() == ReportMode::Off {
            return original;
        }
        let (mut observation, role) =
            self.role_comparison_base(guard, purpose, actor_and_role, Some(original));
        if let Some(role) = role {
            let answer = candidate(&role);
            if answer.is_none() {
                observation.lookup_status = "context-unavailable".into();
            }
            observation.recorded_allowed = answer;
            if purpose == "admission" {
                observation.would_deny = answer.map(|allowed| original && !allowed);
            }
        }
        self.sink.record(observation);
        original
    }

    fn comparison_base(
        &self,
        guard: &str,
        purpose: &str,
        user: &User,
        original: Option<bool>,
    ) -> (RoleObservation, Option<User>) {
        let (observation, role) =
            self.role_comparison_base(guard, purpose, (&user.id, &user.role), original);
        (
            observation,
            role.map(|role| User {
                role,
                ..user.clone()
            }),
        )
    }

    fn role_comparison_base(
        &self,
        guard: &str,
        purpose: &str,
        actor_and_role: (&str, &str),
        original: Option<bool>,
    ) -> (RoleObservation, Option<String>) {
        let mut observation = RoleObservation {
            actor: actor_and_role.0.to_owned(),
            asserted_role: actor_and_role.1.to_owned(),
            recorded_actor: None,
            recorded_role: None,
            action: purpose.into(),
            resource: guard.into(),
            lookup_status: "unregistered".into(),
            asserted_allowed: original,
            recorded_allowed: None,
            would_deny: None,
            would_change_scope: None,
        };
        let mut recorded_role = None;
        match self.roles.lookup(actor_and_role.0) {
            Ok(Some(record)) => {
                observation.recorded_actor = Some(record.actor_id);
                observation.recorded_role = record.role.filter(|role| !role.trim().is_empty());
                if let Some(role) = &observation.recorded_role {
                    observation.lookup_status = "registered".into();
                    recorded_role = Some(role.clone());
                } else {
                    observation.lookup_status = "missing-role".into();
                }
            }
            Ok(None) => {}
            Err(error) => {
                observation.lookup_status = match error {
                    boss_core::role_of_record::RoleLookupError::Ambiguous(_) => "ambiguous",
                    _ => "unavailable",
                }
                .into();
            }
        }
        (observation, recorded_role)
    }
}
