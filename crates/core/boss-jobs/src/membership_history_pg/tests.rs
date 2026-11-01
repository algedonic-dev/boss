//! The bounded read against a real schema (design c195e83e). Each test
//! writes originals through the ordinary producers (the outbox, its
//! group triggers, the relay), opens the window by the DATABASE clock so
//! the cut falls between relayed rows, and reads the snapshot back.
use super::*;
use crate::membership_replay::replay;
use crate::{JobsRepository, PgJobs, StationSpec};
use boss_core::event::Event;
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::json;
use sha2::Digest;
use std::sync::Arc;

#[tokio::test]
async fn seed_timeout_counts_keep_the_original_candidate_filter_and_twenty_largest() {
    let db = TestDb::new().await;
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::query(
        "CREATE TEMP TABLE audit_log(id BIGINT,kind TEXT,payload JSONB,original_group JSONB)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let mut carried = Vec::new();
    let mut expected = Vec::new();
    for n in 1..=25_i64 {
        let id = format!("packet-{n:02}");
        carried.push(id.clone());
        sqlx::query("INSERT INTO audit_log SELECT 1,'jobs.job.updated',jsonb_build_object('id',$1::text),NULL FROM generate_series(1,$2)")
            .bind(&id).bind(n).execute(&mut *conn).await.unwrap();
        if n >= 6 {
            expected.push(json!({"packet_id":id,"historical_rows":n}));
        }
    }
    // Each would dominate the top twenty if its original predicate were lost.
    for (id, seq, kind, group) in [
        ("outside", 1, "jobs.job.updated", None),
        ("packet-01", 11, "jobs.job.updated", None),
        ("packet-02", 1, "unrelated.kind", None),
        (
            "packet-03",
            1,
            "jobs.job.updated",
            Some(json!({"id":"excluded"})),
        ),
    ] {
        sqlx::query("INSERT INTO audit_log SELECT $1,$2,jsonb_build_object('id',$3::text),$4 FROM generate_series(1,100)")
            .bind(seq).bind(kind).bind(id).bind(group).execute(&mut *conn).await.unwrap();
    }
    let actual: serde_json::Value = sqlx::query_scalar(observation::PACKET_HISTORY_COUNTS_SQL)
        .bind(&carried)
        .bind(10_i64)
        .bind(SNAPSHOT_KINDS)
        .bind(["excluded"])
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    expected.reverse();
    assert_eq!(actual, json!(expected));
    let predicate = SEED_CTES
        .split("FROM audit_log a WHERE")
        .nth(1)
        .unwrap()
        .split(") candidates")
        .next()
        .unwrap();
    assert!(
        observation::PACKET_HISTORY_COUNTS_SQL.contains(predicate),
        "the diagnostic must count the same pre-DISTINCT candidates as the unchanged source"
    );
}

#[tokio::test]
async fn a_timed_out_seed_analysis_keeps_bounded_planner_and_packet_cost_evidence() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let (job, step) = task_packet(Utc::now().date_naive());
    world.admit(&job, &[step]).await;
    world.relay().await;
    let cut: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    // Delay execution, not planning, in this isolated database. The
    // production query and its real statement timeout remain unchanged.
    sqlx::raw_sql("ALTER FUNCTION original_packet_identity(text,jsonb) RENAME TO fixture_packet_identity; CREATE FUNCTION original_packet_identity(text,jsonb) RETURNS text LANGUAGE plpgsql STABLE STRICT AS $$ BEGIN IF current_query() LIKE 'EXPLAIN (ANALYZE%' THEN PERFORM pg_sleep(0.1); END IF; RETURN fixture_packet_identity($1,$2); END $$;")
        .execute(&world.db.pool).await.unwrap();
    let mut tx = world.db.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .unwrap();
    let snapshot: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let budget =
        StatementBudget::until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .with_statement_limit(std::time::Duration::from_millis(50));
    let check = || Ok(());
    let mut report = crate::membership_observation::Observation::default();
    let why = observation::plans(
        &mut tx,
        &[job.id.to_string()],
        &[],
        &Cut {
            seq: cut,
            since: Utc::now(),
            cutoff: cut,
        },
        &Budget {
            census: 1000,
            rows: 1000,
            bytes: BYTE_BOUND,
        },
        MetadataKeys {
            kinds: PROJECTED_KINDS,
            keys: &[],
            siblings: Some(IDENTITY_KEYS),
        },
        Some(budget.checked(&check)),
        &mut report,
    )
    .await
    .unwrap_err();
    assert!(why.contains("statement timeout"), "{why}");
    let failed = report
        .phases
        .iter()
        .find(|p| p.name == "packet_rows")
        .unwrap();
    assert!(failed.plan.is_none());
    assert!(failed.why.as_ref().unwrap().contains("statement timeout"));
    let planned = report
        .phases
        .iter()
        .find(|p| p.name == "packet_rows_timeout_plan")
        .expect("timeout must conserve a planner-only plan at the same cut");
    let encoded_plan = serde_json::to_string(planned.plan.as_ref().unwrap()).unwrap();
    assert!(!encoded_plan.contains("Actual Rows"));
    assert!(!encoded_plan.contains("Execution Time"));
    assert_eq!(planned.binds_sha256, failed.binds_sha256);
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["seed_timeout"]["sqlstate"], json!("57014"));
    assert_eq!(value["seed_timeout"]["phase"], json!("packet_rows"));
    assert_eq!(value["seed_timeout"]["order_index_valid"], json!(true));
    assert_eq!(
        value["seed_timeout"]["largest_packets"],
        json!([
            {"packet_id":job.id.to_string(),"historical_rows":2}
        ])
    );
    let reusable: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        reusable, 1,
        "timeout recovery must retain the same owned transaction"
    );
    let after: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        snapshot, after,
        "fallback must use the original repeatable-read snapshot"
    );
    tx.rollback().await.unwrap();
    for invalid in [true, false] {
        sqlx::query("DROP INDEX audit_log_created_at_id")
            .execute(&world.db.pool)
            .await
            .unwrap();
        if invalid {
            // An actually failed concurrent build leaves a real INVALID
            // catalog row, rather than manufacturing the catalog answer.
            let error = sqlx::query(
                "CREATE UNIQUE INDEX CONCURRENTLY audit_log_created_at_id ON audit_log ((1))",
            )
            .execute(&world.db.pool)
            .await
            .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("23505")
            );
        }
        let mut tx = world.db.pool.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .unwrap();
        let budget =
            StatementBudget::until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .with_statement_limit(std::time::Duration::from_millis(50));
        let mut report = crate::membership_observation::Observation::default();
        let why = observation::plans(
            &mut tx,
            &[job.id.to_string()],
            &[],
            &Cut {
                seq: cut,
                since: Utc::now(),
                cutoff: cut,
            },
            &Budget {
                census: 1000,
                rows: 1000,
                bytes: BYTE_BOUND,
            },
            MetadataKeys {
                kinds: PROJECTED_KINDS,
                keys: &[],
                siblings: Some(IDENTITY_KEYS),
            },
            Some(budget.checked(&check)),
            &mut report,
        )
        .await
        .unwrap_err();
        assert!(why.contains("statement timeout"));
        let evidence = report.seed_timeout.as_ref().unwrap();
        assert_eq!(evidence.order_index_valid, invalid.then_some(false));
        assert!(
            evidence
                .largest_packets
                .as_ref()
                .is_some_and(|rows| rows.len() == 1)
        );
        let index = report
            .phases
            .iter()
            .find(|p| p.name == "seed_timeout_order_index_valid")
            .unwrap();
        assert_eq!(index.why.is_some(), !invalid);
        tx.rollback().await.unwrap();
    }
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION original_packet_identity(text,jsonb) RETURNS text LANGUAGE plpgsql STABLE STRICT AS $$ BEGIN RAISE EXCEPTION 'fixture non-timeout refusal' USING ERRCODE='22012'; END $$;")
        .execute(&world.db.pool).await.unwrap();
    let mut tx = world.db.pool.begin().await.unwrap();
    let mut report = crate::membership_observation::Observation::default();
    let why = observation::plans(
        &mut tx,
        &[job.id.to_string()],
        &[],
        &Cut {
            seq: cut,
            since: Utc::now(),
            cutoff: cut,
        },
        &Budget {
            census: 1000,
            rows: 1000,
            bytes: BYTE_BOUND,
        },
        MetadataKeys {
            kinds: PROJECTED_KINDS,
            keys: &[],
            siblings: Some(IDENTITY_KEYS),
        },
        Some(budget.checked(&check)),
        &mut report,
    )
    .await
    .unwrap_err();
    assert!(why.contains("fixture non-timeout refusal"));
    assert!(
        report.seed_timeout.is_none(),
        "another SQL error is not a timeout measurement"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn window_cut_observation_keeps_the_original_order_refusal_and_actual_cut() {
    let db = TestDb::new().await;
    let mut conn = db.pool.acquire().await.unwrap();
    // A private connection-local relation models disordered insertion
    // instants without altering any original log row or its triggers.
    sqlx::query("CREATE TEMP TABLE audit_log(id BIGINT,created_at TIMESTAMPTZ)")
        .execute(&mut *conn)
        .await
        .unwrap();
    let since = DateTime::from_timestamp(100, 0).unwrap();
    for (id, seconds) in [(1_i64, 98), (2, 101), (3, 99)] {
        sqlx::query("INSERT INTO audit_log VALUES($1,$2)")
            .bind(id)
            .bind(DateTime::from_timestamp(seconds, 0).unwrap())
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    for observed in [false, true] {
        let mut report = crate::membership_observation::Observation::default();
        let cut = window_cut(&mut conn, since, 2, None, observed.then_some(&mut report))
            .await
            .unwrap();
        assert_eq!(cut, 1);
        if observed {
            assert_eq!(report.window_cut, Some(1));
            let expected = hex::encode(sha2::Sha256::digest(
                serde_json::to_vec(&(since, 1_i64)).unwrap(),
            ));
            assert!(
                report
                    .phases
                    .iter()
                    .all(|phase| phase.binds_sha256.as_deref() == Some(&expected))
            );
        } else {
            assert!(report.phases.is_empty());
        }
        let mut report = crate::membership_observation::Observation::default();
        let why = window_cut(&mut conn, since, 3, None, observed.then_some(&mut report))
            .await
            .unwrap_err();
        assert_eq!(
            why,
            "audit insertion instants do not follow audit order at the window cut"
        );
        if observed {
            assert_eq!(report.window_cut, Some(3));
            assert_eq!(
                report.phases.last().unwrap().why.as_deref(),
                Some(why.as_str())
            );
        }
    }
}

#[tokio::test]
async fn a_source_deadline_owns_native_lock_refusal_and_rollback() {
    let db = TestDb::new().await;
    let source_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let source_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&source_pool)
        .await
        .unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK audit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pool = source_pool.clone();
    let budget = Arc::new(crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        usize::MAX,
    ));
    let mut owner = tokio::spawn(async move {
        projection_source_with_budget(
            &pool,
            Utc::now() - chrono::Duration::hours(48),
            None,
            &JobScope::All,
            None,
            250_000,
            Some(&budget),
        )
        .await
    });
    let observed = tokio::time::timeout(std::time::Duration::from_secs(5), &mut owner).await;
    // The source pool has one known backend. Read its actual state from a
    // DIFFERENT connection before releasing the lock it was waiting for.
    // A timeout that detaches that SQL would leave this exact PID waiting.
    let still_held_state = if observed.is_ok() {
        Some(sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT state,wait_event_type FROM pg_stat_activity WHERE datname=current_database() AND pid=$1",
        ).bind(source_pid).fetch_all(&db.pool).await)
    } else {
        None
    };
    // A negative implementation is cleaned up before assertions too:
    // unblock its SQL, join any retained owner, then drain the single pool.
    blocker.rollback().await.unwrap();
    let completed_inside_bound = observed.is_ok();
    let result = match observed {
        Ok(result) => result.unwrap(),
        Err(_) => owner.await.unwrap(),
    };
    let usable: i32 = sqlx::query_scalar("SELECT 7")
        .fetch_one(&source_pool)
        .await
        .unwrap();
    assert_eq!(usable, 7);
    assert!(
        completed_inside_bound,
        "native source reset/ignored its original deadline"
    );
    let activity = still_held_state.unwrap().unwrap();
    eprintln!("OWNED_SOURCE_RETURN backend_pid={source_pid} blocker_still_held_state={activity:?}");
    assert!(
        activity
            .iter()
            .all(|(_, wait)| wait.as_deref() != Some("Lock")),
        "source returned while its exact native backend still waited for the held audit lock: {activity:?}"
    );
    assert!(
        activity
            .iter()
            .all(|(state, _)| state != "idle in transaction (aborted)"),
        "returning a refusal includes completed native rollback: {activity:?}"
    );
    let why = match result {
        Ok(_) => panic!("the source must refuse the locked read"),
        Err(why) => why,
    };
    assert!(
        why.contains("statement timeout") || why.contains("lock timeout"),
        "the actual native statement, rather than an unstarted budget check, must have refused: {why}"
    );
}

