//! End-to-end: drive content (bulletins) writes through PgContent +
//! PgAuditWriter, snapshot bulletins + bulletin_dismissals, drop,
//! rebuild from `audit_log`, assert match.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use boss_content::ContentRepository;
use boss_content::PgContent;
use boss_content::rebuild_content;
use boss_content::types::*;
use boss_core::publisher::DomainPublisher;
use boss_events::PgAuditWriter;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct BulletinRow {
    id: Uuid,
    title: String,
    body: String,
    actor_id: String,
    posted_on: NaiveDate,
    expires_on: Option<NaiveDate>,
    priority: String,
    audience: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct DismissalRow {
    bulletin_id: Uuid,
    employee_id: String,
    dismissed_at: DateTime<Utc>,
}

async fn snapshot_bulletins(pool: &PgPool) -> Vec<BulletinRow> {
    sqlx::query_as("SELECT id, title, body, actor_id, posted_on, expires_on, priority, audience, created_at, updated_at FROM bulletins ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_dismissals(pool: &PgPool) -> Vec<DismissalRow> {
    sqlx::query_as("SELECT bulletin_id, employee_id, dismissed_at FROM bulletin_dismissals ORDER BY bulletin_id, employee_id")
        .fetch_all(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_bulletins_and_dismissals() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgContent::new(db.pool.clone()));
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "content")
        .with_audit(Arc::new(PgAuditWriter::new(db.pool.clone())));

    // 1. Create two bulletins.
    let now1 = Utc::now();
    let b1 = repo
        .create_bulletin_at(
            BulletinDraft {
                id: None,
                title: "Welcome".into(),
                body: "Welcome to the team".into(),
                posted_on: Some(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
                expires_on: None,
                priority: BulletinPriority::Normal,
                audience: Audience::default(),
            },
            "emp-author",
            now1,
        )
        .await
        .unwrap();
    publisher
        .emit_at(
            boss_content::events::BULLETIN_CREATED,
            serde_json::to_value(&b1).unwrap(),
            now1,
        )
        .await;

    let now2 = Utc::now();
    let b2 = repo
        .create_bulletin_at(
            BulletinDraft {
                id: None,
                title: "Holiday hours".into(),
                body: "Office closed Friday".into(),
                posted_on: Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap()),
                expires_on: Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
                priority: BulletinPriority::Pinned,
                audience: Audience::default(),
            },
            "emp-author",
            now2,
        )
        .await
        .unwrap();
    publisher
        .emit_at(
            boss_content::events::BULLETIN_CREATED,
            serde_json::to_value(&b2).unwrap(),
            now2,
        )
        .await;

    // 2. Update b1.
    let now3 = Utc::now();
    let b1u = repo
        .update_bulletin_at(
            b1.id,
            BulletinPatch {
                title: Some("Welcome (revised)".into()),
                body: None,
                expires_on: None,
                priority: Some(BulletinPriority::Urgent),
                audience: None,
            },
            now3,
        )
        .await
        .unwrap();
    publisher
        .emit_at(
            boss_content::events::BULLETIN_UPDATED,
            serde_json::to_value(&b1u).unwrap(),
            now3,
        )
        .await;

    // 3. Dismiss b1 for one employee.
    let now4 = Utc::now();
    repo.dismiss_bulletin_at(b1.id, "emp-reader", now4)
        .await
        .unwrap();
    publisher
        .emit_at(
            boss_content::events::BULLETIN_DISMISSED,
            serde_json::json!({
                "bulletin_id": b1.id,
                "employee_id": "emp-reader",
                "dismissed_at": now4,
            }),
            now4,
        )
        .await;

    // 4. Snapshot.
    let bulletins_before = snapshot_bulletins(&db.pool).await;
    let dismissals_before = snapshot_dismissals(&db.pool).await;
    assert_eq!(bulletins_before.len(), 2);
    assert_eq!(dismissals_before.len(), 1);

    // 5. Wipe + rebuild.
    let report = rebuild_content(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.bulletins_upserted, 3, "2 created + 1 updated");
    assert_eq!(report.dismissals_inserted, 1);

    // 6. Reconstructed projections must match.
    let bulletins_after = snapshot_bulletins(&db.pool).await;
    let dismissals_after = snapshot_dismissals(&db.pool).await;
    assert_eq!(bulletins_before, bulletins_after, "bulletins mismatch");
    assert_eq!(dismissals_before, dismissals_after, "dismissals mismatch");
}
