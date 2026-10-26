//! A bounded read must project accepted payloads once, without a separate
//! sizing pass, while retaining PostgreSQL's exact projected byte count.
use boss_core::{audit::AuditWriter, event::Event};
use boss_events::{
    PgAuditWriter,
    history_snapshot::{MetadataKeys, read_on},
};
use boss_testing::TestDb;
use chrono::Utc;
use serde_json::json;

#[tokio::test]
async fn bounded_id_pages_keep_whole_groups_global_prefixes_and_exact_payload_bounds() {
    use boss_core::port::EventBus;
    use boss_events::{
        history_snapshot::read_paged_on,
        outbox::{drain_outbox_once, record_event_in_tx},
    };
    use boss_testing::RecordingEventBus;
    use std::sync::Arc;
    let db = TestDb::new().await;
    let now = Utc::now();
    let writer = PgAuditWriter::new(db.pool.clone());
    writer
        .write(&Event::new(
            "jobs",
            "fixture.registry",
            json!({"original":"global prefix"}),
            now,
        ))
        .await
        .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    for (kind, payload) in [
        (
            "jobs.job.updated",
            json!({"id":"p","metadata":{"keep":"first","discard":"not evaluated"}}),
        ),
        (
            "jobs.job.updated",
            json!({"id":"p","metadata":{"keep":"second","discard":"not evaluated"}}),
        ),
        (
            "fixture.sibling",
            json!({"job_id":"p","discard":"large sibling metadata is not needed"}),
        ),
    ] {
        record_event_in_tx(&mut tx, &Event::new("jobs", kind, payload, now))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &bus, 100).await.unwrap();
    for (kind, payload) in [(
        "jobs.job.updated",
        json!({"id":"q","metadata":{"keep":"unrelated"}}),
    )] {
        writer
            .write(&Event::new("jobs", kind, payload, now))
            .await
            .unwrap();
    }
    let mut conn = db.pool.acquire().await.unwrap();
    let cutoff: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT MIN(id) FROM audit_log WHERE kind='jobs.job.updated'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    let keys = vec!["keep".to_owned()];
    let metadata = MetadataKeys {
        kinds: &["jobs.job.updated"],
        keys: &keys,
        siblings: Some(&["job_id"]),
    };
    for ids in [None, Some(vec!["p".to_owned()])] {
        let original = read_on(
            &mut conn,
            cutoff,
            after,
            ids.as_deref(),
            &["jobs.job.updated"],
            &["fixture.registry"],
            metadata,
            100,
            1_000_000,
        )
        .await
        .unwrap();
        let paged = read_paged_on(
            &mut conn,
            cutoff,
            after,
            ids.as_deref(),
            &["jobs.job.updated"],
            &["fixture.registry"],
            metadata,
            100,
            1_000_000,
            1,
        )
        .await
        .unwrap();
        let signature = |rows: &[boss_events::history_snapshot::OriginalEntry]| {
            rows.iter()
                .map(|r| {
                    (
                        r.row.seq,
                        r.row.event_id,
                        r.row.kind.clone(),
                        r.row.payload.clone(),
                        r.original_group.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&paged.0), signature(&original.0));
        assert_eq!(paged.1, original.1);
        assert!(
            paged.0.iter().any(|r| r.row.seq == after),
            "original sibling before the page cut is retained"
        );
        assert!(paged.0.iter().any(|r| r.row.kind == "fixture.registry"));
        assert_eq!(
            read_paged_on(
                &mut conn,
                cutoff,
                after,
                ids.as_deref(),
                &["jobs.job.updated"],
                &["fixture.registry"],
                metadata,
                100,
                paged.1 - 1,
                1
            )
            .await
            .unwrap_err(),
            "complete native membership history exceeds byte bound"
        );
        assert_eq!(
            read_paged_on(
                &mut conn,
                cutoff,
                after,
                ids.as_deref(),
                &["jobs.job.updated"],
                &["fixture.registry"],
                metadata,
                1,
                1_000_000,
                1
            )
            .await
            .unwrap_err(),
            "complete native membership history exceeds row bound"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn projected_history_is_read_once_and_keeps_exact_byte_bounds() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    for value in ["first", "最後"] {
        writer
            .write(&Event::new(
                "history-test",
                "fixture.history",
                json!({
                    "metadata": {"keep": value, "discard": "unneeded"}
                }),
                Utc::now(),
            ))
            .await
            .unwrap();
    }
    let mut conn = db.pool.acquire().await.unwrap();
    // This wrapper lives only in this isolated TestDb. Count actual SQL
    // projection evaluations, rather than pinning a production query string.
    sqlx::raw_sql(
        "CREATE SEQUENCE history_projection_calls;
        CREATE FUNCTION public.jsonb_each(data jsonb) RETURNS TABLE(key text,value jsonb)
        LANGUAGE plpgsql VOLATILE AS $$ BEGIN
          PERFORM nextval('history_projection_calls');
          RETURN QUERY SELECT * FROM pg_catalog.jsonb_each(data);
        END $$;
        SET search_path=public,pg_catalog;",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let cutoff: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let expected = json!([
        {"metadata": {"keep": "first"}},
        {"metadata": {"keep": "最後"}}
    ]);
    let bytes: i64 = sqlx::query_scalar(
        "SELECT SUM(octet_length(value::text))::bigint FROM jsonb_array_elements($1)",
    )
    .bind(&expected)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let keys = vec!["keep".to_owned()];
    let metadata = MetadataKeys {
        kinds: &["fixture.history"],
        keys: &keys,
        siblings: None,
    };
    let (rows, observed_bytes) = read_on(
        &mut conn,
        cutoff,
        0,
        None,
        &["fixture.history"],
        &[],
        metadata,
        8,
        bytes,
    )
    .await
    .unwrap();
    assert_eq!(observed_bytes, bytes);
    assert_eq!(
        rows.iter()
            .map(|r| r.row.payload.clone())
            .collect::<Vec<_>>(),
        expected.as_array().unwrap().clone()
    );
    assert!(
        rows.windows(2)
            .all(|pair| pair[0].row.seq < pair[1].row.seq)
    );
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM history_projection_calls")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        calls, 2,
        "accepted rows must not be projected again for an upfront sizing scan"
    );
    assert_eq!(
        read_on(
            &mut conn,
            cutoff,
            0,
            None,
            &["fixture.history"],
            &[],
            metadata,
            8,
            bytes - 1
        )
        .await
        .unwrap_err(),
        "complete native membership history exceeds byte bound"
    );
    assert_eq!(
        read_on(
            &mut conn,
            cutoff,
            0,
            None,
            &["fixture.history"],
            &[],
            metadata,
            1,
            bytes
        )
        .await
        .unwrap_err(),
        "complete native membership history exceeds row bound"
    );
    assert!(
        read_on(
            &mut conn,
            0,
            0,
            None,
            &["fixture.history"],
            &[],
            metadata,
            8,
            0
        )
        .await
        .unwrap()
        .0
        .is_empty()
    );
    assert_eq!(
        read_on(
            &mut conn,
            0,
            0,
            None,
            &["fixture.history"],
            &[],
            metadata,
            8,
            -1
        )
        .await
        .unwrap_err(),
        "complete native membership history exceeds byte bound"
    );
}