#[tokio::test]
async fn original_source_budget_includes_waiting_for_a_pool_connection() {
    let db = TestDb::new().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let held = pool.acquire().await.unwrap();
    let budget = crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_millis(100),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        usize::MAX,
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        projection_source_with_budget(
            &pool,
            Utc::now() - chrono::Duration::hours(48),
            None,
            &JobScope::All,
            None,
            250_000,
            Some(&budget),
        ),
    )
    .await;
    drop(held);
    let why = result
        .expect("pool acquisition spends original source deadline")
        .err()
        .expect("pool source must refuse");
    assert!(
        why.contains("pool") && why.contains("deadline exhausted"),
        "{why}"
    );
    let held = pool.acquire().await.unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let budget = crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        cancelled.clone(),
        usize::MAX,
    );
    let read = projection_source_with_budget(
        &pool,
        Utc::now() - chrono::Duration::hours(48),
        None,
        &JobScope::All,
        None,
        250_000,
        Some(&budget),
    );
    tokio::pin!(read);
    tokio::select! {
        _ = &mut read => panic!("the known held single connection cannot be acquired"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let cancelled_result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut read).await;
    drop(held);
    let why = cancelled_result
        .expect("cancellation must not wait for the original30s source pool deadline")
        .err()
        .expect("cancelled source cannot acquire or publish");
    assert!(why.contains("cancelled"), "{why}");
}

#[tokio::test]
async fn a_source_budget_preserves_original_seed_groups_scope_and_cut() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let (mut job, step) = task_packet(Utc::now().date_naive());
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    world.update(&job).await;
    world.relay().await;
    let old = projection_source(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        None,
        250_000,
    )
    .await
    .unwrap();
    let budget = crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        usize::MAX,
    );
    let next = projection_source_with_budget(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        None,
        250_000,
        Some(&budget),
    )
    .await
    .unwrap();
    assert_eq!(next.input.audit_cutoff, old.input.audit_cutoff);
    assert_eq!(next.input.since, old.input.since);
    assert_eq!(next.input.actor, old.input.actor);
    assert_eq!(next.input.scope, old.input.scope);
    assert_eq!(
        serde_json::to_value(&next.input.groups).unwrap(),
        serde_json::to_value(&old.input.groups).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&next.input.initial).unwrap(),
        serde_json::to_value(&old.input.initial).unwrap()
    );
    let reading = replay(&next.input);
    let current = flow(&reading, "q.platform-admin.task");
    assert_eq!(current.departed, Some(1), "{:?}", current.unknown);
    assert!(next.input.registry_prefix_complete && next.input.packet_prefix_complete);
}

#[tokio::test]
async fn roster_batches_spend_one_deadline_instead_of_resetting_it() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let (job, step) = task_packet(Utc::now().date_naive());
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let values: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT DISTINCT original_group FROM audit_log WHERE original_group IS NOT NULL LIMIT 3",
    )
    .fetch_all(&world.db.pool)
    .await
    .unwrap();
    let references: Vec<GroupReference> = values
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    assert_eq!(
        references.len(),
        3,
        "actual original producer groups, not invented references"
    );
    let cutoff: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    // Only this isolated TestDb's reader is delayed. No production function
    // or real source is changed, and each group's canonical hash is retained.
    sqlx::query("ALTER FUNCTION original_group_member_hash(uuid,bigint) RENAME TO fixture_original_group_member_hash")
        .execute(&world.db.pool).await.unwrap();
    sqlx::query("CREATE FUNCTION original_group_member_hash(uuid,bigint DEFAULT NULL) RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.15); RETURN fixture_original_group_member_hash($1,$2); END $$")
        .execute(&world.db.pool).await.unwrap();
    let mut control = world.db.pool.begin().await.unwrap();
    let whole =
        boss_events::original_groups::read_groups_on(&mut control, &references, cutoff, 4096, 1)
            .await
            .unwrap();
    control.rollback().await.unwrap();
    assert_eq!(
        whole.len(),
        3,
        "all original groups remain readable without the shared time refusal"
    );
    let mut owned = world.db.pool.begin().await.unwrap();
    let budget =
        StatementBudget::until(tokio::time::Instant::now() + std::time::Duration::from_millis(350));
    let check = || Ok(());
    let result = boss_events::original_groups::read_groups_on_with_budget(
        &mut owned,
        &references,
        cutoff,
        4096,
        1,
        Some(budget.checked(&check)),
    )
    .await;
    owned.rollback().await.unwrap();
    let why = match result {
        Ok(_) => panic!("the third native roster cannot restart the original350ms budget"),
        Err(why) => why,
    };
    assert!(
        why.contains("timeout") || why.contains("budget expired"),
        "{why}"
    );
    let mut cancelled_read = world.db.pool.begin().await.unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let replay_budget = crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(3),
        cancelled.clone(),
        usize::MAX,
    );
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let check = || {
        // Four checks bracket the first header and roster statements.
        // Refuse the next batch before it dispatches another query.
        if checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 4 {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        }
        replay_budget.check()
    };
    let statement =
        StatementBudget::until(tokio::time::Instant::from_std(replay_budget.deadline()));
    let result = boss_events::original_groups::read_groups_on_with_budget(
        &mut cancelled_read,
        &references,
        cutoff,
        4096,
        1,
        Some(statement.checked(&check)),
    )
    .await;
    cancelled_read.rollback().await.unwrap();
    assert!(result.err().unwrap().contains("cancelled"));
    assert_eq!(checks.load(std::sync::atomic::Ordering::Relaxed), 5);
}

