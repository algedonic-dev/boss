//! A registered agent writes with the role its registry row holds.
//!
//! Backlog 4e51bf23 (review 42cd8068, N1), measured again 2026-10-08 on
//! origin/main 10d780b3c: the pod's doors send `platform-admin` at
//! operator tier for every caller that names itself, the login door
//! replaced only the id (`User { id: actor, ..user }`), and the step
//! authority check read that asserted role first. So `agent-codex`,
//! whose live row holds role `null`, wrote as platform-admin, and
//! `agent-claude`, whose row holds `engineering-agent`, did too. A
//! row's role decided nothing.
//!
//! The door now judges a registered agent by its row, behind a mode
//! word of its own (`agent-role`; absent is `off`): under `enforce` the
//! role and the tier come from the row and whatever the caller asserted
//! is dropped, and a 403 that follows names who, which row, what the
//! row gives and what was asserted. Pinned here through the in-memory
//! registry and the real router:
//!
//!  - a row with role `null` is refused a platform-admin step;
//!  - a row with role `platform-admin` is admitted to it;
//!  - an asserted role above the row's is ignored, and the refusal
//!    says so;
//!  - an `agent-*` id with no row holds no role;
//!  - a person signing an `emp-*` id on the same door is untouched;
//!  - with the word at `off` or `report` nothing changes at all, which
//!    is what makes this car's landing refuse no live caller.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::machine_gate::Mode;
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agents::{AgentInput, AgentsRegistry, InMemoryAgents, LoginDoor, resolve_login};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![PERSON.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == PERSON)
    }
}

const PERSON: &str = "emp-bootstrap-admin";
const ADMIN_LOGIN: &str = "claude@algedonic.dev";
const ADMIN_AGENT: &str = "agent-claude";
const NARROW_LOGIN: &str = "narrow@algedonic.dev";
const NARROW_AGENT: &str = "agent-narrow";
const NULL_LOGIN: &str = "codex@algedonic.dev";
const NULL_AGENT: &str = "agent-codex";
/// The role a row with none writes as: the one core gives a caller that
/// carries none.
const NO_ROLE: &str = boss_core::roles::GUEST_ROLE;

fn agent(id: &str, login: &str, role: Option<&str>) -> AgentInput {
    AgentInput {
        id: id.to_string(),
        display_name: id.to_string(),
        default_model: "opus-5".to_string(),
        aliases: vec![login.to_string()],
        role: role.map(str::to_string),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    }
}

/// The real router behind the login door, layered as `boss_jobs_api.rs`
/// layers it, with the door's word held at `mode`.
///
/// The policy grants EVERY role here the coarse step write and the
/// packet read, so the one thing left to refuse a claim is the step's
/// own `authority_role` — the check the packet names.
async fn app(mode: Mode) -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for role in ["platform-admin", "engineering-agent", NO_ROLE] {
        policy = policy
            .allow(role, Action::Create, Resource::job(), Scope::All)
            .allow(role, Action::Read, Resource::job(), Scope::All)
            .allow(role, Action::Update, Resource::step(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher.clone(),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    // The rows land through the port, as a tenant's declaration does.
    let registry = Arc::new(InMemoryAgents::new());
    let stamp = publisher.stamp_with_actor(publisher.default_actor()).await;
    registry
        .publish(
            &[
                agent(ADMIN_AGENT, ADMIN_LOGIN, Some("platform-admin")),
                agent(NARROW_AGENT, NARROW_LOGIN, Some("engineering-agent")),
                agent(NULL_AGENT, NULL_LOGIN, None),
            ],
            PublishMode::InsertIfAbsent,
            &stamp,
        )
        .await
        .expect("declare the agents");
    let door = Arc::new(LoginDoor::new(registry, publisher).judging_rows_when(move || mode));
    router(state)
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login))
}

/// The header the pod's doors build today: the id, and `platform-admin`
/// at operator tier whoever the id is.
fn asserting_admin(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}}"#
    )
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
    (status, json)
}

