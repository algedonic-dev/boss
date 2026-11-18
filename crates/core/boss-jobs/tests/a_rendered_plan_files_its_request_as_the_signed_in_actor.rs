//! A rendered plan files its mutating request as the signed-in actor
//! (backlog 7aef2405, David's request 2026-11-05, reshaped 2004Z).
//!
//! Measured that day: a requires_approval verb already renders its plan
//! onto the request's approve step at filing, and the passkey signs those
//! bytes. What was missing was the FILING. Every act listed for David —
//! the disk_tight alarm's `rebuild-a-failed-replica`, three tenant merges
//! whose plans were rendered — meant typing `boss ops forge <verb> …` in
//! a terminal, because the website had no surface that files a planned
//! verb and the agents' harnesses refuse the mutating filing.
//!
//! So a plan-* preview packet that rendered clean files its write from
//! the record: `POST /api/jobs/{plan}/file-the-request`. What is held
//! here, through the router the jobs API serves:
//!
//!   1. The write is read off the PLAN PACKET — the verb whose plan_verb
//!      it ran, its host, its args — and the allowlist's own rules
//!      (`boss_jobs::ops_verbs::validate`, the one `boss ops` runs); the
//!      body supplies nothing.
//!   2. It is filed as the caller that signed the POST, through the same
//!      create door `boss ops` uses, so policy's Create on job:ops-request
//!      decides, and the packet carries `opened_by` = that caller.
//!   3. Filing is not approving: the filed request's approve step is
//!      READY and waiting on its sign-off, exactly as a terminal-filed one.
//!   4. No filer supplies `plan_sha256` — not in the body, not smuggled in
//!      the plan packet's args.
//!   5. Only a plan that answered exit 0 files, and once while its request
//!      is open.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PERSON: &str = "emp-david";
const AGENT: &str = "agent-claude";
/// A role that may read every packet and open none.
const READER: &str = "auditor";

struct OnePersonRoster;

