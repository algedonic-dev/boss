//! An edge into a passkey-protected protocol is removed only by a car
//! (backlog a4b57aca, review R1 of car 8715776d).
//!
//! The workflow publish judge (`boss_jobs::publish_passkey`) protects a
//! parent that admits or fulfils a packet of a protected kind, and reads
//! those edges off the LIVE rule registry. The rule registry has its own
//! HTTP doors — `POST /api/dispatcher/rules/{name}/publish` and
//! `/retire` — that ask registry policy alone, so without this pin an
//! actor holding rule-write and workflow-publish policy could cut the
//! parent -> protected-child edge live (which is itself the strip: the
//! parent's packets no longer pass through the passkey) and then publish
//! the parent unsigned. These pin:
//!
//! 1. retiring the rule that draws the edge is refused 409, the rule
//!    stays live, and the unsigned parent publish is still refused;
//! 2. publishing a version that redirects the spawn away from the
//!    protected child is refused 409;
//! 3. a version that keeps the edge publishes, and a rule whose edge
//!    another rule still draws retires — no widening;
//! 4. a rule into an UNPROTECTED kind retires on policy alone.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_dispatcher::cascade::Cascade;
use boss_dispatcher::http::{HttpState, router};
use boss_dispatcher::liveness::DispatcherLiveness;
use boss_dispatcher::rules::authoring;
use boss_dispatcher::rules::registry::parse_raw;
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::TestDb;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn actor() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Automation("test".into())
}