#[tokio::test]
async fn starting_state_observation_keeps_one_original_cut_and_emits_no_payload() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let (mut job, mut step) = task_packet(Utc::now().date_naive());
    job.metadata["private_fixture_payload"] = json!("never-return-this-payload");
    step.metadata["private_fixture_payload"] = json!("never-return-this-payload");
    world.admit(&job, &[step]).await;
    // Use the original manifest/chunk routing shape, not a synthetic
    // direct job_id on a manifest-only claim.
    let claim = uuid::Uuid::new_v4();
    let text = json!({"selected":{"job_id":job.id}}).to_string();
    let split = text.len() / 2;
    let now = Utc::now();
    let mut originals = vec![
        Event::new(
            "jobs",
            e::STATION_SELECTION_EVIDENCE,
            json!({"claim_event_id":claim,"index":0,"text":&text[..split]}),
            now,
        ),
        Event::new(
            "jobs",
            e::STATION_SELECTION_EVIDENCE,
            json!({"claim_event_id":claim,"index":1,"text":&text[split..]}),
            now,
        ),
        Event::new(
            "jobs",
            e::STATION_CLAIMED,
            json!({"receipt_manifest":{"version":2}}),
            now,
        ),
    ];
    originals[2].id = claim;
    let mut tx = world.db.pool.begin().await.unwrap();
    for event in &originals {
        record_event_in_tx(&mut tx, event).await.unwrap();
    }
    tx.commit().await.unwrap();
    world.relay().await;
    let since = world.open_window().await;
    let report = world
        .jobs
        .membership_starting_state_observation(
            since,
            Some("alice".into()),
            &JobScope::All,
            1000,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert_eq!(report.state, "observed", "{report:?}");
    assert!(report.until.unwrap() > since);
    assert!(report.audit_cutoff.unwrap() >= report.window_cut.unwrap());
    assert_eq!(report.carried_packets, 1);
    for name in [
        "window_cut_order_plan",
        "window_cut_order_check",
        "packet_rows",
        "manifest_claims",
        "selection_evidence",
        "full_seed",
        "stream_decode",
    ] {
        assert!(
            report.phases.iter().any(|phase| phase.name == name),
            "missing {name}: {report:?}"
        );
    }
    let encoded = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap()["since"],
        json!(since)
    );
    let order_plan = report
        .phases
        .iter()
        .find(|phase| phase.name == "window_cut_order_plan")
        .unwrap();
    let plan = order_plan.plan.as_ref().unwrap();
    assert!(plan[0]["Plan"]["Total Cost"].is_number());
    let estimated = serde_json::to_string(plan).unwrap();
    for executor_field in ["Actual Rows", "Actual Loops", "Execution Time"] {
        assert!(
            !estimated.contains(executor_field),
            "order plan must not execute a second history scan: {executor_field}"
        );
    }
    let order_check = report
        .phases
        .iter()
        .find(|phase| phase.name == "window_cut_order_check")
        .unwrap();
    assert!(order_check.plan.is_none());
    assert!(
        order_check.rows.is_none(),
        "a scalar boolean does not measure scanned rows"
    );
    assert_eq!(order_check.binds_sha256, order_plan.binds_sha256);
    assert!(
        order_plan
            .why
            .as_deref()
            .unwrap()
            .contains("planner estimates only")
    );
    assert!(!encoded.contains("never-return-this-payload"));
    assert!(!encoded.contains("private_fixture_payload"));
    assert!(
        report
            .phases
            .iter()
            .filter_map(|phase| phase.plan.as_ref())
            .all(|plan| plan.is_array())
    );
    assert!(
        report
            .phases
            .iter()
            .filter(|phase| phase.plan.is_some())
            .all(|phase| phase.sql_sha256.as_ref().unwrap().len() == 64
                && phase.binds_sha256.as_ref().unwrap().len() == 64)
    );
}

#[tokio::test]
async fn starting_state_observation_refuses_an_empty_authorization() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let since = world.open_window().await;
    let refusal = world
        .jobs
        .membership_starting_state_observation(
            since,
            Some("alice".into()),
            &JobScope::None,
            1000,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
    assert!(refusal.to_string().contains("scope"));
}

