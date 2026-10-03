//! Report-only precursor of design abf9eeae: observe the record through
//! ports while returning exactly the authority the caller has today.

use std::sync::Arc;

use async_trait::async_trait;
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord, RoleRecord};
use boss_policy_client::role_reporting::{ReportMode, ReportTally, ReportingPolicyClient};
use boss_policy_client::{Action, Decision, FakePolicyClient, PolicyClient, Resource, Scope, User};

#[tokio::test]
async fn an_asserted_policy_error_is_unknown_and_visible_without_registry_work() {
    struct Dark;
    #[async_trait]
    impl PolicyClient for Dark {
        async fn check(
            &self,
            _: &User,
            _: Action,
            _: Resource,
        ) -> Result<Decision, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "policy dark".into(),
            ))
        }
        async fn scope_predicate(
            &self,
            _: &User,
            _: Resource,
        ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "policy dark".into(),
            ))
        }
    }
    let tally = Arc::new(ReportTally::new(16));
    let policy = ReportingPolicyClient::new(
        Arc::new(Dark),
        Arc::new(Registry(Ok(None))),
        tally.clone(),
        ReportMode::Report,
    );
    assert!(
        policy
            .check(&caller(), Action::Update, Resource::class())
            .await
            .is_err()
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "asserted-policy-unavailable"
    );
    assert!(
        serde_json::to_value(&report.rows[0].observation).unwrap()["asserted_allowed"].is_null()
    );
    assert_eq!(report.rows[0].observation.would_deny, None);
    assert!(!report.durable_window);
}

struct Registry(Result<Option<RoleRecord>, RoleLookupError>);

#[async_trait]
impl RoleOfRecord for Registry {
    async fn role_for(&self, _actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        self.0.clone()
    }
}

fn caller() -> User {
    let mut user = User::service("registered-agent");
    user.id = "agent-example".into();
    user
}

fn inner() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::class(),
                Scope::All,
            )
            .build(),
    )
}

#[tokio::test]
async fn a_registered_role_is_compared_by_policy_but_the_asserted_answer_is_preserved() {
    let tally = Arc::new(ReportTally::new(16));
    let policy = ReportingPolicyClient::new(
        inner(),
        Arc::new(Registry(Ok(Some(RoleRecord {
            actor_id: "agent-example".into(),
            role: Some("engineering-agent".into()),
        })))),
        tally.clone(),
        ReportMode::Report,
    );
    let user = caller();
    let decision = policy
        .check(&user, Action::Update, Resource::class())
        .await
        .unwrap();
    assert!(matches!(decision, Decision::Allow { scope: Scope::All }));
    assert_eq!(user.role, "platform-admin");
    assert_eq!(user.access_tier, boss_policy_client::AccessTier::Operator);
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    let observation = &report.rows[0].observation;
    assert_eq!(observation.actor, "agent-example");
    assert_eq!(observation.asserted_role, "platform-admin");
    assert_eq!(
        observation.recorded_role.as_deref(),
        Some("engineering-agent")
    );
    assert_eq!(observation.would_deny, Some(true));
    assert_eq!(report.rows[0].count, 1);
}

#[tokio::test]
async fn missing_null_and_dark_records_are_visible_and_preserve_the_asserted_answer() {
    for (lookup, status) in [
        (Ok(None), "unregistered"),
        (
            Ok(Some(RoleRecord {
                actor_id: "agent-example".into(),
                role: None,
            })),
            "missing-role",
        ),
        (
            Err(RoleLookupError::Unavailable("registry dark".into())),
            "unavailable",
        ),
        (
            Err(RoleLookupError::Ambiguous("two rows".into())),
            "ambiguous",
        ),
    ] {
        let tally = Arc::new(ReportTally::new(16));
        let policy = ReportingPolicyClient::new(
            inner(),
            Arc::new(Registry(lookup)),
            tally.clone(),
            ReportMode::Report,
        );
        assert!(
            policy
                .check(&caller(), Action::Update, Resource::class())
                .await
                .unwrap()
                .is_allowed()
        );
        let report = tally.snapshot();
        assert_eq!(report.rows[0].observation.lookup_status, status);
        assert_eq!(report.rows[0].observation.would_deny, None);
        assert_eq!(report.rows[0].observation.recorded_role, None);
    }
}

