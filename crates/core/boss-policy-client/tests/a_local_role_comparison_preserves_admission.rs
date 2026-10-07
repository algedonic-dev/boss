use async_trait::async_trait;
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord, RoleRecord};
use boss_policy_client::engine::{PolicyDecisionObserver, PolicyEngine};
use boss_policy_client::in_memory::InMemoryPolicy;
use boss_policy_client::port::PolicyRepository;
use boss_policy_client::role_reporting::{LocalReportingObserver, ReportMode, ReportTally};
use boss_policy_client::{AccessTier, Action, Decision, Resource, Scope, User};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Roles(AtomicUsize);
#[async_trait]
impl RoleOfRecord for Roles {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(actor, "actual-caller");
        Ok(Some(RoleRecord {
            actor_id: "canonical-caller".into(),
            role: Some("recorded-role".into()),
        }))
    }
}

fn caller() -> User {
    User {
        id: "actual-caller".into(),
        role: "asserted-role".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

#[tokio::test]
async fn local_report_compares_once_without_changing_the_asserted_allow() {
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
    let roles = Arc::new(Roles(AtomicUsize::new(0)));
    let tally = Arc::new(ReportTally::new(10));
    let observer = Arc::new(LocalReportingObserver::new(
        repo.clone(),
        roles.clone(),
        tally.clone(),
        Arc::new(ReportMode::Report),
        std::time::Duration::from_secs(1),
    ));
    let engine = PolicyEngine::with_observer(repo, observer);
    assert!(
        engine
            .check(&caller(), Action::Read, Resource::job())
            .await
            .unwrap()
            .is_allowed()
    );
    assert_eq!(roles.0.load(Ordering::SeqCst), 1);
    let report = tally.snapshot();
    assert!(!report.durable_window);
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.count, 1);
    assert_eq!(row.observation.actor, "actual-caller");
    assert_eq!(
        row.observation.recorded_actor.as_deref(),
        Some("canonical-caller")
    );
    assert_eq!(row.observation.asserted_allowed, Some(true));
    assert_eq!(row.observation.recorded_allowed, Some(false));
    assert_eq!(row.observation.would_deny, Some(true));
}

#[tokio::test]
async fn off_mode_does_no_registry_or_sink_work() {
    let repo = Arc::new(InMemoryPolicy::new());
    let roles = Arc::new(Roles(AtomicUsize::new(0)));
    let tally = Arc::new(ReportTally::new(10));
    let observer = Arc::new(LocalReportingObserver::new(
        repo.clone(),
        roles.clone(),
        tally.clone(),
        Arc::new(ReportMode::Off),
        std::time::Duration::from_secs(1),
    ));
    let engine = PolicyEngine::with_observer(repo, observer);
    assert!(
        !engine
            .check(&caller(), Action::Read, Resource::job())
            .await
            .unwrap()
            .is_allowed()
    );
    assert_eq!(roles.0.load(Ordering::SeqCst), 0);
    assert!(tally.snapshot().rows.is_empty());
}

struct PendingRoles;
#[async_trait]
impl RoleOfRecord for PendingRoles {
    async fn role_for(&self, _: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_hung_comparison_is_visible_and_cannot_hold_admission_open() {
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
    let tally = Arc::new(ReportTally::new(10));
    let observer = Arc::new(LocalReportingObserver::new(
        repo.clone(),
        Arc::new(PendingRoles),
        tally.clone(),
        Arc::new(ReportMode::Report),
        std::time::Duration::from_millis(5),
    ));
    let engine = PolicyEngine::with_observer(repo, observer);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        engine.check(&caller(), Action::Read, Resource::job()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.is_allowed());
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "comparison-timeout"
    );
    assert_eq!(report.rows[0].observation.recorded_allowed, None);
}

#[tokio::test]
async fn an_expired_original_override_is_unavailable_without_rejudging_admission() {
    let repo = Arc::new(InMemoryPolicy::new());
    let at = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let expiry = at + chrono::Duration::seconds(1);
    repo.upsert_user_override(
        &boss_policy_client::UserOverride {
            id: "captured-allow".into(),
            user_id: caller().id,
            resource: Resource::job(),
            action: Action::Read,
            scope: Scope::All,
            reason: "fixture".into(),
            expires_at: Some(expiry),
        },
        "fixture",
    )
    .await
    .unwrap();
    let tally = Arc::new(ReportTally::new(10));
    let observer = LocalReportingObserver::new(
        repo,
        Arc::new(Roles(AtomicUsize::new(0))),
        tally.clone(),
        Arc::new(ReportMode::Report),
        std::time::Duration::from_secs(1),
    );
    let original = Ok((Decision::Allow { scope: Scope::All }, Some(expiry)));
    observer
        .observe(&caller(), Action::Read, Resource::job(), at, &original)
        .await;
    let report = tally.snapshot();
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "comparison-expired"
    );
    assert_eq!(report.rows[0].observation.recorded_allowed, None);
    assert_eq!(report.rows[0].observation.would_deny, None);
    assert!(matches!(original, Ok((_, Some(value))) if value == expiry));
}