#[tokio::test]
async fn starting_state_observation_cancellation_keeps_sql_until_actual_terminal() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(world.db.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut blocker = world.db.pool.begin().await.unwrap();
    sqlx::query("LOCK audit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancelled.clone();
    let owned_pool = pool.clone();
    let owner = tokio::spawn(async move {
        observation::observe_bounded(
            &owned_pool,
            Utc::now() - chrono::Duration::hours(24),
            Some("alice".into()),
            &JobScope::All,
            1000,
            std::time::Duration::from_secs(10),
            flag,
        )
        .await
    });
    let entered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wait: Option<String> =
                sqlx::query_scalar("SELECT wait_event_type FROM pg_stat_activity WHERE pid=$1")
                    .bind(pid)
                    .fetch_one(&world.db.pool)
                    .await
                    .unwrap();
            if wait.as_deref() == Some("Lock") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    let retained = !owner.is_finished();
    blocker.rollback().await.unwrap();
    let report = owner.await.unwrap().unwrap();
    assert!(
        entered.is_ok(),
        "the actual known backend must enter the held SQL first"
    );
    assert!(
        retained,
        "cooperative cancellation cannot detach the actual blocked query"
    );
    assert_eq!(report.state, "unavailable");
    assert!(
        report.why.as_ref().unwrap().contains("cancelled"),
        "{report:?}"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid=$1")
        .bind(pid)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert_eq!(
        state, "idle",
        "return requires the actual owned rollback to finish"
    );
}

#[tokio::test]
async fn starting_state_observation_preserves_partial_plans_and_owns_timeout_cleanup() {
    let world = World::new(&station("q.platform-admin.task")).await;
    let (job, step) = task_packet(Utc::now().date_naive());
    world.admit(&job, &[step]).await;
    let now = Utc::now();
    let claim = Event::new(
        "jobs",
        e::STATION_CLAIMED,
        json!({"receipt_manifest":{"version":2}}),
        now,
    );
    let mut tx = world.db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &claim).await.unwrap();
    tx.commit().await.unwrap();
    world.relay().await;
    let since = world.open_window().await;
    // Delay only the isolated fixture's manifest decoder, after the
    // packet_rows plan can complete. No production setting is changed.
    sqlx::raw_sql("ALTER FUNCTION original_claim_packet_identity(jsonb,uuid,bigint,boolean) RENAME TO fixture_claim_packet_identity; CREATE FUNCTION original_claim_packet_identity(jsonb,uuid,bigint,boolean DEFAULT FALSE) RETURNS text LANGUAGE plpgsql STABLE STRICT AS $$ BEGIN PERFORM pg_sleep(1); RETURN fixture_claim_packet_identity($1,$2,$3,$4); END $$;")
        .execute(&world.db.pool).await.unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(world.db.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let report = observation::observe_bounded(
        &pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        1000,
        std::time::Duration::from_millis(300),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert_eq!(report.state, "unavailable");
    assert!(
        report.why.as_ref().unwrap().contains("timeout"),
        "{report:?}"
    );
    assert!(
        report
            .phases
            .iter()
            .any(|phase| phase.name == "packet_rows" && phase.plan.is_some()),
        "completed original plan must survive: {report:?}"
    );
    assert!(
        report
            .phases
            .iter()
            .any(|phase| phase.name == "manifest_claims" && phase.why.is_some()),
        "{report:?}"
    );
    assert!(report.audit_cutoff.unwrap() >= report.window_cut.unwrap());
    for name in ["window_cut_order_plan", "window_cut_order_check"] {
        assert!(
            report.phases.iter().any(|phase| phase.name == name),
            "earlier order evidence must survive later timeout: {report:?}"
        );
    }
    let incomplete = report.seed_timeout.as_ref().unwrap();
    assert!(
        incomplete.largest_packets.is_none(),
        "expired evidence is not an empty count"
    );
    for name in [
        "manifest_claims_timeout_plan",
        "seed_timeout_order_index_valid",
        "seed_timeout_largest_packets",
    ] {
        let phase = report
            .phases
            .iter()
            .find(|phase| phase.name == name)
            .unwrap();
        assert!(phase.plan.is_none());
        assert!(
            phase
                .why
                .as_ref()
                .is_some_and(|why| why.contains("deadline") || why.contains("expired"))
        );
    }
    let state: String = sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid=$1")
        .bind(pid)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert_eq!(
        state, "idle",
        "actual transaction rollback must finish before returning report"
    );
    let setting: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(setting, "0", "LOCAL diagnostic settings cannot leak");
}

fn station(name: &str) -> StationSpec {
    let source: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    serde_json::from_value(
        source["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone(),
    )
    .unwrap()
}

struct World {
    db: TestDb,
    jobs: PgJobs,
    bus: Arc<dyn EventBus>,
}

#[tokio::test]
async fn known_clock_pruned_packets_do_not_spend_the_shared_seed_budget() {
    let mut spec = station("q.platform-admin.task");
    spec.predicate.status = Some(JobStatus::Closed);
    spec.terminal_window_days = Some(0);
    let world = World::new(&spec).await;
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    let today = Utc::now().date_naive();
    for _ in 0..3 {
        let (mut job, _) = task_packet(today);
        job.status = JobStatus::Closed;
        job.closed_on = Some(today - chrono::Duration::days(2));
        world.admit(&job, &[]).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(input.initial.pruned_terminal, 3);
    assert!(
        !input
            .clock
            .unknown
            .iter()
            .any(|gap| gap.since < input.until && since < gap.until)
    );
    let mut conn = world.db.pool.acquire().await.unwrap();
    let bounded = starting_state(
        &mut conn,
        Cut {
            seq: input.initial.cut,
            since,
            cutoff: input.audit_cutoff,
        },
        None,
        &input.groups,
        &input.clock,
        Budget {
            census: 4096,
            rows: 0,
            bytes: 0,
        },
        MetadataKeys {
            kinds: PROJECTED_KINDS,
            keys: &[],
            siblings: Some(IDENTITY_KEYS),
        },
        &[Retention {
            days: 0,
            predicate: Some(spec.predicate.clone()),
        }],
        Some("alice"),
        &JobScope::All,
        input.until,
        None,
        None,
    )
    .await
    .expect("known-clock pruning needs no retained witness or seed bytes");
    assert_eq!(bounded.pruned_terminal, 3);
    assert!(bounded.pruned_census.is_none());
    assert!(bounded.rows.is_empty());
    assert!(input.initial.pruned_census.is_none());
    // Merely having an unknown span is insufficient: neither adjoining
    // half-open boundary intersects this source window.
    let mut disjoint = input.clock.clone();
    for (start, end) in [
        (since - chrono::Duration::seconds(1), since),
        (input.until, input.until + chrono::Duration::seconds(1)),
    ] {
        disjoint
            .unknown
            .push(boss_clock_client::mapping_history::UnknownClockSpan {
                since: start,
                until: end,
                reason: "disjoint fixture interval".into(),
            });
    }
    let bounded = starting_state(
        &mut conn,
        Cut {
            seq: input.initial.cut,
            since,
            cutoff: input.audit_cutoff,
        },
        None,
        &input.groups,
        &disjoint,
        Budget {
            census: 4096,
            rows: 0,
            bytes: 0,
        },
        MetadataKeys {
            kinds: PROJECTED_KINDS,
            keys: &[],
            siblings: Some(IDENTITY_KEYS),
        },
        &[Retention {
            days: 0,
            predicate: Some(spec.predicate.clone()),
        }],
        Some("alice"),
        &JobScope::All,
        input.until,
        None,
        None,
    )
    .await
    .expect("disjoint unknown spans cannot charge this window for a witness");
    assert_eq!(bounded.pruned_terminal, 3);
    assert!(bounded.pruned_census.is_none());
}

#[tokio::test]
async fn original_pruned_census_keeps_the_complete_sorted_roster_without_registry_copies() {
    for population in [0, 3] {
        let mut spec = station("q.platform-admin.task");
        spec.predicate.status = Some(JobStatus::Closed);
        spec.terminal_window_days = Some(0);
        let world = World::new(&spec).await;
        sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
            .execute(&world.db.pool)
            .await
            .unwrap();
        let today = Utc::now().date_naive();
        let mut expected = Vec::new();
        for _ in 0..population {
            let (mut job, _) = task_packet(today);
            job.status = JobStatus::Closed;
            job.closed_on = Some(today - chrono::Duration::days(2));
            expected.push(job.id.to_string());
            world.admit(&job, &[]).await;
        }
        world.relay().await;
        let since = world.open_window().await;
        sqlx::query("UPDATE clock_mapping_mode SET epoch=gen_random_uuid() WHERE id=1")
            .execute(&world.db.pool)
            .await
            .unwrap();
        world.relay().await;
        let input = snapshot(
            &world.db.pool,
            since,
            Some("alice".into()),
            &JobScope::All,
            4096,
        )
        .await
        .unwrap();
        assert_eq!(input.initial.pruned_terminal, population);
        assert!(
            input
                .clock
                .unknown
                .iter()
                .any(|gap| gap.since < input.until && since < gap.until)
        );
        if population == 0 {
            assert!(input.initial.pruned_census.is_none());
        } else {
            expected.sort();
            let witness = input.initial.pruned_census.as_ref().unwrap();
            let ids: Vec<_> = witness.packets.iter().map(|p| p.id.clone()).collect();
            assert_eq!(ids, expected);
            assert_eq!(
                ids.iter().collect::<BTreeSet<_>>().len(),
                population as usize
            );
            assert!(
                serde_json::to_value(witness)
                    .unwrap()
                    .get("registry")
                    .is_none(),
                "registry originals already live in input.groups and must not be copied into the census"
            );
        }
    }
}

/// 2eaa uses the already-read Q3 original census, not a newly selected
/// transaction UUID. A quiet expired packet stays excluded under both
/// original WALL models while their activation remains unknown.
#[tokio::test]
async fn complete_original_pruned_census_proves_a_quiet_expired_outcome() {
    let mut spec = station("q.platform-admin.task");
    spec.name = "expired-census.fixture".into();
    spec.predicate = crate::station_queue::StationPredicate {
        kind: Some("fixture".into()),
        status: Some(JobStatus::Closed),
        ..Default::default()
    };
    spec.terminal_window_days = Some(0);
    let world = World::new(&spec).await;
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    let today = Utc::now().date_naive();
    let (mut job, _) = task_packet(today);
    job.status = JobStatus::Closed;
    job.closed_on = Some(today - chrono::Duration::days(2));
    job.tags = vec!["café".into()];
    world.admit(&job, &[]).await;
    world.relay().await;
    let since = world.open_window().await;
    sqlx::query("UPDATE clock_mapping_mode SET epoch=gen_random_uuid() WHERE id=1")
        .execute(&world.db.pool)
        .await
        .unwrap();
    world.relay().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(input.initial.pruned_terminal, 1);
    assert!(input.groups.iter().flat_map(|g| &g.rows).all(|row| {
        crate::membership_replay::packet_identity(row).as_deref()
            != Some(job.id.to_string().as_str())
    }));
    let gap = input
        .clock
        .unknown
        .iter()
        .find(|gap| gap.since >= since)
        .unwrap();
    assert!(input.clock.date_at(gap.since).is_err());
    assert!(
        input
            .clock
            .outcome_date_bounds(gap.since, gap.until, input.audit_cutoff)
            .is_ok()
    );
    let reading = replay(&input);
    let counted = flow(&reading, &spec.name);
    assert!(
        counted.unknown.is_empty(),
        "complete original pruned census should prove this expired outcome: {:?}",
        counted.unknown
    );
    assert_eq!(counted.arrived, Some(0));
    assert_eq!(counted.departed, Some(0));
    let witness = input.initial.pruned_census.as_ref().unwrap();
    let witness_bytes = serde_json::to_vec(witness).unwrap().len() as i64;
    let witness_rows = witness.packets.len() as i64;
    assert!(serde_json::to_string(witness).unwrap().contains("café"));
    let retaining = [Retention {
        days: 0,
        predicate: Some(spec.predicate.clone()),
    }];
    let mut conn = world.db.pool.acquire().await.unwrap();
    for (rows, bytes, refusal) in [
        (witness_rows, witness_bytes, None),
        (witness_rows - 1, witness_bytes, Some("row bound")),
        (witness_rows, witness_bytes - 1, Some("byte bound")),
    ] {
        let bounded = starting_state(
            &mut conn,
            Cut {
                seq: input.initial.cut,
                since,
                cutoff: input.audit_cutoff,
            },
            None,
            &input.groups,
            &input.clock,
            Budget {
                census: 4096,
                rows,
                bytes,
            },
            MetadataKeys {
                kinds: PROJECTED_KINDS,
                keys: &[],
                siblings: Some(IDENTITY_KEYS),
            },
            &retaining,
            Some("alice"),
            &JobScope::All,
            input.until,
            None,
            None,
        )
        .await;
        if let Some(reason) = refusal {
            assert!(bounded.unwrap_err().contains(reason));
        } else {
            assert_eq!(
                serde_json::to_value(bounded.unwrap().pruned_census).unwrap(),
                serde_json::to_value(&input.initial.pruned_census).unwrap()
            );
        }
    }
    drop(conn);
    let projection = crate::membership_replay::projection::MembershipProjection::rebuild(&input)
        .expect("complete pruned census must support a native checkpoint");
    let checkpoint = serde_json::to_value(&projection).unwrap();
    assert_eq!(
        checkpoint["schema"], 2,
        "legacy checkpoints cannot claim original census evidence"
    );
    assert_eq!(
        checkpoint["pruned_census"],
        serde_json::to_value(&input.initial.pruned_census).unwrap()
    );
    let binding = crate::membership_replay::projection::ProjectionBinding::from_input(&input);
    let mut restored: crate::membership_replay::projection::MembershipProjection =
        serde_json::from_value(checkpoint.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(restored.read(since, &binding).unwrap()).unwrap(),
        serde_json::to_value(&reading).unwrap()
    );
    let later = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    let mut delta = later.clone();
    delta.since = input.until;
    delta.initial = Default::default();
    delta
        .groups
        .retain(|group| group.rows.iter().any(|row| row.seq > input.audit_cutoff));
    let mut changed_authorization = delta.clone();
    let service_source = projection_source(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        Some(crate::membership_projection::ProjectionCursor {
            until: input.until,
            audit_cutoff: input.audit_cutoff,
            currently_authorized: input.currently_authorized.clone(),
        }),
        4096,
    )
    .await
    .unwrap();
    assert!(service_source.replacement_ids.is_empty());
    let service_budget = crate::membership_replay::ReplayBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        usize::MAX,
    );
    let mut service_projection = projection.clone();
    service_projection.reconcile_checked(&service_source.input,
        service_source.replacement_history.as_ref(), &service_source.replacement_ids,
        &service_budget).expect("the actual service reconcile path must preserve the original full authorization binding");
    assert_eq!(
        serde_json::to_value(&service_projection).unwrap()["pruned_census"],
        checkpoint["pruned_census"]
    );
    let (live, _) = task_packet(today);
    world.admit(&live, &[]).await;
    world.relay().await;
    let full_after_live = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    let mut live_delta = full_after_live.clone();
    live_delta.since = service_source.input.until;
    live_delta.initial = Default::default();
    live_delta.groups.retain(|group| {
        group
            .rows
            .iter()
            .any(|row| row.seq > service_source.input.audit_cutoff)
    });
    let replacement_ids: BTreeSet<String> = [live.id.to_string()].into_iter().collect();
    // Exercise the port's selected-history reconciliation using native
    // originals. Selection is explicit here, not claimed as a late-relay event.
    let mut selected_history = full_after_live.clone();
    selected_history.currently_authorized = Some(replacement_ids.clone());
    selected_history.initial.pruned_terminal = 0;
    selected_history.initial.pruned_census = None;
    let mut replacement_projection = service_projection.clone();
    replacement_projection
        .reconcile_checked(
            &live_delta,
            Some(&selected_history),
            &replacement_ids,
            &service_budget,
        )
        .expect("unpruned native replacement must preserve the full caller binding");
    assert_eq!(
        serde_json::to_value(&replacement_projection).unwrap()["pruned_census"],
        checkpoint["pruned_census"]
    );
    assert_eq!(
        serde_json::to_value(
            replacement_projection
                .read(since, &binding)
                .unwrap()
                .stations
        )
        .unwrap(),
        serde_json::to_value(replay(&full_after_live).stations).unwrap()
    );
    changed_authorization.currently_authorized = Some(Default::default());
    let mut authorization_control = projection.clone();
    let before = serde_json::to_value(&authorization_control).unwrap();
    assert!(
        authorization_control
            .reconcile(&changed_authorization, None, &Default::default())
            .unwrap_err()
            .to_string()
            .contains("pruned census authorization changed")
    );
    assert_eq!(
        serde_json::to_value(&authorization_control).unwrap(),
        before
    );
    let mut touched = delta.clone();
    let mut touched_row = input.groups[0].rows[0].clone();
    touched_row.seq = input.audit_cutoff + 1;
    touched_row.created_at = Some(input.until);
    touched_row.event.id = uuid::Uuid::new_v4();
    touched_row.event.kind = crate::events::JOB_UPDATED.into();
    touched_row.event.payload = serde_json::to_value(&job).unwrap();
    touched.audit_cutoff = touched_row.seq;
    touched.groups = vec![crate::membership_replay::ReplayGroup {
        rows: vec![touched_row],
        original: None,
    }];
    let mut touched_control = projection.clone();
    assert!(
        touched_control
            .advance(&touched)
            .unwrap_err()
            .to_string()
            .contains("touches an originally pruned packet")
    );
    assert_eq!(serde_json::to_value(&touched_control).unwrap(), before);
    restored
        .advance(&delta)
        .expect("unchanged original census must survive an empty delta");
    let continued = serde_json::to_value(&restored).unwrap();
    assert_eq!(
        continued["pruned_census"], checkpoint["pruned_census"],
        "continuation must preserve original source cut and window, not relabel it"
    );
    let continued_reading = restored.read(since, &binding).unwrap();
    let expected = replay(&later);
    assert_eq!(
        serde_json::to_value(&continued_reading).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let mut missing = checkpoint.clone();
    missing.as_object_mut().unwrap().remove("pruned_census");
    let missing: crate::membership_replay::projection::MembershipProjection =
        serde_json::from_value(missing).unwrap();
    assert!(
        missing.read(since, &binding).is_err(),
        "a certificate without its original census must rebuild"
    );
    let mut legacy = checkpoint;
    legacy["schema"] = 1.into();
    legacy.as_object_mut().unwrap().remove("pruned_census");
    let legacy: crate::membership_replay::projection::MembershipProjection =
        serde_json::from_value(legacy).unwrap();
    assert!(
        legacy.read(since, &binding).is_err(),
        "legacy generation must rebuild"
    );
}

impl World {
    /// The clock fact first, so every instant of a later window maps to
    /// a known business date; then the original registry.
    async fn new(spec: &StationSpec) -> Self {
        Self::with_db(spec, TestDb::new().await).await
    }

    async fn with_db(spec: &StationSpec, db: TestDb) -> Self {
        let jobs = PgJobs::new(db.pool.clone());
        let bus: Arc<dyn EventBus> = RecordingEventBus::new();
        sqlx::query(
            "INSERT INTO clock_mapping_mode(id,mode,epoch) VALUES(1,'wall',gen_random_uuid())",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let workflow = crate::registry::WorkflowSpec::platform_seed(
            "fixture",
            "Original",
            "fixture",
            vec!["custom".into()],
            vec![],
        );
        let now = chrono::Utc::now();
        let mut tx = db.pool.begin().await.unwrap();
        for event in [
            Event::new("jobs", e::STATION_PUBLISHED, json!(spec), now),
            Event::new("jobs", e::WORKFLOW_PUBLISHED, json!(workflow), now),
        ] {
            record_event_in_tx(&mut tx, &event).await.unwrap();
        }
        tx.commit().await.unwrap();
        let world = Self { db, jobs, bus };
        world.relay().await;
        world
    }

    async fn relay(&self) {
        drain_outbox_once(&self.db.pool, &self.bus, 10_000)
            .await
            .unwrap();
    }

    async fn admit(&self, job: &Job, steps: &[Step]) {
        let now = chrono::Utc::now();
        self.jobs
            .create_job_at(
                job,
                now,
                &[Event::new("jobs", e::JOB_CREATED, json!(job), now)],
            )
            .await
            .unwrap();
        for step in steps {
            self.jobs
                .add_step_at(
                    step,
                    now,
                    &[Event::new("jobs", e::STEP_CREATED, json!(step), now)],
                )
                .await
                .unwrap();
        }
    }

    async fn update(&self, job: &Job) {
        let now = chrono::Utc::now();
        self.jobs
            .update_job_at(
                job,
                job.status,
                now,
                &[Event::new("jobs", e::JOB_UPDATED, json!(job), now)],
            )
            .await
            .unwrap();
    }

    /// The window opens between two database-clock instants, so every
    /// row relayed before it sits below the cut and every later one above.
    async fn open_window(&self) -> DateTime<Utc> {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let since: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        since
    }
}

/// An open packet of the `q.platform-admin.task` shape: a member while
/// `proof_probe` is absent.
fn task_packet(today: NaiveDate) -> (Job, Step) {
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "bounded"),
        "Bounded",
        "alice",
        Priority::Standard,
        today,
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Task", 0);
    step.status = StepStatus::Ready;
    step.metadata["authority_role"] = json!("platform-admin");
    (job, step)
}

fn flow<'a>(
    reading: &'a crate::membership_replay::MembershipReading,
    name: &str,
) -> &'a crate::membership_replay::MembershipFlow {
    reading.stations.iter().find(|f| f.station == name).unwrap()
}

/// THE FINISH LINE'S BOUND TEST (item ab09f1af, check 1). The packet's
/// whole history is larger than the row bound the read is given, and a
/// genesis fold refused exactly this ("complete native membership
/// history exceeds row bound", every station UNKNOWN). Folding from the
/// cut, the same read answers counted flows.
#[tokio::test(flavor = "multi_thread")]
async fn a_window_whose_full_history_exceeds_the_row_bound_still_answers() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, step) = task_packet(chrono::Utc::now().date_naive());
    world.admit(&job, std::slice::from_ref(&step)).await;
    for revision in 0..40 {
        job.metadata["revision"] = json!(revision);
        world.update(&job).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    world.update(&job).await;
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    world.update(&job).await;
    world.relay().await;

    let bound = 20;
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE kind=ANY($1)")
        .bind(PACKET_KINDS)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert!(
        history > bound,
        "the premise: the packet's full history ({history} rows) exceeds the bound ({bound})"
    );
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        bound,
    )
    .await
    .unwrap();
    assert!(input.initial.cut > 0);
    let seeded: Vec<_> = input
        .initial
        .rows
        .iter()
        .map(|r| r.event.kind.as_str())
        .collect();
    assert_eq!(
        seeded,
        vec![e::STEP_CREATED, e::JOB_UPDATED],
        "the starting state is the packet's latest snapshot per entity, nothing older"
    );
    // By sequence, not by a metadata key: no station reads `revision`,
    // so the read no longer carries it (backlog 45b25020).
    let latest: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log WHERE kind=$1 AND id<=$2")
        .bind(e::JOB_UPDATED)
        .bind(input.initial.cut)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert_eq!(
        input.initial.rows[1].seq, latest,
        "the Job snapshot is the LATEST one before the cut"
    );
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, Some(1), "{:?}", task.unknown);
    assert_eq!(task.departed, Some(1), "{:?}", task.unknown);
    assert_eq!(task.served, Some(0));
}

