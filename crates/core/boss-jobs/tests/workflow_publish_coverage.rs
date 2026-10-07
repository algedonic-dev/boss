//! Unknown holder evidence must never authorize a runtime publication.
use boss_core::{actor::ActorId, job::JobId};
use boss_jobs::registry::{InMemoryWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec};

fn candidate() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "coverage-probe",
        "Coverage probe",
        "platform",
        vec!["custom".into()],
        vec![StepSpec {
            title: "finish".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            terminal: Some(Terminal {
                outcome: "done".into(),
            }),
            ..Default::default()
        }],
    )
}

#[tokio::test]
async fn no_publication_door_accepts_unknown_holder_evidence() {
    let actor = ActorId::Automation("coverage-test".into());
    for door in 0..3 {
        let registry = InMemoryWorkflows::new();
        let spec = candidate();
        let now = chrono::Utc::now();
        let result = match door {
            0 => {
                registry.create_draft(spec, &actor, now).await.unwrap();
                registry.publish("coverage-probe", &actor, now).await
            }
            1 => {
                registry
                    .publish_authored(spec, JobId::new(), &actor, now)
                    .await
            }
            _ => {
                registry
                    .publish_authored_if_absent(spec, JobId::new(), &actor, now)
                    .await
            }
        };
        assert!(
            result.is_err(),
            "door {door} published with no holder snapshot"
        );
        assert!(registry.get_active("coverage-probe").await.is_err());
        assert!(
            registry
                .recorded_events()
                .iter()
                .all(|e| e.kind != boss_jobs::events::WORKFLOW_PUBLISHED)
        );
    }
}

use boss_policy_client::coverage::{CoverageSnapshot, CoverageSnapshotSource, Key, Person};
use boss_policy_client::types::AccessTier;
use std::sync::Arc;

struct Fixed(Result<CoverageSnapshot, String>);
#[async_trait::async_trait]
impl CoverageSnapshotSource for Fixed {
    async fn snapshot(&self) -> Result<CoverageSnapshot, String> {
        self.0.clone()
    }
}
fn snapshot() -> CoverageSnapshot {
    CoverageSnapshot {
        rules: boss_policy_client::defaults::default_rules(),
        overrides: vec![],
        roster: vec![Person {
            id: "emp-founder".into(),
            role: Some("platform-admin".into()),
            active: true,
            hire_date: None,
        }],
        keys: vec![Key {
            employee_id: "emp-founder".into(),
            access_tier: AccessTier::Operator,
        }],
    }
}

