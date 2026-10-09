//! The service-read guard and the refusal event, on the real adapters:
//! `PgPolicy` under the doors and the outbox under the announcer
//! (backlog 0028804f; review 1a73d5ce F1 and F2). The door tests in
//! `http.rs` run over the in-memory adapter; these hold the same answers
//! to Postgres, where "nothing was written" is a table to read.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use boss_policy::check_mode::{CheckMode, Mode};
use boss_policy::coverage::CoverageSources;
use boss_policy::http::{PolicyApiState, router};
use boss_policy::port::PolicyRepository;
use boss_policy::postgres::PgPolicy;
use boss_policy::{PolicyEngine, PolicyRule, Scope, User, default_rules, refusals, service_read};
use boss_policy_client::controls::READ_POLICY_RULE;
use boss_policy_client::coverage::{Key, Person, WorkflowFacts};
use boss_testing::TestDb;
use tower::ServiceExt;

/// Every source the lockout guard reads, dark — as they are once the
/// people and jobs APIs cannot ask policy.
struct Dark;

#[async_trait]
impl CoverageSources for Dark {
    async fn roster(&self) -> Result<Vec<Person>, String> {
        Err("GET /api/people: connection refused".into())
    }
    async fn keys(&self) -> Result<Vec<Key>, String> {
        Err("GET tiers: connection refused".into())
    }
    async fn workflows(&self) -> Result<Vec<WorkflowFacts>, String> {
        Err("GET /api/workflows: connection refused".into())
    }
}

/// The doors as the service mounts them, under `enforce`.
fn mounted(db: &TestDb) -> (Arc<PgPolicy>, Router) {
    let repo = Arc::new(PgPolicy::new(db.pool.clone()));
    let app = router(PolicyApiState {
        repo: repo.clone(),
        engine: Arc::new(PolicyEngine::new(repo.clone())),
        sources: Arc::new(Dark),
        check_mode: CheckMode::fixed(Mode::Enforce),
    });
    let outbox = Arc::new(boss_events::outbox::PgOutboxRecorder::new(db.pool.clone()));
    (repo, refusals::recorded(app, outbox))
}

fn founder() -> String {
    serde_json::json!({"id": "emp-founder", "role": "platform-admin", "access_tier": "user"})
        .to_string()
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<String>,
    caller: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", caller);
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b)),
        None => req.body(Body::empty()),
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("infallible");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn a_service_reads(repo: &Arc<PgPolicy>) -> bool {
    PolicyEngine::new(repo.clone())
        .ask(&User::service("jobs"), READ_POLICY_RULE)
        .await
        .expect("engine")
        == boss_policy::Decision::Allow { scope: Scope::All }
}

async fn audit_rows(db: &TestDb, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM policy_rule_audit WHERE target_id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .expect("read policy_rule_audit")
}

/// A FACT THAT LIVES TWICE (CLAUDE.md §9a): the kind the doors emit
/// (`refusals::POLICY_WRITE_REFUSED`) and the `event_kinds` row its migration declares.
/// An emitted-but-undeclared kind is what the audit integrity check
/// exists to catch; this holds the two equal in the table the migrations
/// build.
#[tokio::test(flavor = "multi_thread")]
async fn the_refused_policy_write_kind_is_registered() {
    let db = TestDb::new().await;
    let registered: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind_pattern, source FROM event_kinds \
         WHERE kind_pattern LIKE 'policy.write.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    assert_eq!(
        registered,
        vec![(
            refusals::POLICY_WRITE_REFUSED.to_string(),
            "policy".to_string()
        )]
    );
}

