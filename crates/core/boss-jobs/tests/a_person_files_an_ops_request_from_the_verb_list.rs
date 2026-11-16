//! A person files an ops request from the verb list (backlog 7aef2405,
//! David 2026-11-07 ~15:05Z: "In general, I just want to be using the
//! website UI for my controls").
//!
//! Measured that day: the plan-page half of this item had landed (car
//! 72e7a790, `POST /api/jobs/{plan}/file-the-request`), yet the gate
//! volume move was still filed from the pod shell line by line —
//! `retire-a-user-volume w-1 gate`, `wipe-a-partition w-1 nvme1n1p1`,
//! each with no `filed_from_plan`, each signed by the shell's actor. A
//! plan page only files the write of a plan someone already filed, and
//! filing the plan, or any verb at all, still took a terminal. This is
//! the other half the reshape named: file a verb "from the verb list".
//!
//! What is held here, through the router the jobs API serves:
//!
//!   1. `GET /api/jobs/ops-verbs` is the allowlist the runner enforces,
//!      every verb file, with the params a filer gives — an approval
//!      verb's `plan_sha256` is not one of them.
//!   2. `POST /api/jobs/file-an-ops-request` files EXACTLY what `boss
//!      ops` files for the same call: the same envelope, from one
//!      function both doors call, through the same create door, as the
//!      caller that signed it — and filing is not approving.
//!   3. An unlisted verb, a bad param, a host the verb does not serve,
//!      a supplied hash and any key past `{host, verb, args}` are each
//!      refused before a packet exists, in no terminal's words.
//!   4. It is a PERSON's door: an agent or automation is refused even
//!      with the role that may open a packet, so no agent gains a filing
//!      path here; the policy still decides for a person.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::ops_verbs;
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PERSON: &str = "emp-david";
/// A person on the roster whose role may read every packet and open none.
const READER: &str = "emp-reader";
const AGENT: &str = "agent-claude";

const PVC: &str = "pvc-0a1b2c3d-0000-4000-8000-00000000abcd";
const REPLICA: &str = "pvc-0a1b2c3d-0000-4000-8000-00000000abcd-r-1234abcd";

struct Roster;

#[async_trait::async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![PERSON.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == PERSON || id == READER)
    }
}

