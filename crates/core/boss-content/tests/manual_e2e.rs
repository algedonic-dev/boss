//! Manual — in-memory + Postgres port-conformance tests.

#![cfg(feature = "postgres")]

use boss_content::types::{Audience, ManualPatch, ManualSectionDraft, UserContext};
use boss_content::{ContentError, ContentRepository, InMemoryContent, PgContent};
use boss_testing::TestDb;

fn ctx(id: &str, role: &str, department: Option<&str>) -> UserContext {
    UserContext {
        id: id.into(),
        role: role.into(),
        department: department.map(str::to_string),
    }
}

fn draft(slug: &str, parent: Option<&str>, title: &str) -> ManualSectionDraft {
    ManualSectionDraft {
        slug: slug.into(),
        parent_slug: parent.map(str::to_string),
        title: title.into(),
        body: format!("body for {title}"),
        sort_order: 0,
        audience: Audience::all(),
        published: true,
    }
}

// --- in-memory ------------------------------------------------------------

#[tokio::test]
async fn in_memory_create_and_tree() {
    let repo = InMemoryContent::new();
    let user = ctx("emp-x", "cto", None);
    repo.create_section(draft("welcome", None, "Welcome"), "hr-01")
        .await
        .unwrap();
    repo.create_section(draft("benefits", None, "Benefits"), "hr-01")
        .await
        .unwrap();
    repo.create_section(
        draft("benefits/health", Some("benefits"), "Health"),
        "hr-01",
    )
    .await
    .unwrap();

    let tree = repo.manual_tree(&user).await.unwrap();
    assert_eq!(tree.len(), 3);
    // Parent-before-children by definition of the sort (parent_slug NULLS FIRST ~ None first).
    // Both top-level slugs ("welcome", "benefits") land before the child with parent_slug="benefits".
    let first_child_idx = tree
        .iter()
        .position(|s| s.slug == "benefits/health")
        .unwrap();
    let benefits_idx = tree.iter().position(|s| s.slug == "benefits").unwrap();
    assert!(first_child_idx > benefits_idx);
}

#[tokio::test]
async fn in_memory_create_rejects_bad_parent() {
    let repo = InMemoryContent::new();
    let err = repo
        .create_section(draft("orphan", Some("no-such-parent"), "Orphan"), "hr-01")
        .await
        .unwrap_err();
    assert!(matches!(err, ContentError::Validation(_)));
}

#[tokio::test]
async fn in_memory_create_rejects_duplicate_slug() {
    let repo = InMemoryContent::new();
    repo.create_section(draft("welcome", None, "Welcome"), "hr-01")
        .await
        .unwrap();
    let err = repo
        .create_section(draft("welcome", None, "Dup"), "hr-01")
        .await
        .unwrap_err();
    assert!(matches!(err, ContentError::Validation(_)));
}

#[tokio::test]
async fn in_memory_update_bumps_version_and_history() {
    let repo = InMemoryContent::new();
    repo.create_section(draft("welcome", None, "Welcome"), "hr-01")
        .await
        .unwrap();

    let v2 = repo
        .update_section(
            "welcome",
            ManualPatch {
                body: Some("Now with actual content".into()),
                reason: Some("filling in placeholder".into()),
                ..Default::default()
            },
            "hr-01",
        )
        .await
        .unwrap();
    assert_eq!(v2.current_version, 2);
    assert_eq!(v2.body, "Now with actual content");

    let history = repo.section_history("welcome").await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 2); // newest first
    assert_eq!(history[0].reason.as_deref(), Some("filling in placeholder"));
    assert_eq!(history[1].version, 1);
}

#[tokio::test]
async fn in_memory_get_respects_audience_and_published() {
    let repo = InMemoryContent::new();
    let sales_only = ManualSectionDraft {
        audience: Audience(serde_json::json!({ "departments": ["sales"] })),
        ..draft("sales/pricing", None, "Pricing guide")
    };
    repo.create_section(sales_only, "hr-01").await.unwrap();

    let sales = ctx("emp-s", "sales-rep", Some("sales"));
    let service = ctx("emp-f", "service-tech", Some("field-service"));
    assert!(
        repo.get_section("sales/pricing", &sales)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        repo.get_section("sales/pricing", &service)
            .await
            .unwrap()
            .is_none()
    );

    repo.update_section(
        "sales/pricing",
        ManualPatch {
            published: Some(false),
            ..Default::default()
        },
        "hr-01",
    )
    .await
    .unwrap();
    assert!(
        repo.get_section("sales/pricing", &sales)
            .await
            .unwrap()
            .is_none(),
        "unpublished section hides even from audience match",
    );
}

// --- Postgres -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pg_create_update_history() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());
    let user = ctx("emp-hr", "hr-lead", Some("hr"));

    // Unique slug per test run so parallel runs don't collide.
    let slug = format!("pg-test/{}", uuid::Uuid::new_v4());
    let parent_slug = format!("pg-test-parent/{}", uuid::Uuid::new_v4());

    repo.create_section(
        ManualSectionDraft {
            slug: parent_slug.clone(),
            title: "Parent".into(),
            ..draft("_", None, "_")
        },
        "emp-hr",
    )
    .await
    .unwrap();

    let s = repo
        .create_section(
            ManualSectionDraft {
                slug: slug.clone(),
                parent_slug: Some(parent_slug.clone()),
                title: "Child".into(),
                ..draft("_", None, "_")
            },
            "emp-hr",
        )
        .await
        .unwrap();
    assert_eq!(s.current_version, 1);
    assert_eq!(s.parent_slug.as_deref(), Some(parent_slug.as_str()));

    let updated = repo
        .update_section(
            &slug,
            ManualPatch {
                title: Some("Child (v2)".into()),
                reason: Some("rename".into()),
                ..Default::default()
            },
            "emp-hr",
        )
        .await
        .unwrap();
    assert_eq!(updated.current_version, 2);
    assert_eq!(updated.title, "Child (v2)");

    let history = repo.section_history(&slug).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 2);
    assert_eq!(history[0].title, "Child (v2)");
    assert_eq!(history[1].version, 1);

    let fetched = repo.get_section(&slug, &user).await.unwrap().unwrap();
    assert_eq!(fetched.title, "Child (v2)");
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_update_missing_slug_errors() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());
    let err = repo
        .update_section(
            "no-such-section-for-sure",
            ManualPatch {
                body: Some("hi".into()),
                ..Default::default()
            },
            "emp-hr",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ContentError::NotFound(_)));
}