#[tokio::test]
async fn all_three_doors_judge_the_actual_candidate_and_conserve_rejected_writes() {
    let actor = ActorId::Automation("coverage-test".into());
    for door in 0..3 {
        let registry = InMemoryWorkflows::guarded(Arc::new(Fixed(Ok(snapshot()))));
        let mut spec = candidate();
        spec.steps[0].sign_offs_required = vec!["nobody-holds-this".into()];
        let now = chrono::Utc::now();
        if door == 0 {
            registry
                .create_draft(spec.clone(), &actor, now)
                .await
                .unwrap();
        }
        let before = registry.recorded_events();
        let result = match door {
            0 => registry.publish(&spec.kind, &actor, now).await,
            1 => {
                registry
                    .publish_authored(spec.clone(), JobId::new(), &actor, now)
                    .await
            }
            _ => {
                registry
                    .publish_authored_if_absent(spec.clone(), JobId::new(), &actor, now)
                    .await
            }
        };
        let error = result.expect_err("new orphan must refuse");
        assert!(
            error.to_string().contains("step-signoff:nobody-holds-this"),
            "{error}"
        );
        assert!(registry.get_active(&spec.kind).await.is_err());
        assert_eq!(
            serde_json::to_value(registry.recorded_events()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

#[tokio::test]
async fn covered_candidate_passes_with_one_exact_publication_fact() {
    let registry = InMemoryWorkflows::guarded(Arc::new(Fixed(Ok(snapshot()))));
    let actor = ActorId::Automation("coverage-test".into());
    let now = chrono::Utc::now();
    let spec = candidate();
    registry
        .create_draft(spec.clone(), &actor, now)
        .await
        .unwrap();
    let published = registry.publish(&spec.kind, &actor, now).await.unwrap();
    assert_eq!(published, registry.get_active(&spec.kind).await.unwrap());
    let events = registry.recorded_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == boss_jobs::events::WORKFLOW_PUBLISHED)
            .count(),
        1
    );
}

#[tokio::test]
async fn equal_first_record_replay_needs_no_new_holder_read_or_write() {
    let registry = InMemoryWorkflows::new();
    let mut spec = candidate();
    spec.status = boss_jobs::registry::WorkflowStatus::Active;
    spec.version = 1;
    registry.seed(spec.clone()).unwrap();
    let before = registry.recorded_events();
    let actual = registry
        .publish_authored_if_absent(
            spec.clone(),
            JobId::new(),
            &ActorId::Automation("coverage-test".into()),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(actual, spec);
    assert_eq!(
        serde_json::to_value(registry.recorded_events()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[tokio::test]
async fn a_dark_roster_is_unavailable_evidence_not_an_orphan_verdict() {
    for door in 0..3 {
        let mut unknown = snapshot();
        unknown.roster.clear();
        let registry = InMemoryWorkflows::guarded(Arc::new(Fixed(Ok(unknown))));
        let spec = candidate();
        let actor = ActorId::Automation("coverage-test".into());
        let now = chrono::Utc::now();
        if door == 0 {
            registry
                .create_draft(spec.clone(), &actor, now)
                .await
                .unwrap();
        }
        let before = registry.recorded_events();
        let result = match door {
            0 => registry.publish(&spec.kind, &actor, now).await,
            1 => {
                registry
                    .publish_authored(spec, JobId::new(), &actor, now)
                    .await
            }
            _ => {
                registry
                    .publish_authored_if_absent(spec, JobId::new(), &actor, now)
                    .await
            }
        };
        assert!(
            matches!(
                result,
                Err(boss_jobs::registry::WorkflowError::CoverageUnavailable(_))
            ),
            "door {door}: {result:?}"
        );
        assert_eq!(
            serde_json::to_value(registry.recorded_events()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

struct Paused {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl CoverageSnapshotSource for Paused {
    async fn snapshot(&self) -> Result<CoverageSnapshot, String> {
        self.entered.notify_one();
        self.resume.notified().await;
        Ok(snapshot())
    }
}

#[tokio::test]
async fn draft_changed_during_holder_read_is_judged_as_the_actual_promoted_candidate() {
    let source = Arc::new(Paused {
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let registry = Arc::new(InMemoryWorkflows::guarded(source.clone()));
    let actor = ActorId::Automation("coverage-test".into());
    let now = chrono::Utc::now();
    registry
        .create_draft(candidate(), &actor, now)
        .await
        .unwrap();
    let publishing = {
        let registry = registry.clone();
        let actor = actor.clone();
        tokio::spawn(async move { registry.publish("coverage-probe", &actor, now).await })
    };
    source.entered.notified().await;
    let mut changed = candidate();
    changed.steps[0].sign_offs_required = vec!["nobody-holds-this".into()];
    registry.create_draft(changed, &actor, now).await.unwrap();
    let before = registry.recorded_events();
    source.resume.notify_one();
    assert!(
        publishing
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("step-signoff:nobody-holds-this")
    );
    assert!(registry.get_active("coverage-probe").await.is_err());
    assert_eq!(
        serde_json::to_value(registry.recorded_events()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_refusals_conserve_rows_and_outbox_at_all_three_doors() {
    let db = boss_testing::TestDb::new().await;
    let actor = ActorId::Automation("coverage-test".into());
    for door in 0..3 {
        let registry = boss_jobs::registry::PgWorkflows::guarded(
            db.pool.clone(),
            Arc::new(Fixed(Ok(snapshot()))),
        );
        let mut spec = candidate();
        spec.kind = format!("coverage-refusal-{door}");
        spec.steps[0].sign_offs_required = vec!["nobody-holds-this".into()];
        let now = chrono::Utc::now();
        if door == 0 {
            registry
                .create_draft(spec.clone(), &actor, now)
                .await
                .unwrap();
        }
        let before = registry.list_versions(&spec.kind).await.unwrap();
        let facts_before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let result = match door {
            0 => registry.publish(&spec.kind, &actor, now).await,
            1 => {
                registry
                    .publish_authored(spec.clone(), JobId::new(), &actor, now)
                    .await
            }
            _ => {
                registry
                    .publish_authored_if_absent(spec.clone(), JobId::new(), &actor, now)
                    .await
            }
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("step-signoff:nobody-holds-this")
        );
        assert_eq!(registry.list_versions(&spec.kind).await.unwrap(), before);
        let facts_after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(facts_after, facts_before);
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_holder_read_holds_no_registry_lock_and_changed_draft_is_refused() {
    let db = boss_testing::TestDb::new().await;
    let source = Arc::new(Paused {
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let registry = Arc::new(boss_jobs::registry::PgWorkflows::guarded(
        db.pool.clone(),
        source.clone(),
    ));
    let actor = ActorId::Automation("coverage-test".into());
    let now = chrono::Utc::now();
    registry
        .create_draft(candidate(), &actor, now)
        .await
        .unwrap();
    let publishing = {
        let registry = registry.clone();
        let actor = actor.clone();
        tokio::spawn(async move { registry.publish("coverage-probe", &actor, now).await })
    };
    source.entered.notified().await;
    // A real competing workflow write must finish while external HTTP
    // evidence is stalled; a lock across that read would time out here.
    let mut changed = candidate();
    changed.steps[0].sign_offs_required = vec!["nobody-holds-this".into()];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        registry.create_draft(changed, &actor, now),
    )
    .await
    .unwrap()
    .unwrap();
    let before = registry.list_versions("coverage-probe").await.unwrap();
    let facts_before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    source.resume.notify_one();
    assert!(
        publishing
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("step-signoff:nobody-holds-this")
    );
    assert_eq!(
        registry.list_versions("coverage-probe").await.unwrap(),
        before
    );
    let facts_after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(facts_after, facts_before);
}

#[tokio::test]
async fn automation_inactive_and_keyless_rosters_cannot_hold_new_controls() {
    for holder in 0..3 {
        let mut facts = snapshot();
        match holder {
            0 => {
                facts.roster[0].id = "automation:founder".into();
                facts.keys[0].employee_id = "automation:founder".into();
            }
            1 => {
                facts.roster[0].active = false;
            }
            _ => facts.keys.clear(),
        }
        let registry = InMemoryWorkflows::guarded(Arc::new(Fixed(Ok(facts))));
        let mut spec = candidate();
        spec.steps[0].sign_offs_required = vec!["platform-admin".into()];
        assert!(
            registry
                .publish_authored(
                    spec,
                    JobId::new(),
                    &ActorId::Automation("coverage-test".into()),
                    chrono::Utc::now()
                )
                .await
                .is_err(),
            "holder case {holder}"
        );
        assert!(registry.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn an_existing_gap_does_not_prevent_its_repair_or_an_unrelated_covered_publication() {
    let registry = InMemoryWorkflows::guarded(Arc::new(Fixed(Ok(snapshot()))));
    let mut broken = candidate();
    broken.kind = "old-gap".into();
    broken.status = boss_jobs::registry::WorkflowStatus::Active;
    broken.steps[0].sign_offs_required = vec!["nobody-holds-this".into()];
    registry.seed(broken).unwrap();
    let actor = ActorId::Automation("coverage-test".into());
    registry
        .publish_authored(candidate(), JobId::new(), &actor, chrono::Utc::now())
        .await
        .unwrap();
    let mut repair = candidate();
    repair.kind = "old-gap".into();
    registry
        .publish_authored(repair, JobId::new(), &actor, chrono::Utc::now())
        .await
        .unwrap();
    assert!(
        registry.get_active("old-gap").await.unwrap().steps[0]
            .sign_offs_required
            .is_empty()
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_outbox_failure_rolls_back_candidate_and_incumbent() {
    let db = boss_testing::TestDb::new().await;
    let registry =
        boss_jobs::registry::PgWorkflows::guarded(db.pool.clone(), Arc::new(Fixed(Ok(snapshot()))));
    let actor = ActorId::Automation("coverage-test".into());
    let now = chrono::Utc::now();
    registry
        .publish_authored(candidate(), JobId::new(), &actor, now)
        .await
        .unwrap();
    registry
        .create_draft(candidate(), &actor, now)
        .await
        .unwrap();
    let before = registry.list_versions("coverage-probe").await.unwrap();
    sqlx::query("DROP TABLE event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        registry
            .publish("coverage-probe", &actor, now)
            .await
            .is_err()
    );
    assert_eq!(
        registry.list_versions("coverage-probe").await.unwrap(),
        before
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn competing_guarded_first_records_publish_one_exact_row_and_fact() {
    let db = boss_testing::TestDb::new().await;
    let registry =
        boss_jobs::registry::PgWorkflows::guarded(db.pool.clone(), Arc::new(Fixed(Ok(snapshot()))));
    let actor = ActorId::Automation("coverage-test".into());
    let now = chrono::Utc::now();
    let first = candidate();
    let mut second = candidate();
    second.description = Some("different contender".into());
    let (a, b) = tokio::join!(
        registry.publish_authored_if_absent(first, JobId::new(), &actor, now),
        registry.publish_authored_if_absent(second, JobId::new(), &actor, now),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let active = registry.get_active("coverage-probe").await.unwrap();
    assert_eq!(
        registry.list_versions("coverage-probe").await.unwrap(),
        vec![active.clone()]
    );
    let payloads: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(boss_jobs::events::WORKFLOW_PUBLISHED)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let mut expected = serde_json::to_value(active).unwrap();
    expected["_actor"] = serde_json::json!(actor.to_string());
    assert_eq!(payloads, vec![expected]);
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn trusted_platform_bootstrap_preserves_insert_only_semantics_without_holder_reads() {
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::registry::PgWorkflows::for_bootstrap(db.pool.clone());
    let actor = ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::Utc::now();
    boss_jobs::workflow_seed::seed_workflows(&registry, &[candidate()], &actor, now, false)
        .await
        .unwrap();
    let active = registry.get_active("coverage-probe").await.unwrap();
    registry.retire(&active.kind, &actor, now).await.unwrap();
    let before = registry.list_versions(&active.kind).await.unwrap();
    let report =
        boss_jobs::workflow_seed::seed_workflows(&registry, &[candidate()], &actor, now, false)
            .await
            .unwrap();
    assert_eq!(
        report.rows[0].outcome,
        boss_jobs::workflow_seed::SeedOutcome::Retired { newest: 1 }
    );
    assert_eq!(registry.list_versions(&active.kind).await.unwrap(), before);
    assert!(registry.get_active(&active.kind).await.is_err());
}

#[test]
fn the_bootstrap_capability_is_not_selected_by_runtime_requests() {
    let root = boss_testing::repo_root();
    let runtime =
        std::fs::read_to_string(root.join("crates/core/boss-jobs/src/bin/boss_jobs_api.rs"))
            .unwrap();
    assert!(runtime.contains("PgWorkflows::guarded("));
    assert!(!runtime.contains("::for_bootstrap("));
    for path in ["http/kinds.rs", "http/steps.rs", "http/mod.rs"] {
        let body =
            std::fs::read_to_string(root.join("crates/core/boss-jobs/src").join(path)).unwrap();
        assert!(
            !body.contains("::for_bootstrap("),
            "runtime request selected bootstrap: {path}"
        );
        assert!(
            !body.contains("::for_fixture("),
            "runtime request selected fixture: {path}"
        );
    }
    let seeder = std::fs::read_to_string(
        root.join("crates/core/boss-jobs/src/bin/boss_platform_workflow_seed.rs"),
    )
    .unwrap();
    assert!(seeder.contains("PgWorkflows::for_bootstrap("));
    assert!(!seeder.contains("PgWorkflows::for_fixture("));
}
