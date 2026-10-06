//! `POST /api/jobs/{id}/convert` moves a packet to another version of
//! its protocol, and the move is true of the packet and on the record.
//!
//! MEASURED (backlog 1e973965, the draft-design analyst on 4347a1af,
//! 2026-09-23). The door answered `converted: true` after changing only
//! `jobs.workflow_version`. Each step row keeps what materialisation
//! copied onto it from the admission version — on page-audit c0d2caf0
//! (pinned v1, active v3) `measure` and `file` still held v1 procedure
//! text after v3 changed them — and a step the target inserts was never
//! created. The interim car refused both moves.
//!
//! Design 7cf202a9 decided the door (all five questions accepted as
//! proposed, 2026-09-23), and these pin it: a re-pin re-projects every
//! step not yet completed and materialises every inserted step (Q2);
//! it records a `jobs.job.repinned` event and appends to the packet's
//! reserved `repins` list (Q3); only the authority that publishes a
//! protocol version may move a packet to one (Q4); and a dry run
//! answers the same verdict and plan without writing (Q1).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{WorkflowSpec, WorkflowStatus, seedable_platform_workflows};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "ship-a-change";

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

/// The platform's active ship-a-change, as seeded.
fn active_spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == KIND)
        .expect("ship-a-change is a platform workflow")
}

/// A later version of it, edited by `edit` and PUBLISHED: the row the
/// packet opened under is retired and the target is the active one, as
/// a publish leaves them — a packet is moved only onto a version that
/// ran (backlog ce8b7d66; [`a_draft_is_not_a_version_a_packet_is_moved_to`]).
/// Seeded rather than published so the edits need not pass the publish
/// gate; the conversion names its target with `to_version`.
async fn next_version(
    kinds: &InMemoryWorkflows,
    bump: i32,
    edit: impl FnOnce(&mut WorkflowSpec),
) -> i32 {
    let spec = later_version(bump, WorkflowStatus::Active, edit);
    kinds
        .retire(KIND, &admin_actor(), chrono::Utc::now())
        .await
        .expect("retire the admission version");
    let version = spec.version;
    kinds.seed(spec).expect("seed target version");
    version
}

/// The spec of a later version, not yet written anywhere.
fn later_version(
    bump: i32,
    status: WorkflowStatus,
    edit: impl FnOnce(&mut WorkflowSpec),
) -> WorkflowSpec {
    let mut spec = active_spec();
    spec.version += bump;
    spec.status = status;
    edit(&mut spec);
    spec
}

fn admin_actor() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Human("emp-bootstrap-admin".into())
}

fn set_procedure(spec: &mut WorkflowSpec, slug: &str, text: &str) {
    let step = spec
        .steps
        .iter_mut()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("step {slug} in {KIND}"));
    let mut defaults = match &step.metadata_defaults {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    defaults.insert("procedure".into(), serde_json::Value::String(text.into()));
    step.metadata_defaults = serde_json::Value::Object(defaults);
}

struct App {
    router: axum::Router,
    kinds: Arc<InMemoryWorkflows>,
    jobs: Arc<InMemoryJobs>,
}

fn app() -> App {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    // The engineer writes jobs exactly as the admin does — every
    // permission the door asked for before Q4 — and does not publish
    // protocols.
    for role in ["platform-admin", "engineer"] {
        for (action, resource) in [
            (Action::Create, Resource::job()),
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow(role, action, resource, Scope::All);
        }
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        policy
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    App {
        router: router(state),
        kinds,
        jobs,
    }
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
const ENGINEER: &str = r#"{"id":"emp-engineer","role":"engineer","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

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

fn req_as(user: &str, method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user)
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    req_as(ADMIN, method, uri, body)
}

async fn get_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full
}

fn step_named<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("a {slug} step on the packet: {job}"))
}

/// A packet opened under the active version, standing at `build`.
async fn open_at_build(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": KIND,
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    complete(
        app,
        &id,
        "scope",
        serde_json::json!({"summary": "s", "excludes": "e"}),
    )
    .await;
    id
}