/// c195e83e Q5: groups are read 500 ids at a time, and where a batch
/// boundary falls can never change what the read answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_batch_boundary_never_changes_a_verdict() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let today = chrono::Utc::now().date_naive();
    let mut packets = Vec::new();
    for _ in 0..3 {
        let (job, step) = task_packet(today);
        world.admit(&job, std::slice::from_ref(&step)).await;
        packets.push(job);
    }
    world.relay().await;
    let since = world.open_window().await;
    for job in &mut packets {
        job.metadata["proof_probe"] = json!("left");
        world.update(job).await;
        job.metadata.as_object_mut().unwrap().remove("proof_probe");
        world.update(job).await;
    }
    world.relay().await;
    let mut answers = Vec::new();
    let mut until = None;
    for batch in [1, 2, GROUP_READ_BATCH] {
        let mut input = snapshot_batched(
            &world.db.pool,
            since,
            Some("alice".into()),
            &JobScope::All,
            4096,
            batch,
        )
        .await
        .unwrap();
        assert!(
            input.groups.iter().filter(|g| g.original.is_some()).count() > 2,
            "enough groups that batches of one and two really split them"
        );
        // Each read takes its own transaction instant as the window's
        // end; pin it so only the batch size differs between readings.
        input.until = *until.get_or_insert(input.until);
        answers.push((
            serde_json::to_value(&input.groups).unwrap(),
            serde_json::to_value(&input.initial).unwrap(),
            serde_json::to_value(replay(&input)).unwrap(),
        ));
    }
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[1], answers[2]);
    let reading: crate::membership_replay::MembershipReading =
        serde_json::from_value(answers[2].2.clone()).unwrap();
    assert_eq!(flow(&reading, "q.platform-admin.task").arrived, Some(3));
}

