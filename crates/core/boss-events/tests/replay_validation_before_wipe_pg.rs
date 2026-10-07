//! A checked replay validates under its protected transaction before any wipe.

use boss_events::replay::{Applied, replay_projection_checked};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn validation_refusal_preserves_rows_and_rolls_back_validation_writes() {
    let db = TestDb::new().await;
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO manual_sections (id, slug, title, body) VALUES ($1,'kept','Kept','Original')",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let error = replay_projection_checked(
        &db.pool,
        boss_core::rebuild::lock_key("validation-test"),
        &["DELETE FROM manual_sections"],
        "kind = 'test.none'",
        async |conn, events| {
            assert!(events.is_empty());
            let body: String =
                sqlx::query_scalar("SELECT body FROM manual_sections WHERE slug = 'kept'")
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?;
            assert_eq!(body, "Original", "validation sees rows BEFORE the wipe");
            sqlx::query("UPDATE manual_sections SET body = 'Must roll back'")
                .execute(&mut *conn)
                .await
                .map_err(|e| e.to_string())?;
            Err("causal validation refusal".into())
        },
        async |_, _| Ok(Applied::Yes),
    )
    .await
    .expect_err("validation refuses");
    assert_eq!(error, "causal validation refusal");
    let body: String = sqlx::query_scalar("SELECT body FROM manual_sections WHERE slug = 'kept'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(body, "Original");
    replay_projection_checked(
        &db.pool,
        boss_core::rebuild::lock_key("validation-test"),
        &["DELETE FROM manual_sections"],
        "kind = 'test.none'",
        async |_, _| Ok(()),
        async |_, _| Ok(Applied::Yes),
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manual_sections")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "successful validation still permits ordinary replay"
    );
}
