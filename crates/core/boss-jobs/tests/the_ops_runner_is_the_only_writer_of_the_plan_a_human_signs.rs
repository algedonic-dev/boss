//! The ops runner is the only writer of the plan a human signs, and the
//! only executor of what that plan authorises (backlog 6c9183de, row E
//! of design b08725c2's order; design f623e425 option A, decided by
//! David 2026-09-25).
//!
//! WHY. An ops-request's `approve` step carries the plan the host runner
//! rendered and the request it was rendered for, and David's passkey
//! signs the step's whole shape. The signer half (v10, 2026-10-07)
//! reserved `decision` and `comment` to his session; the runner's keys
//! stayed open to anyone with Update on the step, so a passkey could be
//! asked to sign a plan no runner rendered — the runner's re-render and
//! the ceremony binding REFUSED the swap afterwards, and nothing
//! PREVENTED it. The same held for `execute`: any machine-door caller
//! could claim it, return it to ready, or answer it with invented output.
//!
//! The mechanism landed inert across five cars (field writer 460a75db,
//! publish lint c8b240cb, credential door 3a166d49, credential executor
//! 1a90e4d8, S1-S4 f3365343). It waited on one fact: every host whose
//! runner answers ops-requests presents a credential the jobs API
//! resolves — forge since 2026-10-09, boss-gcp since its first issuance
//! was delivered on 2026-10-10 (rotation f194ab3e). This car makes
//! `runner:ops` resolvable and declares it on the row.
//!
//! These tests drive THE TREE'S ROW through the real credential door
//! (`runner_credential::mount`), because the row is the declaration and
//! the declaration is the car. Nothing here touches a live system of
//! record: every app is an in-memory router.

mod ops_request_rows;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::WorkflowSpec;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use ops_request_rows::{
    APPROVE_RUNNER_KEYS, NOTHING_TO_DO_RUNNER_KEYS, REVIEW_RUNNER_KEYS, RUNNER, WRITTEN_BY,
    declared_row, without_the_runner_half,
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// What each host's slot holds in the fixture's mounted directory.
const FORGE: &str = "forge-runner-credential-fixture";
const GCP: &str = "boss-gcp-runner-credential-fixture";

/// The execution evidence consumed as independently recorded machine proof.
const EXECUTE_RUNNER_KEYS: [&str; 3] = ["disposition", "output", "exit_code"];

// ---------------------------------------------------------------------
// The declaration
// ---------------------------------------------------------------------

fn step<'a>(row: &'a WorkflowSpec, title: &str) -> &'a boss_jobs::registry::StepSpec {
    row.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("ops-request has a `{title}` step"))
}

fn writer_of(row: &WorkflowSpec, title: &str, key: &str) -> Option<String> {
    step(row, title)
        .fields
        .iter()
        .find(|f| f.name == key)
        .unwrap_or_else(|| panic!("`{title}` declares `{key}` as a field"))
        .writer
        .clone()
}