fn req(method: &str, uri: &str, signed_as: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", asserting_admin(signed_as))
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// A `ship-a-change` packet opened by the PERSON, and its `scope` step
/// — ready at admission, `authority_role = "platform-admin"`.
async fn a_platform_admin_step(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            PERSON,
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": PERSON,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a person's create is untouched: {job}"
    );
    let job_id = job["id"].as_str().expect("job id").to_string();
    let (status, full) = send(
        app,
        req(
            "GET",
            &format!("/api/jobs/{job_id}"),
            PERSON,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let scope = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("scope step present")
        .clone();
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    assert_eq!(
        scope["metadata"]["authority_role"], "platform-admin",
        "precondition: the step wants platform-admin"
    );
    (job_id, scope["id"].as_str().expect("step id").to_string())
}

async fn claim(app: &axum::Router, signed_as: &str) -> (StatusCode, serde_json::Value) {
    let (job_id, step_id) = a_platform_admin_step(app).await;
    send(
        app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps/{step_id}/claim"),
            signed_as,
            serde_json::json!({}),
        ),
    )
    .await
}

#[tokio::test]
async fn a_row_with_no_role_is_refused_a_platform_admin_step() {
    let app = app(Mode::Enforce).await;
    let (status, body) = claim(&app, NULL_LOGIN).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["authority_role"], "platform-admin",
        "what the step wanted: {body}"
    );
    let said = &body["role_of_record"];
    assert_eq!(said["actor"], NULL_AGENT, "who: {body}");
    assert_eq!(said["signed_as"], NULL_LOGIN, "{body}");
    assert_eq!(said["registry_row"], NULL_AGENT, "which row: {body}");
    assert_eq!(
        said["row_role"],
        serde_json::Value::Null,
        "what the row gives: {body}"
    );
    assert_eq!(said["judged_as_role"], NO_ROLE, "{body}");
    assert_eq!(said["judged_at_tier"], "user", "{body}");
    assert_eq!(said["asserted_role"], "platform-admin", "{body}");
    let why = said["why"].as_str().expect("why");
    assert!(
        why.contains("asserted `platform-admin`") && why.contains("ignored"),
        "the refusal says the asserted role was ignored: {why}"
    );
}

#[tokio::test]
async fn a_row_holding_platform_admin_is_admitted_to_it() {
    let app = app(Mode::Enforce).await;
    let (status, body) = claim(&app, ADMIN_LOGIN).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(body["assignee_id"], ADMIN_AGENT, "{body}");
    assert!(
        body.get("role_of_record").is_none(),
        "an admitted write carries no refusal note: {body}"
    );
}

#[tokio::test]
async fn an_asserted_role_above_the_rows_is_ignored_and_the_refusal_says_so() {
    let app = app(Mode::Enforce).await;
    // Signed with the canonical id rather than the alias: the row is
    // found either way.
    for signed_as in [NARROW_LOGIN, NARROW_AGENT] {
        let (status, body) = claim(&app, signed_as).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{signed_as}: {body}");
        let compared = body["actor_roles"].as_array().expect("actor_roles");
        assert!(
            !compared.is_empty() && compared.iter().all(|r| r == "engineering-agent"),
            "the authority check saw the row's role and no other: {body}"
        );
        let said = &body["role_of_record"];
        assert_eq!(said["actor"], NARROW_AGENT, "{body}");
        assert_eq!(said["signed_as"], signed_as, "{body}");
        assert_eq!(said["row_role"], "engineering-agent", "{body}");
        assert_eq!(said["judged_as_role"], "engineering-agent", "{body}");
        assert_eq!(said["judged_at_tier"], "user", "{body}");
        assert_eq!(said["asserted_role"], "platform-admin", "{body}");
        assert_eq!(said["asserted_tier"], "operator", "{body}");
        let why = said["why"].as_str().expect("why");
        assert!(
            why.contains("agent-narrow")
                && why.contains("`engineering-agent`")
                && why.contains("asserted `platform-admin`")
                && why.contains("ignored"),
            "{why}"
        );
    }
}

#[tokio::test]
async fn an_agent_id_with_no_row_holds_no_role() {
    let app = app(Mode::Enforce).await;
    let (status, body) = claim(&app, "agent-ghost").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let said = &body["role_of_record"];
    assert_eq!(said["actor"], "agent-ghost", "{body}");
    assert_eq!(said["registry_row"], serde_json::Value::Null, "{body}");
    assert_eq!(said["judged_as_role"], NO_ROLE, "{body}");
    assert!(
        said["why"]
            .as_str()
            .expect("why")
            .contains("the agents registry holds no row"),
        "{body}"
    );
}

/// A person on the machine door (`emp-david` running `boss` on the LAN)
/// is not in the agents registry and is not this door's question: the
/// claim that worked before works now, under `enforce`.
#[tokio::test]
async fn a_person_signing_an_employee_id_is_untouched() {
    let app = app(Mode::Enforce).await;
    let (status, body) = claim(&app, PERSON).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(body["assignee_id"], PERSON, "{body}");
}

/// What makes the landing safe: until the word says `enforce` the door
/// does exactly what it did, for every row.
#[tokio::test]
async fn off_and_report_change_nothing() {
    for mode in [Mode::Off, Mode::Report] {
        let app = app(mode).await;
        for (signed_as, lands_as) in [
            (NULL_LOGIN, NULL_AGENT),
            (NARROW_LOGIN, NARROW_AGENT),
            (ADMIN_LOGIN, ADMIN_AGENT),
            ("agent-ghost", "agent-ghost"),
        ] {
            let (status, body) = claim(&app, signed_as).await;
            assert!(
                status.is_success(),
                "{mode:?} {signed_as}: {status}: {body}"
            );
            assert_eq!(body["assignee_id"], lands_as, "{mode:?}: {body}");
        }
    }
}
