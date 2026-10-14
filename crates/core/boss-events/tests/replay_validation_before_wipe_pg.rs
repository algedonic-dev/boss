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

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_cannot_drop_duplicate_or_mutate_rows_before_wiping() {
    use boss_events::replay::replay_projection_planned;
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO manual_sections (id,slug,title,body) VALUES ($1,'kept','Kept','Original')",
    )
    .bind(uuid::Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO audit_log (event_id,timestamp,source,kind,payload) VALUES ($1,now(),'test','test.plan',$2)").bind(uuid::Uuid::new_v4()).bind(serde_json::json!({"fact":1})).execute(&db.pool).await.unwrap();
    for mode in 0..3 {
        let result = replay_projection_planned(
            &db.pool,
            boss_core::rebuild::lock_key("plan-test"),
            &["DELETE FROM manual_sections"],
            "kind='test.plan'",
            async |_, events| {
                let mut plan = events.to_vec();
                match mode {
                    0 => plan.clear(),
                    1 => plan.push(events[0].clone()),
                    _ => plan[0].payload = serde_json::json!({"fact":2}),
                }
                Ok(plan)
            },
            async |_, _| Ok(Applied::Yes),
        )
        .await;
        assert!(result.unwrap_err().contains("conserve every original"));
        let body: String = sqlx::query_scalar("SELECT body FROM manual_sections WHERE slug='kept'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(body, "Original");
    }
    let stats = replay_projection_checked(
        &db.pool,
        boss_core::rebuild::lock_key("plan-test"),
        &[],
        "kind='test.plan'",
        async |_, rows| {
            assert_eq!(rows.len(), 1);
            Ok(())
        },
        async |_, event| {
            assert_eq!(event.payload, serde_json::json!({"fact":1}));
            Ok(Applied::Yes)
        },
    )
    .await
    .unwrap();
    assert_eq!(stats.processed, 1);
    assert_eq!(stats.skipped, 0);
}
