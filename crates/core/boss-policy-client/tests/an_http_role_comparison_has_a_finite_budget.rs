use async_trait::async_trait;
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord, RoleRecord};
use boss_policy_client::role_reporting::{ReportMode, ReportTally, ReportingPolicyClient};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope, User};
use std::sync::Arc;
use std::time::Duration;

struct PendingRegistry;
#[async_trait]
impl RoleOfRecord for PendingRegistry {
    async fn role_for(&self, _: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn unavailable_comparison_cannot_hold_a_successful_admission_open() {
    let tally = Arc::new(ReportTally::new(8));
    let client = ReportingPolicyClient::new(
        Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    Action::Read,
                    Resource::class(),
                    Scope::All,
                )
                .build(),
        ),
        Arc::new(PendingRegistry),
        tally.clone(),
        ReportMode::Report,
    );
    let decision = tokio::time::timeout(
        Duration::from_secs(1),
        client.check(&User::service("people"), Action::Read, Resource::class()),
    )
    .await
    .expect("comparison must not block admission indefinitely")
    .unwrap();
    assert_eq!(
        decision,
        boss_policy_client::Decision::Allow { scope: Scope::All }
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "comparison-timeout"
    );
    assert_eq!(report.rows[0].observation.asserted_allowed, Some(true));
    assert_eq!(report.rows[0].observation.would_deny, None);
}

#[tokio::test]
async fn unavailable_comparison_preserves_the_original_scope_predicate() {
    let tally = Arc::new(ReportTally::new(8));
    let inner: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Read,
                Resource::class(),
                Scope::All,
            )
            .build(),
    );
    let user = User::service("people");
    let expected = inner
        .scope_predicate(&user, Resource::class())
        .await
        .unwrap();
    let client = ReportingPolicyClient::new(
        inner,
        Arc::new(PendingRegistry),
        tally.clone(),
        ReportMode::Report,
    );
    let actual = tokio::time::timeout(
        Duration::from_secs(1),
        client.scope_predicate(&user, Resource::class()),
    )
    .await
    .expect("comparison must not block the original predicate")
    .unwrap();
    assert_eq!(actual, expected);
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "comparison-timeout"
    );
    assert_eq!(report.rows[0].observation.would_deny, None);
}