#[tokio::test]
async fn a_role_comparison_preserves_every_other_user_field_and_the_original_scope() {
    use boss_policy_client::{PolicyClientError, Predicate};
    struct Witness {
        original: User,
    }
    #[async_trait]
    impl PolicyClient for Witness {
        async fn check(
            &self,
            user: &User,
            _: Action,
            _: Resource,
        ) -> Result<Decision, PolicyClientError> {
            let expected = User {
                role: user.role.clone(),
                ..self.original.clone()
            };
            assert_eq!(
                serde_json::to_value(user).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            Ok(Decision::Allow {
                scope: if user.role == "platform-admin" {
                    Scope::Self_
                } else {
                    Scope::All
                },
            })
        }
        async fn scope_predicate(
            &self,
            user: &User,
            _: Resource,
        ) -> Result<Predicate, PolicyClientError> {
            Ok(Predicate::OwnerIs {
                user_id: user.id.clone(),
            })
        }
    }
    let mut user = caller();
    user.department = Some("unchanged".into());
    user.access_tier = boss_policy_client::AccessTier::User;
    let tally = Arc::new(ReportTally::new(16));
    let policy = ReportingPolicyClient::new(
        Arc::new(Witness {
            original: user.clone(),
        }),
        Arc::new(Registry(Ok(Some(RoleRecord {
            actor_id: "canonical-other-id".into(),
            role: Some("tenant-role".into()),
        })))),
        tally.clone(),
        ReportMode::Report,
    );
    let actual = policy
        .check(&user, Action::Update, Resource::class())
        .await
        .unwrap();
    assert!(matches!(
        actual,
        Decision::Allow {
            scope: Scope::Self_
        }
    ));
    assert!(
        matches!(policy.scope_predicate(&user, Resource::class()).await.unwrap(), Predicate::OwnerIs {user_id} if user_id == user.id)
    );
    assert_eq!(tally.snapshot().rows.len(), 2);
    for row in tally.snapshot().rows {
        assert_eq!(row.observation.would_change_scope, Some(true));
        assert_eq!(row.observation.would_deny, Some(false));
        assert_eq!(
            row.observation.recorded_actor.as_deref(),
            Some("canonical-other-id")
        );
    }
}

#[tokio::test]
async fn off_reads_no_registry_and_records_nothing() {
    struct Unread;
    #[async_trait]
    impl RoleOfRecord for Unread {
        async fn role_for(&self, _actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
            panic!("off must not ask the registry")
        }
    }
    let tally = Arc::new(ReportTally::new(16));
    let policy =
        ReportingPolicyClient::new(inner(), Arc::new(Unread), tally.clone(), ReportMode::Off);
    assert!(
        policy
            .check(&caller(), Action::Update, Resource::class())
            .await
            .unwrap()
            .is_allowed()
    );
    assert!(tally.snapshot().rows.is_empty());
}

#[tokio::test]
async fn a_tally_overflow_is_explicit_and_storage_stays_bounded() {
    let tally = Arc::new(ReportTally::new(1));
    let policy = ReportingPolicyClient::new(
        inner(),
        Arc::new(Registry(Ok(None))),
        tally.clone(),
        ReportMode::Report,
    );
    for id in ["first", "second", "third"] {
        let mut user = caller();
        user.id = id.into();
        assert!(
            policy
                .check(&user, Action::Update, Resource::class())
                .await
                .unwrap()
                .is_allowed()
        );
    }
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.overflow, 2);
}

#[tokio::test]
async fn oversized_observation_strings_are_counted_as_overflow_not_retained() {
    let tally = Arc::new(ReportTally::new(16));
    let policy = ReportingPolicyClient::new(
        inner(),
        Arc::new(Registry(Ok(None))),
        tally.clone(),
        ReportMode::Report,
    );
    let mut user = caller();
    user.id = "x".repeat(65_536);
    assert!(
        policy
            .check(&user, Action::Update, Resource::class())
            .await
            .unwrap()
            .is_allowed()
    );
    assert!(tally.snapshot().rows.is_empty());
    assert_eq!(tally.snapshot().overflow, 1);
}
