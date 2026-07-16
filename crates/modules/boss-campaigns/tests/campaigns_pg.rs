//! The campaign birth contract (Q4): one transaction lands the
//! domain row, the `subjects` identity row (Q1 write-through), and
//! the `campaigns.campaign.created` outbox event (#118) — and the
//! rebuilder reproduces the domain row from the log alone. This is
//! the gap the crate exists to close: the identity-only mint wrote
//! no event, so a campaign with no Job about it yet existed live but
//! not in any rebuild.

#![cfg(feature = "postgres")]

use boss_campaigns::PgCampaigns;
use boss_campaigns::port::CampaignsRepository;
use boss_campaigns::types::Campaign;
use boss_testing::TestDb;
use chrono::{NaiveDate, TimeZone, Utc};

fn campaign(id: &str, name: &str) -> Campaign {
    Campaign {
        id: id.into(),
        name: name.into(),
        status: "active".into(),
        starts_on: NaiveDate::from_ymd_opt(2026, 7, 16),
        ends_on: None,
        metadata: serde_json::json!({"channel": "taproom"}),
        created_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_lands_row_identity_and_outbox_event_atomically() {
    let db = TestDb::new().await;
    let repo = PgCampaigns::new(db.pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, 0).unwrap();

    let inserted = repo
        .create_campaign_at(&campaign("cmp-spring-2026", "Spring Launch"), now)
        .await
        .unwrap();
    assert!(inserted);

    let (rows, identities, events): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM campaigns WHERE id = 'cmp-spring-2026'")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        sqlx::query_scalar(
            "SELECT count(*) FROM subjects WHERE kind = 'campaign' AND id = 'cmp-spring-2026'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE kind = 'campaigns.campaign.created' \
             AND payload->>'id' = 'cmp-spring-2026'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
    );
    assert_eq!((rows, identities, events), (1, 1, 1));

    // Idempotent re-create: no second row, no second event —
    // replays and the daemon's boot-time pool sync can't
    // double-write the log.
    let inserted_again = repo
        .create_campaign_at(&campaign("cmp-spring-2026", "Spring Launch RENAMED"), now)
        .await
        .unwrap();
    assert!(!inserted_again);
    let events_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'campaigns.campaign.created'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events_after, 1, "re-create must not emit");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_campaigns_from_the_log_alone() {
    let db = TestDb::new().await;

    // Two births + one duplicate created event for the same id (the
    // #128 shape: repeated events per id must not abort the rebuild;
    // newest wins).
    for (id, name) in [
        ("cmp-a", "Alpha"),
        ("cmp-b", "Beta"),
        ("cmp-a", "Alpha Renamed"),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES (gen_random_uuid(), '2026-07-16T00:00:00Z'::timestamptz, 'test', \
                     'campaigns.campaign.created', $1)",
        )
        .bind(serde_json::json!({"id": id, "name": name, "status": "active"}))
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let n = boss_campaigns::rebuild::rebuild_campaigns(&db.pool)
        .await
        .expect("rebuild must tolerate repeated events per id");
    assert_eq!(n, 2);

    let name: String = sqlx::query_scalar("SELECT name FROM campaigns WHERE id = 'cmp-a'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(name, "Alpha Renamed", "newest event wins");
}
