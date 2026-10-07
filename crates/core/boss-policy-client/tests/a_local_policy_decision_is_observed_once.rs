use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use boss_policy_client::engine::{PolicyDecisionObserver, PolicyEngine};
use boss_policy_client::in_memory::InMemoryPolicy;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::{AccessTier, Action, Decision, Resource, Scope, User};

#[derive(Default)]
struct Observations(Mutex<Vec<(String, String, bool)>>);

#[async_trait]
impl PolicyDecisionObserver for Observations {
    async fn observe(
        &self,
        user: &User,
        _action: Action,
        _resource: Resource,
        _at: chrono::DateTime<chrono::Utc>,
        result: &Result<(Decision, Option<chrono::DateTime<chrono::Utc>>), PolicyError>,
    ) {
        self.0.lock().unwrap().push((
            user.id.clone(),
            user.role.clone(),
            result
                .as_ref()
                .is_ok_and(|(decision, _)| decision.is_allowed()),
        ));
    }
}

fn caller() -> User {
    User {
        id: "signed-caller".into(),
        role: "asserted-role".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

#[tokio::test]
async fn check_and_scope_predicate_observe_each_actual_decision_once() {
    let repo = Arc::new(InMemoryPolicy::new());
    repo.upsert_rule(
        &boss_policy_client::PolicyRule::new(
            "asserted-role",
            Resource::job(),
            Action::Read,
            Scope::All,
        ),
        "fixture",
    )
    .await
    .unwrap();
    let sink = Arc::new(Observations::default());
    let engine = PolicyEngine::with_observer(repo, sink.clone());
    assert!(
        engine
            .check(&caller(), Action::Read, Resource::job())
            .await
            .unwrap()
            .is_allowed()
    );
    engine
        .scope_predicate(&caller(), Resource::job())
        .await
        .unwrap();
    let rows = sink.0.lock().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r == &("signed-caller".into(), "asserted-role".into(), true))
    );
}

#[tokio::test]
async fn override_early_return_keeps_original_expiry_and_is_observed() {
    let repo = Arc::new(InMemoryPolicy::new());
    let expires = chrono::Utc::now() + chrono::Duration::hours(1);
    repo.upsert_user_override(
        &boss_policy_client::UserOverride {
            id: "temporary-deny".into(),
            user_id: caller().id,
            resource: Resource::job(),
            action: Action::Read,
            scope: Scope::None,
            reason: "temporary".into(),
            expires_at: Some(expires),
        },
        "fixture",
    )
    .await
    .unwrap();
    let sink = Arc::new(Observations::default());
    let engine = PolicyEngine::with_observer(repo, sink.clone());
    let (decision, expiry) = engine
        .check_until(&caller(), Action::Read, Resource::job())
        .await
        .unwrap();
    assert!(!decision.is_allowed());
    assert_eq!(expiry, Some(expires));
    assert_eq!(sink.0.lock().unwrap().len(), 1);
}