/// c195e83e Q1: a terminal packet is left out only when no registered
/// station's terminal window can still hold it, each window read live.
/// Under a zero-day window, a packet closed two days ago is past it and
/// one closed today is not; once the live registry declares seven
/// days, neither is left out.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_packet_is_left_out_only_past_the_widest_live_terminal_window() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    // The schema's own historical station rows declare 7- and 14-day
    // windows; the first half of this test needs a registry declaring
    // only the zero-day one, which is itself the proof that the live
    // rows are read.
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    // A station that retains the packets' kind for zero days: a packet
    // closed today is inside its window, one closed two days ago is
    // past it. With no retaining station at all, no terminal packet can
    // be counted, and both would be left out (backlog bbeff5cb).
    let mut same_day = station("design-decided");
    same_day.name = "same-day.fixture".into();
    same_day.predicate.kind = Some("fixture".into());
    same_day.terminal_window_days = Some(0);
    crate::stations::StationRegistry::publish_declared(
        &crate::PgStations::new(world.db.pool.clone()),
        same_day,
        &boss_core::actor::ActorId::Automation("membership-test".into()),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let today = chrono::Utc::now().date_naive();
    let (open, _) = task_packet(today);
    let (mut long_closed, _) = task_packet(today);
    long_closed.status = JobStatus::Closed;
    long_closed.closed_on = today.checked_sub_days(chrono::Days::new(2));
    let (mut closed_today, _) = task_packet(today);
    closed_today.status = JobStatus::Closed;
    closed_today.closed_on = Some(today);
    for job in [&open, &long_closed, &closed_today] {
        world.admit(job, &[]).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    let read = || async {
        snapshot(
            &world.db.pool,
            since,
            Some("alice".into()),
            &JobScope::All,
            4096,
        )
        .await
        .unwrap()
    };
    let seeded = |input: &ReplayInput| -> BTreeSet<String> {
        input
            .initial
            .rows
            .iter()
            .filter_map(crate::membership_replay::packet_identity)
            .collect()
    };
    let narrow = read().await;
    assert_eq!(narrow.initial.pruned_terminal, 1);
    assert!(seeded(&narrow).contains(&open.id.to_string()));
    assert!(seeded(&narrow).contains(&closed_today.id.to_string()));
    assert!(!seeded(&narrow).contains(&long_closed.id.to_string()));

    // The live registry declares a terminal window. Its publication is
    // NOT relayed, so only the live row can widen the census. Its kind
    // clause names the packets' own kind: a station that could never
    // count them would not hold them (backlog bbeff5cb, the test below).
    let mut retained = station("design-decided");
    retained.name = "retention.fixture".into();
    retained.predicate.kind = Some("fixture".into());
    retained.terminal_window_days = Some(7);
    crate::stations::StationRegistry::publish_declared(
        &crate::PgStations::new(world.db.pool.clone()),
        retained,
        &boss_core::actor::ActorId::Automation("membership-test".into()),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let wide = read().await;
    assert_eq!(wide.initial.pruned_terminal, 0);
    assert!(seeded(&wide).contains(&long_closed.id.to_string()));
}

/// Backlog bbeff5cb: inside a terminal window, a frozen terminal packet
/// is carried only for a station whose Job-level clauses it meets. One
/// global window carried every packet closed within the widest window
/// whatever the station asked of it: measured 2026-10-22, 38,811
/// packets closed within my-watchlist's 14 days were carried, of which
/// one carried `submitted_by` and 25 closed design docs fell inside
/// design-decided's 7, and the starting state passed the 250k row bound.
/// Step clauses are not judged here — a packet that meets every Job
/// clause is carried whatever its steps — and a self clause binds to
/// the reader, as the replay binds it.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_packet_is_carried_only_for_a_station_that_could_count_it() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    let stations = crate::PgStations::new(world.db.pool.clone());
    let actor = boss_core::actor::ActorId::Automation("membership-test".into());
    for (name, from) in [
        ("decided.fixture", "design-decided"),
        ("watch.fixture", "my-watchlist"),
    ] {
        let mut live = station(from);
        live.name = name.into();
        crate::stations::StationRegistry::publish_declared(
            &stations,
            live,
            &actor,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }
    let today = chrono::Utc::now().date_naive();
    let closed = |kind: &str, submitted_by: Option<&str>| {
        let (mut job, _) = task_packet(today);
        job.kind = kind.into();
        job.status = JobStatus::Closed;
        job.closed_on = Some(today);
        if let Some(who) = submitted_by {
            job.metadata = json!({"submitted_by": who});
        }
        job
    };
    let nobodys = closed("fixture", None);
    let design = closed("design-doc", None);
    let mine = closed("fixture", Some("alice"));
    let theirs = closed("fixture", Some("bob"));
    for job in [&nobodys, &design, &mine, &theirs] {
        world.admit(job, &[]).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    let seeded: BTreeSet<String> = input
        .initial
        .rows
        .iter()
        .filter_map(crate::membership_replay::packet_identity)
        .collect();
    assert!(
        seeded.contains(&design.id.to_string()),
        "design-decided can count it"
    );
    assert!(
        seeded.contains(&mine.id.to_string()),
        "alice's watchlist can count it"
    );
    assert!(
        !seeded.contains(&nobodys.id.to_string()),
        "no station can count it"
    );
    assert!(
        !seeded.contains(&theirs.id.to_string()),
        "bob's, not alice's"
    );
    assert_eq!(input.initial.pruned_terminal, 2);
}

/// c195e83e Q2: a transaction relayed partly before the window and partly
/// after it is replayed whole, never split into the starting state, and
/// the existing rule for a group crossing the window start still holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_straddling_the_cut_is_replayed_whole_and_never_seeded() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (job, step) = task_packet(chrono::Utc::now().date_naive());
    let now = chrono::Utc::now();
    let admitted = Event::new("jobs", e::JOB_CREATED, json!(job), now);
    let stepped = Event::new("jobs", e::STEP_CREATED, json!(step), now);
    let mut tx = world.db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &admitted).await.unwrap();
    record_event_in_tx(&mut tx, &stepped).await.unwrap();
    tx.commit().await.unwrap();
    drain_outbox_once(&world.db.pool, &world.bus, 1)
        .await
        .unwrap();
    let since = world.open_window().await;
    world.relay().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert!(
        input
            .initial
            .rows
            .iter()
            .all(|r| r.event.id != admitted.id && r.event.id != stepped.id),
        "no row of a group that reaches past the cut is in the starting state"
    );
    let whole = input
        .groups
        .iter()
        .find(|g| g.rows.iter().any(|r| r.event.id == admitted.id))
        .unwrap();
    assert_eq!(whole.rows.len(), 2);
    assert!(whole.rows.iter().any(|r| r.seq <= input.initial.cut));
    assert!(whole.rows.iter().any(|r| r.seq > input.initial.cut));
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, None);
    assert!(
        task.unknown.iter().any(|u| u.reason.contains("crosses")),
        "{:?}",
        task.unknown
    );
}

