//! Every scheduling write asks policy, in a scope that reaches the
//! employee it touches (backlog 11721a25, 2026-09-27).
//!
//! WHY IT EXISTS. The five scheduling reads were gated on 2026-09-25
//! (a621d091, `a_schedule_is_read_by_its_employee_or_by_scope.rs`); the
//! writes beside them were not. Measured on origin/main: a request with
//! no identity (anything reaching the jobs port, or the LAN machine
//! door), the visitor and auditor sessions, and any signed-in employee
//! could add or remove availability, book or cancel a tech's
//! assignment, rewrite a shift pattern, or materialise every shift in
//! the company — for anyone.
//!
//! The rule is the reads' resource with the write's action: Create on
//! `schedule` to add (availability, an assignment, a shift pattern, a
//! materialise), Update to move an assignment's status, Delete to remove
//! one — in a grant whose scope reaches the employee (`all` anyone,
//! `team` the caller and their reports, `self` the caller). A materialise
//! writes every employee's week, so only `all` reaches it.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_jobs::scheduling::PgScheduling;
use boss_jobs::scheduling::http::{SchedulingApiState, router};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{TestDb, TestRequest};
use serde_json::{Value, json};
use sqlx::PgPool;

const MINE: &str = "emp-tech-001";
const THEIRS: &str = "emp-tech-002";
const JOB: &str = "11111111-1111-1111-1111-111111111111";

/// The core default rules, as the live policy service holds them.
fn defaults() -> Arc<dyn PolicyClient> {
    Arc::new(
        boss_policy_client::defaults::default_rules()
            .into_iter()
            .fold(FakePolicyClient::builder(), |b, r| {
                b.allow(r.role, r.action, r.resource, r.scope)
            })
            .build(),
    )
}

fn app(pool: PgPool, policy: Arc<dyn PolicyClient>) -> Router {
    router(SchedulingApiState {
        repo: Arc::new(PgScheduling::new(pool)),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy,
    })
}

