//! A workflow publish needs a person's passkey iff the LIVE row is
//! passkey-protected — and the SERVER says so (backlog a4b57aca).
//!
//! David, 2026-11-08: "only require passkey approval if the current
//! protocol requires a passkey anywhere ... an agent can't strip a
//! passkey requirement off a protocol anywhere". Until this car the
//! passkey on a protocol publish lived only in `publish-workflow.sh`;
//! `POST /api/workflows/{kind}/publish` asked policy and published any
//! row, a presence-carrying one included. These pin the server:
//!
//! (a) a live presence row, published unsigned, is refused;
//! (b) a candidate REMOVING presence from it, unsigned, is refused;
//! (c) a parent re-wired away from a protected child, unsigned, is refused;
//! (d) a plain row and a new kind publish on policy alone;
//! (e) a candidate ADDING presence publishes on policy alone;
//! (f) a stamped publish of a protected row is accepted, and only for the
//!     signed bytes over the signed live version;
//! (g) publish-a-protocol itself is protected.
//!
//! Every case runs against both registry adapters; the jobs store that
//! holds the signing packet is the in-memory one.

mod signed_publish;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::step_shape_hash;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::department::rules::FakeDispatcherRules;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{WorkflowRegistry, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use signed_publish::{sha, signed_packet};
use tower::ServiceExt;

fn agent() -> User {
    User {
        id: "agent-claude".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn actor() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Automation("test".into())
}

/// A viable row in the platform bundle's own TOML shape. `presence`
/// puts a passkey on its sign-off step; `label` lets a candidate differ.
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

fn spec(text: &str) -> WorkflowSpec {
    boss_jobs::seed_loader::parse_workflows(text, "platform", "fixture.toml")
        .expect("the fixture row parses")
        .remove(0)
}

async fn live(reg: &dyn WorkflowRegistry, text: &str) {
    let s = spec(text);
    let kind = s.kind.clone();
    reg.create_draft(s, &actor(), chrono::Utc::now())
        .await
        .unwrap();
    reg.publish(&kind, &actor(), chrono::Utc::now())
        .await
        .unwrap();
}

async fn draft(reg: &dyn WorkflowRegistry, text: &str) {
    reg.create_draft(spec(text), &actor(), chrono::Utc::now())
        .await
        .unwrap();
}

/// A rule that admits `child` packets from `parent` packets — the
/// `jobs.spawn` shape the live dispatcher serves.
fn spawn_rule(parent: &str, child: &str) -> Value {
    json!({
        "name": format!("{parent}-files-{child}"),
        "on_event": "jobs.job.closed",
        "when": format!("kind = \"{parent}\""),
        "do": [{"handler": "jobs.spawn", "args": {"kind": format!("\"{child}\"")}}],
    })
}

fn app(reg: Arc<dyn WorkflowRegistry>, jobs: Arc<InMemoryJobs>, rules: Vec<Value>) -> Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Read,
                Resource::workflow(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Create,
                Resource::workflow(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::workflow(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    router(JobsApiState {
        kind_registry: Some(reg),
        publish_rules: Some(Arc::new(FakeDispatcherRules(rules))),
        ..JobsApiState::minimal(
            jobs,
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn publish(app: Router, kind: &str, body: Option<Value>) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workflows/{kind}/publish"))
        .header("x-boss-user", serde_json::to_string(&agent()).unwrap())
        .header("content-type", "application/json");
    let body = body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap()));
    let resp = app.oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn active_label(reg: &dyn WorkflowRegistry, kind: &str) -> String {
    reg.get_active(kind).await.unwrap().label
}

async fn the_rule_holds(reg: Arc<dyn WorkflowRegistry>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let guarded = row_toml("guarded", "Guarded v1", true);
    live(reg.as_ref(), &guarded).await;
    live(reg.as_ref(), &row_toml("parent", "Parent v1", false)).await;
    live(reg.as_ref(), &row_toml("plain", "Plain v1", false)).await;
    live(reg.as_ref(), &row_toml("growing", "Growing v1", false)).await;
    let rules = vec![spawn_rule("parent", "guarded")];
    let app = || app(reg.clone(), jobs.clone(), rules.clone());

    // (a) a live presence row, published unsigned, is refused.
    draft(reg.as_ref(), &row_toml("guarded", "Guarded v2", true)).await;
    let (status, said) = publish(app(), "guarded", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("passkey-protected"), "{said}");
    assert!(
        said.contains("step approve declares assurance_required = presence"),
        "{said}"
    );
    assert_eq!(active_label(reg.as_ref(), "guarded").await, "Guarded v1");
    reg.discard_draft("guarded", 2, &actor(), chrono::Utc::now())
        .await
        .unwrap();

    // (b) a candidate that REMOVES presence is judged on the live row.
    draft(
        reg.as_ref(),
        &row_toml("guarded", "Guarded stripped", false),
    )
    .await;
    let (status, said) = publish(app(), "guarded", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert_eq!(active_label(reg.as_ref(), "guarded").await, "Guarded v1");
    reg.discard_draft("guarded", 3, &actor(), chrono::Utc::now())
        .await
        .unwrap();

    // (c) a parent re-wired away from its protected child is refused:
    // the live rules say it admits `guarded`, whatever the candidate says.
    draft(reg.as_ref(), &row_toml("parent", "Parent rewired", false)).await;
    let (status, said) = publish(app(), "parent", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("admits or fulfils guarded packets"), "{said}");
    assert!(said.contains("parent-files-guarded"), "{said}");
    assert_eq!(active_label(reg.as_ref(), "parent").await, "Parent v1");

    // (d) a plain row, and a kind with no live row, need policy alone.
    draft(reg.as_ref(), &row_toml("plain", "Plain v2", false)).await;
    let (status, said) = publish(app(), "plain", None).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(active_label(reg.as_ref(), "plain").await, "Plain v2");
    draft(reg.as_ref(), &row_toml("brand-new", "Brand new", true)).await;
    let (status, said) = publish(app(), "brand-new", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a new kind has no live row to protect: {said}"
    );

    // (e) a candidate ADDING presence publishes unsigned.
    draft(reg.as_ref(), &row_toml("growing", "Growing v2", true)).await;
    let (status, said) = publish(app(), "growing", None).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    // ...and from then on it is protected.
    draft(reg.as_ref(), &row_toml("growing", "Growing v3", false)).await;
    let (status, said) = publish(app(), "growing", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    let growing_v3 = reg
        .list_versions("growing")
        .await
        .unwrap()
        .last()
        .unwrap()
        .version;
    reg.discard_draft("growing", growing_v3, &actor(), chrono::Utc::now())
        .await
        .unwrap();

    // (f) a stamped publish of a protected row: the signed bytes over
    // the signed live version are accepted; other bytes are not.
    let path = "infra/platform/workflows/guarded.toml";
    let next = row_toml("guarded", "Guarded v4 signed", false);
    let live_v = reg.get_active("guarded").await.unwrap().version.to_string();
    let packet = signed_packet(&jobs, "guarded", path, &next, &live_v).await;
    draft(
        reg.as_ref(),
        &row_toml("guarded", "Guarded tampered", false),
    )
    .await;
    let body = json!({"approval": packet, "spec_path": path, "spec_toml": next});
    let (status, said) = publish(app(), "guarded", Some(body.clone())).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a draft that is not the signed row: {said}"
    );
    assert!(said.contains("is not the row publish-a-protocol"), "{said}");
    let tampered = reg
        .list_versions("guarded")
        .await
        .unwrap()
        .last()
        .unwrap()
        .version;
    reg.discard_draft("guarded", tampered, &actor(), chrono::Utc::now())
        .await
        .unwrap();
    let wrong = json!({"approval": packet, "spec_path": path, "spec_toml": next.replace("signed", "other")});
    draft(reg.as_ref(), &next).await;
    let (status, said) = publish(app(), "guarded", Some(wrong)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "bytes the passkey did not sign: {said}"
    );
    assert!(
        said.contains("these are not the bytes that were signed"),
        "{said}"
    );
    let (status, said) = publish(app(), "guarded", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(
        active_label(reg.as_ref(), "guarded").await,
        "Guarded v4 signed"
    );
}

#[tokio::test]
async fn the_rule_holds_on_the_in_memory_registry() {
    the_rule_holds(Arc::new(InMemoryWorkflows::for_fixture())).await;
}

#[tokio::test]
async fn the_rule_holds_on_the_postgres_registry() {
    let db = boss_testing::TestDb::new().await;
    let reg = boss_jobs::registry::PgWorkflows::for_fixture(db.pool.clone());
    the_rule_holds(Arc::new(reg)).await;
}

/// (h) RETIRING a protected row does not shed its passkey (review R2 of
/// car 8715776d). "No live row" used to read as "nothing to protect", so
/// retire-then-publish was a two-step strip: retire the presence row on
/// Retire policy, draft it without presence, publish it unsigned as if
/// it were new. A kind's baseline is its live row or, with none live,
/// the row it last published; only a kind that NEVER published is new.
/// Indirect protection keeps the same way, both for a retired parent and
/// for a parent whose protected child was retired.
async fn retiring_a_protected_row_keeps_its_passkey(reg: Arc<dyn WorkflowRegistry>) {
    let jobs = Arc::new(InMemoryJobs::new());
    live(reg.as_ref(), &row_toml("kept", "Kept v1", true)).await;
    live(
        reg.as_ref(),
        &row_toml("kept-parent", "Kept parent v1", false),
    )
    .await;
    live(reg.as_ref(), &row_toml("let-go", "Let go v1", false)).await;
    let rules = vec![spawn_rule("kept-parent", "kept")];
    let app = || app(reg.clone(), jobs.clone(), rules.clone());
    let now = chrono::Utc::now;

    // A retired parent of a protected child: still protected.
    reg.retire("kept-parent", &actor(), now()).await.unwrap();
    draft(
        reg.as_ref(),
        &row_toml("kept-parent", "Kept parent v2", false),
    )
    .await;
    let (status, said) = publish(app(), "kept-parent", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("admits or fulfils kept packets"), "{said}");

    // The protected row itself, retired and redrafted without presence.
    reg.retire("kept", &actor(), now()).await.unwrap();
    draft(reg.as_ref(), &row_toml("kept", "Kept stripped", false)).await;
    let (status, said) = publish(app(), "kept", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.contains("passkey-protected"), "{said}");
    assert!(
        reg.get_active("kept").await.is_err(),
        "nothing went live for kept"
    );

    // Its parent stays protected through the retired child's last row.
    let (status, said) = publish(app(), "kept-parent", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");

    // No widening: a retired PLAIN row republishes on policy alone.
    reg.retire("let-go", &actor(), now()).await.unwrap();
    draft(reg.as_ref(), &row_toml("let-go", "Let go v2", false)).await;
    let (status, said) = publish(app(), "let-go", None).await;
    assert_eq!(status, StatusCode::OK, "{said}");

    // And the signed road reopens a retired protected kind: the signed
    // live version is the row it last published.
    let path = "infra/platform/workflows/kept.toml";
    let stripped = row_toml("kept", "Kept stripped", false);
    let last = reg
        .list_versions("kept")
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.status == boss_jobs::registry::WorkflowStatus::Retired)
        .map(|s| s.version)
        .max()
        .unwrap()
        .to_string();
    let packet = signed_packet(&jobs, "kept", path, &stripped, &last).await;
    let body = json!({"approval": packet, "spec_path": path, "spec_toml": stripped});
    let (status, said) = publish(app(), "kept", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(active_label(reg.as_ref(), "kept").await, "Kept stripped");
}

#[tokio::test]
async fn retiring_a_protected_row_keeps_its_passkey_in_memory() {
    retiring_a_protected_row_keeps_its_passkey(Arc::new(InMemoryWorkflows::for_fixture())).await;
}

#[tokio::test]
async fn retiring_a_protected_row_keeps_its_passkey_on_postgres() {
    let db = boss_testing::TestDb::new().await;
    let reg = boss_jobs::registry::PgWorkflows::for_fixture(db.pool.clone());
    retiring_a_protected_row_keeps_its_passkey(Arc::new(reg)).await;
}

async fn passkey_read(app: Router, kind: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workflows/{kind}/passkey"))
        .header("x-boss-user", serde_json::to_string(&agent()).unwrap())
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

/// (i) THE JUDGEMENT IS READABLE BEFORE A PUBLISH IS TRIED (backlog
/// 5af64799). publish-drift sent publish-to-github — protected by its
/// approve step's presence, but not a held kind — down the unattended
/// road on 2026-11-18: the CLI created draft v18, this server refused
/// the unsigned publish 409, and v18 was stranded. The client road must
/// route a protected kind to its owner's passkey BEFORE it writes, and
/// it must ask THIS rule rather than re-derive it in shell (CLAUDE.md
/// §9a). So `GET /api/workflows/{kind}/passkey` answers what
/// [`boss_jobs::publish_passkey::judge`] would say of an unsigned
/// publish, in its words: protected by the row's own presence step, by
/// an edge into a protected kind, through a retired row, or not at all.
async fn the_server_answers_whether_a_publish_needs_a_passkey(reg: Arc<dyn WorkflowRegistry>) {
    let jobs = Arc::new(InMemoryJobs::new());
    live(reg.as_ref(), &row_toml("guarded", "Guarded v1", true)).await;
    live(reg.as_ref(), &row_toml("parent", "Parent v1", false)).await;
    live(reg.as_ref(), &row_toml("plain", "Plain v1", false)).await;
    live(reg.as_ref(), &row_toml("kept", "Kept v1", true)).await;
    reg.retire("kept", &actor(), chrono::Utc::now())
        .await
        .unwrap();
    // A draft never went live: it is no baseline and protects nothing.
    draft(reg.as_ref(), &row_toml("only-drafted", "Drafted", true)).await;
    let rules = vec![spawn_rule("parent", "guarded")];
    let app = || app(reg.clone(), jobs.clone(), rules.clone());

    let (status, said) = passkey_read(app(), "guarded").await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["kind"], "guarded", "{said}");
    assert_eq!(said["protected"], true, "{said}");
    assert_eq!(said["live_version"], 1, "{said}");
    assert_eq!(
        said["why"], "its step approve declares assurance_required = presence",
        "the judge's own words: {said}"
    );

    let (status, said) = passkey_read(app(), "parent").await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["protected"], true, "{said}");
    assert!(
        said["why"]
            .as_str()
            .is_some_and(|w| w.contains("admits or fulfils guarded packets")
                && w.contains("parent-files-guarded")),
        "{said}"
    );

    let (status, said) = passkey_read(app(), "kept").await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["protected"], true, "retired is not new: {said}");
    assert_eq!(said["retired"], true, "{said}");

    for kind in ["plain", "only-drafted", "never-heard-of"] {
        let (status, said) = passkey_read(app(), kind).await;
        assert_eq!(status, StatusCode::OK, "{kind}: {said}");
        assert_eq!(said["protected"], false, "{kind}: {said}");
        assert_eq!(said["why"], Value::Null, "{kind}: {said}");
    }
    let (_, said) = passkey_read(app(), "never-heard-of").await;
    assert_eq!(said["live_version"], Value::Null, "never published: {said}");

    // The read and the write are one rule: what the read calls
    // protected, an unsigned publish is refused for, and the reverse.
    draft(reg.as_ref(), &row_toml("parent", "Parent v2", false)).await;
    let (status, refused) = publish(app(), "parent", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    draft(reg.as_ref(), &row_toml("plain", "Plain v2", false)).await;
    let (status, said) = publish(app(), "plain", None).await;
    assert_eq!(status, StatusCode::OK, "{said}");
}

#[tokio::test]
async fn the_server_answers_whether_a_publish_needs_a_passkey_in_memory() {
    the_server_answers_whether_a_publish_needs_a_passkey(
        Arc::new(InMemoryWorkflows::for_fixture()),
    )
    .await;
}

#[tokio::test]
async fn the_server_answers_whether_a_publish_needs_a_passkey_on_postgres() {
    let db = boss_testing::TestDb::new().await;
    let reg = boss_jobs::registry::PgWorkflows::for_fixture(db.pool.clone());
    the_server_answers_whether_a_publish_needs_a_passkey(Arc::new(reg)).await;
}

/// The read answers an unread rule graph the way the publish does: a
/// row protected by its own presence step needs no edge and answers;
/// any other row cannot be judged, and says so with a 503 rather than
/// reading as unprotected.
#[tokio::test]
async fn the_passkey_read_cannot_answer_over_an_unread_rule_graph() {
    struct Dark;
    #[async_trait::async_trait]
    impl boss_jobs::department::rules::DispatcherRules for Dark {
        async fn enforced_rules(&self, _: &User) -> Result<Vec<Value>, String> {
            Err("the dispatcher did not answer".into())
        }
    }
    let reg: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::for_fixture());
    live(reg.as_ref(), &row_toml("plain", "Plain v1", false)).await;
    live(reg.as_ref(), &row_toml("guarded", "Guarded v1", true)).await;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Read,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let app = || {
        let bus = RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        router(JobsApiState {
            kind_registry: Some(reg.clone()),
            publish_rules: Some(Arc::new(Dark)),
            ..JobsApiState::minimal(
                Arc::new(InMemoryJobs::new()),
                bus,
                DomainPublisher::new(bus_dyn, "jobs"),
                policy.clone(),
                Arc::new(boss_clock_client::WallClockClient),
            )
        })
    };
    let (status, said) = passkey_read(app(), "plain").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
    assert!(
        said.as_str()
            .is_some_and(|s| s.contains("an unread edge is not an absent one")),
        "{said}"
    );
    let (status, said) = passkey_read(app(), "guarded").await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["protected"], true, "{said}");
}

/// (g) publish-a-protocol — the row that decides what a passkey can
/// publish — is itself protected, so the rule protects itself: the
/// shipped row declares presence, and an unsigned publish over it is
/// refused.
#[tokio::test]
async fn publish_a_protocol_is_itself_protected() {
    let text = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/platform/workflows/publish-a-protocol.toml"),
    )
    .unwrap();
    let shipped = spec(&text);
    assert_eq!(
        boss_jobs::publish_passkey::presence_step(&shipped),
        Some("approve")
    );
    let reg: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::for_fixture());
    live(reg.as_ref(), &text).await;
    reg.create_draft(spec(&text), &actor(), chrono::Utc::now())
        .await
        .unwrap();
    let (status, said) = publish(
        app(reg, Arc::new(InMemoryJobs::new()), vec![]),
        "publish-a-protocol",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
}

/// The edges are read off the LIVE rule registry; a registry the
/// publisher cannot read refuses a row that might be protected only
/// through one (an unread edge is not an absent one) — while a row with
/// its own presence step is refused by that step, read or not.
#[tokio::test]
async fn an_unread_rule_registry_refuses_rather_than_admits() {
    struct Dark;
    #[async_trait::async_trait]
    impl boss_jobs::department::rules::DispatcherRules for Dark {
        async fn enforced_rules(&self, _: &User) -> Result<Vec<Value>, String> {
            Err("the dispatcher did not answer".into())
        }
    }
    let reg: Arc<dyn WorkflowRegistry> = Arc::new(InMemoryWorkflows::for_fixture());
    live(reg.as_ref(), &row_toml("plain", "Plain v1", false)).await;
    draft(reg.as_ref(), &row_toml("plain", "Plain v2", false)).await;
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let app = router(JobsApiState {
        kind_registry: Some(reg.clone()),
        publish_rules: Some(Arc::new(Dark)),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let (status, said) = publish(app, "plain", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
    assert!(
        said.contains("an unread edge is not an absent one"),
        "{said}"
    );
    assert_eq!(active_label(reg.as_ref(), "plain").await, "Plain v1");
}

/// ONE definition of "what a signature is", in two languages: the
/// server's Rust port and `infra/lib/signed-publish.jq` (with
/// `infra/lib/step-shape.sh`), run over the same packets, answer the
/// same verdict in the same words (CLAUDE.md §9a).
#[test]
fn a_signature_is_judged_alike_in_jq_and_rust() {
    let root = boss_testing::repo_root();
    let path = "infra/platform/workflows/guarded.toml";
    let bytes_sha = sha("row bytes");
    let metadata = json!({
        "decision": "approved", "registry": "workflow", "row_name": "guarded",
        "spec_path": path, "spec_sha256": bytes_sha, "live_version": "3",
    });
    let shape = step_shape_hash("Approve", &metadata);
    let stamp = json!({
        "authority_id": "emp-david", "role": "platform-admin",
        "stamped_at": "2026-11-08T22:00:00Z", "shape_hash": shape,
        "assurance": "presence", "presence_nonce": "n1",
    });
    let good = json!({
        "id": "0123456789abcdef", "kind": "publish-a-protocol", "status": "open",
        "metadata": {"registry": "workflow", "row_name": "guarded"},
        "steps": [
            {"spec_slug": "approve", "title": "Approve", "status": "completed",
             "assurance_required": "presence", "sign_offs_required": ["platform-admin"],
             "metadata": metadata, "sign_offs": [stamp]},
            {"spec_slug": "publish", "title": "Publish", "status": "ready", "metadata": {}},
        ],
    });
    let with = |pointer: &str, v: Value| {
        let mut p = good.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        p.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert(key.to_string(), v);
        p
    };
    let cases = [
        good.clone(),
        with("/kind", json!("ops-request")),
        with("/status", json!("closed")),
        with("/metadata/row_name", json!("other")),
        with("/steps/1/status", json!("completed")),
        with("/steps/0/status", json!("ready")),
        with("/steps/0/metadata/decision", json!("rejected")),
        with("/steps/0/assurance_required", json!("session")),
        with("/steps/0/sign_offs/0/authority_id", json!("agent-claude")),
        with("/steps/0/sign_offs/0/assurance", json!("session")),
        with("/steps/0/metadata/spec_sha256", json!("0000")),
        with(
            "/steps/0/sign_offs/0/voided_at",
            json!("2026-11-08T23:00:00Z"),
        ),
    ];
    let approvers = boss_jobs::publish_passkey::approvers();
    assert_eq!(approvers, vec!["emp-david".to_string()]);
    let dir = boss_testing::scratch_dir("signature-alike");
    for (i, packet) in cases.iter().enumerate() {
        let file = dir.join(format!("packet-{i}.json"));
        std::fs::write(&file, serde_json::to_vec(packet).unwrap()).unwrap();
        let script = format!(
            ". '{lib}/step-shape.sh'; shape=$(shape_hash_of \"$(jq -c '.steps[0]' '{f}')\"); \
             jq -c -f '{lib}/signed-publish.jq' --arg shape \"$shape\" --argjson approvers '{a}' \
             --arg verb_file infra/ops/verbs/publish-workflow.json --arg registry workflow \
             --arg name guarded --arg path '{path}' --arg sha '{bytes_sha}' --arg step publish '{f}'",
            lib = root.join("infra/lib").display(),
            f = file.display(),
            a = serde_json::to_string(&approvers).unwrap(),
        );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "jq failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let jq: Value = serde_json::from_slice(&out.stdout).unwrap();
        let rust_shape = step_shape_hash(
            packet["steps"][0]["title"].as_str().unwrap(),
            &packet["steps"][0]["metadata"],
        );
        let rust = boss_jobs::publish_passkey::judge_signature(
            packet,
            &rust_shape,
            &approvers,
            boss_jobs::publish_passkey::VERB_FILE,
            "workflow",
            "guarded",
            path,
            &bytes_sha,
            "publish",
        );
        let rust = match rust {
            Ok(v) => json!({"by": v.by, "at": v.at, "live": v.live}),
            Err(why) => json!({"refuse": why}),
        };
        assert_eq!(rust, jq, "case {i} judged differently");
    }
}