/// c195e83e Q3 for Steps: a current Step with no original at the cut and
/// no creation in the window makes its own packet UNKNOWN.
#[tokio::test(flavor = "multi_thread")]
async fn a_current_step_with_no_original_makes_its_packet_unknown() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (job, step) = task_packet(chrono::Utc::now().date_naive());
    world.admit(&job, &[]).await;
    world.jobs.add_step(&step).await.unwrap();
    world.relay().await;
    let since = world.open_window().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert!(
        input
            .initial
            .unknown
            .get(&job.id.to_string())
            .is_some_and(|reason| reason.contains("current Step")),
        "{:?}",
        input.initial.unknown
    );
    assert_eq!(flow(&replay(&input), "q.platform-admin.task").arrived, None);
}

/// Backlog 45b25020: the map's 24 h and 48 h reads both refused on the
/// 128 MiB byte bound, because every Job and Step row moved whole.
/// Measured live 2026-10-22: 500 `jobs.job.updated` rows were 12.9 MB,
/// 98% of it metadata no station predicate reads (`brief` alone 3.9 MB,
/// `regate_receipt` 1.9 MB) — about 430 MB a day of job updates. The
/// bound stays; what moves is only the metadata a predicate evaluates,
/// so a packet whose rows each outweigh the bound still answers, and
/// the keys the station does read (`proof_probe` on the Job,
/// `authority_role` on the Step) still decide its membership.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_no_predicate_reads_never_spends_the_byte_bound() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, mut step) = task_packet(chrono::Utc::now().date_naive());
    let bound: i64 = 64 * 1024;
    let brief = "x".repeat(bound as usize);
    job.metadata["brief"] = json!(brief);
    step.metadata["receipt"] = json!(brief);
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    world.update(&job).await;
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    world.update(&job).await;
    world.relay().await;

    let whole: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(octet_length(payload::text)),0)::bigint FROM audit_log",
    )
    .fetch_one(&world.db.pool)
    .await
    .unwrap();
    assert!(
        whole > 4 * bound,
        "the premise: the rows moved whole ({whole} bytes) outweigh the bound ({bound})"
    );
    let input = snapshot_bounded(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        1000,
        GROUP_READ_BATCH,
        bound,
    )
    .await
    .unwrap();
    let rows: Vec<_> = input
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .chain(&input.initial.rows)
        .collect();
    assert!(
        rows.iter().any(|r| r.event.kind == e::JOB_UPDATED) && !input.initial.rows.is_empty(),
        "both the window and the starting state were read"
    );
    for row in rows {
        let metadata = &row.event.payload["metadata"];
        assert!(
            metadata.get("brief").is_none() && metadata.get("receipt").is_none(),
            "{} carried metadata no predicate reads",
            row.event.kind
        );
    }
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, Some(1), "{:?}", task.unknown);
    assert_eq!(task.departed, Some(1), "{:?}", task.unknown);
}

/// Backlog 4744f5bd: after 45b25020 the 48 h read still refused on the
/// byte bound, because a Job or Step row's transaction siblings moved
/// whole. Measured live 2026-10-22 over 48 h (189,054 rows): the kinds
/// the replay applies were 83.3 MB projected, every other kind 104.5 MB,
/// the ceiling on their siblings — `step.done.task` 21.8,
/// `jobs.metadata.conditionally_merged` 19.6,
/// `step.done.gate-verdict` 18.5, each mostly a copy of the metadata
/// the Job row had just had cut — against 134 MB. The replay reads such
/// a sibling only for its packet identity, so it carries only the keys
/// that identity is read from: 3.5 MB for all of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_sibling_the_replay_never_applies_carries_only_its_packet_identity() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, step) = task_packet(chrono::Utc::now().date_naive());
    let bound: i64 = 64 * 1024;
    let brief = "x".repeat(bound as usize);
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    let now = chrono::Utc::now();
    world
        .jobs
        .update_job_at(
            &job,
            job.status,
            now,
            &[
                Event::new("jobs", e::JOB_UPDATED, json!(job), now),
                Event::new(
                    "jobs",
                    "step.done.task",
                    json!({"job_id": job.id.to_string(), "step_id": step.id.to_string(),
                           "kind": "task", "metadata": {"brief": brief}}),
                    now,
                ),
            ],
        )
        .await
        .unwrap();
    world.relay().await;

    let sibling: i64 = sqlx::query_scalar(
        "SELECT octet_length(payload::text)::bigint FROM audit_log WHERE kind='step.done.task'",
    )
    .fetch_one(&world.db.pool)
    .await
    .unwrap();
    assert!(
        sibling > bound,
        "the premise: the sibling alone ({sibling} bytes) outweighs the bound ({bound})"
    );
    for scope in [JobScope::All, JobScope::OwnerIs("alice".into())] {
        let input = snapshot_bounded(
            &world.db.pool,
            since,
            Some("alice".into()),
            &scope,
            1000,
            GROUP_READ_BATCH,
            bound,
        )
        .await
        .unwrap();
        let done = input
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .find(|r| r.event.kind == "step.done.task")
            .expect("the sibling is read with its group");
        assert_eq!(done.event.payload, json!({"job_id": job.id.to_string()}));
        assert_eq!(
            crate::membership_replay::packet_identity(done),
            Some(job.id.to_string())
        );
        let reading = replay(&input);
        let task = flow(&reading, "q.platform-admin.task");
        assert_eq!(task.departed, Some(1), "{scope:?} {:?}", task.unknown);
    }
}

/// The keys a sibling carries are the keys `packet_identity` reads: a
/// sibling's identity is the same whole or cut to `IDENTITY_KEYS`, for
/// every shape a producer writes (CLAUDE.md §9a — the projection and the
/// reader are two spellings of one fact, so this pins them together).
#[test]
fn a_sibling_cut_to_its_identity_keys_has_the_same_packet_identity() {
    let cut = |payload: &serde_json::Value| -> serde_json::Value {
        payload
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| IDENTITY_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            })
            .unwrap_or_else(|| payload.clone())
    };
    let row = |payload: serde_json::Value| HistoricalEvent {
        seq: 1,
        created_at: None,
        event: Event::new("jobs", "step.done.task", payload, chrono::Utc::now()),
    };
    for payload in [
        json!({"job_id": "j", "id": "i", "step": {"job_id": "s"}, "metadata": {"x": 1}}),
        json!({"job_id": null, "id": "i", "metadata": {"job_id": "m"}}),
        json!({"step": {"job_id": "s", "title": "t"}, "id": "i", "brief": "b"}),
        json!({"step": ["job_id"], "id": "i"}),
        json!({"id": 7, "payload": {"job_id": "p"}}),
        json!({"title": "no identity"}),
        json!(["job_id"]),
    ] {
        assert_eq!(
            crate::membership_replay::packet_identity(&row(cut(&payload))),
            crate::membership_replay::packet_identity(&row(payload.clone())),
            "{payload}"
        );
    }
}

/// c195e83e Q4: the sibling match of the history read has its index,
/// built by the ordinary schema path, and valid.
#[tokio::test(flavor = "multi_thread")]
async fn the_original_group_sibling_match_is_indexed() {
    let db = TestDb::new().await;
    let (definition, valid): (String, bool) = sqlx::query_as(
        "SELECT pg_get_indexdef(i.indexrelid),i.indisvalid FROM pg_index i \
         JOIN pg_class c ON c.oid=i.indexrelid WHERE c.relname='audit_log_original_group_id'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(valid);
    assert!(
        definition.contains("(original_group ->> 'id'::text)"),
        "{definition}"
    );
}

/// 6bc062b1: both predicates of the unchanged order check must be
/// available from the index built by the ordinary schema path.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_cut_order_check_has_a_valid_covering_index() {
    let db = TestDb::new().await;
    let indexes: Vec<(String, bool)> = sqlx::query_as(
        "SELECT pg_get_indexdef(indexrelid),indisvalid FROM pg_index \
         WHERE indrelid='audit_log'::regclass",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        indexes.iter().any(|(definition, valid)| {
            *valid && definition.ends_with("USING btree (created_at, id)")
        }),
        "the order check lacks a valid (created_at, id) covering index: {indexes:?}"
    );
}