async fn seed(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    for emp in [MINE, THEIRS] {
        sqlx::query(
            "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
             VALUES ($1, $1, $1 || '@boss.example', 'service-tech', 'service', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(emp)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id, account_type) \
         VALUES ('acc-sched-test', 'Test Co', 'Director', 'Austin', 'TX', 'gold', '2025-06-01', $1, 'wholesale-distributor') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(MINE)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, status, priority, opened_on) \
         VALUES ($1, 'service-visit', 'account', 'acc-sched-test', 'Test job', $2, 'open', 'standard', CURRENT_DATE) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(uuid::Uuid::parse_str(JOB).unwrap())
    .bind(MINE)
    .execute(pool)
    .await
    .unwrap();
}

/// `x-boss-user` for `id` at `role`, with `reports`.
fn caller(id: &str, role: &str, reports: &[&str]) -> String {
    json!({ "id": id, "role": role, "direct_report_ids": reports }).to_string()
}

fn admin() -> String {
    caller("emp-bootstrap-admin", "platform-admin", &[])
}

fn availability(emp: &str) -> Value {
    json!({
        "employee_id": emp,
        "kind": "pto",
        "starts_at": "2026-06-01T00:00:00Z",
        "ends_at": "2026-06-02T00:00:00Z",
    })
}

fn assignment(emp: &str) -> Value {
    json!({
        "tech_id": emp,
        "target_job_id": JOB,
        "kind": "wo",
        "starts_at": "2026-05-10T09:00:00Z",
        "ends_at": "2026-05-10T12:00:00Z",
    })
}

fn shift(emp: &str) -> Value {
    json!({
        "employee_id": emp,
        "day_of_week": 1,
        "starts_at_time": "08:00:00",
        "ends_at_time": "17:00:00",
        "effective_from": "2026-01-01",
    })
}

async fn post(app: &Router, user: Option<&str>, uri: &str, body: &Value) -> (StatusCode, Value) {
    let mut req = TestRequest::post(uri).json(body);
    if let Some(user) = user {
        req = req.header("x-boss-user", user);
    }
    let resp = req.send(app).await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(Value::Null);
    (resp.status, body)
}

async fn delete(app: &Router, user: Option<&str>, uri: &str) -> StatusCode {
    let mut req = TestRequest::delete(uri);
    if let Some(user) = user {
        req = req.header("x-boss-user", user);
    }
    req.send(app).await.status
}

/// What the tables hold: (availability, assignments + their statuses,
/// shift patterns), each as (employee, detail).
async fn tables(pool: &PgPool) -> (Vec<String>, Vec<(String, String)>, Vec<String>) {
    let avail: Vec<(String,)> =
        sqlx::query_as("SELECT employee_id FROM tech_availability ORDER BY employee_id")
            .fetch_all(pool)
            .await
            .unwrap();
    let assign: Vec<(String, String)> =
        sqlx::query_as("SELECT tech_id, status FROM scheduled_assignments ORDER BY tech_id")
            .fetch_all(pool)
            .await
            .unwrap();
    let shifts: Vec<(String,)> =
        sqlx::query_as("SELECT employee_id FROM tech_shift_patterns ORDER BY employee_id")
            .fetch_all(pool)
            .await
            .unwrap();
    (
        avail.into_iter().map(|r| r.0).collect(),
        assign,
        shifts.into_iter().map(|r| r.0).collect(),
    )
}

/// The deploy superuser places one of each on `emp`, answering the
/// availability and assignment ids.
async fn place(app: &Router, emp: &str) -> (String, String) {
    let (s, a) = post(
        app,
        Some(&admin()),
        "/api/scheduling/availability",
        &availability(emp),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let (s, b) = post(
        app,
        Some(&admin()),
        "/api/scheduling/assignments",
        &assignment(emp),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    (
        a["id"].as_str().unwrap().to_string(),
        b["id"].as_str().unwrap().to_string(),
    )
}

/// On the core default rules nobody but the deploy superuser writes a
/// schedule. Each write is refused to every other caller — its own
/// schedule included — and the tables are exactly what the superuser
/// wrote.
#[tokio::test(flavor = "multi_thread")]
async fn every_schedule_write_is_refused_without_a_grant() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let app = app(db.pool.clone(), defaults());
    let (avail_id, assign_id) = place(&app, MINE).await;
    let before = tables(&db.pool).await;
    let status_uri = format!("/api/scheduling/assignments/{assign_id}/status");

    for (who, user) in [
        ("no identity", None),
        (
            "visitor",
            Some(caller("guest@algedonic.dev", "visitor", &[])),
        ),
        (
            "audit-readonly",
            Some(caller("guest@algedonic.dev", "audit-readonly", &[])),
        ),
        ("the employee", Some(caller(MINE, "service-tech", &[]))),
        (
            "another employee",
            Some(caller(THEIRS, "service-tech", &[])),
        ),
    ] {
        let user = user.as_deref();
        for (uri, body) in [
            ("/api/scheduling/availability", availability(MINE)),
            ("/api/scheduling/assignments", assignment(MINE)),
            ("/api/scheduling/shift-patterns", shift(MINE)),
            ("/api/scheduling/shift-patterns/materialize", json!({})),
            (status_uri.as_str(), json!({"status": "cancelled"})),
        ] {
            let (status, _) = post(&app, user, uri, &body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: POST {uri}");
        }
        for uri in [
            format!("/api/scheduling/availability/{avail_id}"),
            format!("/api/scheduling/assignments/{assign_id}"),
        ] {
            assert_eq!(
                delete(&app, user, &uri).await,
                StatusCode::FORBIDDEN,
                "{who}: DELETE {uri}"
            );
        }
    }
    assert_eq!(tables(&db.pool).await, before, "nothing moved");
}

/// The deploy superuser holds every schedule write on the default rules
/// — it is what a sibling service signs as.
#[tokio::test(flavor = "multi_thread")]
async fn the_deploy_superuser_writes_any_schedule_on_the_default_rules() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let app = app(db.pool.clone(), defaults());
    let (avail_id, assign_id) = place(&app, THEIRS).await;
    let (s, _) = post(
        &app,
        Some(&admin()),
        "/api/scheduling/shift-patterns",
        &shift(THEIRS),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post(
        &app,
        Some(&admin()),
        &format!("/api/scheduling/assignments/{assign_id}/status"),
        &json!({"status": "confirmed"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = post(
        &app,
        Some(&admin()),
        "/api/scheduling/shift-patterns/materialize",
        &json!({"weeks_ahead": 1}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        delete(
            &app,
            Some(&admin()),
            &format!("/api/scheduling/availability/{avail_id}")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete(
            &app,
            Some(&admin()),
            &format!("/api/scheduling/assignments/{assign_id}")
        )
        .await,
        StatusCode::NO_CONTENT
    );
}

/// A `team` grant — how a tenant lets a manager run their techs' weeks —
/// writes the caller's and their reports' schedules and no one else's,
/// and cannot materialise the company's.
#[tokio::test(flavor = "multi_thread")]
async fn a_team_grant_writes_only_its_teams_schedules() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        [Action::Create, Action::Update, Action::Delete]
            .into_iter()
            .fold(FakePolicyClient::builder(), |b, a| {
                b.allow("service-mgr", a, Resource::schedule(), Scope::Team)
            })
            .build(),
    );
    let app = app(db.pool.clone(), policy);
    let mgr = caller("emp-mgr", "service-mgr", &[MINE]);
    let (s, _) = post(
        &app,
        Some(&mgr),
        "/api/scheduling/availability",
        &availability(MINE),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "a report's availability");
    let (s, _) = post(
        &app,
        Some(&mgr),
        "/api/scheduling/availability",
        &availability(THEIRS),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "someone else's");
    let (s, _) = post(
        &app,
        Some(&mgr),
        "/api/scheduling/shift-patterns",
        &shift(THEIRS),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = post(
        &app,
        Some(&mgr),
        "/api/scheduling/shift-patterns/materialize",
        &json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a materialise reaches everyone");

    // The row decides whose it is, not the request: an id outside the
    // team is refused, one inside it is removed.
    let seeded = app_admin_place(&db.pool, THEIRS).await;
    assert_eq!(
        delete(
            &app,
            Some(&mgr),
            &format!("/api/scheduling/assignments/{}", seeded.1)
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete(
            &app,
            Some(&mgr),
            &format!("/api/scheduling/availability/{}", seeded.0)
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let (s, _) = post(
        &app,
        Some(&mgr),
        &format!("/api/scheduling/assignments/{}/status", seeded.1),
        &json!({"status": "cancelled"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let mine = app_admin_place(&db.pool, MINE).await;
    assert_eq!(
        delete(
            &app,
            Some(&mgr),
            &format!("/api/scheduling/assignments/{}", mine.1)
        )
        .await,
        StatusCode::NO_CONTENT
    );
}

/// Status and body text of a by-id write.
async fn by_id(app: &Router, user: &str, method: &str, uri: &str) -> (StatusCode, String) {
    let req = match method {
        "DELETE" => TestRequest::delete(uri),
        _ => TestRequest::post(uri).json(&json!({"status": "cancelled"})),
    };
    let resp = req.header("x-boss-user", user).send(app).await;
    (resp.status, resp.body_text())
}

/// The adversarial review of this car (2026-09-27): a refusal by id
/// named the row's employee, and answered a missing id in other words,
/// so a narrow grant learned whose a row was and which ids are real.
/// Every by-id refusal is one answer, naming nobody — the three writes
/// by id alike.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_by_id_says_neither_whose_row_nor_whether_it_exists() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        [Action::Update, Action::Delete]
            .into_iter()
            .fold(FakePolicyClient::builder(), |b, a| {
                b.allow("service-tech", a, Resource::schedule(), Scope::Self_)
            })
            .build(),
    );
    let app = app(db.pool.clone(), policy);
    let (avail, assign) = app_admin_place(&db.pool, THEIRS).await;
    let me = caller(MINE, "service-tech", &[]);
    let missing = "6f1e9b99-0000-4000-8000-00000000dead";
    for (method, theirs, nothing) in [
        (
            "DELETE",
            format!("/api/scheduling/availability/{avail}"),
            format!("/api/scheduling/availability/{missing}"),
        ),
        (
            "DELETE",
            format!("/api/scheduling/assignments/{assign}"),
            format!("/api/scheduling/assignments/{missing}"),
        ),
        (
            "POST",
            format!("/api/scheduling/assignments/{assign}/status"),
            format!("/api/scheduling/assignments/{missing}/status"),
        ),
    ] {
        let a = by_id(&app, &me, method, &theirs).await;
        let b = by_id(&app, &me, method, &nothing).await;
        assert_eq!(a.0, StatusCode::FORBIDDEN, "{method} {theirs}");
        assert!(!a.1.contains(THEIRS), "names the row's employee: {a:?}");
        assert_eq!(a, b, "{method}: tells a real id from a missing one");
    }
}

/// Place rows through a superuser app over the same database.
async fn app_admin_place(pool: &PgPool, emp: &str) -> (String, String) {
    place(&app(pool.clone(), defaults()), emp).await
}
