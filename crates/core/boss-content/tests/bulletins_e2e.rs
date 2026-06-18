//! Bulletin board — integration tests against both the in-memory and
//! Postgres adapters. Keeps the port invariants honest regardless of
//! which implementation is in play.

#![cfg(feature = "postgres")]

use boss_content::types::{Audience, BulletinDraft, BulletinPatch, BulletinPriority, UserContext};
use boss_content::{ContentError, ContentRepository, InMemoryContent, PgContent};
use boss_testing::TestDb;
use chrono::NaiveDate;

fn ctx(id: &str, role: &str, department: Option<&str>) -> UserContext {
    UserContext {
        id: id.into(),
        role: role.into(),
        department: department.map(str::to_string),
    }
}

fn draft(title: &str, priority: BulletinPriority, audience: Audience) -> BulletinDraft {
    BulletinDraft {
        id: None,
        title: title.into(),
        body: format!("{title} body goes here"),
        posted_on: None,
        expires_on: None,
        priority,
        audience,
    }
}

// In-memory port-conformance tests -----------------------------------------

#[tokio::test]
async fn in_memory_create_and_list() {
    let repo = InMemoryContent::new();
    let b = repo
        .create_bulletin(
            draft("Lunch tomorrow", BulletinPriority::Normal, Audience::all()),
            "emp-001",
        )
        .await
        .unwrap();
    assert_eq!(b.title, "Lunch tomorrow");
    assert_eq!(b.actor_id, "emp-001");
    assert!(!b.dismissed_by_viewer);

    let today = chrono::Utc::now().date_naive();
    let rows = repo
        .list_bulletins_for(&ctx("emp-viewer", "cto", None), today, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, b.id);
}

#[tokio::test]
async fn in_memory_create_rejects_blank_title() {
    let repo = InMemoryContent::new();
    let bad = BulletinDraft {
        id: None,
        title: "   ".into(),
        ..draft("x", BulletinPriority::Normal, Audience::all())
    };
    let err = repo.create_bulletin(bad, "emp-001").await.unwrap_err();
    assert!(matches!(err, ContentError::Validation(_)));
}

#[tokio::test]
async fn in_memory_priority_and_recency_sort() {
    let repo = InMemoryContent::new();
    repo.create_bulletin(
        draft("older normal", BulletinPriority::Normal, Audience::all()),
        "a",
    )
    .await
    .unwrap();
    repo.create_bulletin(
        draft("newer normal", BulletinPriority::Normal, Audience::all()),
        "a",
    )
    .await
    .unwrap();
    repo.create_bulletin(
        draft("pinned", BulletinPriority::Pinned, Audience::all()),
        "a",
    )
    .await
    .unwrap();
    repo.create_bulletin(
        draft("urgent", BulletinPriority::Urgent, Audience::all()),
        "a",
    )
    .await
    .unwrap();

    let today = chrono::Utc::now().date_naive();
    let rows = repo
        .list_bulletins_for(&ctx("emp-viewer", "cto", None), today, false)
        .await
        .unwrap();
    // Urgent → Pinned → Normal (recent first within priority).
    let titles: Vec<&str> = rows.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles[0], "urgent");
    assert_eq!(titles[1], "pinned");
    // The two normals are present, and newer beats older.
    let normals: Vec<&str> = titles[2..].to_vec();
    assert_eq!(normals[0], "newer normal");
    assert_eq!(normals[1], "older normal");
}

#[tokio::test]
async fn in_memory_audience_filter_respects_department() {
    let repo = InMemoryContent::new();
    repo.create_bulletin(
        draft(
            "Field-service only",
            BulletinPriority::Normal,
            Audience(serde_json::json!({ "departments": ["field-service"] })),
        ),
        "emp-001",
    )
    .await
    .unwrap();
    repo.create_bulletin(
        draft("Everyone", BulletinPriority::Normal, Audience::all()),
        "emp-001",
    )
    .await
    .unwrap();

    let today = chrono::Utc::now().date_naive();
    let sales_rows = repo
        .list_bulletins_for(&ctx("emp-s", "sales-rep", Some("sales")), today, false)
        .await
        .unwrap();
    assert_eq!(sales_rows.len(), 1);
    assert_eq!(sales_rows[0].title, "Everyone");

    let service_rows = repo
        .list_bulletins_for(
            &ctx("emp-fs", "service-tech", Some("field-service")),
            today,
            false,
        )
        .await
        .unwrap();
    assert_eq!(service_rows.len(), 2);
}