async fn convert_as(
    app: &axum::Router,
    user: &str,
    id: &str,
    to: i32,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        req_as(
            user,
            "POST",
            &format!("/api/jobs/{id}/convert"),
            serde_json::json!({ "to_version": to }),
        ),
    )
    .await
}

async fn convert(app: &axum::Router, id: &str, to: i32) -> (StatusCode, serde_json::Value) {
    convert_as(app, ADMIN, id, to).await
}

fn repinned_events(jobs: &InMemoryJobs) -> Vec<serde_json::Value> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::events::JOB_REPINNED)
        .map(|e| e.payload)
        .collect()
}

/// THE MEASURED CASE: the target changes the procedure of a step the
/// packet has not reached. Before the interim car: 200, `converted:
/// true`, and the `build` row still carrying the admission text. Now
/// the row reads the target's text, and the step the packet already
/// completed keeps the text it ran under.
#[tokio::test]
async fn a_pending_procedure_is_reprojected_and_a_completed_one_is_kept() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let before = get_job(&app, &id).await;
    let from = before["workflow_version"].as_i64().expect("pinned");
    let to = next_version(&kinds, 1, |s| {
        set_procedure(s, "build", "Build it, and say what you measured.");
        set_procedure(s, "scope", "Scope it, in writing.");
    })
    .await;

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["converted"], true, "{body}");

    let after = get_job(&app, &id).await;
    assert_eq!(after["workflow_version"], to);
    assert_eq!(
        step_named(&after, "build")["metadata"]["procedure"],
        "Build it, and say what you measured.",
        "the pending step reads the version the packet was moved to"
    );
    assert_eq!(
        step_named(&after, "scope")["metadata"]["procedure"],
        step_named(&before, "scope")["metadata"]["procedure"],
        "the completed step keeps the text it ran under"
    );

    // On the record, twice: the packet's own list, and the event.
    let repins = after["metadata"]["repins"].as_array().expect("repins list");
    assert_eq!(repins.len(), 1, "{repins:?}");
    assert_eq!(repins[0]["from"], from);
    assert_eq!(repins[0]["to"], to);
    assert_eq!(repins[0]["by"], "emp-bootstrap-admin");
    let build = repins[0]["reprojected"]
        .as_array()
        .expect("reprojected")
        .iter()
        .find(|r| r["step"] == "build")
        .expect("build named as re-projected");
    assert!(
        build["changed"]
            .as_array()
            .expect("changed")
            .iter()
            .any(|c| c == "`procedure`"),
        "{build}"
    );
    let events = repinned_events(&jobs);
    assert_eq!(events.len(), 1, "one move, one event: {events:?}");
    assert_eq!(events[0]["job_id"], id.as_str());
    assert_eq!(events[0]["to"], to);
}

/// A step the target inserts ahead of the packet gets a row, pending,
/// and the record names it.
#[tokio::test]
async fn an_inserted_step_is_materialised_and_named() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let to = next_version(&kinds, 2, |s| {
        let mut extra = s
            .steps
            .iter()
            .find(|st| st.title == "settled")
            .expect("settled step")
            .clone();
        extra.title = "archived".to_string();
        s.steps.push(extra);
    })
    .await;

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = get_job(&app, &id).await;
    let archived = step_named(&after, "archived");
    assert_ne!(archived["status"], "completed", "{archived}");
    assert_eq!(
        after["steps"].as_array().map(Vec::len),
        Some(active_spec().steps.len() + 1)
    );
    assert_eq!(repinned_events(&jobs)[0]["inserted"][0]["step"], "archived");
}

