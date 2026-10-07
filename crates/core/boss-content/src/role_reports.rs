//! Adapter from captured domain audience inputs to process-owned role reporting.
use crate::port::AudienceObserver;
use crate::types::{Audience, UserContext};
use boss_policy_client::role_guard::RoleGuardReporter;
use std::sync::Arc;

pub struct RoleAudienceObserver {
    reporter: Arc<RoleGuardReporter>,
}
impl RoleAudienceObserver {
    pub fn new(reporter: Arc<RoleGuardReporter>) -> Self {
        Self { reporter }
    }
}
impl AudienceObserver for RoleAudienceObserver {
    fn observe(&self, guard: &str, user: &UserContext, audience: &Audience, original: bool) {
        self.reporter.observe_role_context(
            guard,
            "visibility",
            (&user.id, &user.role),
            original,
            |role| {
                Some(audience.matches(&UserContext {
                    role: role.to_owned(),
                    ..user.clone()
                }))
            },
        );
    }
}
