//! Opening a requisition is Create on `employee` — hiring — in a scope
//! that covers it (backlog dda8fd97, 2026-09-27).
//!
//! WHY IT EXISTS. Found by the guest-roster builder (cda177ef):
//! `POST /api/people/requisitions` took no caller. A request with no
//! identity at all (the LAN machine door), a header naming the visitor
//! or auditor role, and any employee's session each opened a
//! requisition — and, because the write is an upsert whose conflict arm
//! sets `status`, each could also rewrite the status of one already
//! open. The gateway refuses the read-only sessions' writes at the edge
//! (07e797b4), but a signed-in employee and the LAN reached the handler.
//!
//! The requisition is covered the way its list is filtered: as its
//! hiring manager's row in its department would be — `all` covers every
//! one, `department:<d>` that department's, `team` and `self` the ones
//! the caller or a report of theirs is hiring for.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_people::requisitions::requisitions_router;
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::TestDb;
use sqlx::PgPool;
use tower::ServiceExt;

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
    requisitions_router(
        pool,
        None,
        Arc::new(boss_clock_client::WallClockClient),
        Some(policy),
    )
}

fn caller(id: &str, role: &str) -> String {
    serde_json::json!({ "id": id, "role": role }).to_string()
}

async fn seed_hiring_managers(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    for (id, dept) in [("emp-mgr-hr", "hr"), ("emp-mgr-svc", "service")] {
        sqlx::query(
            "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
             VALUES ($1, $1, $1 || '@boss.example', 'service-manager', $2, '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(dept)
        .execute(pool)
        .await
        .unwrap();
    }
}

fn body(id: &str, department: &str, manager: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "role": "service-tech",
        "department": department,
        "status": status,
        "opened_on": "2026-05-01",
        "target_fill_date": "2026-07-01",
        "location": "loc-test",
        "headcount": 2,
        "hiring_manager_id": manager,
    })
}

async fn open(app: &Router, user: Option<&str>, body: &serde_json::Value) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/people/requisitions")
        .header("content-type", "application/json");
    if let Some(user) = user {
        req = req.header("x-boss-user", user);
    }
    app.clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

async fn statuses(pool: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT id, status FROM requisitions ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// On the core default rules nobody but the deploy superuser holds
/// Create on `employee`: no identity, the visitor, the auditor and an
/// ordinary employee are each refused, nothing is written — and an
/// open requisition's status is not rewritten through the upsert.
#[tokio::test(flavor = "multi_thread")]
async fn a_requisition_is_refused_to_every_caller_without_a_create_grant() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    let app = app(db.pool.clone(), defaults());

    let admin = caller("emp-bootstrap-admin", "platform-admin");
    let opened = open(
        &app,
        Some(&admin),
        &body("req-open", "service", "emp-mgr-svc", "open"),
    )
    .await;
    assert_eq!(
        opened,
        StatusCode::CREATED,
        "the deploy superuser opens one"
    );

    for (who, user) in [
        ("no identity", None),
        ("visitor", Some(caller("guest@algedonic.dev", "visitor"))),
        (
            "audit-readonly",
            Some(caller("guest@algedonic.dev", "audit-readonly")),
        ),
        (
            "an employee",
            Some(caller("emp-mgr-svc", "service-manager")),
        ),
    ] {
        let new = open(
            &app,
            user.as_deref(),
            &body("req-new", "service", "emp-mgr-svc", "open"),
        )
        .await;
        assert_eq!(new, StatusCode::FORBIDDEN, "{who} opened a requisition");
        let flip = open(
            &app,
            user.as_deref(),
            &body("req-open", "service", "emp-mgr-svc", "filled"),
        )
        .await;
        assert_eq!(flip, StatusCode::FORBIDDEN, "{who} rewrote a status");
    }
    assert_eq!(
        statuses(&db.pool).await,
        vec![("req-open".to_string(), "open".to_string())]
    );
}

