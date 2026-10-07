//! Real classes write -> role registry -> authenticated people read ->
//! unchanged policy HTTP engine. A finite request budget detects a
//! resolver accidentally mounted inside policy or inside its registry reads.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::get};
use boss_classes::{InMemoryClasses, http::ClassesApiState, role_reports};
use boss_core::{machine_token::Source, primitives::Class};
use boss_policy::{
    check_mode::{CheckMode, Mode},
    http::PolicyApiState,
};
use boss_policy_client::role_reader::HttpRoleReader;
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{
    Action, CurrentUser, InMemoryPolicy, PolicyClient, PolicyEngine, PolicyRule,
    ReqwestPolicyClient, Resource, Scope, User, controls,
};
use serde_json::{Value, json};

const REGISTRY_BUDGET: usize = 4;

struct UnreadCoverage;

#[async_trait::async_trait]
impl boss_policy::coverage::CoverageSources for UnreadCoverage {
    async fn roster(&self) -> Result<Vec<boss_policy_client::coverage::Person>, String> {
        panic!("a policy check must not read the write guard's roster")
    }
    async fn keys(&self) -> Result<Vec<boss_policy_client::coverage::Key>, String> {
        panic!("a policy check must not read the write guard's keys")
    }
    async fn workflows(&self) -> Result<Vec<boss_policy_client::coverage::WorkflowFacts>, String> {
        panic!("a policy check must not read the write guard's workflows")
    }
}

#[tokio::test]
async fn classes_report_names_an_unloaded_snapshot_without_claiming_a_clean_window() {
    use boss_policy_client::FakePolicyClient;
    use boss_policy_client::role_reader::{MonotonicRoleSnapshotClock, SnapshotRoleReader};
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(MonotonicRoleSnapshotClock),
    ));
    let app = role_reports::mount_snapshot(
        ClassesApiState {
            classes: Arc::new(InMemoryClasses::new(vec![])),
            policy: Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "report-reader",
                        Action::Read,
                        Resource::policy_rule(),
                        Scope::All,
                    )
                    .build(),
            ),
        },
        roles,
        Arc::new(ReportMode::Report),
        Arc::new(ReportTally::new(8)),
    );
    let (url, task) = serve(app).await;
    let mut user = User::service("reader");
    user.role = "report-reader".into();
    let response = reqwest::Client::new()
        .get(format!("{url}/api/classes/actor-role-reports"))
        .header("x-boss-user", serde_json::to_string(&user).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["snapshot"]["state"], "never-loaded");
    assert_eq!(body["report"]["durable_window"], false);
    task.abort();
}

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}

#[derive(Clone)]
struct Registry {
    reads: Arc<AtomicUsize>,
    policy: Arc<dyn PolicyClient>,
    role: Value,
    dark: bool,
}

fn budget(registry: &Registry) {
    assert!(
        registry.reads.fetch_add(1, Ordering::SeqCst) < REGISTRY_BUDGET,
        "registry/policy recursion crossed the finite budget"
    );
}

async fn agents(State(r): State<Registry>, CurrentUser(user): CurrentUser) -> Json<Value> {
    budget(&r);
    assert_eq!(user.id, "automation:classes");
    Json(
        json!({"data":[{"id":"agent-example", "aliases":["agent-alias"], "role": r.role}], "total":1}),
    )
}

async fn automations(State(r): State<Registry>, CurrentUser(user): CurrentUser) -> Json<Value> {
    budget(&r);
    assert_eq!(user.id, "automation:classes");
    Json(json!({"data":[], "total":0}))
}

async fn people(
    State(r): State<Registry>,
    CurrentUser(user): CurrentUser,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    budget(&r);
    assert_eq!(user.id, "automation:classes");
    // The actual people roster's authenticated policy read. This HTTP
    // policy adapter is NOT the classes role decorator, so it cannot
    // re-enter role lookup. This is the production dependency graph.
    assert!(
        r.policy
            .ask(&user, controls::READ_EMPLOYEE)
            .await
            .unwrap()
            .is_allowed()
    );
    if r.dark {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!([])).into_response()
}