#[tokio::test]
async fn in_memory_expiry_filters_out_old_rows() {
    let repo = InMemoryContent::new();
    let expired = BulletinDraft {
        expires_on: Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
        ..draft("January notice", BulletinPriority::Normal, Audience::all())
    };
    repo.create_bulletin(expired, "emp-001").await.unwrap();

    let today = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    let rows = repo
        .list_bulletins_for(&ctx("emp-viewer", "cto", None), today, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
async fn in_memory_dismiss_hides_for_caller_only() {
    let repo = InMemoryContent::new();
    let b = repo
        .create_bulletin(
            draft("Shared notice", BulletinPriority::Normal, Audience::all()),
            "emp-001",
        )
        .await
        .unwrap();

    repo.dismiss_bulletin(b.id, "emp-alice").await.unwrap();

    let today = chrono::Utc::now().date_naive();

    let for_alice = repo
        .list_bulletins_for(&ctx("emp-alice", "cto", None), today, false)
        .await
        .unwrap();
    assert_eq!(for_alice.len(), 0, "Alice dismissed → hidden");

    let for_bob = repo
        .list_bulletins_for(&ctx("emp-bob", "cto", None), today, false)
        .await
        .unwrap();
    assert_eq!(for_bob.len(), 1, "Bob sees it — dismissal is per-employee");

    let for_alice_included = repo
        .list_bulletins_for(&ctx("emp-alice", "cto", None), today, true)
        .await
        .unwrap();
    assert_eq!(for_alice_included.len(), 1);
    assert!(for_alice_included[0].dismissed_by_viewer);
}

#[tokio::test]
async fn in_memory_dismiss_is_idempotent() {
    let repo = InMemoryContent::new();
    let b = repo
        .create_bulletin(
            draft("x", BulletinPriority::Normal, Audience::all()),
            "emp-001",
        )
        .await
        .unwrap();
    repo.dismiss_bulletin(b.id, "emp-a").await.unwrap();
    repo.dismiss_bulletin(b.id, "emp-a").await.unwrap(); // no-op, no error
}

#[tokio::test]
async fn in_memory_update_merges_patch() {
    let repo = InMemoryContent::new();
    let b = repo
        .create_bulletin(
            draft("Orig", BulletinPriority::Normal, Audience::all()),
            "emp-001",
        )
        .await
        .unwrap();

    let updated = repo
        .update_bulletin(
            b.id,
            BulletinPatch {
                title: Some("Updated title".into()),
                priority: Some(BulletinPriority::Urgent),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.title, "Updated title");
    assert_eq!(updated.priority, BulletinPriority::Urgent);
    assert_eq!(updated.body, b.body, "body unchanged");
}

// Postgres parity ----------------------------------------------------------
// One tight round-trip against the real adapter — exercise the shape
// end-to-end. Detailed semantics are already covered by the in-memory
// tests above.

#[tokio::test(flavor = "multi_thread")]
async fn pg_round_trip_create_list_dismiss() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());

    let b = repo
        .create_bulletin(
            draft(
                "PG smoke",
                BulletinPriority::Pinned,
                Audience(serde_json::json!({ "departments": ["finance"] })),
            ),
            "emp-pg",
        )
        .await
        .unwrap();

    let today = chrono::Utc::now().date_naive();

    // Caller in finance sees it.
    let hits = repo
        .list_bulletins_for(&ctx("emp-viewer", "cfo", Some("finance")), today, false)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, b.id);
    assert_eq!(hits[0].priority, BulletinPriority::Pinned);

    // Caller outside finance does not.
    let misses = repo
        .list_bulletins_for(&ctx("emp-sales", "sales-rep", Some("sales")), today, false)
        .await
        .unwrap();
    assert_eq!(misses.len(), 0);

    // Dismiss + verify.
    repo.dismiss_bulletin(b.id, "emp-viewer").await.unwrap();
    let after_dismiss = repo
        .list_bulletins_for(&ctx("emp-viewer", "cfo", Some("finance")), today, false)
        .await
        .unwrap();
    assert_eq!(after_dismiss.len(), 0);

    // list_all stays honest — admin surface shows everything.
    let admin = repo.list_all_bulletins().await.unwrap();
    assert_eq!(admin.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_update_and_delete() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());

    let b = repo
        .create_bulletin(
            draft("Initial", BulletinPriority::Normal, Audience::all()),
            "emp-pg",
        )
        .await
        .unwrap();

    let updated = repo
        .update_bulletin(
            b.id,
            BulletinPatch {
                body: Some("New body text".into()),
                expires_on: Some(Some(NaiveDate::from_ymd_opt(2030, 1, 1).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.body, "New body text");
    assert_eq!(updated.title, b.title);
    assert!(updated.expires_on.is_some());

    repo.delete_bulletin(b.id).await.unwrap();
    let err = repo.delete_bulletin(b.id).await.unwrap_err();
    assert!(matches!(err, ContentError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_create_with_client_id_is_idempotent_on_retry() {
    use uuid::Uuid;
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());

    // Client supplies a stable id; double-submit should land one row.
    let id = Uuid::new_v4();
    let mut d = draft(
        "Retry-safe bulletin",
        BulletinPriority::Normal,
        Audience::all(),
    );
    d.id = Some(id);

    let first = repo.create_bulletin(d.clone(), "emp-pg").await.unwrap();
    let retry = repo.create_bulletin(d, "emp-pg").await.unwrap();

    assert_eq!(first.id, id);
    assert_eq!(retry.id, id, "retry returns the same row, not a new one");

    let all = repo.list_all_bulletins().await.unwrap();
    assert_eq!(
        all.iter().filter(|b| b.id == id).count(),
        1,
        "only one row landed despite two create calls"
    );
}

#[tokio::test]
async fn in_memory_create_with_client_id_is_idempotent_on_retry() {
    use uuid::Uuid;
    let repo = InMemoryContent::new();

    let id = Uuid::new_v4();
    let mut d = draft(
        "Retry-safe bulletin",
        BulletinPriority::Normal,
        Audience::all(),
    );
    d.id = Some(id);

    let first = repo.create_bulletin(d.clone(), "emp-1").await.unwrap();
    let retry = repo.create_bulletin(d, "emp-1").await.unwrap();

    assert_eq!(first.id, id);
    assert_eq!(retry.id, id);
}