/// THE SAFETY VERDICT STILL REFUSES. A required field added to a step
/// the packet has already completed would claim evidence it never
/// collected; re-projection cannot make that true, so the move is
/// refused, the obstacle names the step, and nothing is written.
#[tokio::test]
async fn a_move_that_demands_evidence_retroactively_is_refused_and_writes_nothing() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 3, |s| {
        let scope = s
            .steps
            .iter_mut()
            .find(|st| st.title == "scope")
            .expect("scope");
        scope.fields.push(boss_core::job::StepField {
            required: true,
            ..boss_core::job::StepField::new("blast_radius", "string")
        });
    })
    .await;

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["converted"], false);
    assert!(
        body["obstacles"]
            .as_array()
            .expect("obstacles")
            .iter()
            .any(|o| o["step"] == "scope"),
        "{body}"
    );
    let after = get_job(&app, &id).await;
    assert_eq!(after["workflow_version"], pinned, "the pin stays");
    assert!(after["metadata"].get("repins").is_none(), "{after}");
    assert!(repinned_events(&jobs).is_empty());
}

/// Q4: moving a packet is the registry owner's act. A caller who may
/// write the job but may not publish a protocol version is refused,
/// and the pin stays where it was.
#[tokio::test]
async fn a_job_writer_who_cannot_publish_a_protocol_may_not_move_a_packet() {
    let App {
        router: app, kinds, ..
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 1, |s| set_procedure(s, "build", "New text.")).await;

    let (status, body) = convert_as(&app, ENGINEER, &id, to).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(get_job(&app, &id).await["workflow_version"], pinned);
}

/// Backlog ce8b7d66: a packet is moved only onto a version that RAN.
/// A draft is its author's workspace — written on Create/Update, never
/// through the publish gate — so moving a live packet onto one would
/// make it that packet's protocol with nobody having made it live. The
/// dry run and the move answer the same obstacle, and nothing is
/// written. Publish the draft first, then move the packet onto it.
#[tokio::test]
async fn a_draft_is_not_a_version_a_packet_is_moved_to() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let draft = later_version(1, WorkflowStatus::Draft, |s| {
        set_procedure(s, "build", "Unpublished text.")
    });
    let to = draft.version;
    kinds.seed(draft).expect("seed the draft");

    let (status, body) = send(
        &app,
        req(
            "GET",
            &format!("/api/jobs/{id}/convert?to_version={to}"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["convertible"], false, "{body}");

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["converted"], false);
    assert!(
        body["obstacles"]
            .as_array()
            .expect("obstacles")
            .iter()
            .any(|o| o["reason"].as_str().is_some_and(|r| r.contains("draft"))),
        "the obstacle says the target is a draft: {body}"
    );
    assert_eq!(get_job(&app, &id).await["workflow_version"], pinned);
    assert!(repinned_events(&jobs).is_empty());
}

/// Q1: the dry run is a READ — the verdict and the plan the write would
/// carry, with nothing written. A cohort is previewed by running it in
/// a loop, which is why a job reader may ask it.
#[tokio::test]
async fn a_dry_run_answers_the_plan_and_writes_nothing() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 1, |s| set_procedure(s, "build", "New text.")).await;
    let events_before = jobs.recorded_events().len();

    let (status, body) = send(
        &app,
        req_as(
            ENGINEER,
            "GET",
            &format!("/api/jobs/{id}/convert?to_version={to}"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["convertible"], true, "{body}");
    assert_eq!(body["from"], pinned);
    assert_eq!(body["to"], to);
    assert!(
        body["reprojected"]
            .as_array()
            .expect("reprojected")
            .iter()
            .any(|r| r["step"] == "build"),
        "{body}"
    );
    assert_eq!(get_job(&app, &id).await["workflow_version"], pinned);
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "a dry run records nothing"
    );
}