/// On Postgres, after a real boot reconcile: retiring or narrowing the
/// service read is refused 409 with every source dark, the row and its
/// audit trail are exactly as the boot left them, a service still reads,
/// and each refusal is ONE row in the outbox, of the declared kind, that
/// names the caller, the door and the rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_narrowing_writes_no_row_and_one_outbox_event() {
    let db = TestDb::new().await;
    let (repo, app) = mounted(&db);
    repo.bootstrap_reconcile(&default_rules())
        .await
        .expect("boot");
    let kept = service_read::rule();
    let own = format!("/api/policy/rules/{}", kept.id);
    let audited = audit_rows(&db, &kept.id).await;
    let narrowed = PolicyRule {
        scope: Scope::None,
        ..kept.clone()
    };
    let body = serde_json::json!({"rule": narrowed}).to_string();
    for (method, uri, body) in [
        (Method::DELETE, own.clone(), None),
        (Method::PUT, own.clone(), Some(body.clone())),
        (Method::POST, "/api/policy/rules".to_string(), Some(body)),
        // The retirement again: a repeat is not stated twice.
        (Method::DELETE, own.clone(), None),
    ] {
        let (status, text) = send(&app, method.clone(), &uri, body, &founder()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {text}");
        assert!(text.contains(&kept.id), "{text}");
        assert_eq!(
            repo.rule_for(&kept.id).await.expect("read"),
            Some(kept.clone())
        );
        assert_eq!(audit_rows(&db, &kept.id).await, audited, "{method} {uri}");
        assert!(a_service_reads(&repo).await, "{method} {uri}");
    }
    let stated: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT source, kind, payload FROM event_outbox WHERE kind = $1 ORDER BY id",
    )
    .bind(refusals::POLICY_WRITE_REFUSED)
    .fetch_all(&db.pool)
    .await
    .expect("read event_outbox");
    let doors: Vec<&str> = stated
        .iter()
        .map(|(_, _, payload)| payload["door"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        doors,
        vec![
            "DELETE /api/policy/rules/{id}",
            "PUT /api/policy/rules/{id}",
            "POST /api/policy/rules",
        ]
    );
    for (source, _, payload) in &stated {
        assert_eq!(source, "policy");
        assert_eq!(payload["target"], kept.id.as_str());
        assert_eq!(payload["caller"], "emp-founder");
        assert_eq!(payload["status"], 409);
        assert_eq!(payload["_actor"], "emp-founder");
    }
}

/// (d) THE BOOT, ON POSTGRES. An operator-owned narrowing (residue from
/// before the guard, or a hand edit) is PRESERVED by reconcile — the
/// boot is not refused and does not restore it — and the boot's report
/// names it. The restore the report names is then written through the
/// door with every source dark, and a service reads again.
#[tokio::test(flavor = "multi_thread")]
async fn a_boot_reports_a_narrowed_rule_and_the_named_restore_is_written() {
    let db = TestDb::new().await;
    let (repo, app) = mounted(&db);
    let kept = service_read::rule();
    let narrowed = PolicyRule {
        scope: Scope::None,
        ..kept.clone()
    };
    repo.upsert_rule(&narrowed, "emp-someone")
        .await
        .expect("seed the residue past the door");
    let stats = repo
        .bootstrap_reconcile(&default_rules())
        .await
        .expect("a boot is never refused");
    assert_eq!(stats.preserved, 1, "{stats:?}");
    assert!(
        !a_service_reads(&repo).await,
        "reconcile did not restore it"
    );
    let report = service_read::found(repo.as_ref())
        .await
        .expect("read")
        .expect("the boot reports it");
    assert!(report.contains(&kept.id), "{report}");
    assert!(
        report.contains(&service_read::restore_request()),
        "{report}"
    );

    let restore = serde_json::json!({"rule": kept}).to_string();
    assert!(service_read::restore_request().contains(&restore));
    let (status, text) = send(
        &app,
        Method::POST,
        "/api/policy/rules",
        Some(restore),
        &founder(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(a_service_reads(&repo).await);
    assert_eq!(
        service_read::found(repo.as_ref()).await.expect("read"),
        None
    );
    let by: String = sqlx::query_scalar("SELECT updated_by FROM policy_rules WHERE id = $1")
        .bind(&kept.id)
        .fetch_one(&db.pool)
        .await
        .expect("read");
    assert_eq!(by, "emp-founder", "the restore is the caller's, on the row");

    // A bootstrap-owned row that drifted IS refreshed by the next boot.
    sqlx::query("UPDATE policy_rules SET scope = 'none', updated_by = 'bootstrap' WHERE id = $1")
        .bind(&kept.id)
        .execute(&db.pool)
        .await
        .expect("drift a bootstrap-owned row");
    repo.bootstrap_reconcile(&default_rules())
        .await
        .expect("boot");
    assert!(a_service_reads(&repo).await);
}
