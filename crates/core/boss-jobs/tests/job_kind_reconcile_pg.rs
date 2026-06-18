//! Postgres-backed coverage for `JobKindRegistry::bootstrap_reconcile`.
//!
//! The InMemory adapter is exercised in `registry::tests` (lib test).
//! This file proves the Pg adapter has matching semantics — if they
//! drift, the bootstrap loop in boss-jobs-api would silently apply
//! one branch in dev tests and a different branch in production.

#![cfg(feature = "postgres")]

use boss_core::job::JobId;
use boss_jobs::registry::{
    JobKindRegistry, JobKindSpec, JobKindStatus, KindReconcileStats, PgJobKinds,
};
use boss_testing::TestDb;
use sqlx::Row;

fn spec(kind: &str, label: &str) -> JobKindSpec {
    JobKindSpec::platform_seed(kind, label, "platform", vec!["account".into()], Vec::new())
}

async fn created_by(db: &TestDb, kind: &str) -> Option<String> {
    let row = sqlx::query("SELECT created_by FROM job_kinds WHERE kind = $1 AND status = 'active'")
        .bind(kind)
        .fetch_optional(&db.pool)
        .await
        .expect("read created_by");
    row.map(|r| r.try_get::<String, _>("created_by").expect("decode"))
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_inserts_missing_kinds_as_bootstrap_owned() {
    let db = TestDb::new().await;
    let registry = PgJobKinds::new(db.pool.clone());

    let stats = registry
        .bootstrap_reconcile(&[spec("job-kind-design", "Design a JobKind")])
        .await
        .expect("reconcile");

    assert_eq!(
        stats,
        KindReconcileStats {
            inserted: 1,
            refreshed: 0,
            preserved: 0,
            unchanged: 0,
        }
    );

    let live = registry
        .get_active("job-kind-design")
        .await
        .expect("active row visible");
    assert_eq!(live.label, "Design a JobKind");
    assert_eq!(live.version, 1);
    assert_eq!(live.status, JobKindStatus::Active);
    assert_eq!(
        created_by(&db, "job-kind-design").await.as_deref(),
        Some("bootstrap"),
        "fresh insert must be bootstrap-owned"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_refreshes_drifted_bootstrap_rows() {
    let db = TestDb::new().await;
    let registry = PgJobKinds::new(db.pool.clone());

    registry
        .bootstrap_reconcile(&[spec("job-kind-design", "Old Label")])
        .await
        .expect("seed bootstrap");
    let original_created_at = registry
        .get_active("job-kind-design")
        .await
        .unwrap()
        .created_at;

    let stats = registry
        .bootstrap_reconcile(&[spec("job-kind-design", "New Label")])
        .await
        .expect("refresh");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.refreshed, 1);
    assert_eq!(stats.preserved, 0);
    assert_eq!(stats.unchanged, 0);

    let live = registry.get_active("job-kind-design").await.unwrap();
    assert_eq!(live.label, "New Label", "drift should self-heal");
    assert_eq!(live.version, 1, "refresh must not bump version");
    assert_eq!(
        live.created_at, original_created_at,
        "refresh must preserve created_at — a fixup is not a publish event"
    );
    assert_eq!(
        created_by(&db, "job-kind-design").await.as_deref(),
        Some("bootstrap"),
        "refresh must keep the bootstrap discriminator"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_preserves_operator_edits() {
    let db = TestDb::new().await;
    let registry = PgJobKinds::new(db.pool.clone());

    // Seed an operator-owned row directly (created_by != 'bootstrap').
    sqlx::query(
        "INSERT INTO job_kinds
            (kind, version, status, label, description, category,
             subject_kinds, steps, metadata_schema, entitlements,
             on_complete_create, owning_team, authoring_job_id,
             created_by, created_at)
         VALUES ('job-kind-design', 1, 'active', 'Operator Label', NULL, 'platform',
                 '[\"account\"]'::jsonb, '[]'::jsonb,
                 '{}'::jsonb, '{}'::jsonb, '[]'::jsonb,
                 'platform', NULL, 'emp-cto', NOW())",
    )
    .execute(&db.pool)
    .await
    .expect("seed operator row");

    let stats = registry
        .bootstrap_reconcile(&[spec("job-kind-design", "Default Label")])
        .await
        .expect("reconcile");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.refreshed, 0);
    assert_eq!(stats.preserved, 1);
    assert_eq!(stats.unchanged, 0);

    let live = registry.get_active("job-kind-design").await.unwrap();
    assert_eq!(
        live.label, "Operator Label",
        "operator edits must survive reconcile"
    );
    assert_eq!(
        created_by(&db, "job-kind-design").await.as_deref(),
        Some("emp-cto"),
        "preserve must leave created_by intact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_no_op_when_already_matching() {
    let db = TestDb::new().await;
    let registry = PgJobKinds::new(db.pool.clone());

    let body = spec("job-kind-design", "Design a JobKind");
    registry
        .bootstrap_reconcile(std::slice::from_ref(&body))
        .await
        .expect("seed");
    let stats = registry
        .bootstrap_reconcile(&[body])
        .await
        .expect("reconcile");

    assert_eq!(stats.inserted, 0);
    assert_eq!(stats.refreshed, 0);
    assert_eq!(stats.preserved, 0);
    assert_eq!(stats.unchanged, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_publish_authored_supersedes_active_and_stamps_provenance() {
    let db = TestDb::new().await;
    let registry = PgJobKinds::new(db.pool.clone());

    // Seed a bootstrap row first, so the publish path actually
    // exercises the supersede branch (not just an insert).
    registry
        .bootstrap_reconcile(&[spec("morning-brew", "Bootstrap Label")])
        .await
        .expect("seed bootstrap");

    let job_id = JobId::new();
    let published = registry
        .publish_authored(spec("morning-brew", "Job-Authored Label"), job_id)
        .await
        .expect("publish");

    assert_eq!(published.kind, "morning-brew");
    assert_eq!(published.version, 2, "supersede must bump version");
    assert_eq!(published.status, JobKindStatus::Active);
    assert_eq!(
        published.authoring_job_id.expect("authoring stamped"),
        *job_id.inner().as_uuid(),
    );

    let live = registry.get_active("morning-brew").await.unwrap();
    assert_eq!(live.version, 2);
    assert_eq!(live.label, "Job-Authored Label");

    // The previous bootstrap-owned row is now retired.
    let v1 = registry.get_version("morning-brew", 1).await.unwrap();
    assert_eq!(v1.status, JobKindStatus::Retired);

    // Provenance — created_by reflects the meta-Job that authored
    // this version. The bootstrap reconciler's "preserve operator
    // edits" branch keys off this string.
    let row = sqlx::query("SELECT created_by FROM job_kinds WHERE kind = $1 AND version = $2")
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
        .bootstrap_reconcile(&[spec("morning-brew", "Updated Bootstrap Default")])
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
