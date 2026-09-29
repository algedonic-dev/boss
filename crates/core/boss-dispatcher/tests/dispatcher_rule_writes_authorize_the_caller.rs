//! The three dispatcher rule WRITES — `POST /api/dispatcher/rules` (a
//! draft), `POST /api/dispatcher/rules/{name}/publish` and
//! `POST /api/dispatcher/rules/{name}/retire` — authorize their caller
//! at the service (backlog 847af5c7, 2026-09-27).
//!
//! Until this car none of the three took a `CurrentUser` or asked
//! policy. The gateway refused only a read-only session, so ANY session
//! with a writable role could retire a rule through it, and any pod could
//! through the ClusterIP machine door (`boss-dispatcher-internal`), which
//! the gateway does not front. Dispatcher rules drive side effects —
//! spawns, ops-requests, converges — so retiring `auto-park-on-gate-green`
//! or publishing a rule that files ops-requests is a trust-boundary act.
//!
//! What this pins, per route, through the dispatcher's real router and
//! the policy DEFAULTS core ships (not a hand-built grant list, so a
//! default that drifts is caught here):
//!
//! 1. **No identity is refused 401** — no `x-boss-user` header, and a
//!    header that only claims the no-header sentinel's role.
//! 2. **A visitor, the auditor, break-glass and a writable tenant role
//!    are refused 403** — `dispatcher-rule` is platform-admin's alone.
//! 3. **A policy service that cannot answer is refused 503** — fail
//!    closed, never read as an allow.
//! 4. **Platform-admin is admitted, and the row names the SIGNED caller**
//!    — `created_by` on the draft, `published_by` on the promoted row,
//!    `retired_by` on the retired one — and never a name from the body.
//!
//! Every refusal runs against a pool that cannot connect: a handler that
//! reached the table before refusing would answer a storage error, not
//! the status asserted.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_dispatcher::cascade::Cascade;
use boss_dispatcher::http::{HttpState, router};
use boss_dispatcher::liveness::DispatcherLiveness;
use boss_policy_client::defaults::default_rules;
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{
    Action, Decision, InMemoryPolicy, PolicyClient, PolicyClientError, PolicyEngine, PolicyRule,
    Predicate, Resource, Scope,
};
use boss_testing::TestDb;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The policy an install runs with no tenant grants beyond one writable
/// tenant role: core's `default_rules()` plus `brewer`, who may create,
/// update and close packets and update steps — the shape of an ordinary
/// working role that the gateway lets write.
struct DefaultsPolicy(PolicyEngine<InMemoryPolicy>);

fn defaults_policy() -> Arc<dyn PolicyClient> {
    let mut rules = default_rules();
    for action in [Action::Create, Action::Update, Action::Close] {
        rules.push(PolicyRule::new(
            "brewer",
            Resource::job(),
            action,
            Scope::All,
        ));
    }
    rules.push(PolicyRule::new(
        "brewer",
        Resource::step(),
        Action::Update,
        Scope::All,
    ));
    Arc::new(DefaultsPolicy(PolicyEngine::new(Arc::new(
        InMemoryPolicy::with_rules(rules),
    ))))
}

#[async_trait]
impl PolicyClient for DefaultsPolicy {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        self.0
            .check(user, action, resource)
            .await
            .map_err(|e| PolicyClientError::Transport(e.to_string()))
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        self.0
            .scope_predicate(user, resource)
            .await
            .map_err(|e| PolicyClientError::Transport(e.to_string()))
    }
}

/// A policy service that cannot be asked — the outage shape.
struct UnreachablePolicy;

#[async_trait]
impl PolicyClient for UnreachablePolicy {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }

    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }
}

fn state(pool: sqlx::PgPool, policy: Arc<dyn PolicyClient>) -> HttpState {
    HttpState {
        live: Arc::new(DispatcherLiveness::default()),
        pool,
        authored_rules_dir: None,
        cascade: Arc::new(Cascade::default()),
        policy,
        clock: Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: chrono::Utc::now(),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
        calendar: Arc::new(boss_calendar_client::FakeCalendarClient::new()),
        firings: None,
    }
}

/// A pool that answers nothing: port 1 refuses.
fn unreachable_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy("postgres://boss:boss@127.0.0.1:1/boss")
        .expect("a lazy pool never connects at construction")
}

fn signer(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: None,
    })
    .expect("a User always serialises")
}

/// `boss tenant publish`'s identity, spelled as its `SEED_USER` spells it
/// (crates/orchestrators/boss-cli/src/tenant_publish.rs) — the live
/// caller that lands every tenant rule, which must keep working.
const TENANT_SEED: &str = r#"{"id":"automation:tenant-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

fn draft_body(name: &str) -> Value {
    json!({
        "name": name,
        "on_event": "step.done.task",
        "do": [{"handler": "jobs.spawn", "args": {}}],
        "source": "tenant:acme",
    })
}