/// A department grant — the example tenant's recruiter holds `employee`
/// Create at `department:hr` — opens its own department's requisitions
/// and no other's.
#[tokio::test(flavor = "multi_thread")]
async fn a_department_grant_opens_only_its_own_departments_requisitions() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "recruiter",
                Action::Create,
                Resource::employee(),
                Scope::Department("hr".into()),
            )
            .build(),
    );
    let app = app(db.pool.clone(), policy);
    let recruiter = caller("emp-rec", "recruiter");

    let own = open(
        &app,
        Some(&recruiter),
        &body("req-hr", "hr", "emp-mgr-hr", "open"),
    )
    .await;
    assert_eq!(own, StatusCode::CREATED);
    let other = open(
        &app,
        Some(&recruiter),
        &body("req-svc", "service", "emp-mgr-svc", "open"),
    )
    .await;
    assert_eq!(other, StatusCode::FORBIDDEN);
    assert_eq!(
        statuses(&db.pool).await,
        vec![("req-hr".to_string(), "open".to_string())]
    );
}

// ---- An EXISTING row is judged as stored (review of 8388132a) ------------
//
// The write is an upsert whose conflict arm sets `status`, so a POST
// naming an existing id rewrites THAT row. Checking the grant against
// the request alone let a scoped caller restate someone else's
// requisition in its own department, or with itself as the hiring
// manager, and flip its status — while the event recorded the
// restated department the row never had.

fn caller_with_reports(id: &str, role: &str, reports: &[&str]) -> String {
    serde_json::json!({ "id": id, "role": role, "direct_report_ids": reports }).to_string()
}

/// The deploy superuser at `all`, beside `role` holding `employee`
/// Create at `scope`.
fn admin_and(role: &str, scope: Scope) -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::employee(),
                Scope::All,
            )
            .allow(role, Action::Create, Resource::employee(), scope)
            .build(),
    )
}

const ADMIN: (&str, &str) = ("emp-bootstrap-admin", "platform-admin");

async fn seed_report(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
         VALUES ('emp-rep', 'emp-rep', 'emp-rep@boss.example', 'service-tech', 'service', '2024-01-15', 'loc-test', 'full-time', 'active', 'emp-mgr-svc') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The requisition events recorded so far, oldest first.
async fn recorded(pool: &PgPool) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'people.requisition.opened' \
         ORDER BY timestamp",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The reviewer's case: the recruiter (`department:hr`) posts the
/// service requisition the admin opened, restated as `hr`, as filled.
/// Refused; the row stays open and in service; nothing is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn a_department_grant_cannot_restate_another_departments_requisition() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    let app = app(
        db.pool.clone(),
        admin_and("recruiter", Scope::Department("hr".into())),
    );
    let admin = caller(ADMIN.0, ADMIN.1);
    let opened = open(
        &app,
        Some(&admin),
        &body("req-svc", "service", "emp-mgr-svc", "open"),
    )
    .await;
    assert_eq!(opened, StatusCode::CREATED);

    let restated = open(
        &app,
        Some(&caller("emp-rec", "recruiter")),
        &body("req-svc", "hr", "emp-mgr-hr", "filled"),
    )
    .await;
    assert_eq!(restated, StatusCode::FORBIDDEN);
    assert_eq!(
        statuses(&db.pool).await,
        vec![("req-svc".to_string(), "open".to_string())]
    );
    assert_eq!(recorded(&db.pool).await.len(), 1, "only the admin's open");
}

