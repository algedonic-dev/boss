//! Bounded native reader fixtures, not a claim about production wait causes.
use boss_testing::repo_root;
use std::process::Command;

#[test]
fn database_identity_and_io_accounting_refuse_replacement_or_missing_evidence() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/jobs-database-test.py"))
        .output()
        .expect("python3 is required; silence is not evidence");
    assert!(
        output.status.success(),
        "database observation controls failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The fixed SQL file as written, converting only its result type for sqlx.
#[cfg(feature = "postgres")]
fn fixed_sql() -> String {
    let path = repo_root().join("infra/forge/jobs-database-observation.sql");
    let sql = std::fs::read_to_string(path).unwrap();
    format!("{}::text;", sql.trim().trim_end_matches(';'))
}

#[cfg(feature = "postgres")]
async fn observe(
    connection: &mut sqlx::pool::PoolConnection<sqlx::Postgres>,
    sql: &str,
) -> serde_json::Value {
    let raw: String = sqlx::query_scalar(sql)
        .fetch_one(&mut **connection)
        .await
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// One public.audit_log index as PostgreSQL itself reports it.
#[cfg(feature = "postgres")]
#[derive(Debug)]
struct Truth {
    name: String,
    oid: i64,
    valid: bool,
    has_stats: bool,
    view_idx_scan: Option<i64>,
}

/// Read the catalog, the cumulative-statistics store and the view
/// directly, so each row of the observation is compared with its source.
#[cfg(feature = "postgres")]
async fn truth(connection: &mut sqlx::pool::PoolConnection<sqlx::Postgres>) -> Vec<Truth> {
    let rows: Vec<(String, i64, bool, bool, Option<i64>)> = sqlx::query_as(
        "SELECT c.relname::text, c.oid::bigint, i.indisvalid, \
         pg_stat_have_stats('relation', (SELECT oid FROM pg_database WHERE datname = current_database()), c.oid), \
         s.idx_scan \
         FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
         LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = i.indexrelid \
         WHERE i.indrelid = 'public.audit_log'::regclass ORDER BY 1",
    )
    .fetch_all(&mut **connection)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(name, oid, valid, has_stats, view_idx_scan)| Truth {
            name,
            oid,
            valid,
            has_stats,
            view_idx_scan,
        })
        .collect()
}

/// Every row carries the catalog's own oid and validity, whether the
/// statistics store holds an entry, and idx_scan ONLY when it does.
#[cfg(feature = "postgres")]
fn assert_rows_match(observation: &serde_json::Value, truth: &[Truth]) {
    let rows = observation["rows"].as_array().unwrap();
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["index_name"].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = truth.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, expected, "only public.audit_log indexes: {rows:?}");
    for (row, t) in rows.iter().zip(truth) {
        assert_eq!(row["schema_name"], "public");
        assert_eq!(row["table_name"], "audit_log");
        assert_eq!(row["index_oid"].as_i64(), Some(t.oid), "oid is numeric");
        assert_eq!(row["indisvalid"].as_bool(), Some(t.valid), "{row}");
        assert_eq!(row["stats_present"].as_bool(), Some(t.has_stats), "{row}");
        if t.has_stats {
            assert_eq!(row["idx_scan"].as_i64(), t.view_idx_scan, "{row}");
        } else {
            assert!(row["idx_scan"].is_null(), "no entry is not zero: {row}");
        }
    }
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn fixed_statistics_observe_audit_indexes_only_when_their_statistics_exist() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut connection = db.pool.acquire().await.unwrap();
    // This isolated database has no production rows. A failed concurrent build
    // leaves a real invalid index rather than a fixture claiming it exists.
    connection.execute("DROP TABLE public.audit_log CASCADE; CREATE TABLE public.audit_log (id bigint PRIMARY KEY, created_at timestamptz NOT NULL); INSERT INTO public.audit_log VALUES (1,now()),(2,now()); CREATE INDEX audit_log_created_at_id ON public.audit_log (created_at,id); CREATE TABLE public.not_audit_log (id bigint PRIMARY KEY)").await.unwrap();
    assert!(connection.execute("CREATE UNIQUE INDEX CONCURRENTLY audit_log_invalid_control ON public.audit_log ((1))").await.is_err());
    let sql = fixed_sql();
    // PostgreSQL 16 does NOT grant pg_stat_have_stats to PUBLIC (measured here,
    // run a3977f41). read-jobs-database runs this file as `psql -U boss`, the
    // image's POSTGRES_USER and so its bootstrap superuser; a role that cannot
    // call it would lose the whole fixed query, so name that cause here.
    let callable: bool = sqlx::query_scalar(
        "SELECT has_function_privilege(current_user, 'pg_stat_have_stats(text,oid,oid)', 'EXECUTE')",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert!(
        callable,
        "the observer role must be able to call pg_stat_have_stats"
    );

    // 1. Nothing has scanned the two indexes built after the INSERTs, so the
    // statistics store holds no entry for them, while pg_stat_user_indexes,
    // built from the catalogs, still answers idx_scan 0 for each (review
    // 38174765 F1). That zero was never observed: the field must say
    // unavailable and name both, rather than certify an unread index unused.
    let before = truth(&mut connection).await;
    let created = before
        .iter()
        .find(|t| t.name == "audit_log_created_at_id")
        .unwrap();
    assert!(!created.has_stats, "fixture: never scanned yet: {before:?}");
    assert_eq!(created.view_idx_scan, Some(0), "the view reads it as zero");
    assert!(before.iter().any(|t| t.has_stats), "pkey took the INSERTs");
    let document = observe(&mut connection, &sql).await;
    let observation = &document["audit_log_indexes"];
    assert_eq!(observation["state"], "unavailable", "{observation}");
    let why = observation["why"].as_str().unwrap();
    for t in &before {
        assert_eq!(why.contains(t.name.as_str()), !t.has_stats, "{why}");
    }
    assert_rows_match(observation, &before);
    assert!(
        before.iter().any(|t| !t.valid),
        "the invalid control exists"
    );

    // 2. Drop the invalid index (it can never be scanned), force a real scan
    // of audit_log_created_at_id, and flush this backend's pending counters.
    // Only now does every index hold an entry, and the field reads observed.
    connection
        .execute("DROP INDEX public.audit_log_invalid_control; SET enable_seqscan = off; SET enable_bitmapscan = off")
        .await
        .unwrap();
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM public.audit_log WHERE created_at <= now() ORDER BY created_at, id",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(ids, [1, 2]);
    connection
        .execute("RESET enable_seqscan; RESET enable_bitmapscan")
        .await
        .unwrap();
    connection
        .execute("SELECT pg_stat_force_next_flush()")
        .await
        .unwrap();
    let after = truth(&mut connection).await;
    assert!(after.iter().all(|t| t.has_stats), "{after:?}");
    let created = after
        .iter()
        .find(|t| t.name == "audit_log_created_at_id")
        .unwrap();
    assert!(
        created.view_idx_scan.unwrap_or(0) >= 1,
        "the forced scan was counted: {after:?}"
    );
    let document = observe(&mut connection, &sql).await;
    let observation = &document["audit_log_indexes"];
    assert_eq!(observation["state"], "observed", "{observation}");
    assert!(observation["why"].is_null(), "{observation}");
    assert_rows_match(observation, &after);
    // stats_reset is the database's own value, read beside the counters.
    let reset: String = sqlx::query_scalar(
        "SELECT COALESCE(to_jsonb(stats_reset), 'null'::jsonb)::text FROM pg_stat_database WHERE datname = current_database()",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let reset: serde_json::Value = serde_json::from_str(&reset).unwrap();
    let object = observation.as_object().unwrap();
    assert_eq!(object.get("stats_reset"), Some(&reset), "{observation}");

    // 3. Still catalog-only: under another session's ACCESS EXCLUSIVE lock
    // on public.audit_log the query answers within the verb's lock_timeout,
    // and inside a transaction it holds no lock on the table or its indexes.
    let mut holder = db.pool.acquire().await.unwrap();
    holder
        .execute("BEGIN; LOCK TABLE public.audit_log IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    connection
        .execute(
            "BEGIN READ ONLY; SET LOCAL statement_timeout = 5000; SET LOCAL lock_timeout = 1000",
        )
        .await
        .unwrap();
    let locked = observe(&mut connection, &sql).await;
    assert_eq!(locked["audit_log_indexes"]["state"], "observed");
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
         WHERE l.pid = pg_backend_pid() AND (c.oid = 'public.audit_log'::regclass \
         OR c.oid IN (SELECT indexrelid FROM pg_index WHERE indrelid = 'public.audit_log'::regclass))",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(held, 0, "the observation locks no audit_log relation");
    connection.execute("ROLLBACK").await.unwrap();
    holder.execute("ROLLBACK").await.unwrap();
    drop(holder);

    // 4. No table, no indexes: unavailable, naming what is absent.
    connection
        .execute("DROP TABLE public.audit_log CASCADE")
        .await
        .unwrap();
    let missing = observe(&mut connection, &sql).await;
    assert_eq!(missing["audit_log_indexes"]["state"], "unavailable");
    assert!(
        missing["audit_log_indexes"]["why"]
            .as_str()
            .unwrap()
            .contains("public.audit_log")
    );
    assert_eq!(missing["audit_log_indexes"]["rows"], serde_json::json!([]));
}

/// The second fixed file, read only when the first reports the statement
/// statistics present (backlog 891e48c9).
#[cfg(feature = "postgres")]
fn statements_sql() -> String {
    let path = repo_root().join("infra/forge/jobs-database-statements.sql");
    let sql = std::fs::read_to_string(path).unwrap();
    format!("{}::text;", sql.trim().trim_end_matches(';'))
}

/// 891e48c9: temp-file spills measured at 1.55 TB a day could not be
/// attributed to a statement, because pg_stat_statements was not
/// configured and the observation did not say what it could not see. The
/// fixed observation now carries the spill counters beside the memory
/// settings that decide them, and states the statement statistics as
/// present or absent; the statements file reads the top ten writers with
/// normalized text only.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn temp_spills_and_their_statements_are_observed_or_said_absent() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut connection = db.pool.acquire().await.unwrap();
    let sql = fixed_sql();

    let preloaded: bool = sqlx::query_scalar(
        "SELECT 'pg_stat_statements' = ANY(string_to_array(replace(replace(current_setting('shared_preload_libraries'), ' ', ''), '\"', ''), ','))",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();

    // 0. AS MIGRATED: the schema creates the extension in every database
    // (20261029215507-pg-stat-statements.sql, tuning car ee905fd3), while
    // the preload is the server's own setting, so a database built from the
    // current schema has it INSTALLED whether or not its server loads the
    // library. The gate's postgres does not: gate-run b638d354 measured
    // extension_version 1.10, library_preloaded false. That is a correct
    // absent, naming shared_preload_libraries, never present and never
    // "not installed".
    let migrated: Option<String> = sqlx::query_scalar(
        "SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements'",
    )
    .fetch_optional(&mut *connection)
    .await
    .unwrap();
    let migrated = migrated.expect("the current schema creates pg_stat_statements");
    let document = observe(&mut connection, &sql).await;
    let source = &document["statements_source"];
    assert_eq!(
        source["extension_version"].as_str(),
        Some(migrated.as_str()),
        "{source}"
    );
    assert_eq!(source["library_preloaded"].as_bool(), Some(preloaded));
    if preloaded {
        assert_eq!(source["state"], "present", "{source}");
        assert!(source["why"].is_null(), "{source}");
    } else {
        assert_eq!(source["state"], "absent", "{source}");
        let why = source["why"].as_str().unwrap();
        assert!(why.contains("shared_preload_libraries"), "{source}");
        assert!(!why.contains("not installed"), "{source}");
    }

    // 1. ABSENT: dropped from this isolated database, no extension at all.
    // The spill counters and the five settings are the server's own, and
    // the source says absent, naming the missing extension.
    connection
        .execute("DROP EXTENSION pg_stat_statements")
        .await
        .unwrap();
    let document = observe(&mut connection, &sql).await;
    let io = &document["database_io"];
    let (files, bytes): (i64, i64) = sqlx::query_as(
        "SELECT temp_files, temp_bytes FROM pg_stat_database WHERE datname = current_database()",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert!(io["temp_files"].as_i64().unwrap() <= files, "{io}");
    assert!(io["temp_bytes"].as_i64().unwrap() <= bytes, "{io}");
    let settings = &document["memory_settings"];
    assert_eq!(settings["state"], "observed", "{settings}");
    let rows = settings["rows"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "hash_mem_multiplier",
            "log_temp_files",
            "shared_preload_libraries",
            "temp_tablespaces",
            "work_mem"
        ]
    );
    for row in rows {
        let truth: String = sqlx::query_scalar("SELECT setting FROM pg_settings WHERE name = $1")
            .bind(row["name"].as_str().unwrap())
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(row["setting"].as_str(), Some(truth.as_str()), "{row}");
    }
    let source = &document["statements_source"];
    assert_eq!(source["state"], "absent", "{source}");
    assert!(source["extension_version"].is_null(), "{source}");
    assert_eq!(source["library_preloaded"].as_bool(), Some(preloaded));
    assert!(
        source["why"].as_str().unwrap().contains("not installed"),
        "{source}"
    );

    // 2. INSTALLED: the extension's own version is reported. Present only
    // when the server also preloads it; otherwise absent, naming why.
    let available: Option<String> = sqlx::query_scalar(
        "SELECT default_version FROM pg_available_extensions WHERE name = 'pg_stat_statements'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert!(
        available.is_some(),
        "the gate's postgres:16 ships contrib; silence is not evidence"
    );
    connection
        .execute("CREATE EXTENSION pg_stat_statements")
        .await
        .unwrap();
    let version: String = sqlx::query_scalar(
        "SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let document = observe(&mut connection, &sql).await;
    let source = &document["statements_source"];
    assert_eq!(source["extension_version"].as_str(), Some(version.as_str()));
    if preloaded {
        assert_eq!(source["state"], "present", "{source}");
        assert!(source["why"].is_null(), "{source}");
        let top = observe(&mut connection, &statements_sql()).await;
        assert_eq!(top["state"], "observed", "{top}");
        assert!(top["rows"].as_array().unwrap().len() <= 10, "{top}");
    } else {
        assert_eq!(source["state"], "absent", "{source}");
        assert!(
            source["why"]
                .as_str()
                .unwrap()
                .contains("shared_preload_libraries"),
            "{source}"
        );
        // Not preloaded, the view errors when executed but still plans: the
        // fixed file names only columns the installed extension has.
        let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {}", statements_sql()))
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        assert!(!plan.is_empty());
    }

    // 3. PRESENT, against rows whose answer is known: session-temporary
    // relations of the view's shape shadow it (pg_temp is searched first),
    // so the file's order, limit, filter, masking and truncation run here.
    let mut fixture = db.pool.acquire().await.unwrap();
    fixture
        .execute(
            "CREATE TEMP TABLE pg_stat_statements (queryid bigint, dbid oid, calls bigint, \
             temp_blks_read bigint, temp_blks_written bigint, total_exec_time double precision, query text); \
             CREATE TEMP TABLE pg_stat_statements_info (dealloc bigint, stats_reset timestamptz); \
             INSERT INTO pg_stat_statements_info VALUES (0, now()); \
             INSERT INTO pg_stat_statements SELECT n, (SELECT oid FROM pg_database WHERE datname = current_database()), \
             n, n, n * 100, n * 1.5, 'SELECT DISTINCT ON (pid) id FROM audit_log WHERE id <= $1 /* ' || n || ' */' \
             FROM generate_series(1, 12) n; \
             INSERT INTO pg_stat_statements VALUES (100, 0, 1, 0, 0, 1.0, 'SELECT never_spilled'); \
             INSERT INTO pg_stat_statements VALUES (200, 0, 1, 0, 5000, 1.0, \
             'SET   application_name = ''secret-literal'';  SELECT ' || repeat('x', 400))",
        )
        .await
        .unwrap();
    let top = observe(&mut fixture, &statements_sql()).await;
    assert_eq!(top["state"], "observed", "{top}");
    assert_eq!(
        top["transaction_read_only"], "off",
        "the verb sets it, not the file"
    );
    let rows = top["rows"].as_array().unwrap();
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| r["queryid"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        [200, 12, 11, 10, 9, 8, 7, 6, 5, 4],
        "top ten by temp_blks_written"
    );
    let masked = rows[0]["query"].as_str().unwrap();
    assert!(!masked.contains("secret-literal"), "{masked}");
    assert!(
        masked.starts_with("SET application_name = '?'; SELECT x"),
        "{masked}"
    );
    assert_eq!(masked.chars().count(), 240, "{masked}");
    assert!(rows[0]["database"].is_null(), "dbid 0 names no database");
    assert_eq!(rows[1]["database"].as_str(), Some(db.name()));
    assert_eq!(rows[1]["temp_blks_written"].as_i64(), Some(1200));
    assert_eq!(rows[1]["calls"].as_i64(), Some(12));
    assert_eq!(rows[1]["total_exec_time"].as_f64(), Some(18.0));
    // Sorted here: jsonb orders keys by length, and serde_json may keep it.
    let mut keys: Vec<&String> = rows[1].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "calls",
            "database",
            "query",
            "query_withheld",
            "queryid",
            "temp_blks_read",
            "temp_blks_written",
            "total_exec_time"
        ]
    );
    assert_eq!(top["limited_to"].as_i64(), Some(10));
    assert!(top["info"]["stats_reset"].is_string(), "{top}");
}

/// What the statements file masks, stated once and read in two places:
/// the verb's registered `about`, which every read-jobs-database ops
/// request is approved and audited against, and the `meaning` the file
/// attaches to its own answer. Review c5ce84a8 found the about still
/// saying "No query text" (F1) and the file claiming every quoted literal
/// masked when four shapes passed through (F3); the cases below pin the
/// mechanism, and this sentence pins the claim to it.
const MASK_CLAIM: &str = "query text is lexed from its first 2048 characters and masked: single-quoted string bodies (standard, E, U& and B/X forms) and line comments are replaced by ? markers; the first dollar-quote or block-comment opener masks the rest of the text; a backslash inside a standard string masks the rest of the text; line comments end at CR or LF; double-quoted identifiers are kept";

/// All thirteen native inputs from review be75935a, including the three
/// beyond the reader's top-ten boundary. Exercise the actual SQL rather
/// than a second implementation of its masking rules.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn the_thirteen_review_shapes_fail_closed_at_the_first_opener() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut fixture = db.pool.acquire().await.unwrap();
    fixture.execute(r#"
CREATE TEMP TABLE pg_stat_statements (queryid bigint, dbid oid, calls bigint, temp_blks_read bigint, temp_blks_written bigint, total_exec_time double precision, query text);
CREATE TEMP TABLE pg_stat_statements_info (dealloc bigint, stats_reset timestamptz);
INSERT INTO pg_stat_statements_info VALUES (0, now());
CREATE TEMP TABLE review_cases (id int, q text);
INSERT INTO review_cases VALUES (1, 'COMMENT ON TABLE t1 IS $é$ SECRET-B1-repaired $é$');
INSERT INTO review_cases VALUES (2, '/* outer /* inner */ SECRET-B2-repaired ' || repeat('x', 2100) || ' */ SELECT 1');
INSERT INTO review_cases VALUES (3, '/* /*/ x */ SECRET-R1-inner-opener-slash ' || repeat('x', 2100) || ' */ SELECT 1');
INSERT INTO review_cases VALUES (4, '/* header */ DO $$ DECLARE secret_r2_body_code int := 0; BEGIN /* step */ secret_r2_body_code := secret_r2_body_code + 1; ' || repeat('PERFORM 1; ', 200) || 'END $$');
INSERT INTO review_cases VALUES (5, 'COMMENT ON TABLE t1 IS -- note' || chr(13) || '''' || chr(10) || 'SECRET-R3-string-after-lone-CR''');
INSERT INTO review_cases VALUES (6, 'DO -- note' || chr(13) || ' $$' || chr(10) || ' DECLARE secret_r3b_body_code int; BEGIN NULL; END $$ -- $$');
INSERT INTO review_cases VALUES (7, 'CREATE TABLE t7 (a text DEFAULT $a$x$a$, b text DEFAULT $b$ $a$ SECRET-R4-cross-tag $b$, c text DEFAULT $b$y$b$)');
INSERT INTO review_cases VALUES (8, 'COMMENT ON TABLE t1 IS $t$ SECRET-ctl-ascii $t$');
INSERT INTO review_cases VALUES (9, '/* header */ DO $$ DECLARE secret_ctl_short int := 0; BEGIN /* step */ secret_ctl_short := 1; END $$');
INSERT INTO review_cases VALUES (10, 'COMMENT ON TABLE t1 IS -- note' || chr(13) || chr(10) || '''SECRET-ctl-crlf''');
INSERT INTO review_cases VALUES (11, 'COMMENT ON TABLE t1 IS /* x */ ''abc */ SECRET-R5-string-after-comment''');
INSERT INTO review_cases VALUES (12, 'DO $$ BEGIN NULL; END $$ /* note */');
INSERT INTO review_cases VALUES (13, 'COMMENT ON TABLE t1 IS /* $$ x */ ''a $$ SECRET-R6-string-after-dollar-in-comment''');
INSERT INTO pg_stat_statements SELECT id, 0, 1, 0, 1000 - id, 1, q FROM review_cases;
"#).await.unwrap();
    let expected = [
        "COMMENT ON TABLE t1 IS $?$",
        "/*?*/",
        "/*?*/",
        "/*?*/",
        "COMMENT ON TABLE t1 IS /*?*/ '?'",
        "DO /*?*/ $?$",
        "CREATE TABLE t7 (a text DEFAULT $?$",
        "COMMENT ON TABLE t1 IS $?$",
        "/*?*/",
        "COMMENT ON TABLE t1 IS /*?*/ '?'",
        "COMMENT ON TABLE t1 IS /*?*/",
        "DO $?$",
        "COMMENT ON TABLE t1 IS /*?*/",
    ];
    let mut failures = Vec::new();
    for (first, count) in [(1, 10), (11, 3)] {
        if first == 11 {
            fixture
                .execute("DELETE FROM pg_stat_statements WHERE queryid <= 10")
                .await
                .unwrap();
        }
        let top = observe(&mut fixture, &statements_sql()).await;
        let rows = top["rows"].as_array().unwrap();
        assert_eq!(rows.len(), count, "{top}");
        for (index, row) in rows.iter().enumerate() {
            let id = first + index;
            assert_eq!(row["queryid"].as_u64(), Some(id as u64), "{row}");
            let wanted = expected[id - 1];
            if row["query"].as_str() != Some(wanted) || !row["query_withheld"].is_null() {
                failures.push(format!("case {id}: expected {wanted:?}, got {row}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// F1 and F3 of review c5ce84a8: the verb's contract names the query text
/// it now prints and exactly what the mask covers, in the same words as
/// the file that does the masking.
#[test]
fn the_verb_contract_states_the_statement_text_it_prints_and_its_mask() {
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/read-jobs-database.json"))
            .unwrap(),
    )
    .unwrap();
    let about = verb["about"].as_str().unwrap();
    assert!(
        !about.contains("No query text"),
        "the verb prints pg_stat_statements text: {about}"
    );
    for named in [
        "temp_statements",
        "pg_stat_statements",
        "temp_blks_written",
        "server-wide",
        "240 characters",
        MASK_CLAIM,
    ] {
        assert!(about.contains(named), "about lacks {named:?}: {about}");
    }
    let statements =
        std::fs::read_to_string(repo_root().join("infra/forge/jobs-database-statements.sql"))
            .unwrap();
    let meaning = statements
        .lines()
        .find(|line| line.trim_start().starts_with("'meaning',"))
        .unwrap();
    assert!(meaning.contains(MASK_CLAIM), "{meaning}");
    assert!(
        !statements.contains("any quoted literal"),
        "F3: the old claim"
    );
    let observation =
        std::fs::read_to_string(repo_root().join("infra/forge/jobs-database-observation.sql"))
            .unwrap();
    assert!(
        observation
            .lines()
            .next()
            .unwrap()
            .contains("jobs-database-statements.sql"),
        "the observation's no-query-text line points at the file that prints it"
    );
}

/// F3 of review c5ce84a8: the reviewer's fourteen adversarial texts
/// (run-c5ce84a8/mask-cases.sql, PostgreSQL 16.15), then the shapes the
/// lexer itself has to get right, each run through the fixed file over a
/// shadow pg_stat_statements. `None` is a text the file must withhold.
/// Only case 8 may keep SECRET: a double-quoted identifier is schema.
/// Case 1 was the reviewer's ALTER ROLE with a password literal; the
/// no-secrets lint (rightly) refuses that shape in a tracked file and
/// pins its role-password allow-list to two named files, so the same
/// shape, a utility statement keeping one literal verbatim, is a
/// COMMENT ON ROLE here.
#[cfg(feature = "postgres")]
const MASK_CASES: &[(&str, Option<&str>)] = &[
    (
        "COMMENT ON ROLE boss IS 'hunter2-SECRET'",
        Some("COMMENT ON ROLE boss IS '?'"),
    ),
    ("SELECT 'it''s SECRET' AS a", Some("SELECT '?' AS a")),
    (
        r"SELECT E'pa\'ss SECRET-TOKEN' AS a",
        Some("SELECT E'?' AS a"),
    ),
    ("SELECT $$SECRET-dollar$$ AS a", Some("SELECT $?$")),
    ("SELECT $t$SECRET tagged$t$ AS a", Some("SELECT $?$")),
    ("/* token=SECRET-comment */ SELECT 1", Some("/*?*/")),
    ("SELECT 1 -- david@SECRET.example", Some("SELECT 1 /*?*/")),
    (
        r#"SELECT "david@SECRET.example" FROM t"#,
        Some(r#"SELECT "david@SECRET.example" FROM t"#),
    ),
    (r"SELECT U&'d\0061vid SECRET' AS a", Some("SELECT U&'?'")),
    ("SELECT 'a', 'b SECRET', $1", Some("SELECT '?', '?', $1")),
    (
        r#"SELECT "a'b" = 'SECRET-after-quoted-ident'"#,
        Some(r#"SELECT "a'b" = '?'"#),
    ),
    (
        r#"COPY t FROM PROGRAM 'curl -H "Authorization: Bearer SECRET"' "#,
        Some("COPY t FROM PROGRAM '?' "),
    ),
    ("SELECT 'line1\nSECRET line2' AS a", Some("SELECT '?' AS a")),
    (
        r"SELECT E'\\' , 'SECRET-after-escaped-backslash'",
        Some("SELECT E'?' , '?'"),
    ),
    // A block-comment opener conservatively masks the entire suffix.
    ("/* a /* b */ SECRET */ SELECT 1", Some("/*?*/")),
    // A backslash in a standard string: read the same whichever way
    // standard_conforming_strings is set, by masking to the end.
    (r"SELECT 'a\'b SECRET' AS a", Some("SELECT '?'")),
    // An identifier ending in e is not an E prefix, ASCII or not.
    (r"SELECT CASE'a\' , 'SECRET'", Some("SELECT CASE'?'")),
    (r"SELECT éE'a\' , 'SECRET'", Some("SELECT éE'?'")),
    // An opener masks the suffix even when its close is beyond the cut.
    ("SELECT $$SECRET", Some("SELECT $?$")),
    ("SELECT 1 /* SECRET", Some("SELECT 1 /*?*/")),
    // Parameters and division are not quoting.
    (
        "SELECT $1, $2 FROM t WHERE x = $3 AND y = a/b",
        Some("SELECT $1, $2 FROM t WHERE x = $3 AND y = a/b"),
    ),
    // A quote inside a comment, and comment marks inside a string.
    ("SELECT 1 /* it's */ , 'SECRET'", Some("SELECT 1 /*?*/")),
    ("SELECT '/* x' , 'SECRET'", Some("SELECT '?' , '?'")),
    // A line comment ends at its newline.
    ("SELECT 1 -- c SECRET\n, 2", Some("SELECT 1 /*?*/ , 2")),
    // One dollar tag inside another.
    ("SELECT $a$ x $b$ SECRET $b$ y $a$ AS z", Some("SELECT $?$")),
];

/// Review d1c3485f B1/B2 use the actual fixed SQL over PostgreSQL,
/// rather than a second implementation of its lexer.
#[cfg(feature = "postgres")]
async fn masked_statement(text: &str) -> serde_json::Value {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut fixture = db.pool.acquire().await.unwrap();
    fixture
        .execute(
            "CREATE TEMP TABLE pg_stat_statements (queryid bigint, dbid oid, calls bigint, \
             temp_blks_read bigint, temp_blks_written bigint, total_exec_time double precision, query text); \
             CREATE TEMP TABLE pg_stat_statements_info (dealloc bigint, stats_reset timestamptz); \
             INSERT INTO pg_stat_statements_info VALUES (0, now())",
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO pg_stat_statements VALUES (1, 0, 1, 0, 1, 1.0, $1)")
        .bind(text)
        .execute(&mut *fixture)
        .await
        .unwrap();
    let top = observe(&mut fixture, &statements_sql()).await;
    assert_eq!(top["rows"].as_array().unwrap().len(), 1, "{top}");
    top["rows"][0].clone()
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn unicode_dollar_tags_never_print_their_bodies() {
    let mut failures = Vec::new();
    for text in ["SELECT $é$SECRET$é$", "DO $fé$ SECRET body $fé$"] {
        let row = masked_statement(text).await;
        let query = row["query"].as_str().unwrap_or("");
        if !row["query_withheld"].is_null() || query.contains("SECRET") || !query.contains("$?$") {
            failures.push(format!("{text}: {row}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn a_nested_comment_cut_after_its_inner_close_masks_the_suffix() {
    let text = format!(
        "/* outer /* inner */ SECRET{} */ SELECT 1",
        "x".repeat(2048)
    );
    let row = masked_statement(&text).await;
    assert_eq!(row["query"], "/*?*/", "{row}");
    assert!(row["query_withheld"].is_null(), "{row}");
    let control = masked_statement("/* outer /* inner */ SECRET */ SELECT 1").await;
    assert_eq!(control["query"], "/*?*/", "{control}");
    assert!(control["query_withheld"].is_null(), "{control}");
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn every_quoting_form_in_statement_text_is_masked_or_withheld() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut fixture = db.pool.acquire().await.unwrap();
    fixture
        .execute(
            "CREATE TEMP TABLE pg_stat_statements (queryid bigint, dbid oid, calls bigint, \
             temp_blks_read bigint, temp_blks_written bigint, total_exec_time double precision, query text); \
             CREATE TEMP TABLE pg_stat_statements_info (dealloc bigint, stats_reset timestamptz); \
             INSERT INTO pg_stat_statements_info VALUES (0, now())",
        )
        .await
        .unwrap();
    // The lexer reads only the first 2048 characters: a literal or a
    // dollar body cut there is still masked, and nothing past
    // the cut is ever read.
    let long_literal = format!("SELECT '{}', 'SECRET'", "x".repeat(3000));
    let long_dollar = format!("SELECT $${}$$, 'SECRET'", "x".repeat(3000));
    let mut cases: Vec<(&str, Option<&str>)> = MASK_CASES.to_vec();
    cases.push((long_literal.as_str(), Some("SELECT '?'")));
    cases.push((long_dollar.as_str(), Some("SELECT $?$")));
    // The file lists ten at a time, so the cases run in batches of ten.
    for batch in cases.chunks(10) {
        fixture
            .execute("DELETE FROM pg_stat_statements")
            .await
            .unwrap();
        for (index, (text, _)) in batch.iter().enumerate() {
            sqlx::query("INSERT INTO pg_stat_statements VALUES ($1, 0, 1, 0, $2, 1.0, $3)")
                .bind(index as i64)
                .bind(1000 - index as i64)
                .bind(*text)
                .execute(&mut *fixture)
                .await
                .unwrap();
        }
        let top = observe(&mut fixture, &statements_sql()).await;
        assert_eq!(top["state"], "observed", "{top}");
        let rows = top["rows"].as_array().unwrap();
        assert_eq!(rows.len(), batch.len(), "{top}");
        for (row, (text, expected)) in rows.iter().zip(batch) {
            let query = row["query"].as_str();
            assert_eq!(query, *expected, "masking {text:?}: {row}");
            match expected {
                Some(_) => assert!(row["query_withheld"].is_null(), "{row}"),
                None => assert!(
                    row["query_withheld"].as_str().unwrap().contains("2048"),
                    "{row}"
                ),
            }
            if !text.starts_with(r#"SELECT "david@"#) {
                assert!(!query.unwrap_or("").contains("SECRET"), "{text:?}: {row}");
            }
        }
    }
}

/// The optional fixed query is parsed by PostgreSQL even when its module is
/// not preloaded in the isolated test server. That refusal is not live data.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn statement_wal_fixed_sql_is_text_free_or_names_the_unloaded_module() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut connection = db.pool.acquire().await.unwrap();
    connection
        .execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .await
        .unwrap();
    let sql =
        std::fs::read_to_string(repo_root().join("infra/forge/jobs-database-statement-wal.sql"))
            .unwrap();
    let sql = format!("{}::text;", sql.trim().trim_end_matches(';'));
    let result = sqlx::query_scalar::<_, String>(&sql)
        .fetch_one(&mut *connection)
        .await;
    match result {
        Ok(raw) => {
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(value["state"], "observed");
            assert_eq!(value["source"], "pg_stat_statements(false)");
            let rows = value["rows"].as_array().unwrap();
            assert!(rows.len() <= 128);
            for row in rows {
                assert!(row.get("query").is_none());
                assert!(row["wal_bytes"].is_string());
            }
        }
        Err(sqlx::Error::Database(error)) => {
            assert_eq!(error.code().as_deref(), Some("55000"), "{error}");
            assert_eq!(
                error.message(),
                "pg_stat_statements must be loaded via shared_preload_libraries"
            );
            eprintln!(
                "statement WAL counters UNAVAILABLE in this isolated server: module not preloaded; no successful counter sample claimed"
            );
        }
        Err(error) => panic!("fixed statement WAL query failed: {error}"),
    }
}

/// Execute the unchanged fixed projection in PostgreSQL against a test-owned
/// statistics port. The largest writer sorts after the hash-ordered cutoff;
/// its decimal text also sorts before "9", so lexical DESC is insufficient.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn statement_wal_bound_keeps_the_numeric_largest_writer() {
    use sqlx::Executor;
    let db = boss_testing::TestDb::new().await;
    let mut transaction = db.pool.begin().await.unwrap();
    transaction
        .execute(
            "CREATE SCHEMA statement_wal_fixture;
             CREATE FUNCTION statement_wal_fixture.pg_stat_statements(showtext boolean)
             RETURNS TABLE(dbid oid, userid oid, queryid bigint, toplevel boolean,
                           calls bigint, wal_records bigint, wal_fpi bigint, wal_bytes numeric)
             LANGUAGE sql AS $$
               SELECT 1::oid, 2::oid,
                      CASE WHEN n = 129 THEN 99999999 ELSE n END::bigint,
                      true, 1::bigint, 1::bigint, 0::bigint,
                      CASE WHEN n = 129 THEN 1000000 ELSE 9 END::numeric
               FROM generate_series(1, 129) AS n WHERE NOT showtext
             $$;
             CREATE VIEW statement_wal_fixture.pg_stat_statements_info AS
               SELECT '2026-01-01T00:00:00Z'::timestamptz AS stats_reset, 0::bigint AS dealloc;
             SET LOCAL search_path = statement_wal_fixture, public, pg_catalog;",
        )
        .await
        .unwrap();
    let sql =
        std::fs::read_to_string(repo_root().join("infra/forge/jobs-database-statement-wal.sql"))
            .unwrap();
    let sql = format!("{}::text;", sql.trim().trim_end_matches(';'));
    let raw = sqlx::query_scalar::<_, String>(&sql)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["total"], 129);
    assert_eq!(value["returned"], 128);
    assert_eq!(value["omitted"], 1);
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 128);
    let largest = rows.iter().find(|row| row["queryid"] == "99999999");
    assert!(
        largest.is_some(),
        "the row bound omitted the largest WAL writer"
    );
    assert_eq!(largest.unwrap()["wal_bytes"], "1000000");
    assert!(rows.iter().all(|row| row.get("query").is_none()));
    transaction.rollback().await.unwrap();
}
