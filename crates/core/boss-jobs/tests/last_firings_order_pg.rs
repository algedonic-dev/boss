use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, NEWEST_FIRST, OUTCOME_FIRED, PgDispatcherFirings,
};
use boss_testing::TestDb;
use serde_json::Value;
use sqlx::Row;

// A sort above Unique handles one row per rule. A sort below that reduction
// handles the whole retained firing history, which is the incident's spill.
fn reads_unreduced_firings(plan: &Value) -> bool {
    if matches!(plan["Node Type"].as_str(), Some("Unique" | "Aggregate")) {
        return false;
    }
    plan["Relation Name"] == "dispatcher_firings"
        || plan["Plans"]
            .as_array()
            .is_some_and(|children| children.iter().any(reads_unreduced_firings))
}

fn sorts_firing_history(plan: &Value) -> bool {
    (matches!(
        plan["Node Type"].as_str(),
        Some("Sort" | "Incremental Sort")
    ) && reads_unreduced_firings(plan))
        || plan["Plans"]
            .as_array()
            .is_some_and(|children| children.iter().any(sorts_firing_history))
}

#[tokio::test]
async fn the_production_scale_prepared_last_firings_read_does_not_sort_retained_history() {
    let db = TestDb::new().await;
    // Own one connection, so pg_prepared_statements and the repository refer
    // to the same session. Leave planner and prepared-plan selection at their
    // defaults; only the session's sort budget is reduced.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*db.pool.connect_options()).clone())
        .await
        .unwrap();
    let mut fixture = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO dispatcher_firings \
         (firing_id, rule_name, fired_on, fired_at, outcome) \
         SELECT 'plan-' || lpad(i::text, 6, '0'), \
                'rule-' || lpad((i % 64)::text, 2, '0'), 'synthetic.event', \
                TIMESTAMPTZ '2026-10-31 00:00:00Z' + (i / 128) * INTERVAL '1 second', \
                CASE WHEN i % 4 = 0 THEN 'dead-letter' ELSE 'fired' END \
         FROM generate_series(1, 75000) AS i",
    )
    .execute(&mut *fixture)
    .await
    .unwrap();
    sqlx::raw_sql("ANALYZE dispatcher_firings")
        .execute(&mut *fixture)
        .await
        .unwrap();
    // The repository owns a Pool, not a transaction port. Commit the one
    // fixture INSERT before lending it the sole connection; this is not a
    // whole-test rollback. Explicit DROP below and TestDb's failure reaper
    // release the isolated database's fixture.
    fixture.commit().await.unwrap();
    sqlx::raw_sql("SET work_mem = '64kB'")
        .execute(&pool)
        .await
        .unwrap();
    // Minimum work_mem makes this modest, narrow fixture distinguish the old
    // sort from the loose walk without exhausting the gate's in-memory PG.
    let repo = PgDispatcherFirings::new(pool.clone());
    for _ in 0..5 {
        repo.last_firings().await.unwrap();
    }
    let rows = repo.last_firings().await.unwrap();
    assert_eq!(rows.len(), 48, "dead-letter-only rules are not firings");
    assert!(rows.windows(2).all(|pair| pair[0].rule < pair[1].rule));
    // Preserve the old reader as a semantic oracle on the SAME bounded data,
    // including byte-order output. This is TestDb-only SQL, never a live probe.
    let old_sql = format!(
        "SELECT rule_name, fired_on, fired_at FROM ( \
         SELECT DISTINCT ON (rule_name) rule_name, fired_on, fired_at \
         FROM dispatcher_firings WHERE outcome = $1 AND outcome = '{OUTCOME_FIRED}' \
         ORDER BY rule_name, {NEWEST_FIRST}) newest ORDER BY rule_name COLLATE \"C\""
    );
    for _ in 0..5 {
        sqlx::query(&old_sql)
            .bind(OUTCOME_FIRED)
            .fetch_all(&pool)
            .await
            .unwrap();
    }
    let old = sqlx::query(&old_sql)
        .bind(OUTCOME_FIRED)
        .fetch_all(&pool)
        .await
        .unwrap();
    let old = old
        .into_iter()
        .map(|r| boss_jobs::dispatcher_firings::RuleLastFiring {
            rule: r.get("rule_name"),
            fired_on: r.get("fired_on"),
            fired_at: r.get("fired_at"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows, old,
        "loose traversal conserves every old reader result"
    );
    let prepared = sqlx::query(
        "SELECT name::text, cardinality(parameter_types)::int AS parameters, generic_plans, custom_plans \
         FROM pg_prepared_statements \
         WHERE statement LIKE 'WITH RECURSIVE newest AS (%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(prepared.len(), 1, "inspect the repository's actual query");
    let name: String = prepared[0].get("name");
    assert!(name.starts_with("sqlx_s_"));
    assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let parameters: i32 = prepared[0].get("parameters");
    let generic_plans: i64 = prepared[0].get("generic_plans");
    let custom_plans: i64 = prepared[0].get("custom_plans");
    assert!(generic_plans + custom_plans >= 6);
    assert!(OUTCOME_FIRED.chars().all(|c| c.is_ascii_alphabetic()));
    let arguments = match parameters {
        0 => String::new(),
        1 => format!("('{OUTCOME_FIRED}')"),
        other => panic!("unexpected repository parameter count: {other}"),
    };
    let explain = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) EXECUTE \"{name}\"{arguments}");
    let plan: Value = sqlx::query_scalar(&explain).fetch_one(&pool).await.unwrap();
    eprintln!("actual last_firings plan (generic={generic_plans}, custom={custom_plans}): {plan}");
    let old_name: String =
        sqlx::query_scalar("SELECT name::text FROM pg_prepared_statements WHERE statement = $1")
            .bind(&old_sql)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(old_name.starts_with("sqlx_s_"));
    assert!(
        old_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    );
    let old_plan: Value = sqlx::query_scalar(&format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) EXECUTE \"{old_name}\"('{OUTCOME_FIRED}')"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    eprintln!("CONTROL old last_firings plan: {old_plan}");
    let fixture_bytes: i64 =
        sqlx::query_scalar("SELECT pg_total_relation_size('dispatcher_firings')")
            .fetch_one(&pool)
            .await
            .unwrap();
    eprintln!("fixture: 75000 narrow rows, relation bytes={fixture_bytes}, session work_mem=64kB");
    sqlx::raw_sql("DROP TABLE dispatcher_firings")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    // This bounded gate fixture cannot reproduce the historical 3-million-row
    // old-reader spill. Its native plan remains separate evidence on 7e12792b;
    // this pin conserves old results and checks the NEW reader's structure.
    assert_eq!(
        plan[0]["Plan"]["Actual Rows"].as_u64(),
        Some(rows.len() as u64)
    );
    assert!(
        !sorts_firing_history(&plan[0]["Plan"]),
        "the actual prepared reader sorts unreduced dispatcher_firings: {plan}"
    );
    fn contains_sort(plan: &Value) -> bool {
        matches!(
            plan["Node Type"].as_str(),
            Some("Sort" | "Incremental Sort")
        ) || plan["Plans"]
            .as_array()
            .is_some_and(|p| p.iter().any(contains_sort))
    }
    assert!(
        !contains_sort(&plan[0]["Plan"]),
        "new reader has a Sort node: {plan}"
    );
    assert_eq!(
        plan[0]["Plan"]["Temp Written Blocks"].as_u64(),
        Some(0),
        "new reader writes temporary blocks: {plan}"
    );
}

#[test]
fn the_live_index_order_is_the_readers_order() {
    let migration = boss_testing::repo_root()
        .join("infra/postgres/schema/20261031231300-last-firings-walk-their-order.sql");
    let sql = std::fs::read_to_string(migration).expect("the ordered index migration exists");
    assert_eq!(sql.lines().next(), Some("-- migrate: no-transaction"));
    assert!(sql.contains("CREATE INDEX CONCURRENTLY dispatcher_firings_newest_fired"));
    assert!(sql.contains(&format!(
        "ON dispatcher_firings (rule_name, {NEWEST_FIRST})"
    )));
    assert!(sql.contains(&format!("WHERE outcome = '{OUTCOME_FIRED}'")));
    assert!(sql.contains("DROP INDEX CONCURRENTLY IF EXISTS dispatcher_firings_newest_fired"));
}