async fn exercise(role: Value, dark: bool, asserted: &str, want: u16, expected: Option<bool>) {
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let repo = Arc::new(InMemoryPolicy::with_rules([
        PolicyRule::new(
            "platform-admin",
            Resource::class(),
            Action::Create,
            Scope::All,
        ),
        PolicyRule::new(
            "platform-admin",
            Resource::employee(),
            Action::Read,
            Scope::All,
        ),
        PolicyRule::new(
            "platform-admin",
            Resource::policy_rule(),
            Action::Read,
            Scope::All,
        ),
        PolicyRule::new(
            "scope-reader",
            Resource::policy_rule(),
            Action::Read,
            Scope::Self_,
        ),
    ]));
    let policy_app = boss_policy::http::router(PolicyApiState {
        engine: Arc::new(PolicyEngine::new(repo.clone())),
        repo,
        check_mode: CheckMode::fixed(Mode::Off),
        sources: Arc::new(UnreadCoverage),
    })
    .layer(axum::middleware::from_fn_with_state(
        policy_calls.clone(),
        async |State(count): State<Arc<AtomicUsize>>,
               request: axum::extract::Request,
               next: axum::middleware::Next| {
            assert!(
                count.fetch_add(1, Ordering::SeqCst) < 8,
                "policy recursion crossed finite budget"
            );
            next.run(request).await
        },
    ));
    let (policy_url, policy_task) = serve(policy_app).await;
    let inner: Arc<dyn PolicyClient> = Arc::new(ReqwestPolicyClient::with_source(
        "classes",
        policy_url,
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    ));
    let reads = Arc::new(AtomicUsize::new(0));
    let registry = Registry {
        reads: reads.clone(),
        policy: inner.clone(),
        role,
        dark,
    };
    let (registry_url, registry_task) = serve(
        Router::new()
            .route("/api/agents", get(agents))
            .route("/api/agents/automations", get(automations))
            .route("/api/people", get(people))
            .with_state(registry),
    )
    .await;
    let roles = Arc::new(
        HttpRoleReader::with_source(
            registry_url.clone(),
            registry_url,
            User::service("classes"),
            Duration::from_secs(2),
            Arc::new(Source::fixed(None)),
        )
        .unwrap(),
    );
    let classes = Arc::new(InMemoryClasses::new(vec![]));
    let tally = Arc::new(ReportTally::new(16));
    let snapshot = Arc::new(boss_policy_client::role_reader::SnapshotRoleReader::new(
        Duration::from_secs(30),
        Arc::new(boss_policy_client::role_reader::MonotonicRoleSnapshotClock),
    ));
    let refreshed = snapshot.refresh_from(roles.as_ref()).await;
    assert_eq!(
        refreshed,
        if dark {
            boss_policy_client::role_reader::RoleRefreshOutcome::Unavailable
        } else {
            boss_policy_client::role_reader::RoleRefreshOutcome::Published
        }
    );
    let app = role_reports::mount_snapshot(
        ClassesApiState {
            classes: classes.clone(),
            policy: inner,
        },
        snapshot,
        Arc::new(ReportMode::Report),
        tally.clone(),
    );
    let (classes_url, classes_task) = serve(app).await;
    let mut caller = User::service("example");
    caller.id = "agent-alias".into();
    caller.role = asserted.into();
    caller.department = Some("unchanged-department".into());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let row = Class {
        subject_kind: "employee".into(),
        code: "tenant-custom-role".into(),
        display_name: "Tenant role".into(),
        parent_code: None,
        member_attribute: Some("role".into()),
        metadata: json!({}),
        sort_order: 1,
        retired_at: None,
    };
    let response = http
        .post(format!("{classes_url}/api/classes/batch"))
        .header("x-boss-user", serde_json::to_string(&caller).unwrap())
        .json(&vec![row])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), want);
    assert_eq!(classes.recorded_events().len(), usize::from(want == 200));
    if want == 200 {
        assert_eq!(
            classes.recorded_events()[0].payload["_actor"],
            "agent-alias",
            "canonical identity is comparison data, never substituted in the event"
        );
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "one bounded registry read per source"
    );
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    let observation = &report.rows[0].observation;
    assert_eq!(observation.would_deny, expected);
    assert_eq!(observation.actor, "agent-alias");
    assert_eq!(observation.asserted_role, asserted);
    assert!(!report.durable_window);
    // The report has role details, so the existing Read policy-rule/all
    // door protects it. Neither an anonymous nor a denied caller reads it.
    assert_eq!(
        http.get(format!("{classes_url}/api/classes/actor-role-reports"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    let mut reader = caller.clone();
    reader.role = "no-grants".into();
    assert_eq!(
        http.get(format!("{classes_url}/api/classes/actor-role-reports"))
            .header("x-boss-user", serde_json::to_string(&reader).unwrap())
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    reader.role = "scope-reader".into();
    assert_eq!(
        http.get(format!("{classes_url}/api/classes/actor-role-reports"))
            .header("x-boss-user", serde_json::to_string(&reader).unwrap())
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    reader.role = "platform-admin".into();
    let response = http
        .get(format!("{classes_url}/api/classes/actor-role-reports"))
        .header("x-boss-user", serde_json::to_string(&reader).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["report"]["durable_window"],
        false
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "reading telemetry does not trigger resolver work"
    );
    assert!(policy_calls.load(Ordering::SeqCst) <= 8);
    classes_task.abort();
    registry_task.abort();
    policy_task.abort();
}

#[tokio::test]
async fn real_http_report_comparison_cannot_recurse_or_change_allow_deny_or_event_identity() {
    exercise(
        json!("tenant-engineer"),
        false,
        "platform-admin",
        200,
        Some(true),
    )
    .await;
    exercise(
        json!("platform-admin"),
        false,
        "tenant-engineer",
        403,
        Some(false),
    )
    .await;
    exercise(Value::Null, false, "platform-admin", 200, None).await;
    exercise(json!("tenant-engineer"), true, "platform-admin", 200, None).await;
}

#[tokio::test]
async fn the_http_budget_control_detects_an_adversarial_recursive_registry_policy_graph() {
    use axum::response::IntoResponse;
    // Deliberately construct the rejected dependency graph in loopback:
    // people -> policy/check -> registry role reader -> people. The same
    // request budget used above must fire, rather than the positive test
    // merely carrying an unexercised guard. This is never production wiring.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reads = Arc::new(AtomicUsize::new(0));
    let policy = Arc::new(ReqwestPolicyClient::with_source(
        "people",
        base.clone(),
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    ));
    let people_reads = reads.clone();
    let policy_registry = base.clone();
    let app = Router::new()
        .route(
            "/api/agents",
            get(|| async { Json(json!({"data":[],"total":0})) }),
        )
        .route(
            "/api/agents/automations",
            get(|| async { Json(json!({"data":[],"total":0})) }),
        )
        .route(
            "/api/people",
            get(move |CurrentUser(user): CurrentUser| {
                let policy = policy.clone();
                let reads = people_reads.clone();
                async move {
                    assert_eq!(user.id, "automation:classes");
                    if reads.fetch_add(1, Ordering::SeqCst) >= REGISTRY_BUDGET {
                        return axum::http::StatusCode::LOOP_DETECTED.into_response();
                    }
                    match policy.ask(&user, controls::READ_EMPLOYEE).await {
                        Ok(_) => Json(json!([])).into_response(),
                        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
            }),
        )
        .route(
            "/api/policy/check",
            axum::routing::post(move |CurrentUser(user): CurrentUser| {
                let base = policy_registry.clone();
                async move {
                    assert_eq!(user.id, "automation:people");
                    let roles = HttpRoleReader::with_source(
                        base.clone(),
                        base,
                        User::service("classes"),
                        Duration::from_secs(2),
                        Arc::new(Source::fixed(None)),
                    )
                    .unwrap();
                    use boss_core::role_of_record::RoleOfRecord;
                    match roles.role_for("actor").await {
                        Ok(_) => Json(json!({"decision":"allow","scope":"all"})).into_response(),
                        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
            }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let roles = HttpRoleReader::with_source(
        base.clone(),
        base,
        User::service("classes"),
        Duration::from_secs(2),
        Arc::new(Source::fixed(None)),
    )
    .unwrap();
    use boss_core::role_of_record::RoleOfRecord;
    let result = tokio::time::timeout(Duration::from_secs(3), roles.role_for("actor"))
        .await
        .unwrap();
    assert!(
        result.is_err(),
        "a recursively authorized registry is unknown, never absence"
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        REGISTRY_BUDGET + 1,
        "the actual HTTP negative control reached and fired the shared budget"
    );
    task.abort();
}