#[async_trait::async_trait]
impl RosterLookup for OnePersonRoster {
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

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
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
            .allow(READER, Action::Read, Resource::job(), Scope::All)
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
        roster: Some(Arc::new(OnePersonRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
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

const ADMIN: Option<(&str, &str)> = Some((AGENT, "platform-admin"));

/// A plan-* preview packet as the runner leaves it: an ops-request for
/// the plan verb, closed, its `execute` step completed with the runner's
/// answer. `exit` None leaves the step ready — not yet rendered.
async fn seed_plan(
    jobs: &InMemoryJobs,
    n: u8,
    verb: &str,
    args: &[&str],
    exit: Option<&str>,
) -> String {
    let id = format!("7aef2405-0000-4000-8000-0000000000{n:02}");
    let job_id = JobId::from_uuid(uuid::Uuid::parse_str(&id).expect("uuid"));
    let job = Job {
        id: job_id,
        status: if exit.is_some() {
            JobStatus::Closed
        } else {
            JobStatus::Open
        },
        metadata: json!({"host": "forge", "verb": verb, "args": args}),
        ..Job::new(
            "ops-request",
            Subject::new("custom", "forge"),
            format!("{verb} {} on forge", args.join(" ")),
            PERSON,
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 5).expect("date"),
        )
    };
    jobs.create_job(&job).await.expect("plan packet");
    let step = Step {
        spec_slug: Some("execute".into()),
        status: if exit.is_some() {
            StepStatus::Completed
        } else {
            StepStatus::Ready
        },
        metadata: match exit {
            Some(code) => json!({
                "disposition": "answered",
                "exit_code": code,
                "output": format!("PLAN {verb}\n…the plan the runner rendered…"),
                "runner_host": "forge",
            }),
            None => json!({}),
        },
        ..Step::new(job_id, "task", "execute", 1)
    };
    jobs.add_step(&step).await.expect("execute step");
    id
}

fn file_uri(id: &str) -> String {
    format!("/api/jobs/{id}/file-the-request")
}

async fn ops_requests_filed_from(app: &axum::Router, plan: &str) -> Vec<Value> {
    let q = urlencode(&json!({"filed_from_plan": plan}).to_string());
    let (status, page) = send(
        app,
        "GET",
        &format!("/api/jobs?kind=ops-request&metadata={q}&full=true&limit=50"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["data"].as_array().cloned().unwrap_or_default()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn step<'a>(job: &'a Value, slug: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
        .unwrap_or_else(|| panic!("no {slug} step on {job}"))
}

/// THE CASE DAVID WAS IN (d661803b): the disk sweep rendered a failed
/// replica's rebuild plan, and acting on it took a terminal. From the
/// plan packet, the write is filed as whoever signed the POST — an agent
/// through the machine door here — with the approval flag the verb file
/// declares, and it waits on the approve step exactly as `boss ops` files
/// it: ready, unsigned.
#[tokio::test]
async fn a_clean_plan_files_its_write_as_the_signed_in_actor() {
    let (app, jobs) = app();
    let args = [
        "pvc-0a1b2c3d-0000-4000-8000-00000000abcd",
        "pvc-0a1b2c3d-0000-4000-8000-00000000abcd-r-1234abcd",
    ];
    let plan = seed_plan(&jobs, 1, "plan-a-failed-replica-rebuild", &args, Some("0")).await;

    let (status, preview) = send(&app, "GET", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["plan"], true, "{preview}");
    assert_eq!(preview["fileable"], true, "{preview}");
    assert_eq!(preview["request"]["verb"], "rebuild-a-failed-replica");
    assert_eq!(preview["request"]["host"], "forge");
    assert_eq!(preview["request"]["args"], json!(args));
    assert_eq!(preview["request"]["requires_approval"], true);
    assert_eq!(preview["request"]["approvers"], json!([PERSON]));
    assert!(preview["open_request"].is_null(), "{preview}");

    let (status, created) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("created id").to_string();
    let (status, filed) = send(&app, "GET", &format!("/api/jobs/{id}"), ADMIN, None).await;
    assert_eq!(status, StatusCode::OK, "{filed}");
    assert_eq!(filed["kind"], "ops-request");
    assert_eq!(
        filed["title"],
        format!("rebuild-a-failed-replica {} on forge", args.join(" "))
    );
    assert_eq!(filed["subject"]["id"], "forge");
    let md = &filed["metadata"];
    assert_eq!(md["host"], "forge");
    assert_eq!(md["verb"], "rebuild-a-failed-replica");
    assert_eq!(md["args"], json!(args), "the args stop before plan_sha256");
    assert_eq!(md["requires_approval"], true);
    assert!(md.get("requires_review").is_none(), "{md}");
    assert_eq!(md["filed_from_plan"], plan);
    assert_eq!(
        md["opened_by"], AGENT,
        "the filer is the caller that signed"
    );

    let approve = step(&filed, "approve");
    assert_eq!(
        approve["status"], "ready",
        "filing is not approving: {approve}"
    );
    assert_eq!(approve["sign_offs"], json!([]), "{approve}");
    assert_ne!(step(&filed, "execute")["status"], "ready");
}

/// The tenant merge's plan files its write with BOTH flags its verb file
/// declares, so the independent review step opens before any passkey.
#[tokio::test]
async fn a_tenant_merge_plan_files_its_write_behind_its_review() {
    let (app, jobs) = app();
    let args = ["llc", "tenant/merge-sponsorship"];
    let plan = seed_plan(&jobs, 2, "plan-a-tenant-merge", &args, Some("0")).await;
    let (status, created) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("created id");
    let (_, filed) = send(&app, "GET", &format!("/api/jobs/{id}"), ADMIN, None).await;
    assert_eq!(filed["metadata"]["verb"], "merge-tenant-main");
    assert_eq!(filed["metadata"]["requires_approval"], true);
    assert_eq!(filed["metadata"]["requires_review"], true);
    assert_eq!(step(&filed, "review")["status"], "ready", "{filed}");
    assert_ne!(step(&filed, "approve")["status"], "ready", "{filed}");
}

/// THE FILER IS POLICY-CHECKED: a caller the policy does not let open an
/// ops-request is refused by the create door, and nothing is filed. An
/// unsigned caller is refused before anything is read.
#[tokio::test]
async fn filing_is_refused_to_a_caller_without_create_on_an_ops_request() {
    let (app, jobs) = app();
    let plan = seed_plan(&jobs, 3, "plan-a-pod-reap", &[], Some("0")).await;

    let (status, said) = send(
        &app,
        "POST",
        &file_uri(&plan),
        Some(("emp-reader", READER)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");
    let (status, said) = send(&app, "POST", &file_uri(&plan), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{said}");
    assert!(ops_requests_filed_from(&app, &plan).await.is_empty());
}

/// NO FILER SUPPLIES THE HASH (backlog fd7090cc). The runner appends
/// sha256 of the SIGNED plan itself; this door takes no body at all, so
/// neither a `plan_sha256` nor args nor an approval flag can ride in. And
/// a plan packet whose own args carry one more value than the plan verb
/// takes — the hash, smuggled — is refused by the allowlist's arg count.
#[tokio::test]
async fn no_filer_supplies_the_plan_hash() {
    let (app, jobs) = app();
    let hash = "0".repeat(64);
    let plan = seed_plan(&jobs, 4, "plan-a-pod-reap", &[], Some("0")).await;
    for body in [
        json!({"plan_sha256": hash}),
        json!({"args": [hash]}),
        json!({"requires_approval": false}),
    ] {
        let (status, said) = send(&app, "POST", &file_uri(&plan), ADMIN, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {said}");
        assert!(said.to_string().contains("plan_sha256"), "{said}");
    }
    assert!(ops_requests_filed_from(&app, &plan).await.is_empty());

    let smuggled = seed_plan(
        &jobs,
        5,
        "plan-a-failed-replica-rebuild",
        &[
            "pvc-0a1b2c3d-0000-4000-8000-00000000abcd",
            "pvc-0a1b2c3d-0000-4000-8000-00000000abcd-r-1234abcd",
            &hash,
        ],
        Some("0"),
    )
    .await;
    let (status, said) = send(&app, "POST", &file_uri(&smuggled), ADMIN, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.to_string().contains("takes 2 arg(s)"), "{said}");
    assert!(ops_requests_filed_from(&app, &smuggled).await.is_empty());
}

/// ONLY A CLEAN PLAN FILES. A plan the runner has not answered, or one
/// it answered with a refusal (exit 78: not a plan), files nothing and
/// says why; a packet whose verb is no write's plan is not a plan at all.
#[tokio::test]
async fn only_a_plan_that_rendered_clean_files() {
    let (app, jobs) = app();
    let pending = seed_plan(&jobs, 6, "plan-a-pod-reap", &[], None).await;
    let refused = seed_plan(&jobs, 7, "plan-a-pod-reap", &[], Some("78")).await;
    let read = seed_plan(&jobs, 8, "uptime", &[], Some("0")).await;

    for (id, says) in [(&pending, "not answered"), (&refused, "exit 78")] {
        let (status, preview) = send(&app, "GET", &file_uri(id), ADMIN, None).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["plan"], true, "{preview}");
        assert_eq!(preview["fileable"], false, "{preview}");
        assert!(
            preview["why"].as_str().unwrap_or("").contains(says),
            "{preview}"
        );
        let (status, said) = send(&app, "POST", &file_uri(id), ADMIN, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(said.to_string().contains(says), "{said}");
    }
    let (status, preview) = send(&app, "GET", &file_uri(&read), ADMIN, None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["plan"], false, "{preview}");
    let (status, _) = send(&app, "POST", &file_uri(&read), ADMIN, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    for id in [&pending, &refused, &read] {
        assert!(ops_requests_filed_from(&app, id).await.is_empty());
    }
}

/// ONCE WHILE IT IS OPEN (correctness protocol: idempotence). A second
/// press files no twin with a second approve step for the same plan; it
/// is refused naming the request already waiting, and the preview names
/// it so the page links it instead of offering the button.
#[tokio::test]
async fn a_plan_files_its_request_once_while_it_is_open() {
    let (app, jobs) = app();
    let plan = seed_plan(&jobs, 9, "plan-a-pod-reap", &[], Some("0")).await;
    let (status, created) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let first = created["id"].as_str().expect("id").to_string();

    let (status, said) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(said.to_string().contains(&first), "{said}");
    let (_, preview) = send(&app, "GET", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(preview["fileable"], false, "{preview}");
    assert_eq!(preview["open_request"], first.as_str(), "{preview}");
    assert_eq!(ops_requests_filed_from(&app, &plan).await.len(), 1);

    // Once that request has closed — refused, or its approval gone stale
    // — the plan files a NEW one, under a new id: the id counts the
    // requests filed before it, so it never answers the closed packet.
    let first_id = JobId::from_uuid(uuid::Uuid::parse_str(&first).expect("uuid"));
    let mut closed = jobs.get_job(&first_id).await.expect("read").expect("filed");
    closed.status = JobStatus::Closed;
    jobs.update_job(&closed)
        .await
        .expect("close the first request");
    let (status, created) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let second = created["id"].as_str().expect("id").to_string();
    assert_ne!(second, first);
    assert_eq!(ops_requests_filed_from(&app, &plan).await.len(), 2);
}

/// THE RACE THE CHECK CANNOT SEE. Two presses can both pass the
/// open-request check before either creates; the id they file under is
/// the plan's and the count's, so both send the same one and the create
/// door admits it once. Reproduced here by planting the request the first
/// press would have created, then pressing: the answer is that packet,
/// and there is still one.
#[tokio::test]
async fn two_presses_that_race_the_check_file_one_request() {
    let (app, jobs) = app();
    let plan = seed_plan(&jobs, 10, "plan-a-pod-reap", &[], Some("0")).await;
    let (status, created) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let first = created["id"].as_str().expect("id").to_string();
    // The second press's view: the first request exists, but its
    // metadata has not reached the check yet — strip the key the check
    // reads, exactly the window two concurrent presses share.
    let first_id = JobId::from_uuid(uuid::Uuid::parse_str(&first).expect("uuid"));
    let mut raced = jobs.get_job(&first_id).await.expect("read").expect("filed");
    let kept = raced.metadata.clone();
    raced.metadata["filed_from_plan"] = Value::Null;
    jobs.update_job(&raced).await.expect("hide the key");
    let (status, again) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    raced.metadata = kept;
    jobs.update_job(&raced).await.expect("restore the key");
    // Two real presses send the same body and are answered 200, the packet
    // that exists; the planted one differs by the hidden key, so the
    // create door refuses it 409 naming the id. Either way: no second.
    assert!(
        status == StatusCode::OK || status == StatusCode::CONFLICT,
        "the create door answers the id it already holds, never a second packet: {status} {again}"
    );
    assert_eq!(again["id"], first.as_str(), "{again}");
    assert_eq!(ops_requests_filed_from(&app, &plan).await.len(), 1);
    let (_, all) = send(
        &app,
        "GET",
        "/api/jobs?kind=ops-request&full=true&limit=50",
        ADMIN,
        None,
    )
    .await;
    let filed: Vec<&Value> = all["data"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|j| j["metadata"]["verb"] == "reap-terminated-pods")
        .collect();
    assert_eq!(filed.len(), 1, "{all}");
}

/// A WEBSITE REFUSAL SPEAKS TO A WEBSITE VIEWER (Codex review of car
/// 72e7a790, P2). The arity refusal carried in from main's 2267b4f6 told
/// a `boss ops` typist that "a hash given here" is refused and "File it
/// as: boss ops …". This door takes no body: nothing was given here and
/// no command is the way in. What a viewer CAN see is the plan's own
/// recorded args, so the refusal names those, as the packet holds them,
/// and why the allowlist will not file them — in no terminal's words.
#[tokio::test]
async fn a_plan_whose_recorded_args_do_not_file_names_them_in_no_terminal_words() {
    let (app, jobs) = app();
    let hash = "0".repeat(64);
    let recorded = [
        "pvc-0a1b2c3d-0000-4000-8000-00000000abcd",
        "pvc-0a1b2c3d-0000-4000-8000-00000000abcd-r-1234abcd",
        hash.as_str(),
    ];
    let plan = seed_plan(
        &jobs,
        11,
        "plan-a-failed-replica-rebuild",
        &recorded,
        Some("0"),
    )
    .await;
    let (status, said) = send(&app, "POST", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    let (status, preview) = send(&app, "GET", &file_uri(&plan), ADMIN, None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let why = preview["why"].as_str().unwrap_or("").to_string();
    for text in [said.as_str().unwrap_or("").to_string(), why] {
        for cli in ["boss ops", "given here", "File it as"] {
            assert!(
                !text.contains(cli),
                "a website refusal says {cli:?}: {text}"
            );
        }
        assert!(text.contains("recorded"), "{text}");
        for arg in recorded {
            assert!(text.contains(arg), "the refusal must name {arg}: {text}");
        }
        assert!(
            text.contains("takes 2 arg(s)") && text.contains("plan_sha256"),
            "{text}"
        );
    }
    assert!(ops_requests_filed_from(&app, &plan).await.is_empty());
}