/// Q3: the record is append-only. The generic metadata PATCH could
/// rewrite or erase it, so it refuses the key and names the door.
#[tokio::test]
async fn the_metadata_patch_refuses_the_repins_list() {
    let App { router: app, .. } = app();
    let id = open_at_build(&app).await;
    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &format!("/api/jobs/{id}/metadata"),
            serde_json::json!({ "repins": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("/convert"), "{body}");
}

// ---------------------------------------------------------------------
// Backlog 4c6b4b74: a backlog-item stuck at `measure` could be neither
// dispatched (its v2 `measure` declares no agent block) nor converted.
// ---------------------------------------------------------------------

const BACKLOG: &str = "backlog-item";

/// The platform's backlog-item as the target, numbered as the live
/// active version was when this was measured (v14, 2026-09-28).
fn backlog_v14() -> WorkflowSpec {
    let mut spec = seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == BACKLOG)
        .expect("backlog-item is a platform workflow");
    spec.version = 14;
    spec.status = WorkflowStatus::Active;
    spec
}

/// backlog-item v2 as the live registry served it on 2026-09-28
/// (`GET /api/workflows/backlog-item/versions/2`): no `prove-delivery`,
/// and every branch read `triage` alone — so a `verify` route proved
/// them all unsatisfiable the moment triage completed, and the engine
/// SKIPPED them while `measure` was still to run. Only the predicates
/// and the step set are v2's; the rest is the target's, so the move
/// differs in exactly what 70da1212's refusal named.
fn backlog_v2() -> WorkflowSpec {
    const V2: [(&str, &str); 7] = [
        (
            "draft-design",
            r#"steps.triage.done AND steps.triage.metadata.disposition = "design""#,
        ),
        ("design-review", "steps.draft-design.done"),
        (
            "build",
            r#"(steps.triage.done AND steps.triage.metadata.disposition = "build") OR (steps.design-review.done AND steps.design-review.metadata.verdict = "approved")"#,
        ),
        (
            "duplicate",
            r#"steps.triage.done AND steps.triage.metadata.disposition = "duplicate""#,
        ),
        (
            "stale",
            r#"steps.triage.done AND steps.triage.metadata.disposition = "stale""#,
        ),
        (
            "declined",
            r#"(steps.triage.done AND steps.triage.metadata.disposition = "decline") OR (steps.design-review.done AND steps.design-review.metadata.verdict = "declined")"#,
        ),
        (
            "closed",
            r#"steps.measure.done OR steps.build.done OR (steps.design-review.done AND NOT (steps.design-review.metadata.verdict = "approved" OR steps.design-review.metadata.verdict = "declined"))"#,
        ),
    ];
    let mut spec = backlog_v14();
    spec.version = 2;
    spec.steps.retain(|s| s.title != "prove-delivery");
    for (slug, ready_when) in V2 {
        spec.steps
            .iter_mut()
            .find(|s| s.title == slug)
            .unwrap_or_else(|| panic!("backlog-item has a `{slug}` step"))
            .ready_when = ready_when.to_string();
    }
    spec
}

/// Complete `slug`: `fields` through the step merge door (every stored
/// key it leaves out is kept), then the status alone — the step PUT
/// writes no metadata since backlog e39a9d2a.
async fn complete(app: &axum::Router, id: &str, slug: &str, fields: serde_json::Value) {
    let full = get_job(app, id).await;
    let step = step_named(&full, slug).clone();
    let step_id = step["id"].as_str().expect("step id");
    if !fields.as_object().expect("fields").is_empty() {
        let (status, body) = send(
            app,
            req(
                "PATCH",
                &format!("/api/jobs/{id}/steps/{step_id}/metadata"),
                fields,
            ),
        )
        .await;
        assert!(status.is_success(), "write {slug}'s keys: {status}: {body}");
    }
    let (status, body) = send(
        app,
        req(
            "PUT",
            &format!("/api/jobs/{id}/steps/{step_id}"),
            serde_json::json!({"status": "completed"}),
        ),
    )
    .await;
    assert!(status.is_success(), "complete {slug}: {status}: {body}");
}