/// A real owned bootstrap must not select/project the same pre-window
/// originals once for sizing and again for transport. All authority and
/// group witnesses still come through the actual native producer/relay.
#[tokio::test(flavor = "multi_thread")]
async fn an_owned_native_bootstrap_projects_each_seed_once() {
    use crate::membership_projection::{MembershipProjectionRepository, ProjectionIdentity};
    use crate::membership_projection_pg::PgMembershipProjection;
    use crate::membership_projection_service::MembershipProjectionService;
    use sqlx::postgres::PgPoolOptions;

    let mut db = TestDb::new().await;
    let options = (*db.pool.connect_options()).clone();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path=public,pg_catalog")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .unwrap();
    db.pool.close().await;
    db.pool = pool;
    // This function and setting exist only in this isolated TestDb. Age
    // BOTH original producer clock facts and audit insertion captures;
    // restore real wall time before exercising unchanged service windows.
    sqlx::raw_sql(r#"
        CREATE FUNCTION public.clock_timestamp() RETURNS timestamptz
        LANGUAGE SQL VOLATILE AS $$
          SELECT pg_catalog.clock_timestamp() -
            COALESCE(pg_catalog.current_setting('boss.fixture_clock_age',true),'0 seconds')::interval
        $$;
        SET boss.fixture_clock_age='49 hours';
    "#).execute(&db.pool).await.unwrap();
    let world = World::with_db(&station("q.platform-admin.task"), db).await;
    let (mut job, mut step) = task_packet(Utc::now().date_naive());
    job.metadata["seed_evaluation_marker"] = json!("original Job 最後");
    step.metadata["seed_evaluation_marker"] = json!("original Step 最後");
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    sqlx::raw_sql(
        r#"
        SET boss.fixture_clock_age='0 seconds';
        CREATE TEMP SEQUENCE seed_projection_calls;
        CREATE FUNCTION public.jsonb_each(data jsonb) RETURNS TABLE(key text,value jsonb)
        LANGUAGE plpgsql VOLATILE AS $$ BEGIN
          IF data ? 'seed_evaluation_marker'
             AND current_query() LIKE '%WITH packet_rows AS MATERIALIZED%' THEN
            PERFORM nextval('seed_projection_calls');
          END IF;
          RETURN QUERY SELECT * FROM pg_catalog.jsonb_each(data);
        END $$;
    "#,
    )
    .execute(&world.db.pool)
    .await
    .unwrap();
    let original_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE kind IN('jobs.job.created','jobs.step.created') AND created_at < pg_catalog.clock_timestamp()-interval '48 hours'"
    ).fetch_one(&world.db.pool).await.unwrap();
    assert_eq!(
        original_rows, 2,
        "both genuine originals must precede the full 48h window"
    );
    let store = Arc::new(PgMembershipProjection::new(world.db.pool.clone()));
    let service = MembershipProjectionService::new(
        Arc::new(PgJobs::new(world.db.pool.clone())),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let started = std::time::Instant::now();
    service.refresh_one(&identity).await.unwrap();
    let stored = store.load(&identity).await.unwrap().unwrap();
    assert!(
        stored.checkpoint.until() - stored.checkpoint.since() >= chrono::Duration::hours(47),
        "optional comparison may not disguise a shortened bootstrap"
    );
    let checkpoint = serde_json::to_value(&stored.checkpoint).unwrap();
    assert!(
        checkpoint["state"]["packets"]
            .get(job.id.to_string())
            .is_some(),
        "real pre-window Job and Step state must reach the durable checkpoint"
    );
    for hours in [24, 48] {
        let reading = service.peek(&identity, hours).await.unwrap().unwrap();
        assert!(
            reading.windows.is_ok(),
            "actual compact consumer must read its native checkpoint"
        );
    }
    let calls: i64 = sqlx::query_scalar("SELECT last_value FROM seed_projection_calls")
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    println!(
        "OWNED_NATIVE_BOOTSTRAP original_seed_rows={original_rows} seed_projection_calls={calls} elapsed_seconds={}",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(
        calls, original_rows,
        "accepted seed originals must not be projected again for an upfront sizing scan"
    );

    // Exact PostgreSQL text bytes, including spaces and multibyte values,
    // remain a hard bound after removing the separate sizing query.
    let input = snapshot(
        &world.db.pool,
        chrono::Utc::now() - chrono::Duration::hours(48),
        Some("alice".into()),
        &JobScope::All,
        1000,
    )
    .await
    .unwrap();
    let carried = vec![job.id.to_string()];
    let excluded: Vec<String> = Vec::new();
    let keys = ["seed_evaluation_marker".to_string()];
    let query = format!("SELECT SUM(octet_length(payload))::bigint FROM ({SEED_SQL}) seeds");
    let exact_bytes: i64 = sqlx::query_scalar(&query)
        .bind(&carried)
        .bind(input.initial.cut)
        .bind(SNAPSHOT_KINDS)
        .bind(&excluded)
        .bind(1000_i64)
        .bind(PROJECTED_KINDS)
        .bind(&keys[..])
        .bind(i64::MAX)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    // A byte refusal must not ship the oversized original to the caller.
    let wire_query = format!(
        "SELECT payload IS NULL FROM ({SEED_SQL}) seeds CROSS JOIN (SELECT $8::bigint AS requested_bytes) request ORDER BY seq"
    );
    let withheld: Vec<bool> = sqlx::query_scalar(&wire_query)
        .bind(&carried)
        .bind(input.initial.cut)
        .bind(SNAPSHOT_KINDS)
        .bind(&excluded)
        .bind(1000_i64)
        .bind(PROJECTED_KINDS)
        .bind(&keys[..])
        .bind(0_i64)
        .fetch_all(&world.db.pool)
        .await
        .unwrap();
    assert_eq!(
        withheld.len(),
        2,
        "both real originals must reach the byte guard"
    );
    assert!(
        withheld.iter().all(|value| *value),
        "oversized seed payload crossed the SQL wire"
    );
    let mut conn = world.db.pool.acquire().await.unwrap();
    for (rows, bytes, expected_error) in [
        (2, exact_bytes, None),
        (2, exact_bytes - 1, Some("byte bound")),
        (1, exact_bytes, Some("row bound")),
    ] {
        let actual = starting_state(
            &mut conn,
            Cut {
                seq: input.initial.cut,
                since: input.since,
                cutoff: input.audit_cutoff,
            },
            None,
            &input.groups,
            &input.clock,
            Budget {
                census: 1000,
                rows,
                bytes,
            },
            MetadataKeys {
                kinds: PROJECTED_KINDS,
                keys: &keys,
                siblings: Some(IDENTITY_KEYS),
            },
            &[],
            None,
            &JobScope::All,
            input.until,
            None,
            None,
        )
        .await;
        if let Some(reason) = expected_error {
            assert!(actual.unwrap_err().contains(reason));
        } else {
            let initial = actual.unwrap();
            assert_eq!(initial.rows.len(), 2);
            assert!(initial.rows.iter().all(|row| {
                row.event.payload["metadata"]["seed_evaluation_marker"]
                    .as_str()
                    .is_some()
            }));
            let compact: usize = initial
                .rows
                .iter()
                .map(|row| serde_json::to_vec(&row.event.payload).unwrap().len())
                .sum();
            assert!(
                (compact as i64) < exact_bytes,
                "JSON reserialization would undercount"
            );
        }
    }
}

/// Conserved native clock originals survive the bounded source port without
/// turning their activation uncertainty into known clock coverage.
#[tokio::test]
async fn native_wall_originals_support_only_invariant_terminal_outcomes() {
    let mut spec = station("q.platform-admin.task");
    spec.predicate.status = None;
    spec.terminal_window_days = Some(7);
    let world = World::new(&spec).await;
    let today = Utc::now().date_naive();
    let (mut job, step) = task_packet(today);
    job.status = JobStatus::Closed;
    job.closed_on = Some(today);
    world.admit(&job, &[step]).await;
    world.relay().await;
    let since = world.open_window().await;
    sqlx::query("UPDATE clock_mapping_mode SET epoch=gen_random_uuid() WHERE id=1")
        .execute(&world.db.pool)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    world.relay().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(input.initial.pruned_terminal, 0);
    let gap = input
        .clock
        .unknown
        .iter()
        .find(|gap| gap.since >= since)
        .unwrap();
    assert!(input.clock.date_at(gap.since).is_err());
    assert!(
        input
            .clock
            .outcome_date_bounds(gap.since, gap.until, input.audit_cutoff)
            .is_ok()
    );
    let reading = replay(&input);
    let counted = flow(&reading, "q.platform-admin.task");
    assert!(counted.unknown.is_empty(), "{:?}", counted.unknown);
    assert_eq!(counted.arrived, Some(0));
    assert_eq!(counted.departed, Some(0));
}
