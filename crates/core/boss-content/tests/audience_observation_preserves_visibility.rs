use boss_content::port::AudienceObserver;
use boss_content::types::{Audience, ManualSectionDraft, UserContext};
use boss_content::{ContentRepository, InMemoryContent};
use serde_json::json;
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Observer(Mutex<Vec<(String, bool, bool)>>);
impl AudienceObserver for Observer {
    fn observe(&self, guard: &str, user: &UserContext, audience: &Audience, original: bool) {
        assert_eq!(user.id, "emp-reader");
        assert_eq!(user.department.as_deref(), Some("it"));
        let candidate = UserContext {
            role: "recorded".into(),
            ..user.clone()
        };
        self.0
            .lock()
            .unwrap()
            .push((guard.into(), original, audience.matches(&candidate)));
    }
}
#[tokio::test]
async fn audience_observation_includes_excluded_rows_and_preserves_asserted_visibility() {
    let observed = Arc::new(Observer::default());
    let repo = InMemoryContent::new().with_audience_observer(observed.clone());
    exercise_manual_observation(&repo, &observed).await;
    exercise_bulletin_observation(&repo, &observed).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_audience_observation_preserves_the_same_visibility_contract() {
    let db = boss_testing::TestDb::new().await;
    let observed = Arc::new(Observer::default());
    let repo =
        boss_content::PgContent::new(db.pool.clone()).with_audience_observer(observed.clone());
    exercise_manual_observation(&repo, &observed).await;
    exercise_bulletin_observation(&repo, &observed).await;
}

async fn exercise_manual_observation(repo: &dyn ContentRepository, observed: &Observer) {
    for (slug, role, published) in [
        ("asserted", "asserted", true),
        ("recorded", "recorded", true),
        ("draft", "asserted", false),
    ] {
        let draft: ManualSectionDraft = serde_json::from_value(json!({
            "slug":slug,"title":slug,"body":"restricted body","audience":{"roles":[role]},"published":published
        })).unwrap();
        repo.create_section(draft, "emp-editor").await.unwrap();
    }
    let user = UserContext {
        id: "emp-reader".into(),
        role: "asserted".into(),
        department: Some("it".into()),
    };
    let rows = repo.manual_tree(&user).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
        vec!["asserted"]
    );
    assert!(repo.get_section("asserted", &user).await.unwrap().is_some());
    assert!(repo.get_section("recorded", &user).await.unwrap().is_none());
    assert!(repo.get_section("draft", &user).await.unwrap().is_none());
    let observations = observed.0.lock().unwrap();
    assert_eq!(
        observations.len(),
        4,
        "unpublished rows short circuit, excluded audiences are observed"
    );
    for guard in ["content-manual-tree-audience", "content-section-audience"] {
        assert!(observations.contains(&(guard.into(), true, false)));
        assert!(observations.contains(&(guard.into(), false, true)));
    }
}

#[tokio::test]
async fn captured_audience_uses_the_shared_role_snapshot_without_changing_visibility() {
    use boss_policy_client::role_guard::RoleGuardReporter;
    use boss_policy_client::role_reader::{
        MonotonicRoleSnapshotClock, RegistryRoles, SnapshotRoleReader,
    };
    use boss_policy_client::role_reporting::{ReportMode, ReportTally};
    let roles = Arc::new(SnapshotRoleReader::new(
        std::time::Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(RegistryRoles::from_sources(
        json!({"data":[{"id":"canonical-reader","aliases":["emp-reader"],"role":"recorded"}],"total":1}),
        json!({"data":[],"total":0}), json!([])).unwrap())));
    let tally = Arc::new(ReportTally::new(8));
    let reporter = Arc::new(RoleGuardReporter::new(
        roles,
        tally.clone(),
        Arc::new(ReportMode::Report),
    ));
    let repo = InMemoryContent::new().with_audience_observer(Arc::new(
        boss_content::role_reports::RoleAudienceObserver::new(reporter),
    ));
    let draft = serde_json::from_value(
        json!({"slug":"restricted", "title":"title", "body":"restricted body",
        "audience":{"roles":["recorded"], "departments":["it"]}}),
    )
    .unwrap();
    repo.create_section(draft, "emp-editor").await.unwrap();
    let user = UserContext {
        id: "emp-reader".into(),
        role: "asserted".into(),
        department: Some("it".into()),
    };
    assert!(
        repo.get_section("restricted", &user)
            .await
            .unwrap()
            .is_none()
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    let observed = &report.rows[0].observation;
    assert_eq!(observed.actor, "emp-reader");
    assert_eq!(observed.recorded_actor.as_deref(), Some("canonical-reader"));
    assert_eq!(observed.asserted_allowed, Some(false));
    assert_eq!(observed.recorded_allowed, Some(true));
    assert_eq!(observed.would_deny, None);
}

async fn exercise_bulletin_observation(repo: &dyn ContentRepository, observed: &Observer) {
    observed.0.lock().unwrap().clear();
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 4).unwrap();
    let mut dismissed = None;
    for (title, role, expiry) in [
        ("asserted", "asserted", None),
        ("recorded", "recorded", None),
        ("dismissed", "asserted", None),
        ("expired", "asserted", Some("2026-10-03")),
    ] {
        let draft = serde_json::from_value(json!({"title":title,"body":"restricted body",
            "posted_on":"2026-10-04", "expires_on":expiry, "audience":{"roles":[role]}}))
        .unwrap();
        let row = repo.create_bulletin(draft, "emp-editor").await.unwrap();
        if title == "dismissed" {
            dismissed = Some(row.id);
        }
    }
    repo.dismiss_bulletin(dismissed.unwrap(), "emp-reader")
        .await
        .unwrap();
    let user = UserContext {
        id: "emp-reader".into(),
        role: "asserted".into(),
        department: Some("it".into()),
    };
    let rows = repo.list_bulletins_for(&user, today, false).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "asserted");
    let rows = repo.list_bulletins_for(&user, today, true).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.title == "dismissed" && row.dismissed_by_viewer)
    );
    let observations = observed.0.lock().unwrap();
    assert_eq!(
        observations.len(),
        6,
        "expired predicates short circuit; dismissal stays after audience"
    );
    assert_eq!(
        observations
            .iter()
            .filter(|(guard, a, b)| guard == "content-bulletin-audience" && *a && !*b)
            .count(),
        4
    );
    assert_eq!(
        observations
            .iter()
            .filter(|(guard, a, b)| guard == "content-bulletin-audience" && !*a && *b)
            .count(),
        2
    );
}