/// A backlog-item admitted under v2 and routed by triage to `route`.
/// `v14` is published afterwards, as it was in the registry.
async fn backlog_item_routed(app: &App, route: &str) -> String {
    app.kinds
        .retire(BACKLOG, &admin_actor(), chrono::Utc::now())
        .await
        .expect("retire the bundle's version");
    app.kinds.seed(backlog_v2()).expect("seed v2");
    let (status, job) = send(
        &app.router,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": BACKLOG,
                "subject": {"subject_kind": "custom", "id": "jobs"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    if step_named(&get_job(&app.router, &id).await, "filed")["status"] != "completed" {
        complete(&app.router, &id, "filed", serde_json::json!({})).await;
    }
    complete(
        &app.router,
        &id,
        "triage",
        serde_json::json!({"disposition": route, "evidence": "measured"}),
    )
    .await;
    app.kinds
        .retire(BACKLOG, &admin_actor(), chrono::Utc::now())
        .await
        .expect("retire v2");
    app.kinds.seed(backlog_v14()).expect("seed v14");
    id
}

fn status_of(job: &serde_json::Value, slug: &str) -> String {
    step_named(job, slug)["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// THE MEASURED CASE, shaped like 70da1212 (v2, opened 2026-09-18):
/// triage routed `verify`, `measure` is active, and the six branches v2
/// proved unsatisfiable are SKIPPED. The dry run refused on seven
/// changed predicates, none of them on a step that had ever been
/// ready. Now the move is taken, the skipped branches are re-derived
/// under v14 — where `measure` can still route to them, so they are
/// pending again — and the packet walks on: `measure` routing `build`
/// opens the build. Recorded exactly as every other move.
#[tokio::test]
async fn a_backlog_item_at_measure_on_v2_converts_and_its_branches_are_re_derived() {
    let app = app();
    let id = backlog_item_routed(&app, "verify").await;
    let step_id = step_named(&get_job(&app.router, &id).await, "measure")["id"]
        .as_str()
        .expect("measure id")
        .to_string();
    let (status, body) = send(
        &app.router,
        req(
            "POST",
            &format!("/api/jobs/{id}/steps/{step_id}/claim"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert!(status.is_success(), "claim measure: {status}: {body}");

    const BRANCHES: [&str; 6] = [
        "draft-design",
        "design-review",
        "build",
        "duplicate",
        "stale",
        "declined",
    ];
    let before = get_job(&app.router, &id).await;
    assert_eq!(status_of(&before, "measure"), "active", "{before}");
    assert_eq!(status_of(&before, "closed"), "pending", "{before}");
    for slug in BRANCHES {
        assert_eq!(
            status_of(&before, slug),
            "skipped",
            "the 70da1212 shape: {slug}"
        );
    }

    let (status, preview) = send(
        &app.router,
        req(
            "GET",
            &format!("/api/jobs/{id}/convert?to_version=14"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["convertible"], true, "{preview}");

    let (status, body) = convert(&app.router, &id, 14).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["converted"], true, "{body}");

    let after = get_job(&app.router, &id).await;
    assert_eq!(after["workflow_version"], 14);
    assert_eq!(status_of(&after, "measure"), "active", "untouched");
    for slug in BRANCHES.iter().chain(&["closed", "prove-delivery"]) {
        assert_eq!(
            status_of(&after, slug),
            "pending",
            "{slug}: v14 can still reach it from `measure`: {after}"
        );
    }

    // On the record exactly as every move is: one entry, one event,
    // and the un-skipping named as the change it is.
    let repins = after["metadata"]["repins"].as_array().expect("repins list");
    assert_eq!(repins.len(), 1, "{repins:?}");
    assert_eq!(repins[0]["from"], 2);
    assert_eq!(repins[0]["to"], 14);
    let build = repins[0]["reprojected"]
        .as_array()
        .expect("reprojected")
        .iter()
        .find(|r| r["step"] == "build")
        .expect("build named as re-projected");
    assert!(
        build["changed"]
            .as_array()
            .expect("changed")
            .iter()
            .any(|c| c == "status"),
        "{build}"
    );
    assert_eq!(repins[0]["inserted"][0]["step"], "prove-delivery");
    assert_eq!(
        repins[0]["unskipped"],
        serde_json::json!(BRANCHES),
        "every step the move un-skipped is named on its own"
    );
    let events = repinned_events(&app.jobs);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["job_id"], id.as_str());
    assert_eq!(events[0]["unskipped"], serde_json::json!(BRANCHES));

    // Not stranded: the route v14 added is the one that opens the build.
    complete(
        &app.router,
        &id,
        "measure",
        serde_json::json!({"disposition": "build", "evidence": "measured again"}),
    )
    .await;
    let walked = get_job(&app.router, &id).await;
    assert_eq!(status_of(&walked, "build"), "ready", "{walked}");
    assert_eq!(status_of(&walked, "draft-design"), "skipped", "{walked}");
}

/// REVIEW OF 28f3f28a: the move is judged, then written. A packet that
/// closes in between must not receive the pending rows the plan made
/// for it while it was open — the write re-checks, refuses whole, and
/// writes nothing (the in-memory half; the Pg half is pinned in
/// repin_writes_the_move_whole_pg.rs).
#[tokio::test]
async fn a_packet_that_closed_after_the_move_was_judged_is_not_written() {
    use boss_jobs::JobsRepository;
    let app = app();
    let id = backlog_item_routed(&app, "verify").await;
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(&id).expect("job id"));
    let job = app.jobs.get_job(&job_id).await.unwrap().expect("packet");
    let rows = app.jobs.list_steps(&job_id).await.unwrap();
    let plan = boss_jobs::repin::plan(&backlog_v2(), &backlog_v14(), &job, &rows).expect("planned");
    assert!(!plan.unskipped().is_empty(), "the plan un-skips: {plan:?}");

    let closed = boss_core::job::Job {
        status: boss_core::job::JobStatus::Closed,
        ..job.clone()
    };
    app.jobs
        .update_job(&closed)
        .await
        .expect("close the packet");

    let stamp = boss_core::publisher::EventStamp::new("jobs", admin_actor());
    let record = boss_jobs::repin::record(&plan, 2, 14, "emp-bootstrap-admin", stamp.timestamp);
    let refused = app
        .jobs
        .repin_workflow_version_at(&job_id, 14, &plan, &record, &stamp)
        .await;
    assert!(
        matches!(refused, Err(boss_jobs::JobsError::TerminalJob { .. })),
        "{refused:?}"
    );
    let after = get_job(&app.router, &id).await;
    assert_eq!(after["workflow_version"], 2, "the pin stays");
    assert!(after["metadata"].get("repins").is_none(), "{after}");
    assert_eq!(status_of(&after, "build"), "skipped", "nothing un-skipped");
    assert!(repinned_events(&app.jobs).is_empty());
}

/// THE CONTROL: triage routed `build`, so `build` is READY under v2's
/// predicate. The predicate that opened it is the old one, and nothing
/// proves v14's agrees — the refusal the packet asked to keep. Nothing
/// is written.
#[tokio::test]
async fn a_backlog_item_whose_build_is_ready_is_still_refused() {
    let app = app();
    let id = backlog_item_routed(&app, "build").await;
    assert_eq!(
        status_of(&get_job(&app.router, &id).await, "build"),
        "ready"
    );

    let (status, body) = convert(&app.router, &id, 14).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let obstacles = body["obstacles"].as_array().expect("obstacles");
    assert!(
        obstacles.iter().any(|o| o["step"] == "build"
            && o["reason"]
                .as_str()
                .is_some_and(|r| r.contains("ready_when"))),
        "{body}"
    );
    assert!(
        obstacles.iter().all(|o| o["step"] == "build"),
        "the skipped branches do not bite: {body}"
    );
    let after = get_job(&app.router, &id).await;
    assert_eq!(after["workflow_version"], 2, "the pin stays");
    assert!(after["metadata"].get("repins").is_none(), "{after}");
    assert!(repinned_events(&app.jobs).is_empty());
}
