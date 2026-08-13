//! Postgres-backed coverage for `WorkflowRegistry::bootstrap_reconcile`.
//!
//! The InMemory adapter is exercised in `registry::tests` (lib test).
//! This file proves the Pg adapter has matching semantics — if they
//! drift, the bootstrap loop in boss-jobs-api would silently apply
//! one branch in dev tests and a different branch in production.

#![cfg(feature = "postgres")]

use boss_core::job::JobId;
use boss_jobs::registry::{
    KindReconcileStats, PgWorkflows, StepSpec, Terminal, WorkflowRegistry, WorkflowSpec,
    WorkflowStatus,
};
use boss_testing::TestDb;
use sqlx::Row;

/// A minimal VIABLE spec — trigger → terminal. Reconcile sets rows
/// active, so it runs the same viability gate publish does; a
/// step-less fixture would be refused (and would never have run in
/// production either).
fn spec(kind: &str, label: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        label,
        "platform",
        vec!["account".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

/// Shared write-path actor + now: every registry write records an
/// event with a who and a when (tests are wallclock-exempt).
fn reconciler() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Automation("bootstrap-reconciler".into())
}

async fn created_by(db: &TestDb, kind: &str) -> Option<String> {
    let row = sqlx::query("SELECT created_by FROM workflows WHERE kind = $1 AND status = 'active'")
        .bind(kind)
        .fetch_optional(&db.pool)
        .await
        .expect("read created_by");
    row.map(|r| r.try_get::<String, _>("created_by").expect("decode"))
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_inserts_missing_kinds_as_bootstrap_owned() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());

    let stats = registry
        .bootstrap_reconcile(
            &[spec("workflow-design", "Design a Workflow")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("reconcile");

    assert_eq!(
        stats,
        KindReconcileStats {
            inserted: 1,
            republished: 0,
            preserved: 0,
            unchanged: 0,
            rejected: 0,
        }
    );

    let live = registry
        .get_active("workflow-design")
        .await
        .expect("active row visible");
    assert_eq!(live.label, "Design a Workflow");
    assert_eq!(live.version, 1);
    assert_eq!(live.status, WorkflowStatus::Active);
    assert_eq!(
        created_by(&db, "workflow-design").await.as_deref(),
        Some("bootstrap"),
        "fresh insert must be bootstrap-owned"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_republishes_drifted_bootstrap_rows_as_a_new_version() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());

    registry
        .bootstrap_reconcile(
            &[spec("workflow-design", "Old Label")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("seed bootstrap");

    let stats = registry
        .bootstrap_reconcile(
            &[spec("workflow-design", "New Label")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("republish");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.republished, 1);
    assert_eq!(stats.preserved, 0);
    assert_eq!(stats.unchanged, 0);

    let live = registry.get_active("workflow-design").await.unwrap();
    assert_eq!(live.label, "New Label", "drift should self-heal");
    // This asserted the opposite — that a refresh preserves the
    // version — which is exactly what defeated the pin: a Job holding
    // v1 kept resolving v1 while v1's body changed underneath it.
    assert_eq!(live.version, 2, "a changed body publishes a version");

    // The superseded version must survive, retired but readable, or a
    // Job pinned to it has nothing to resolve.
    let pinned = registry
        .get_version("workflow-design", 1)
        .await
        .expect("v1 still resolvable");
    assert_eq!(pinned.label, "Old Label");
    assert_eq!(pinned.status, WorkflowStatus::Retired);

    // One active row per kind is a unique index; a republish that left
    // two actives would fail the insert rather than corrupt the table.
    let actives: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflows WHERE kind = $1 AND status = 'active'")
            .bind("workflow-design")
            .fetch_one(&db.pool)
            .await
            .expect("count actives");
    assert_eq!(actives, 1);
    assert_eq!(
        created_by(&db, "workflow-design").await.as_deref(),
        Some("bootstrap"),
        "refresh must keep the bootstrap discriminator"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_preserves_operator_edits() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());

    // Seed an operator-owned row directly (created_by != 'bootstrap').
    sqlx::query(
        "INSERT INTO workflows
            (kind, version, status, label, description, category,
             subject_kinds, steps, metadata_schema, entitlements,
             on_complete_create, owning_team, authoring_job_id,
             created_by, created_at)
         VALUES ('workflow-design', 1, 'active', 'Operator Label', NULL, 'platform',
                 '[\"account\"]'::jsonb, '[]'::jsonb,
                 '{}'::jsonb, '{}'::jsonb, '[]'::jsonb,
                 'platform', NULL, 'emp-cto', NOW())",
    )
    .execute(&db.pool)
    .await
    .expect("seed operator row");

    let stats = registry
        .bootstrap_reconcile(
            &[spec("workflow-design", "Default Label")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("reconcile");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.republished, 0);
    assert_eq!(stats.preserved, 1);
    assert_eq!(stats.unchanged, 0);

    let live = registry.get_active("workflow-design").await.unwrap();
    assert_eq!(
        live.label, "Operator Label",
        "operator edits must survive reconcile"
    );
    assert_eq!(
        created_by(&db, "workflow-design").await.as_deref(),
        Some("emp-cto"),
        "preserve must leave created_by intact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_no_op_when_already_matching() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());

    let body = spec("workflow-design", "Design a Workflow");
    registry
        .bootstrap_reconcile(
            std::slice::from_ref(&body),
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("seed");
    let stats = registry
        .bootstrap_reconcile(&[body], &reconciler(), chrono::Utc::now())
        .await
        .expect("reconcile");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.republished, 0);
    assert_eq!(stats.preserved, 0);
    assert_eq!(stats.unchanged, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_publish_authored_supersedes_active_and_stamps_provenance() {
    let db = TestDb::new().await;
    let registry = PgWorkflows::new(db.pool.clone());

    // Seed a bootstrap row first, so the publish path actually
    // exercises the supersede branch (not just an insert).
    registry
        .bootstrap_reconcile(
            &[spec("morning-brew", "Bootstrap Label")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("seed bootstrap");

    let job_id = JobId::new();
    let published = registry
        .publish_authored(
            spec("morning-brew", "Job-Authored Label"),
            job_id,
            &boss_core::actor::ActorId::Human("emp-cto".into()),
            chrono::Utc::now(),
        )
        .await
        .expect("publish");

    assert_eq!(published.kind, "morning-brew");
    assert_eq!(published.version, 2, "supersede must bump version");
    assert_eq!(published.status, WorkflowStatus::Active);
    assert_eq!(
        published.authoring_job_id.expect("authoring stamped"),
        *job_id.inner().as_uuid(),
    );

    let live = registry.get_active("morning-brew").await.unwrap();
    assert_eq!(live.version, 2);
    assert_eq!(live.label, "Job-Authored Label");

    // The previous bootstrap-owned row is now retired.
    let v1 = registry.get_version("morning-brew", 1).await.unwrap();
    assert_eq!(v1.status, WorkflowStatus::Retired);

    // Provenance — created_by reflects the meta-Job that authored
    // this version. The bootstrap reconciler's "preserve operator
    // edits" branch keys off this string.
    let row = sqlx::query("SELECT created_by FROM workflows WHERE kind = $1 AND version = $2")
        .bind("morning-brew")
        .bind(2)
        .fetch_one(&db.pool)
        .await
        .expect("read created_by");
    let created_by: String = row.try_get("created_by").expect("decode");
    assert_eq!(
        created_by,
        format!("job-{}", job_id),
        "publish_authored must stamp created_by = `job-<authoring_job_id>`"
    );

    // Sanity check: the next bootstrap_reconcile against an updated
    // default does NOT touch the operator-published row.
    let stats = registry
        .bootstrap_reconcile(
            &[spec("morning-brew", "Updated Bootstrap Default")],
            &reconciler(),
            chrono::Utc::now(),
        )
        .await
        .expect("reconcile post-publish");
    assert_eq!(
        stats.preserved, 1,
        "publish_authored must produce an operator-owned row"
    );
    let live2 = registry.get_active("morning-brew").await.unwrap();
    assert_eq!(
        live2.label, "Job-Authored Label",
        "operator publish must survive subsequent reconcile"
    );
}