fn app() -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow("auditor", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

fn signed_as(id: &str, role: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    signer: Option<(&str, &str)>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some((id, role)) = signer {
        req = req.header("x-boss-user", signed_as(id, role));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .expect("request builds");
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

const DAVID: Option<(&str, &str)> = Some((PERSON, "platform-admin"));
const FILE: &str = "/api/jobs/file-an-ops-request";

async fn ops_requests(app: &axum::Router) -> Vec<Value> {
    let (status, page) = send(
        app,
        "GET",
        "/api/jobs?kind=ops-request&full=true&limit=50",
        DAVID,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["data"].as_array().cloned().unwrap_or_default()
}

async fn get_job(app: &axum::Router, id: &str) -> Value {
    let (status, job) = send(app, "GET", &format!("/api/jobs/{id}"), DAVID, None).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    job
}

fn step<'a>(job: &'a Value, slug: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
        .unwrap_or_else(|| panic!("no {slug} step on {job}"))
}

/// What a packet says, without what differs between any two packets
/// filed a moment apart: its id and the times it carries.
fn what_it_says(job: &Value) -> Value {
    let mut md = job["metadata"].clone();
    if let Some(m) = md.as_object_mut() {
        m.remove("opened_at");
    }
    let steps: Vec<Value> = job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| json!([s["spec_slug"], s["kind"], s["status"], s["assignee_id"]]))
        .collect();
    json!({
        "kind": job["kind"], "title": job["title"], "subject": job["subject"],
        "owner_id": job["owner_id"], "status": job["status"],
        "priority": job["priority"], "tags": job["tags"],
        "metadata": md, "steps": steps,
    })
}

/// THE VERB LIST IS THE ALLOWLIST. Every verb file the runner enforces
/// is listed, by name, and each lists the params a FILER gives: an
/// approval verb's trailing `plan_sha256` is the runner's to append from
/// the signed plan (design 17835005), so the form never asks for it.
/// The list is operator reference data: an unnamed caller is refused
/// 401 and a caller whose role may not read packets 403 (gate af039e44).
#[tokio::test]
async fn the_verb_list_is_the_allowlist_as_a_filer_fills_it_in() {
    let app = app();
    let (status, said) = send(&app, "GET", "/api/jobs/ops-verbs", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{said}");
    let (status, said) = send(
        &app,
        "GET",
        "/api/jobs/ops-verbs",
        Some(("emp-nobody", "no-such-role")),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");
    let (status, list) = send(&app, "GET", "/api/jobs/ops-verbs", DAVID, None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let verbs = list["verbs"].as_array().expect("a verbs array");
    let listed: Vec<&str> = verbs.iter().filter_map(|v| v["name"].as_str()).collect();
    let mut files: Vec<&str> = ops_verbs::verb_files().iter().map(|(n, _)| *n).collect();
    files.sort_unstable();
    assert_eq!(listed, files, "the list is every verb file, sorted by name");

    let by = |name: &str| {
        verbs
            .iter()
            .find(|v| v["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed"))
            .clone()
    };
    let write = by("rebuild-a-failed-replica");
    assert_eq!(write["requires_approval"], true, "{write}");
    assert_eq!(write["mutating"], true, "{write}");
    assert_eq!(write["approvers"], json!([PERSON]), "{write}");
    assert_eq!(write["plan_verb"], "plan-a-failed-replica-rebuild");
    let params: Vec<&str> = write["params"]
        .as_array()
        .expect("params")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(!params.contains(&"plan_sha256"), "{write}");
    assert!(!params.is_empty(), "{write}");
    for p in write["params"].as_array().expect("params") {
        assert!(p["pattern"].is_string() || p["one_of"].is_array(), "{p}");
    }
    let plan = by("plan-a-failed-replica-rebuild");
    assert_eq!(plan["plan_for"], "rebuild-a-failed-replica", "{plan}");
    assert_eq!(plan["requires_approval"], false, "{plan}");
    assert!(plan["hosts"].as_array().is_some_and(|h| !h.is_empty()));
}

/// THE FORM FILES EXACTLY WHAT `boss ops` FILES. The same call — host,
/// verb, args — through this door and through the create door with the
/// envelope `boss ops` sends (`ops_verbs::request_envelope`, the one
/// function both call) makes the same packet: kind, title, subject,
/// owner, metadata, steps. The filer is the person who signed, and the
/// approve step is ready and unsigned: filing is not approving.
#[tokio::test]
async fn the_form_files_exactly_what_boss_ops_files() {
    let app = app();
    let call = json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA]});
    let (status, created) = send(&app, "POST", FILE, DAVID, Some(call)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let from_form = get_job(&app, created["id"].as_str().expect("created id")).await;

    let allowlist = ops_verbs::shipped_allowlist().expect("allowlist");
    let v = ops_verbs::validate(
        &allowlist,
        "forge",
        "rebuild-a-failed-replica",
        &[PVC.to_string(), REPLICA.to_string()],
    )
    .expect("boss ops admits the call");
    let (status, created) = send(
        &app,
        "POST",
        "/api/jobs",
        DAVID,
        Some(ops_verbs::request_envelope(&v, PERSON)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let from_terminal = get_job(&app, created["id"].as_str().expect("created id")).await;

    assert_eq!(what_it_says(&from_form), what_it_says(&from_terminal));
    let md = &from_form["metadata"];
    assert_eq!(md["verb"], "rebuild-a-failed-replica");
    assert_eq!(
        md["args"],
        json!([PVC, REPLICA]),
        "the args stop before plan_sha256"
    );
    assert_eq!(md["requires_approval"], true);
    assert_eq!(
        md["opened_by"], PERSON,
        "the filer is the person who signed"
    );
    assert!(md.get("filed_from_plan").is_none(), "{md}");
    let approve = step(&from_form, "approve");
    assert_eq!(
        approve["status"], "ready",
        "filing is not approving: {approve}"
    );
    assert_eq!(approve["sign_offs"], json!([]), "{approve}");
    assert_ne!(step(&from_form, "execute")["status"], "ready");
}

/// AN UNLISTED VERB IS REFUSED, AND SO IS A BAD PARAM — before a packet
/// exists, with the allowlist's reason, in no terminal's words (Codex
/// review of car 72e7a790: a website viewer is not told a CLI command).
/// So is a host the verb does not serve, a hash appended to the args,
/// and any key past `{host, verb, args}`: no filer supplies an owner, an
/// approval flag or the plan's hash.
#[tokio::test]
async fn an_unlisted_verb_and_a_bad_param_are_refused_and_nothing_is_filed() {
    let app = app();
    let hash = "0".repeat(64);
    let cases = [
        (
            json!({"host": "forge", "verb": "rm-rf-slash", "args": []}),
            "not an allowlisted verb",
        ),
        (
            json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": ["../../etc", REPLICA]}),
            "does not match",
        ),
        (
            json!({"host": "nowhere", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA]}),
            "does not serve host",
        ),
        (
            json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA, hash]}),
            "plan_sha256",
        ),
        (
            json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA], "plan_sha256": hash}),
            "plan_sha256",
        ),
        (
            json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA], "requires_approval": false}),
            "requires_approval",
        ),
    ];
    for (body, says) in cases {
        let (status, said) = send(&app, "POST", FILE, DAVID, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {said}");
        let text = said.to_string();
        assert!(text.contains(says), "{body}: {text}");
        for cli in ["boss ops", "given here", "File it as"] {
            assert!(
                !text.contains(cli),
                "a website refusal says {cli:?}: {text}"
            );
        }
    }
    assert!(ops_requests(&app).await.is_empty(), "nothing was filed");
}

/// A PERSON'S DOOR. An agent signing with the role that may open any
/// packet is refused here — it files through no website door, so no
/// agent gains a filing path — and so is an unsigned caller. A person
/// is still judged by the policy: one whose role may not open a packet
/// is refused by the create door. Nothing is filed by any of them.
#[tokio::test]
async fn the_form_is_a_persons_door() {
    let app = app();
    let call = json!({"host": "forge", "verb": "rebuild-a-failed-replica", "args": [PVC, REPLICA]});
    let (status, said) = send(
        &app,
        "POST",
        FILE,
        Some((AGENT, "platform-admin")),
        Some(call.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");
    assert!(said.to_string().contains("person"), "{said}");
    let (status, said) = send(&app, "POST", FILE, None, Some(call.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{said}");
    let (status, said) = send(&app, "POST", FILE, Some((READER, "auditor")), Some(call)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");
    assert!(ops_requests(&app).await.is_empty(), "nothing was filed");
}
