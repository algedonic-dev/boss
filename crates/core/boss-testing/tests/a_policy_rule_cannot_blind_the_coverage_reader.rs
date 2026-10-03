//! A policy rule cannot blind the coverage reader's workflow read
//! (backlog 47aed706, the follow-up of review 771c1308 on car 0df4f091).
//!
//! WHY IT EXISTS. The policy service's coverage read — the hourly
//! backstop today, and the write guard of design b08725c2 once it
//! boards — reads three sources as `automation:policy-coverage` at the
//! operator tier (`boss_policy::coverage::COVERAGE_READER`). Two never
//! ask policy: the roster admits machinery by tier (boss-people
//! `grants::roster_scope`) and the passkey tier counts by the machinery
//! read gate. The third, `GET /api/workflows`, asked policy
//! `READ_WORKFLOW` for that reader's id and role. So the reader's sight
//! hung on a policy row: once a SECOND real person holds workflow read,
//! a rule narrowing `platform-admin:workflow:read` to scope none keeps
//! the control held (by the other person), the guard accepts it — and
//! from then on the guard cannot read the workflows it judges by.
//! Reproduced by the reviewer on the guard's branch: every non-exempt
//! policy write then answers 503, and only break-glass restoring the
//! shipped rule repairs it. It cannot happen while the founder is the
//! only holder, which is why it must be closed before the Apple Passkey
//! Delegate or any second person is enrolled (backlog f12a8980).
//!
//! THE FIX is at the source, the way people's roster already does it:
//! the jobs API admits an operator-tier caller to the active-workflow
//! list without asking policy, so no rule can blind the reader. It
//! widens one read, for machinery only, of rows the shipped basic guest
//! already reads, and refuses nothing.
//!
//! THE SCENE is the reviewer's: two real people holding workflow read
//! (the founder and a delegate, each with a bound key), the narrowing
//! rule applied, and then the guard's own reader — `HttpCoverageSources`
//! over a real socket to the jobs router, with policy decided by the
//! real engine over the same rules. It lives in boss-testing because a
//! boss-jobs edge to boss-policy points upward (layer-order-audit).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec, WorkflowStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy::coverage::{COVERAGE_READER, CoverageSources, HttpCoverageSources};
use boss_policy_client::coverage::{self, Control, Key, Person};
use boss_policy_client::{
    AccessTier, Action, Decision, InMemoryPolicy, PolicyClient, PolicyClientError, PolicyEngine,
    PolicyRepository, PolicyRule, Predicate, Resource, Scope, User, controls,
};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

/// The policy service's own decision, in process: the engine over the
/// same rules the coverage report reads.
struct Engine(PolicyEngine<InMemoryPolicy>);

#[async_trait]
impl PolicyClient for Engine {
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

const DELEGATE_ROLE: &str = "delegate";

fn person(id: &str, role: &str, hire_date: &str) -> Person {
    Person {
        id: id.into(),
        role: Some(role.into()),
        active: true,
        hire_date: Some(hire_date.parse().unwrap()),
    }
}

fn key(employee_id: &str) -> Key {
    Key {
        employee_id: employee_id.into(),
        access_tier: AccessTier::User,
    }
}

fn session(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.into(),
        role: role.into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    })
    .unwrap()
}

/// One active workflow whose step names an authority role — the kind of
/// row the coverage read derives a control from.
fn active_workflow() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        "passkey-promotion",
        "Promote a passkey",
        "test",
        vec!["system".into()],
        vec![
            StepSpec {
                title: "authorise".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.authorise.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    );
    spec.status = WorkflowStatus::Active;
    spec
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_narrowing_the_readers_role_leaves_the_coverage_read_whole() {
    // Two real people hold workflow read: the founder through the
    // shipped platform-admin rule, a delegate through its own role.
    let repo = Arc::new(InMemoryPolicy::new());
    for r in boss_policy_client::defaults::default_rules() {
        repo.upsert_rule(&r, "test").await.unwrap();
    }
    repo.upsert_rule(
        &PolicyRule::new(
            DELEGATE_ROLE,
            Resource::workflow(),
            Action::Read,
            Scope::All,
        ),
        "test",
    )
    .await
    .unwrap();
    let roster = vec![
        person("emp-founder", "platform-admin", "2026-01-01"),
        person("emp-delegate", DELEGATE_ROLE, "2026-09-30"),
    ];
    let keys = vec![key("emp-founder"), key("emp-delegate")];

    // The narrowing: platform-admin's workflow read to scope none.
    repo.upsert_rule(
        &PolicyRule::new(
            "platform-admin",
            Resource::workflow(),
            Action::Read,
            Scope::None,
        ),
        "emp-founder",
    )
    .await
    .unwrap();

    // Coverage still finds a holder, so a guard judging this write by
    // coverage accepts it — the premise of the reproduction.
    let read_workflow = Control::Policy {
        action: controls::READ_WORKFLOW.action(),
        resource: controls::READ_WORKFLOW.resource_name().to_string(),
        all_only: controls::READ_WORKFLOW.all_only(),
    }
    .id();
    let rules = repo.list_rules().await.unwrap();
    let held = coverage::report(&coverage::controls(&[]), &rules, &[], &roster, &keys);
    let row = held
        .iter()
        .find(|h| h.control == read_workflow)
        .unwrap_or_else(|| panic!("{read_workflow} is a declared control: {held:?}"));
    assert_eq!(
        row.holders,
        vec!["emp-delegate".to_string()],
        "after the narrowing only the delegate holds {read_workflow}, so it is not orphaned"
    );

    // The jobs API, deciding by the real engine over those rules.
    let registry = Arc::new(InMemoryWorkflows::new());
    registry.seed(active_workflow()).unwrap();
    let registry: Arc<dyn WorkflowRegistry> = registry;
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(Engine(PolicyEngine::new(repo.clone())));
    let app = router(JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        kind_registry: Some(registry),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });

    // The control: the narrowing bites — the founder's own session (the
    // reader's role, at the user tier) is refused, and the delegate
    // still reads.
    for (who, role, want) in [
        ("emp-founder", "platform-admin", StatusCode::FORBIDDEN),
        ("emp-delegate", DELEGATE_ROLE, StatusCode::OK),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::get("/api/workflows")
                    .header("x-boss-user", session(who, role))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), want, "{who} ({role}) reading /api/workflows");
    }

    // The guard's own reader, over a socket, signing COVERAGE_READER.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reader = HttpCoverageSources::new("http://127.0.0.1:9", format!("http://{addr}"));
    let workflows = reader.workflows().await.unwrap_or_else(|e| {
        panic!(
            "a rule on the reader's role blinded the coverage read ({COVERAGE_READER}): {e} — \
             the guard would answer 503 to every non-exempt policy write"
        )
    });
    assert_eq!(
        workflows
            .iter()
            .map(|w| w.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["passkey-promotion"],
        "the reader sees the active workflows"
    );
    assert!(
        coverage::controls(&workflows)
            .iter()
            .any(|c| c.id() == "authority:platform-admin"),
        "and derives the controls they name"
    );
}
