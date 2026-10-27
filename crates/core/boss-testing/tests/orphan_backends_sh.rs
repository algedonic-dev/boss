//! The passkey-bound backend remedy never treats a signal as its effect.
use boss_testing::repo_root;

#[test]
fn the_orphan_backend_plan_preserves_identity_and_refuses_an_unproven_effect() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/orphan-backends-test.py"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn the_fixed_sql_refuses_a_changed_session_and_reads_an_isolated_termination() {
    use serde_json::Value;
    let db = boss_testing::TestDb::new().await;
    let mut client = db.pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *client)
        .await
        .unwrap();
    let client_task = tokio::spawn(async move {
        sqlx::query("SELECT pg_sleep(30)")
            .execute(&mut *client)
            .await
    });
    let backend: Value = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let value: Value = sqlx::query_scalar("SELECT jsonb_build_object('pid',pid,'backend_start',backend_start,'query_start',query_start,'client_addr',client_addr,'client_port',client_port,'state',state,'backend_type',backend_type,'database',datname) FROM pg_stat_activity WHERE pid=$1")
                .bind(pid).fetch_one(&db.pool).await.unwrap();
            if value["state"] == "active" { break value; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let binding: Value = sqlx::query_scalar("SELECT jsonb_build_object('database',current_database(),'server_started_at',pg_postmaster_start_time())")
        .fetch_one(&db.pool).await.unwrap();
    // Obtain the exact production SQL through its real adapter, substituting
    // only the transport. Execute it solely in this TestDb, never a live port.
    let query_for = |backend: &Value, binding: &Value| {
        let output = std::process::Command::new("python3").arg("-B").arg("-c")
            .arg("import importlib.util,json,sys; s=importlib.util.spec_from_file_location('remedy',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); n=m.Native(None,[],None); n.sql=lambda database,query,mutate=False: (print(query) or {'matches':1,'terminated':True}); n.terminate(json.loads(sys.argv[2]),json.loads(sys.argv[3]))")
            .arg(repo_root().join("infra/forge/orphan-backends.py"))
            .arg(binding.to_string()).arg(backend.to_string()).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    // The ordinary fixture is young: the production age guard must refuse it.
    let young = query_for(&backend, &binding);
    let result: Value = sqlx::query_scalar(&young)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(result["matches"], 0);
    assert!(!client_task.is_finished());
    // For a real effect test only, make the fixture age threshold zero in the
    // generated SQL, preserving every identity predicate. This does not alter
    // the shipped adapter or contact any production database.
    let eligible = young.replace("interval '900 seconds'", "interval '0 seconds'");
    assert_ne!(eligible, young);
    let mut changed_backend = backend.clone();
    changed_backend["backend_start"] = serde_json::json!("1900-01-01T00:00:00+00:00");
    let changed = query_for(&changed_backend, &binding)
        .replace("interval '900 seconds'", "interval '0 seconds'");
    let refused: Value = sqlx::query_scalar(&changed)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(refused["matches"], 0);
    assert!(!client_task.is_finished());
    for (field, value) in [
        ("client_addr", serde_json::json!("203.0.113.9")),
        ("client_port", serde_json::json!(1)),
    ] {
        let mut wrong = backend.clone();
        wrong[field] = value;
        let query =
            query_for(&wrong, &binding).replace("interval '900 seconds'", "interval '0 seconds'");
        let result: Value = sqlx::query_scalar(&query)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            result["matches"], 0,
            "the actual SQL must reject changed {field}"
        );
        assert!(!client_task.is_finished());
    }
    let mut wrong_server = binding.clone();
    wrong_server["server_started_at"] = serde_json::json!("1900-01-01T00:00:00+00:00");
    let query = query_for(&backend, &wrong_server)
        .replace("interval '900 seconds'", "interval '0 seconds'");
    let result: Value = sqlx::query_scalar(&query)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        result["matches"], 0,
        "the actual SQL must reject a changed postmaster lifetime"
    );
    assert!(!client_task.is_finished());
    let actual: Value = sqlx::query_scalar(&eligible)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(actual["matches"], 1);
    assert_eq!(actual["terminated"], true);
    assert!(client_task.await.unwrap().is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE pid=$1")
        .bind(pid)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