/// THE RUNNER HALF IS EVERY KEY THE RUNNER RENDERS, AND THE EXECUTION.
/// On `approve`, the five keys of the one `render_plan` patch and the
/// review run it writes beside them — all inside the shape the passkey
/// signs; on `review`, the plan the reviewer releases; on
/// `nothing-to-do`, the seven-key record that closes a request with no
/// passkey asked, in place of the self-asserted `written_by` (which
/// refused the honest mistake, not a forger). `execute` declares the
/// runner its credentialed executor, so its status and holder move only
/// for the credential of the packet's host. Nothing else changes hands:
/// the signer keeps `decision` and `comment`, and the reviewer's own
/// keys stay the reviewer's to write. And the row lints, which it could
/// not before `runner:ops` was resolvable.
#[test]
fn the_runner_half_is_every_key_the_runner_renders_and_the_execution() {
    let row = declared_row();
    for key in APPROVE_RUNNER_KEYS {
        assert_eq!(
            writer_of(&row, "approve", key).as_deref(),
            Some(RUNNER),
            "approve.{key}"
        );
    }
    for key in REVIEW_RUNNER_KEYS {
        assert_eq!(
            writer_of(&row, "review", key).as_deref(),
            Some(RUNNER),
            "review.{key}"
        );
    }
    for key in NOTHING_TO_DO_RUNNER_KEYS {
        assert_eq!(
            writer_of(&row, "nothing-to-do", key).as_deref(),
            Some(RUNNER),
            "nothing-to-do.{key}"
        );
    }
    assert!(
        step(&row, "nothing-to-do")
            .metadata_defaults
            .get(boss_jobs::written_by::KEY)
            .is_none(),
        "the forger-proof writer replaces the self-asserted written_by, it does not sit beside it"
    );
    assert_eq!(step(&row, "execute").executor.as_deref(), Some(RUNNER));
    for key in EXECUTE_RUNNER_KEYS {
        assert_eq!(
            writer_of(&row, "execute", key).as_deref(),
            Some(RUNNER),
            "execute.{key}"
        );
    }
    assert_eq!(
        step(&row, "execute").claimable,
        Some(true),
        "execute stays a role queue the runner claims through, never a nomination"
    );
    for key in ["decision", "comment"] {
        assert_eq!(
            writer_of(&row, "approve", key).as_deref(),
            Some(boss_jobs::field_writer::SIGNER_WRITER),
            "approve.{key} stays the signer's"
        );
    }
    for key in ["verdict", "reviewed_plan_sha256", "source_run", "findings"] {
        assert_eq!(
            writer_of(&row, "review", key),
            None,
            "review.{key} is the reviewer's record, written by its run"
        );
    }

    // Nothing else declares a writer or an executor.
    let mut declared: Vec<String> = row
        .steps
        .iter()
        .flat_map(|s| {
            s.fields.iter().filter_map(move |f| {
                f.writer
                    .as_ref()
                    .map(|w| format!("{}.{}={w}", s.title, f.name))
            })
        })
        .chain(row.steps.iter().filter_map(|s| {
            s.executor
                .as_ref()
                .map(|e| format!("{} executor={e}", s.title))
        }))
        .collect();
    declared.sort();
    let mut expected: Vec<String> = APPROVE_RUNNER_KEYS
        .iter()
        .map(|k| format!("approve.{k}={RUNNER}"))
        .chain(
            REVIEW_RUNNER_KEYS
                .iter()
                .map(|k| format!("review.{k}={RUNNER}")),
        )
        .chain(
            NOTHING_TO_DO_RUNNER_KEYS
                .iter()
                .map(|k| format!("nothing-to-do.{k}={RUNNER}")),
        )
        .chain(
            EXECUTE_RUNNER_KEYS
                .iter()
                .map(|k| format!("execute.{k}={RUNNER}")),
        )
        .chain([
            "approve.decision=signer".into(),
            "approve.comment=signer".into(),
        ])
        .chain([format!("execute executor={RUNNER}")])
        .collect();
    expected.sort();
    assert_eq!(
        declared, expected,
        "the row declares exactly the two halves"
    );

    assert!(
        boss_jobs::field_writer::RESOLVABLE_PRINCIPALS.contains(&RUNNER),
        "the declaration lands with the principal it names, never before it"
    );
    if let Err(problems) = boss_jobs::workflow_lint::gate_active(&row) {
        panic!("the declared row must pass the publish lint: {problems:?}");
    }
}

/// THE ROAD BACK IS THE SIGNER ROW. Taking the runner half off leaves
/// the two signer keys, no executor, and `written_by` back on
/// `nothing-to-do` — the row live before this car — and it lints.
#[test]
fn the_road_back_is_the_signer_half_alone() {
    let back = without_the_runner_half();
    let declared: Vec<String> = back
        .steps
        .iter()
        .flat_map(|s| {
            s.fields.iter().filter_map(move |f| {
                f.writer
                    .as_ref()
                    .map(|w| format!("{}.{}={w}", s.title, f.name))
            })
        })
        .collect();
    assert_eq!(
        declared,
        vec![
            "approve.decision=signer".to_string(),
            "approve.comment=signer".to_string()
        ]
    );
    assert!(back.steps.iter().all(|s| s.executor.is_none()));
    assert_eq!(
        step(&back, "nothing-to-do").metadata_defaults[boss_jobs::written_by::KEY],
        WRITTEN_BY
    );
    if let Err(problems) = boss_jobs::workflow_lint::gate_active(&back) {
        panic!("the road-back row must pass the publish lint: {problems:?}");
    }
}