/// The three writes, as (label, uri, body).
fn writes(name: &str) -> Vec<(&'static str, String, Option<Value>)> {
    vec![
        (
            "draft",
            "/api/dispatcher/rules".to_string(),
            Some(draft_body(name)),
        ),
        (
            "publish",
            format!("/api/dispatcher/rules/{name}/publish"),
            None,
        ),
        (
            "retire",
            format!("/api/dispatcher/rules/{name}/retire"),
            None,
        ),
    ]
}

async fn post(
    app: &axum::Router,
    uri: &str,
    who: Option<&str>,
    body: Option<&Value>,
) -> (StatusCode, Option<String>, String) {
    let mut req = Request::builder().method("POST").uri(uri);
    if let Some(who) = who {
        req = req.header("x-boss-user", who);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        retry_after,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_write_with_no_identity_is_refused_401() {
    let app = router(state(unreachable_pool(), defaults_policy()));
    let claims_the_sentinel = signer("emp-mallory", User::ANONYMOUS_ROLE);
    // The shape the subject-kinds review flagged (abc2e9d5, LOW-1) and
    // f5e0670d point 2 names here: the no-header sentinel's ID with a
    // platform role passed `is_anonymous()`, which reads the role alone,
    // and the row was signed `anonymous`. A blank id is no caller either.
    let claims_the_anonymous_id = signer(User::ANONYMOUS_ID, "platform-admin");
    let names_nobody = signer("", "platform-admin");
    for (label, uri, body) in writes("r") {
        for (who, header) in [
            ("no header", None),
            (
                "a header claiming the guest role",
                Some(claims_the_sentinel.as_str()),
            ),
            (
                "a header claiming the anonymous id at platform-admin",
                Some(claims_the_anonymous_id.as_str()),
            ),
            ("a blank id at platform-admin", Some(names_nobody.as_str())),
        ] {
            let (status, _, text) = post(&app, &uri, header, body.as_ref()).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{label} with {who} must be refused as unsigned: {text}"
            );
            assert!(
                text.contains("names no caller"),
                "{label} with {who}: the refusal says what is missing: {text}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_write_by_any_role_but_platform_admin_is_refused_403() {
    let app = router(state(unreachable_pool(), defaults_policy()));
    for role in [
        boss_core::roles::VISITOR_ROLE,
        "audit-readonly",
        "break-glass",
        "brewer",
    ] {
        let who = signer("emp-someone", role);
        for (label, uri, body) in writes("r") {
            let (status, _, text) = post(&app, &uri, Some(&who), body.as_ref()).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{label} by `{role}` must be refused by policy: {text}"
            );
        }
    }
}

/// A platform-admin a user override denies is refused 403 at each door,
/// with policy's reason (backlog 59deda40: the doors now ask through the
/// registry-write ladder, so an override narrows the deploy superuser as
/// it narrows anyone). The pool cannot connect, so nothing was written.
#[tokio::test(flavor = "multi_thread")]
async fn a_denying_override_refuses_a_platform_admin_403() {
    let admin_id = "claude@algedonic.dev";
    let policy = [Action::Create, Action::Publish, Action::Retire]
        .into_iter()
        .fold(
            boss_policy_client::FakePolicyClient::builder().with_default_rules(),
            |b, a| {
                b.with_override(boss_policy_client::UserOverride {
                    id: format!("deny-rules-{}", a.as_str()),
                    user_id: admin_id.into(),
                    resource: Resource::dispatcher_rule(),
                    action: a,
                    scope: Scope::None,
                    reason: "rules frozen for the incident".into(),
                    expires_at: None,
                })
            },
        )
        .build();
    let app = router(state(unreachable_pool(), Arc::new(policy)));
    let admin = signer(admin_id, "platform-admin");
    for (label, uri, body) in writes("r") {
        let (status, _, text) = post(&app, &uri, Some(&admin), body.as_ref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {text}");
        assert!(text.contains("rules frozen"), "{label}: {text}");
    }
}

/// A grant narrower than `all` does not reach a rule: a dispatcher rule
/// belongs to no person and no department, so a department-scoped grant
/// cannot be shown to cover it (the registry-write ladder, backlog
/// 59deda40). Until then any allow at any scope passed.
#[tokio::test(flavor = "multi_thread")]
async fn a_department_scoped_grant_is_refused_403() {
    let policy = [Action::Create, Action::Publish, Action::Retire]
        .into_iter()
        .fold(
            boss_policy_client::FakePolicyClient::builder().with_default_rules(),
            |b, a| {
                b.allow(
                    "it-lead",
                    a,
                    Resource::dispatcher_rule(),
                    Scope::Department("it".into()),
                )
            },
        )
        .build();
    let app = router(state(unreachable_pool(), Arc::new(policy)));
    let lead = signer("emp-it-lead", "it-lead");
    for (label, uri, body) in writes("r") {
        let (status, _, text) = post(&app, &uri, Some(&lead), body.as_ref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {text}");
    }
}

/// A role the core defaults grant nothing drafts, publishes and retires
/// once a policy rule grants it the three actions on `dispatcher-rule`
/// at `all`, and each row names that caller.
#[tokio::test(flavor = "multi_thread")]
async fn a_granting_rule_lets_a_non_admin_write_and_each_row_names_it() {
    let db = TestDb::new().await;
    let policy = [Action::Create, Action::Publish, Action::Retire]
        .into_iter()
        .fold(
            boss_policy_client::FakePolicyClient::builder().with_default_rules(),
            |b, a| b.allow("rules-author", a, Resource::dispatcher_rule(), Scope::All),
        )
        .build();
    let app = router(state(db.pool.clone(), Arc::new(policy)));
    let author = signer("emp-rules-author", "rules-author");
    let name = "a-granted-rule";
    let (status, _, text) = post(
        &app,
        "/api/dispatcher/rules",
        Some(&author),
        Some(&draft_body(name)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    for (verb, want) in [
        ("publish", StatusCode::OK),
        ("retire", StatusCode::NO_CONTENT),
    ] {
        let (status, _, text) = post(
            &app,
            &format!("/api/dispatcher/rules/{name}/{verb}"),
            Some(&author),
            None,
        )
        .await;
        assert_eq!(status, want, "{verb}: {text}");
    }
    let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT created_by, published_by, retired_by FROM dispatcher_rules \
         WHERE name = $1 AND version = 1",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let me = Some("emp-rules-author".to_string());
    assert_eq!(row, (me.clone(), me.clone(), me));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_policy_outage_refuses_every_rule_write_503() {
    let app = router(state(unreachable_pool(), Arc::new(UnreachablePolicy)));
    let admin = signer("emp-david", "platform-admin");
    for (label, uri, body) in writes("r") {
        let (status, retry_after, text) = post(&app, &uri, Some(&admin), body.as_ref()).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{label}: an outage is not an allow: {text}"
        );
        assert!(
            retry_after.is_some(),
            "{label}: the outage says when to ask again"
        );
        assert!(
            !text.contains("connection refused"),
            "{label}: the policy service's detail is logged, not handed out: {text}"
        );
    }
}

/// The author is the SIGNED caller: a body naming one is refused as a key
/// the registry does not read, and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_body_cannot_name_its_author() {
    let app = router(state(unreachable_pool(), defaults_policy()));
    let admin = signer("emp-david", "platform-admin");
    let mut body = draft_body("r");
    body["created_by"] = json!("emp-someone-else");
    let (status, _, text) = post(&app, "/api/dispatcher/rules", Some(&admin), Some(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
    assert!(text.contains("created_by"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn platform_admin_drafts_publishes_and_retires_and_each_row_names_its_signer() {
    let db = TestDb::new().await;
    let app = router(state(db.pool.clone(), defaults_policy()));
    let admin = signer("emp-david", "platform-admin");
    let name = "an-authorised-rule";

    // Draft — the SPA editor's shape of caller: a signed-in human.
    let (status, _, text) = post(
        &app,
        "/api/dispatcher/rules",
        Some(&admin),
        Some(&draft_body(name)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let drafted: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(drafted["created_by"], "emp-david", "{drafted}");
    assert_eq!(drafted["status"], "draft");

    // Publish — `boss tenant publish`'s identity, the live caller.
    let (status, _, text) = post(
        &app,
        &format!("/api/dispatcher/rules/{name}/publish"),
        Some(TENANT_SEED),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let promoted: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(promoted["status"], "active", "{promoted}");
    assert_eq!(
        promoted["published_by"], "automation:tenant-seed",
        "{promoted}"
    );
    assert_eq!(
        promoted["created_by"], "emp-david",
        "the draft's author is kept"
    );

    // Retire — a third signer, so each act is seen to record its own.
    let retirer = signer("emp-ops", "platform-admin");
    let (status, _, text) = post(
        &app,
        &format!("/api/dispatcher/rules/{name}/retire"),
        Some(&retirer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let row: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT status, created_by, published_by, retired_by FROM dispatcher_rules \
         WHERE name = $1 AND version = 1",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "retired".to_string(),
            Some("emp-david".to_string()),
            Some("automation:tenant-seed".to_string()),
            Some("emp-ops".to_string()),
        )
    );
}

/// A publish that supersedes an active version retires it, and that
/// retirement is the publisher's act — it names them, not nobody.
#[tokio::test(flavor = "multi_thread")]
async fn the_version_a_publish_supersedes_names_the_publisher_as_its_retirer() {
    let db = TestDb::new().await;
    let app = router(state(db.pool.clone(), defaults_policy()));
    let name = "a-superseded-rule";
    for (who, version) in [("emp-first", 1), ("emp-second", 2)] {
        let admin = signer(who, "platform-admin");
        let mut body = draft_body(name);
        body["version"] = json!(version);
        let (status, _, text) =
            post(&app, "/api/dispatcher/rules", Some(&admin), Some(&body)).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let (status, _, text) = post(
            &app,
            &format!("/api/dispatcher/rules/{name}/publish"),
            Some(&admin),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
    }
    let retired_by: Option<String> = sqlx::query_scalar(
        "SELECT retired_by FROM dispatcher_rules WHERE name = $1 AND version = 1",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(retired_by.as_deref(), Some("emp-second"));
}