/// A viable row; `presence` puts a passkey on its sign-off step.
fn row_toml(kind: &str, label: &str, presence: bool) -> String {
    let assurance = if presence {
        "assurance_required = \"presence\"\n"
    } else {
        ""
    };
    format!(
        r#"[[workflow]]
kind = "{kind}"
label = "{label}"
category = "platform"
subject_kinds = ["custom"]
owning_team = "platform"

[[workflow.step]]
title = "filed"
kind = "task"
ready_when = "true"
fields = [{{ name = "change", field_type = "string", required = true }}]

[[workflow.step]]
title = "approve"
kind = "sign-off"
ready_when = "steps.filed.done"
{assurance}sign_offs_required = ["platform-admin"]
authority_role = "platform-admin"
metadata_defaults = {{ procedure = "Read the change, then decide.", change = "{{metadata.change}}" }}
fields = [{{ name = "decision", field_type = "approved|rejected", required = true }}, {{ name = "change", field_type = "string", required = true }}]

[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.approve.done"
metadata_defaults = {{ outcome_kind = "completed" }}
terminal = {{ outcome = "done" }}
"#
    )
}

fn spec(text: &str) -> boss_jobs::registry::WorkflowSpec {
    boss_jobs::seed_loader::parse_workflows(text, "platform", "fixture.toml")
        .expect("the fixture row parses")
        .remove(0)
}

async fn live(reg: &PgWorkflows, kind: &str, presence: bool) {
    reg.create_draft(
        spec(&row_toml(kind, &format!("{kind} v1"), presence)),
        &actor(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    reg.publish(kind, &actor(), chrono::Utc::now())
        .await
        .unwrap();
}

/// A rule spawning `child` when a `parent` packet's `filed` task is done.
fn rule_toml(name: &str, parent: &str, child: &str, version: u32) -> String {
    format!(
        "[[rule]]\nname = \"{name}\"\nversion = {version}\non_event = \"step.done.task\"\n\
         when = 'workflow_kind = \"{parent}\" AND spec_slug = \"filed\"'\n\
         [[rule.do]]\nhandler = \"jobs.spawn\"\nargs = {{ kind = '\"{child}\"' }}\n"
    )
}

/// Through the reviewed road (the seed's own writes), not the HTTP door.
async fn seeded(pool: &sqlx::PgPool, name: &str, parent: &str, child: &str) {
    let raw = parse_raw(&rule_toml(name, parent, child, 1))
        .unwrap()
        .rules
        .remove(0);
    authoring::create_draft(pool, &raw, Some("tenant:acme"), "emp-test")
        .await
        .unwrap();
    authoring::publish(pool, name, "emp-test").await.unwrap();
}

fn admin() -> String {
    serde_json::to_string(&User {
        id: "agent-claude".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: None,
    })
    .unwrap()
}

fn app(pool: sqlx::PgPool) -> axum::Router {
    let mut policy = FakePolicyClient::builder();
    for action in [
        Action::Read,
        Action::Create,
        Action::Publish,
        Action::Retire,
    ] {
        policy = policy.allow(
            "platform-admin",
            action,
            Resource::dispatcher_rule(),
            Scope::All,
        );
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    router(HttpState {
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
    })
}

async fn post(app: &axum::Router, uri: &str, body: Option<Value>) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("x-boss-user", admin());
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn active_version(pool: &sqlx::PgPool, name: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT version FROM dispatcher_rules WHERE name = $1 AND status = 'active'")
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// The active rules in the read surface's shape (each rule's `RawRule`,
/// as `GET /api/dispatcher/rules` serves it) — what the workflow publish
/// judge reads. Read row by row through the authoring reader, since this
/// fixture's registry holds only the rules it seeded.
async fn enforced(pool: &sqlx::PgPool) -> Vec<Value> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM dispatcher_rules WHERE status = 'active'")
            .fetch_all(pool)
            .await
            .unwrap();
    let mut out = Vec::new();
    for name in names {
        let rule = authoring::get_active(pool, &name).await.unwrap();
        out.push(serde_json::to_value(rule.to_raw()).unwrap());
    }
    out
}

fn draft_body(name: &str, parent: &str, child: &str, version: u32) -> Value {
    let raw = parse_raw(&rule_toml(name, parent, child, version))
        .unwrap()
        .rules
        .remove(0);
    let mut body = serde_json::to_value(raw).unwrap();
    body["source"] = json!("tenant:acme");
    body
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edge_into_a_passkey_protocol_leaves_only_by_car() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    let reg = PgWorkflows::for_fixture(pool.clone());
    live(&reg, "guarded", true).await;
    live(&reg, "parent", false).await;
    live(&reg, "plain", false).await;
    live(&reg, "plain-parent", false).await;
    seeded(&pool, "parent-files-guarded", "parent", "guarded").await;
    seeded(&pool, "plain-parent-files-plain", "plain-parent", "plain").await;
    let app = app(pool.clone());

    // 1. Retiring the edge's rule is refused, and the rule stays live.
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules/parent-files-guarded/retire",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("parent -> guarded"), "{said}");
    assert!(said.contains("passkey-protected"), "{said}");
    assert_eq!(active_version(&pool, "parent-files-guarded").await, Some(1));

    // ...so the parent's unsigned publish is still judged protected.
    reg.create_draft(
        spec(&row_toml("parent", "parent rewired", false)),
        &actor(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let baselines = boss_jobs::registry::passkey_baselines(&mut conn)
        .await
        .unwrap();
    let rules = enforced(&pool).await;
    let verdict = boss_jobs::publish_passkey::judge(
        "parent",
        &baselines,
        &spec(&row_toml("parent", "parent rewired", false)),
        Ok(&rules),
        None,
    );
    let refused = verdict.expect_err("the unsigned parent publish is refused");
    assert!(
        refused.to_string().contains("parent-files-guarded"),
        "{refused}"
    );

    // 2. A version that redirects the spawn to an unprotected kind.
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules",
        Some(draft_body("parent-files-guarded", "parent", "plain", 2)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{said}");
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules/parent-files-guarded/publish",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("parent -> guarded"), "{said}");
    assert_eq!(active_version(&pool, "parent-files-guarded").await, Some(1));

    // 3. A version that keeps the edge publishes...
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules",
        Some(draft_body("parent-files-guarded", "parent", "guarded", 3)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{said}");
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules/parent-files-guarded/publish",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    // ...and a rule whose edge another rule still draws retires.
    seeded(&pool, "parent-also-files-guarded", "parent", "guarded").await;
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules/parent-also-files-guarded/retire",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{said}");

    // 4. An edge into an unprotected kind retires on policy alone.
    let (status, said) = post(
        &app,
        "/api/dispatcher/rules/plain-parent-files-plain/retire",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{said}");
    assert_eq!(
        active_version(&pool, "plain-parent-files-plain").await,
        None
    );
}

/// Review R3 of car 8715776d: the edge graph a workflow publish judges
/// is read INSIDE the fence every passkey-edge write takes, never as a
/// snapshot before it. The race it pins: P is plain, C carries presence,
/// no rule joins them. An unsigned publish of P reads the graph; before
/// its write, an HTTP rule publish ADDS P -> C (adding removes nothing,
/// so the rule door admits it) and commits; the publish resumes and
/// writes P unsigned though P is now protected.
///
/// The rule reader below IS that pause: the first time the publish reads
/// the graph, it starts the edge add and lets it run until it either
/// commits or queues behind a lock. Commit means the read was outside
/// the fence and the publish judged a graph already stale; queueing
/// means the publish holds the fence, so it goes live first, as the plain
/// row it then was, and the edge lands after it.
struct RacingRules {
    pool: sqlx::PgPool,
    add: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>,
    landed_mid_read: std::sync::atomic::AtomicBool,
    queued: std::sync::atomic::AtomicBool,
}

/// Lock waits on an advisory lock in this test's database.
async fn advisory_waiters(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[async_trait::async_trait]
impl boss_jobs::department::rules::DispatcherRules for RacingRules {
    async fn enforced_rules(&self, _: &User) -> Result<Vec<Value>, String> {
        let graph = enforced(&self.pool).await;
        let mut slot = self.add.lock().await;
        if slot.is_none() {
            let pool = self.pool.clone();
            let add = tokio::spawn(async move {
                authoring::publish_from_http(&pool, "parent-files-guarded", "emp-test")
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if add.is_finished() {
                    self.landed_mid_read
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                if advisory_waiters(&self.pool).await > 0 {
                    self.queued.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the edge add neither committed nor queued in 30 s"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            *slot = Some(add);
        }
        Ok(graph)
    }
}

fn jobs_app(
    reg: Arc<dyn WorkflowRegistry>,
    rules: Arc<dyn boss_jobs::department::rules::DispatcherRules>,
) -> axum::Router {
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
    let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
    let mut policy = FakePolicyClient::builder();
    for action in [
        Action::Read,
        Action::Create,
        Action::Update,
        Action::Publish,
    ] {
        policy = policy.allow("platform-admin", action, Resource::workflow(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    boss_jobs::http::router(boss_jobs::http::JobsApiState {
        kind_registry: Some(reg),
        publish_rules: Some(rules),
        ..boss_jobs::http::JobsApiState::minimal(
            Arc::new(boss_jobs::InMemoryJobs::new()),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edge_added_mid_publish_is_judged_by_that_publish() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    let reg = PgWorkflows::for_fixture(pool.clone());
    live(&reg, "guarded", true).await;
    live(&reg, "parent", false).await;
    let raw = parse_raw(&rule_toml("parent-files-guarded", "parent", "guarded", 1))
        .unwrap()
        .rules
        .remove(0);
    authoring::create_draft(&pool, &raw, Some("tenant:acme"), "emp-test")
        .await
        .unwrap();
    reg.create_draft(
        spec(&row_toml("parent", "parent v2", false)),
        &actor(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let racing = Arc::new(RacingRules {
        pool: pool.clone(),
        add: tokio::sync::Mutex::new(None),
        landed_mid_read: Default::default(),
        queued: Default::default(),
    });
    let reg: Arc<dyn WorkflowRegistry> = Arc::new(reg);
    let app = jobs_app(reg.clone(), racing.clone());
    let publish = |app: axum::Router| async move {
        let req = Request::builder()
            .method("POST")
            .uri("/api/workflows/parent/publish")
            .header("x-boss-user", admin());
        let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    };

    let (status, said) = publish(app.clone()).await;
    let add = racing
        .add
        .lock()
        .await
        .take()
        .expect("the publish read the graph");
    let added = add.await.unwrap();
    let landed = racing
        .landed_mid_read
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        !(landed && status == StatusCode::OK),
        "the edge parent -> guarded committed between the publish's graph read and its \
         write, and the publish still went live unsigned: {said}"
    );
    assert!(
        racing.queued.load(std::sync::atomic::Ordering::SeqCst),
        "the edge add did not wait for the publish that was reading the graph"
    );
    // Fenced, the publish linearised first, over a parent that was plain.
    assert_eq!(status, StatusCode::OK, "{said}");
    // The add is admitted after it, and the parent is protected from then.
    assert_eq!(added, Ok(()));
    assert_eq!(active_version(&pool, "parent-files-guarded").await, Some(1));
    reg.create_draft(
        spec(&row_toml("parent", "parent v3", false)),
        &actor(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let (status, said) = publish(app).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("parent-files-guarded"), "{said}");
}

/// The fence is the ADAPTER's, so it binds both publish roads: the
/// endpoint above (`publish_guarded`) and the workflow-publish step
/// (`publish_authored_guarded`), which hands the registry the same
/// guard. Each road's guard, run inside the write, sees an HTTP rule
/// write queue behind it rather than commit (review R3 of car 8715776d).
#[tokio::test(flavor = "multi_thread")]
async fn both_publish_roads_judge_inside_the_passkey_edges_fence() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    let reg = PgWorkflows::for_fixture(pool.clone());
    live(&reg, "guarded", true).await;
    live(&reg, "parent", false).await;
    for (n, road) in ["publish_guarded", "publish_authored_guarded"]
        .into_iter()
        .enumerate()
    {
        let rule = format!("parent-files-guarded-{n}");
        let raw = parse_raw(&rule_toml(&rule, "parent", "guarded", 1))
            .unwrap()
            .rules
            .remove(0);
        authoring::create_draft(&pool, &raw, Some("tenant:acme"), "emp-test")
            .await
            .unwrap();
        let add = Arc::new(tokio::sync::Mutex::new(None));
        let (guard_pool, guard_add, guard_rule) = (pool.clone(), add.clone(), rule.clone());
        let guard = move |_: Vec<boss_jobs::registry::WorkflowSpec>,
                          _: boss_jobs::registry::WorkflowSpec|
              -> boss_jobs::registry::PublishVerdict {
            let (pool, add, rule) = (guard_pool.clone(), guard_add.clone(), guard_rule.clone());
            Box::pin(async move {
                let spawned = pool.clone();
                let handle = tokio::spawn(async move {
                    authoring::publish_from_http(&spawned, &rule, "emp-test")
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while advisory_waiters(&pool).await == 0 {
                    assert!(
                        !handle.is_finished(),
                        "the rule write committed while the guard was judging"
                    );
                    assert!(std::time::Instant::now() < deadline, "no queue in 30 s");
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                *add.lock().await = Some(handle);
                Ok(())
            })
        };
        let candidate = spec(&row_toml("parent", &format!("parent via {road}"), false));
        if n == 0 {
            reg.create_draft(candidate, &actor(), chrono::Utc::now())
                .await
                .unwrap();
            reg.publish_guarded("parent", &guard, &actor(), chrono::Utc::now())
                .await
                .unwrap();
        } else {
            let job = boss_core::job::JobId::from_uuid(uuid::Uuid::new_v4());
            reg.publish_authored_guarded(candidate, job, &guard, &actor(), chrono::Utc::now())
                .await
                .unwrap();
        }
        let handle = add.lock().await.take().expect("the guard ran");
        assert_eq!(handle.await.unwrap(), Ok(()), "{road}: the add lands after");
        assert_eq!(active_version(&pool, &rule).await, Some(1), "{road}");
    }
}
