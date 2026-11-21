use boss_dispatcher::rules::{delivery::Cutover, registry::parse_raw};
use std::collections::BTreeMap;
fn cutover() -> Cutover {
    Cutover {
        lane: "estate-watch".into(),
        consumer: "dispatcher-lane-estate-watch".into(),
        stream_created_ns: "1".into(),
        consumer_created_ns: "2".into(),
        created_at: "2026-10-31T09:00:00Z".into(),
        after_stream_sequence: 10,
        members: BTreeMap::from([("watch".into(), 1)]),
    }
}

// ---- reviewer probes (run e225c364), never committed ----

async fn review_probe_fixture() -> boss_testing::TestDb {
    use boss_dispatcher::rules::authoring;
    let db = boss_testing::TestDb::new().await;
    let raw = parse_raw(
        "[[rule]]\nname=\"watch\"\non_event=\"jobs.estate.observed\"\ndelivery_lane=\"estate-watch\"\n[[rule.do]]\nhandler=\"record\"\n",
    )
    .unwrap()
    .rules
    .remove(0);
    authoring::create_draft(&db.pool, &raw, None, "emp-test")
        .await
        .unwrap();
    authoring::publish(&db.pool, "watch", "emp-test")
        .await
        .unwrap();
    db
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_probe_binding_waits_for_an_open_registry_writer() {
    let db = review_probe_fixture().await;
    let mut writer = db.pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE dispatcher_rules SET delivery_lane=NULL WHERE name='watch' AND status='active'",
    )
    .execute(&mut *writer)
    .await
    .unwrap();
    let pool = db.pool.clone();
    let task = tokio::spawn(async move {
        sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
            .bind("estate-watch")
            .bind(serde_json::to_value(cutover()).unwrap())
            .execute(&pool)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let finished_while_writer_open = task.is_finished();
    writer.commit().await.unwrap();
    let result = task.await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_delivery_lanes")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    println!(
        "PROBE1 finished_while_writer_open={finished_while_writer_open} result={result:?} rows={rows}"
    );
    assert!(
        !finished_while_writer_open,
        "binding validated a snapshot while a registry writer was still open"
    );
    assert!(
        result.is_err(),
        "binding accepted a stale membership snapshot"
    );
    assert_eq!(rows, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_probe_retire_waits_for_an_open_binding_then_refuses() {
    let db = review_probe_fixture().await;
    let mut binding = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO dispatcher_delivery_lanes (name,receipt) VALUES ($1,$2)")
        .bind("estate-watch")
        .bind(serde_json::to_value(cutover()).unwrap())
        .execute(&mut *binding)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let task = tokio::spawn(async move {
        sqlx::query(
            "UPDATE dispatcher_rules SET status='retired' WHERE name='watch' AND status='active'",
        )
        .execute(&pool)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    });
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let finished_while_binding_open = task.is_finished();
    binding.commit().await.unwrap();
    let result = task.await.unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM dispatcher_rules WHERE name='watch' AND status='active' AND delivery_lane='estate-watch'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    println!(
        "PROBE2 finished_while_binding_open={finished_while_binding_open} result={result:?} active={active}"
    );
    assert!(
        !finished_while_binding_open,
        "a retire raced past an uncommitted binding"
    );
    assert!(result.is_err(), "a bound member was retired");
    assert_eq!(active, 1);
}