// ---------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------

/// A jobs API whose registry holds `live` as the active ops-request row,
/// behind the real credential door, whose slot directory holds one
/// current credential for each runner host.
fn fixture(live: WorkflowSpec) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for (action, resource) in [
        (Action::Create, Resource::job()),
        (Action::Read, Resource::job()),
        (Action::Update, Resource::job()),
        (Action::Update, Resource::step()),
        (Action::Read, Resource::workflow()),
        (Action::Create, Resource::workflow()),
        (Action::Update, Resource::workflow()),
        (Action::Publish, Resource::workflow()),
    ] {
        policy = policy.allow("platform-admin", action, resource, Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(live).expect("seed the live ops-request row");
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let slots = boss_testing::scratch_dir("ops-runner-row-e-slots");
    boss_testing::write_file(&slots.join("forge.current"), FORGE);
    boss_testing::write_file(&slots.join("boss-gcp.current"), GCP);
    (
        boss_jobs::runner_credential::mount(router(state), slots),
        jobs,
    )
}

/// One request through the door. `who` is the self-asserted
/// `x-boss-user` id; `credential` the value presented in the runner
/// header, if any.
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    who: &str,
    credential: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri).header(
        "x-boss-user",
        json!({
            "id": who,
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string(),
    );
    if let Some(value) = credential {
        request = request.header(boss_jobs::runner_credential::HEADER, value);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

/// File a forge request the way `boss ops` does; answers its id.
async fn file(app: &axum::Router, metadata: Value) -> String {
    let job = uuid::Uuid::new_v4().to_string();
    let (status, body) = send(
        app,
        "POST",
        "/api/jobs",
        "emp-david",
        None,
        Some(json!({
            "id": job,
            "kind": "ops-request",
            "subject": {"subject_kind": "custom", "id": "forge"},
            "title": "a host request",
            "owner_id": "emp-david",
            "status": "open",
            "priority": "standard",
            "metadata": metadata,
            "tags": [],
            "opened_on": "2026-10-10",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "filing the request: {body}");
    job
}

async fn steps(app: &axum::Router, job: &str) -> Vec<Value> {
    let (status, steps) = send(
        app,
        "GET",
        &format!("/api/jobs/{job}/steps"),
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET steps: {steps}");
    steps.as_array().cloned().unwrap_or_default()
}

async fn step_of(app: &axum::Router, job: &str, slug: &str) -> Value {
    steps(app, job)
        .await
        .into_iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("the request has no `{slug}` step"))
}

fn merge_door(job: &str, step: &Value) -> String {
    format!(
        "/api/jobs/{job}/steps/{}/metadata",
        step["id"].as_str().expect("step id")
    )
}

fn step_put(job: &str, step: &Value) -> String {
    format!(
        "/api/jobs/{job}/steps/{}",
        step["id"].as_str().expect("step id")
    )
}

fn approval_request() -> Value {
    json!({
        "host": "forge",
        "verb": "commission-a-disk",
        "args": ["/dev/disk/by-id/nvme-the-new-one"],
        "requires_approval": true,
    })
}

/// The patch ops-runner.sh `render_plan` writes onto `approve`.
fn the_runners_plan() -> Value {
    json!({
        "plan": "PLAN commission-a-disk on forge\n",
        "verb": "commission-a-disk",
        "host": "forge",
        "args": ["/dev/disk/by-id/nvme-the-new-one"],
        "rendered_plan_sha256": "ab".repeat(32),
    })
}

/// A 409 from the writer rule naming `key` and the runner as its writer.
fn assert_reserved(status: StatusCode, body: &Value, key: &str, context: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{context}: {body}");
    let refused = body["refused_keys"]
        .as_array()
        .unwrap_or_else(|| panic!("{context}: the refusal names no keys: {body}"));
    assert!(
        refused
            .iter()
            .any(|r| r["key"] == key && r["writer"] == RUNNER),
        "{context}: `{key}` must be refused naming its writer `{RUNNER}`: {body}"
    );
}

// ---------------------------------------------------------------------
// The refusals, and the runner that still writes
// ---------------------------------------------------------------------

/// The plan a passkey will sign is written only by the credential for
/// the packet's host. The runner's account typed onto the machine door
/// with no credential — what every machine-door caller can send — is
/// refused; so is the other host's runner; the forge runner presenting
/// its own credential writes the patch it writes today, and the step
/// holds exactly that.
#[tokio::test]
async fn a_key_the_runner_renders_is_written_only_by_the_credential_for_the_packets_host() {
    let (app, _) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    let door = merge_door(&job, &approve);

    for (who, credential, context) in [
        (
            "automation:ops-runner",
            None,
            "the runner's id with no credential",
        ),
        ("agent-claude", None, "an agent session"),
        ("emp-david", None, "David's id on the machine door"),
        (
            "automation:ops-runner",
            Some("not-a-credential"),
            "a value no slot holds",
        ),
        (
            "automation:ops-runner",
            Some(GCP),
            "the boss-gcp runner on a forge request",
        ),
    ] {
        let (status, body) = send(
            &app,
            "PATCH",
            &door,
            who,
            credential,
            Some(the_runners_plan()),
        )
        .await;
        assert_reserved(status, &body, "plan", context);
        let (status, body) = send(
            &app,
            "PATCH",
            &door,
            who,
            credential,
            Some(json!({"review_run": "a-review-run-nobody-landed"})),
        )
        .await;
        assert_reserved(status, &body, "review_run", context);
    }
    assert!(
        step_of(&app, &job, "approve").await["metadata"]
            .get("plan")
            .is_none(),
        "nothing any of them sent reached the step"
    );

    let (status, body) = send(
        &app,
        "PATCH",
        &door,
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the forge runner renders its plan: {body}"
    );
    let md = step_of(&app, &job, "approve").await["metadata"].clone();
    for (key, value) in the_runners_plan().as_object().unwrap() {
        assert_eq!(&md[key], value, "approve.{key} is what the runner wrote");
    }
}

/// `execute` moves only for the credentialed runner of the packet's
/// host: a forged completion, claim or return-to-ready is refused, and
/// the runner's own answer — keys through the merge door, then the
/// status alone, as ops-runner.sh `complete_step` sends them — lands,
/// attributed to its authenticated account whatever the header said.
#[tokio::test]
async fn the_execution_is_the_credentialed_runners() {
    let (app, _) = fixture(declared_row());
    // A read verb: no approval, so execute is ready once filed is done.
    let job = file(
        &app,
        json!({"host": "forge", "verb": "unit-status", "args": ["boss-ops-runner.service"]}),
    )
    .await;
    let filed = step_of(&app, &job, "filed").await;
    let (status, body) = send(
        &app,
        "PUT",
        &step_put(&job, &filed),
        "emp-david",
        None,
        Some(json!({"status": "completed"})),
    )
    .await;
    assert!(status.is_success(), "filed completes: {status} {body}");
    let execute = step_of(&app, &job, "execute").await;
    assert_eq!(execute["status"], "ready", "{execute}");

    for (who, credential) in [
        ("automation:ops-runner", None),
        ("automation:ops-runner", Some(GCP)),
    ] {
        for key in EXECUTE_RUNNER_KEYS {
            let (status, body) = send(
                &app,
                "PATCH",
                &merge_door(&job, &execute),
                who,
                credential,
                Some(json!({key: "forged execution evidence"})),
            )
            .await;
            assert_reserved(status, &body, key, "a forged execution record");
        }
        let (status, body) = send(
            &app,
            "PUT",
            &step_put(&job, &execute),
            who,
            credential,
            Some(json!({"status": "completed"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "a forged completion: {body}");
        let (status, body) = send(
            &app,
            "POST",
            &format!("{}/claim", step_put(&job, &execute)),
            who,
            credential,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "a forged claim: {body}");
    }
    assert_eq!(step_of(&app, &job, "execute").await["status"], "ready");

    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &execute),
        "automation:ops-runner",
        Some(FORGE),
        Some(json!({"disposition": "answered", "output": "active", "exit_code": "0", "runner_host": "forge"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the runner's answer: {body}"
    );
    let (status, body) = send(
        &app,
        "PUT",
        &step_put(&job, &execute),
        "automation:ops-runner",
        Some(FORGE),
        Some(json!({"status": "completed"})),
    )
    .await;
    assert!(
        status.is_success(),
        "the runner's completion: {status} {body}"
    );
    let execute = step_of(&app, &job, "execute").await;
    assert_eq!(execute["status"], "completed", "{execute}");
    assert_eq!(
        execute["completed_by"], "automation:ops-runner",
        "{execute}"
    );
}

/// The record that closes a request with no passkey asked is the
/// credentialed runner's alone. A forged record of all seven keys — the
/// shape `written_by` refused only when the forger signed as someone
/// other than the runner's account — is refused even signed as it.
#[tokio::test]
async fn the_nothing_to_do_record_is_the_credentialed_runners() {
    let (app, _) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let nothing = step_of(&app, &job, "nothing-to-do").await;
    let record = json!({
        "plan": "PLAN nothing\n",
        "verb": "commission-a-disk",
        "host": "forge",
        "args": ["/dev/disk/by-id/nvme-the-new-one"],
        "rendered_plan_sha256": "cd".repeat(32),
        "plan_verb": "plan-commission-a-disk",
        "nothing_to_do": "^PLAN nothing",
        "runner_host": "forge",
    });
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &nothing),
        "automation:ops-runner",
        None,
        Some(record.clone()),
    )
    .await;
    assert_reserved(status, &body, "plan", "the runner's id with no credential");
    assert!(
        step_of(&app, &job, "nothing-to-do").await["metadata"]
            .get("plan")
            .is_none()
    );
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &nothing),
        "automation:ops-runner",
        Some(FORGE),
        Some(record),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the runner's record: {body}"
    );
}

// ---------------------------------------------------------------------
// The road back
// ---------------------------------------------------------------------

/// Draft then publish `row` through the routes `boss workflow publish`
/// calls, carrying no cookie, no presence ticket, no sign-off and no
/// credential; answers the version that went live.
async fn publish(app: &axum::Router, row: &WorkflowSpec) -> i64 {
    let (status, draft) = send(
        app,
        "PUT",
        "/api/workflows/ops-request",
        "emp-david",
        None,
        Some(serde_json::to_value(row).unwrap()),
    )
    .await;
    assert!(status.is_success(), "the draft: {status} {draft}");
    let (status, published) = send(
        app,
        "POST",
        "/api/workflows/ops-request/publish",
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish: {published}");
    draft["version"].as_i64().expect("a version")
}

/// IF A RUNNER IS LOCKED OUT, THE ROAD BACK NEEDS NOTHING IT HOLDS.
/// Publishing the signer row asks only `publish` on `workflow`. A
/// request filed after it takes the runner's uncredentialed plan again;
/// one admitted under the declared row stays under it — packets are
/// pinned — so it is cancelled and refiled, never converted.
#[tokio::test]
async fn the_road_back_is_one_publish_and_a_refile() {
    let (app, _) = fixture(declared_row());
    let in_flight = file(&app, approval_request()).await;

    let version = publish(&app, &without_the_runner_half()).await;
    assert!(version > 1, "the signer row went live as a newer version");

    let fresh = file(&app, approval_request()).await;
    let approve = step_of(&app, &fresh, "approve").await;
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&fresh, &approve),
        "automation:ops-runner",
        None,
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a request filed under the signer row takes the runner's plan with no credential: {body}"
    );

    let approve = step_of(&app, &in_flight, "approve").await;
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&in_flight, &approve),
        "automation:ops-runner",
        None,
        Some(the_runners_plan()),
    )
    .await;
    assert_reserved(
        status,
        &body,
        "plan",
        "a request admitted under the declared row keeps it",
    );
}