/// A `team` grant opens requisitions its holder or a report of theirs
/// hires for, and cannot take over anyone else's by naming itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_team_grant_opens_its_teams_requisitions_and_no_others() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    seed_report(&db.pool).await;
    let app = app(db.pool.clone(), admin_and("manager", Scope::Team));
    let admin = caller(ADMIN.0, ADMIN.1);
    let opened = open(
        &app,
        Some(&admin),
        &body("req-hr", "hr", "emp-mgr-hr", "open"),
    )
    .await;
    assert_eq!(opened, StatusCode::CREATED);

    let mgr = caller_with_reports("emp-mgr-svc", "manager", &["emp-rep"]);
    let for_report = open(
        &app,
        Some(&mgr),
        &body("req-team", "service", "emp-rep", "open"),
    )
    .await;
    assert_eq!(for_report, StatusCode::CREATED, "a report's requisition");
    let for_stranger = open(&app, Some(&mgr), &body("req-x", "hr", "emp-mgr-hr", "open")).await;
    assert_eq!(for_stranger, StatusCode::FORBIDDEN, "someone else's");
    let takeover = open(
        &app,
        Some(&mgr),
        &body("req-hr", "hr", "emp-mgr-svc", "filled"),
    )
    .await;
    assert_eq!(takeover, StatusCode::FORBIDDEN, "restating the manager");
    assert_eq!(
        statuses(&db.pool).await,
        vec![
            ("req-hr".to_string(), "open".to_string()),
            ("req-team".to_string(), "open".to_string()),
        ]
    );
}

/// A `self` grant opens the caller's own requisitions only, and cannot
/// flip another's by naming itself as its hiring manager.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_grant_opens_only_the_callers_own_requisitions() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    let app = app(db.pool.clone(), admin_and("staff", Scope::Self_));
    let admin = caller(ADMIN.0, ADMIN.1);
    let opened = open(
        &app,
        Some(&admin),
        &body("req-svc", "service", "emp-mgr-svc", "open"),
    )
    .await;
    assert_eq!(opened, StatusCode::CREATED);

    let me = caller("emp-mgr-hr", "staff");
    let own = open(
        &app,
        Some(&me),
        &body("req-self", "hr", "emp-mgr-hr", "open"),
    )
    .await;
    assert_eq!(own, StatusCode::CREATED);
    let takeover = open(
        &app,
        Some(&me),
        &body("req-svc", "service", "emp-mgr-hr", "filled"),
    )
    .await;
    assert_eq!(takeover, StatusCode::FORBIDDEN);
    assert_eq!(
        statuses(&db.pool).await,
        vec![
            ("req-self".to_string(), "open".to_string()),
            ("req-svc".to_string(), "open".to_string()),
        ]
    );
}

/// This door opens a requisition or moves its status. Restating any
/// other field of an existing one is refused 409 even at `all` — the
/// upsert would keep the stored value while the event recorded the
/// request's — and a status move records the row as stored.
#[tokio::test(flavor = "multi_thread")]
async fn restating_an_existing_requisition_moves_only_its_status() {
    let db = TestDb::new().await;
    seed_hiring_managers(&db.pool).await;
    let app = app(db.pool.clone(), defaults());
    let admin = caller(ADMIN.0, ADMIN.1);
    let opened = open(
        &app,
        Some(&admin),
        &body("req-svc", "service", "emp-mgr-svc", "open"),
    )
    .await;
    assert_eq!(opened, StatusCode::CREATED);

    for (what, restated) in [
        ("department", body("req-svc", "hr", "emp-mgr-svc", "filled")),
        (
            "hiring manager",
            body("req-svc", "service", "emp-mgr-hr", "filled"),
        ),
    ] {
        let status = open(&app, Some(&admin), &restated).await;
        assert_eq!(status, StatusCode::CONFLICT, "{what} change");
    }
    assert_eq!(
        statuses(&db.pool).await,
        vec![("req-svc".to_string(), "open".to_string())]
    );

    let moved = open(
        &app,
        Some(&admin),
        &body("req-svc", "service", "emp-mgr-svc", "interviewing"),
    )
    .await;
    assert_eq!(moved, StatusCode::CREATED);
    let events = recorded(&db.pool).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[1]["department"], "service");
    assert_eq!(events[1]["hiring_manager_id"], "emp-mgr-svc");
    assert_eq!(events[1]["status"], "interviewing");
}
