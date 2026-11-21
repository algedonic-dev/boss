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
mod signed_publish;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::WorkflowSpec;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
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
const EXECUTE_RUNNER_KEYS: [&str; 4] = ["disposition", "output", "exit_code", "executed"];

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
    (fixture_with_repo(live, jobs.clone()), jobs)
}

fn fixture_with_repo<R: JobsRepository + 'static>(
    live: WorkflowSpec,
    jobs: Arc<R>,
) -> axum::Router {
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
    boss_jobs::runner_credential::mount(router(state), slots)
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

/// A rejected data-only proposal: required filer context is owed at
/// admission, but the real runner cannot render a plan until afterwards.
/// Keep the refusal visible rather than weakening requiredness or
/// inventing an empty plan to make this proposal appear usable.
#[tokio::test]
async fn cef51d_required_filer_plan_refuses_the_real_admission_shape() {
    let mut proposal = declared_row();
    let approve = proposal
        .steps
        .iter_mut()
        .find(|s| s.title == "approve")
        .unwrap();
    for field in &mut approve.fields {
        if field.writer.as_deref() == Some(RUNNER) {
            field.filled_by = boss_core::job::FilledBy::Filer;
        }
    }
    let (app, jobs) = fixture(proposal);
    let id = uuid::Uuid::new_v4();
    let (status, body) = send(
        &app,
        "POST",
        "/api/jobs",
        "emp-david",
        None,
        Some(json!({
            "id": id, "kind": "ops-request",
            "subject": {"subject_kind": "custom", "id": "forge"},
            "title": "a host request", "owner_id": "emp-david",
            "status": "open", "priority": "standard",
            "metadata": approval_request(), "tags": [], "opened_on": "2026-10-10"
        })),
    )
    .await;
    println!("CEF51d rejected required-filer proposal: status={status} body={body}");
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let text = body.as_str().expect("native filer refusal is text");
    assert!(
        text.contains("filer field 'plan' missing on step 'approve'"),
        "{text}"
    );
    assert!(
        text.contains("filer field 'rendered_plan_sha256' missing on step 'approve'"),
        "{text}"
    );
    assert!(
        jobs.get_job(&boss_core::job::JobId::from_uuid(id))
            .await
            .unwrap()
            .is_none()
    );
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

/// The body ops-runner.sh `nominate_approver` PUTs to nominate `who`,
/// READ OUT OF THE RUNNER, not retyped (review R1 of car 91c8e709,
/// 2026-11-06). The cases below judge what the jobs API does with the
/// runner's nomination, and a typed copy of the body judges a body the
/// runner may not send: the review found the runner's bare
/// `{assignee_id}` replacing a person's newer nomination, where a typed
/// copy carrying the fence would have stayed green. So this takes the
/// one `jq -cn --arg w "$na_who" '<filter>'` line of that function and
/// runs its filter with the same jq the runner does.
fn the_runners_nomination(who: &str) -> Value {
    let path = boss_testing::repo_root().join("infra/ops/ops-runner.sh");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let start = src
        .find("\nnominate_approver() {\n")
        .expect("ops-runner.sh defines nominate_approver");
    let rest = &src[start..];
    let function = &rest[..rest.find("\n}\n").expect("nominate_approver ends")];
    let opener = "jq -cn --arg w \"$na_who\" '";
    let lines: Vec<&str> = function
        .lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with(opener))
        .collect();
    let [line] = lines.as_slice() else {
        panic!("nominate_approver builds its body in exactly one `{opener}…'` line: {lines:?}");
    };
    let filter = line[opener.len()..]
        .split('\'')
        .next()
        .expect("the filter closes its quote");
    let out = std::process::Command::new("jq")
        .args(["-cn", "--arg", "w", who, filter])
        .output()
        .expect("jq runs — the gate image carries it, as the runner's host does");
    assert!(
        out.status.success(),
        "the runner's filter `{filter}` runs: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("the runner's body is JSON")
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

/// THE RUNNER'S NOMINATION LANDS ON THE TREE'S ROW (backlog 9f5132dd).
/// Right after the plan, ops-runner.sh `nominate_approver` puts the
/// approval in front of the one approver its verb names with a PUT of
/// `assignee_id` and the `automatic_assignment` fence (see the next
/// case), sent, as every runner write is, under the forge runner's
/// credential. boss-testing's ops_runner_approval_sh pins that the runner
/// sends exactly that; this pins, with the body read out of the runner,
/// that the jobs API takes it on the row as declared, so the approve step
/// reaches the approver's queue rather than waiting unheld on the role,
/// and the plan beside it is untouched.
///
/// AND WHY IT IS THE PUT ALONE. `boss step nominate` also merges a
/// `nomination_requested` record; on this step a machine cannot. The
/// approve step is human-only, and the merge door refuses a non-human
/// writing any key but the context keys and the row's declared runner
/// fields — measured here, the first draft of the runner sent it and a
/// stub said yes. The refusal is held so a later edit that adds the
/// record back meets this case rather than a live 403 on every approval.
#[tokio::test]
async fn the_runners_nomination_of_a_rendered_approval_lands_on_the_declared_row() {
    let (app, _) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the plan: {body}");
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(json!({"nomination_requested": {
            "assignee_id": "emp-david", "why": "a reason",
            "by": "automation:ops-runner", "at": "2026-11-05T22:30:00Z",
        }})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a machine's nomination record on a human-only step: {body}"
    );
    assert!(
        body["refused_keys"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k == "nomination_requested")),
        "the refusal names the key: {body}"
    );
    let (status, body) = send(
        &app,
        "PUT",
        &step_put(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_nomination("emp-david")),
    )
    .await;
    assert!(status.is_success(), "the assignment: {status} {body}");
    let after = step_of(&app, &job, "approve").await;
    assert_eq!(after["assignee_id"], "emp-david", "{after}");
    assert!(
        after["metadata"].get("nomination_requested").is_none(),
        "{after}"
    );
    for (key, value) in the_runners_plan().as_object().unwrap() {
        assert_eq!(
            &after["metadata"][key], value,
            "approve.{key} survives the nomination"
        );
    }
}

/// A PERSON WHO ASSIGNS THE APPROVAL AFTER THE RUNNER READ IT KEEPS IT
/// (review R1 of car 91c8e709, 2026-11-06). The runner decides to
/// nominate off the packet it read before rendering the plan; a person
/// can assign the still-ready approve step while the plan renders or its
/// PATCH is in flight, and the runner's guard on its OWN read cannot see
/// that. The review measured the runner's bare `{assignee_id}` PUT
/// answering 204 here and replacing emp-ada with emp-david. The jobs
/// API already holds the judgement the runner needs — a PUT asking for
/// an `automatic_assignment` of a READY step never replaces its current
/// holder or a newer explicit nomination (`assignment_refusal` in
/// http/steps.rs, composed with the row-version write), answering 409
/// `automatic_assignment_superseded` — so the runner's body carries it,
/// and this drives the runner's own body through the real door in the
/// order the race runs: the read, the plan, the person, the runner.
#[tokio::test]
async fn a_person_who_assigns_the_approval_after_the_runners_read_keeps_it() {
    let (app, _) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    assert!(
        approve["assignee_id"].is_null() && approve["status"] == "ready",
        "the runner's read: a ready approve step nobody holds: {approve}"
    );
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the plan: {body}");
    let (status, body) = send(
        &app,
        "PUT",
        &step_put(&job, &approve),
        "emp-david",
        None,
        Some(json!({"assignee_id": "emp-ada"})),
    )
    .await;
    assert!(
        status.is_success(),
        "a person assigns emp-ada: {status} {body}"
    );
    let (status, body) = send(
        &app,
        "PUT",
        &step_put(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_nomination("emp-david")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the runner's nomination does not replace the person's: {body}"
    );
    assert_eq!(
        body["code"],
        boss_jobs::step_release::AUTOMATIC_ASSIGNMENT_SUPERSEDED,
        "{body}"
    );
    assert_eq!(
        body["holder"], "emp-ada",
        "the refusal names who holds it: {body}"
    );
    let after = step_of(&app, &job, "approve").await;
    assert_eq!(after["assignee_id"], "emp-ada", "Ada keeps it: {after}");
    for (key, value) in the_runners_plan().as_object().unwrap() {
        assert_eq!(
            &after["metadata"][key], value,
            "approve.{key} survives the refused nomination"
        );
    }
}

/// A human decision must not be rewritten underneath its signer, even
/// by the credential that legitimately supplied the original context.
#[tokio::test]
async fn cef51d_the_declared_runner_supplies_context_only_before_the_person_decides() {
    let (app, jobs) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    let door = merge_door(&job, &approve);
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
        "declared authenticated context: {body}"
    );
    for patch in [
        json!({"undeclared_context": "not declared"}),
        json!({"decision": "approved"}),
        json!({"comment": "a person's words"}),
    ] {
        let (status, body) = send(
            &app,
            "PATCH",
            &door,
            "automation:ops-runner",
            Some(FORGE),
            Some(patch),
        )
        .await;
        assert!(
            status.is_client_error(),
            "runner cannot write person or undeclared fields: {body}"
        );
    }
    // Isolated stored-decision fixture: the HTTP write below is still
    // the real credential-mounted metadata door, not a mocked guard.
    let materialized: boss_core::job::Step = serde_json::from_value(approve).unwrap();
    let id = &materialized.id;
    let mut stored = jobs.get_step(id).await.unwrap().unwrap();
    stored.metadata["decision"] = json!("approved");
    jobs.update_step(&stored).await.unwrap();
    let before = stored.metadata.clone();
    let (status, body) = send(
        &app,
        "PATCH",
        &door,
        "automation:ops-runner",
        Some(FORGE),
        Some(json!({"plan": "a different plan after the decision"})),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a decision closes the context window: {body}"
    );
    assert_eq!(jobs.get_step(id).await.unwrap().unwrap().metadata, before);
}

#[tokio::test]
async fn cef51d_a_person_deciding_after_the_read_fences_the_context_merge() {
    let (app, jobs) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    let step: boss_core::job::Step = serde_json::from_value(approve.clone()).unwrap();
    jobs.merge_after_next_read(
        &step.id,
        json!({"decision":"approved"}).as_object().unwrap().clone(),
    );
    let (status, body) = send(
        &app,
        "PATCH",
        &merge_door(&job, &approve),
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the actual row changed after authorization: {body}"
    );
    assert!(
        body["expected_step_version"]
            .as_str()
            .is_some_and(|token| token.starts_with(&format!("{job}/{}/", step.id)))
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["decision"], "approved");
    assert!(
        stored.metadata.get("plan").is_none(),
        "the stale context did not land"
    );
}

/// Block the real owning SQL merge, then commit a competing decision
/// or stamp before releasing it. Lock arrival is observed in Postgres,
/// never inferred from a sleep. This database belongs to TestDb alone.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cef51d_pg_person_decisions_and_stamps_fence_a_waiting_context_merge() {
    let db = boss_testing::TestDb::new().await;
    let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
    let app = fixture_with_repo(declared_row(), jobs.clone());
    let mut outcomes = Vec::new();
    for (stamp_only, resend) in [(false, false), (true, false), (false, true), (true, true)] {
        let job = file(&app, approval_request()).await;
        let approve = step_of(&app, &job, "approve").await;
        if resend {
            let (status, body) = send(
                &app,
                "PATCH",
                &merge_door(&job, &approve),
                "automation:ops-runner",
                Some(FORGE),
                Some(the_runners_plan()),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "initial plan A: {body}");
        }
        let approve = step_of(&app, &job, "approve").await;
        let step: boss_core::job::Step = serde_json::from_value(approve.clone()).unwrap();
        let mut blocker = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM steps WHERE id=$1 FOR UPDATE")
            .bind(*step.id.inner().as_uuid())
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let pending_app = app.clone();
        let door = merge_door(&job, &approve);
        let pending = tokio::spawn(async move {
            send(
                &pending_app,
                "PATCH",
                &door,
                "automation:ops-runner",
                Some(FORGE),
                Some(the_runners_plan()),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%steps%' AND pid<>pg_backend_pid())")
                    .fetch_one(&db.pool).await.unwrap();
                if blocked { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("the actual merge reached its row lock");
        if stamp_only {
            let mut signed_shape = step.clone();
            if resend {
                signed_shape.metadata["plan"] = json!("concurrent plan B");
            }
            let stamp = boss_core::job::SignOffStamp {
                authority_id: "emp-david".into(),
                role: "platform-admin".into(),
                stamped_at: chrono::Utc::now(),
                shape_hash: signed_shape.shape_hash(),
                assurance: boss_core::job::Assurance::Presence,
                presence_nonce: Some("isolated-fixture".into()),
                voided_at: None,
                voided_by_event: None,
            };
            sqlx::query("UPDATE steps SET sign_offs=$2 WHERE id=$1")
                .bind(*step.id.inner().as_uuid())
                .bind(json!([stamp]))
                .execute(&mut *blocker)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE steps SET metadata=metadata || '{\"decision\":\"approved\"}'::jsonb WHERE id=$1")
                .bind(*step.id.inner().as_uuid()).execute(&mut *blocker).await.unwrap();
        }
        if resend {
            sqlx::query("UPDATE steps SET metadata=metadata || $2 WHERE id=$1")
                .bind(*step.id.inner().as_uuid())
                .bind(json!({"plan":"concurrent plan B"}))
                .execute(&mut *blocker)
                .await
                .unwrap();
        }
        let before_events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        blocker.commit().await.unwrap();
        let (status, body) = pending.await.unwrap();
        let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
        eprintln!(
            "cef51d PG stamp_only={stamp_only} resend={resend} HTTP={status} body={body} stored={}",
            stored.metadata
        );
        let context_preserved = if resend {
            stored.metadata["plan"] == "concurrent plan B"
        } else {
            stored.metadata.get("plan").is_none()
        };
        let after_events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        outcomes.push((
            stamp_only,
            resend,
            status,
            context_preserved,
            before_events == after_events,
        ));
        if stamp_only {
            assert_eq!(stored.sign_offs.len(), 1);
        } else {
            assert_eq!(stored.metadata["decision"], "approved");
        }
    }
    for (stamp_only, resend, status, context_preserved, no_events) in outcomes {
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "competing stamp_only={stamp_only} resend={resend}"
        );
        assert!(context_preserved, "the stale context did not land");
        assert!(no_events, "the losing merge recorded no event");
    }
}

#[tokio::test]
async fn cef51d_unchanged_runner_retry_cannot_restore_context_after_a_person_decides() {
    let (app, jobs) = fixture(declared_row());
    let job = file(&app, approval_request()).await;
    let approve = step_of(&app, &job, "approve").await;
    let door = merge_door(&job, &approve);
    let (status, body) = send(
        &app,
        "PATCH",
        &door,
        "automation:ops-runner",
        Some(FORGE),
        Some(the_runners_plan()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "initial plan A: {body}");
    let step: boss_core::job::Step =
        serde_json::from_value(step_of(&app, &job, "approve").await).unwrap();
    let before_events = jobs.recorded_events().len();
    jobs.merge_after_next_read(
        &step.id,
        json!({"plan":"concurrent plan B","decision":"approved"})
            .as_object()
            .unwrap()
            .clone(),
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
        StatusCode::CONFLICT,
        "a same-value read is still a guarded write: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["plan"], "concurrent plan B");
    assert_eq!(stored.metadata["decision"], "approved");
    assert_eq!(
        jobs.recorded_events().len(),
        before_events,
        "the losing retry recorded no event"
    );
}

async fn assert_appended_stamp_fences_context<R: JobsRepository + 'static>(jobs: Arc<R>) {
    let app = fixture_with_repo(declared_row(), jobs.clone());
    let job = file(&app, approval_request()).await;
    let step: boss_core::job::Step =
        serde_json::from_value(step_of(&app, &job, "approve").await).unwrap();
    let (_, version) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    let stamp = boss_core::job::SignOffStamp {
        authority_id: "emp-david".into(),
        role: "platform-admin".into(),
        stamped_at: chrono::Utc::now(),
        shape_hash: step.shape_hash(),
        assurance: boss_core::job::Assurance::Presence,
        presence_nonce: Some("isolated-stamp-port".into()),
        voided_at: None,
        voided_by_event: None,
    };
    let event = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Human("emp-david".into()),
    );
    jobs.append_sign_off(&step.id, &stamp, &event, &[])
        .await
        .unwrap();
    let (signed, after) = jobs.get_step_versioned(&step.id).await.unwrap().unwrap();
    assert_ne!(
        version, after,
        "the actual stamp writer advances the opaque fence"
    );
    assert_eq!(
        signed.metadata, step.metadata,
        "a stamp is not a metadata change"
    );
    let result = jobs
        .merge_step_metadata_checked_at(
            &step.id,
            the_runners_plan().as_object().unwrap(),
            &event,
            None,
            None,
            Some(version),
        )
        .await;
    assert!(
        matches!(result, Err(boss_jobs::port::JobsError::StepChanged { .. })),
        "{result:?}"
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap(),
        signed,
        "the losing merge writes no row or stamp invalidation"
    );
}

#[tokio::test]
async fn cef51d_actual_memory_stamp_writer_advances_the_context_fence() {
    assert_appended_stamp_fences_context(Arc::new(InMemoryJobs::new())).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn cef51d_actual_pg_stamp_writer_advances_the_context_fence() {
    let db = boss_testing::TestDb::new().await;
    assert_appended_stamp_fences_context(Arc::new(boss_jobs::PgJobs::new(db.pool.clone()))).await;
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
        Some(json!({"disposition": "answered", "output": "active", "exit_code": "0", "runner_host": "forge",
            "executed":{"verb":"unit-status","host":"forge","argv":["fixture-only"]}})),
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
    assert_eq!(execute["metadata"]["executed"]["verb"], "unit-status");
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
/// credential. It names a SIGNED publish-a-protocol packet: the live
/// ops-request row's approve step carries a passkey, so since backlog
/// a4b57aca the server publishes over it only for bytes a named
/// approver signed with one. Answers the version that went live.
async fn publish(app: &axum::Router, jobs: &InMemoryJobs, row: &WorkflowSpec) -> i64 {
    let (status, live) = send(
        app,
        "GET",
        "/api/workflows/ops-request",
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the live row: {live}");
    let was_live = live["version"].as_i64().expect("a live version");
    let signed = signed_publish::signed_body(jobs, row, was_live).await;
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
        Some(signed),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish: {published}");
    draft["version"].as_i64().expect("a version")
}

/// IF A RUNNER IS LOCKED OUT, THE ROAD BACK NEEDS NOTHING IT HOLDS.
/// Publishing the signer row asks `publish` on `workflow` and a
/// passkey-signed publish-a-protocol packet — nothing a runner holds. A
/// request filed after it takes the runner's uncredentialed plan again;
/// one admitted under the declared row stays under it — packets are
/// pinned — so it is cancelled and refiled, never converted.
#[tokio::test]
async fn the_road_back_is_one_publish_and_a_refile() {
    let (app, jobs) = fixture(declared_row());
    let in_flight = file(&app, approval_request()).await;

    let version = publish(&app, &jobs, &without_the_runner_half()).await;
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

// ---------------------------------------------------------------------
// How the verb ended picks the terminal
// ---------------------------------------------------------------------

/// THE TERMINAL IS HOW THE VERB ENDED (design b7bb1625, backlog
/// 50a1578f). Ops-request 02035ff0 ran a replica move that failed after
/// 1,204 s with exit 1 and closed `answered` / outcome_kind completed:
/// the row forked on the disposition alone, and nothing on the packet
/// looked failed. The declared row now forks on the exit as well, and
/// this drives each record the runner can write through the REAL engine
/// (the credential door, the merge door, the status PUT, the row's own
/// `ready_when`s) and holds the terminal it readies to the one
/// classifier every reader shares (`boss_jobs::ops_execution`, CLAUDE.md
/// §9a: the row's predicates and the classifier are two spellings of
/// one fact, so they are pinned equal here).
///
/// The engine readies the terminal and the dispatcher completes it, so
/// what is read is the one terminal left `ready` and its materialised
/// `outcome_kind`; every other terminal the execute fork decides is
/// skipped.
#[tokio::test]
async fn the_terminal_a_request_closes_on_is_how_its_verb_ended() {
    use boss_jobs::ops_execution::classify;
    let row = declared_row();
    let outcome_of = |slug: &str| {
        step(&row, slug)
            .terminal
            .as_ref()
            .map(|t| t.outcome.clone())
            .unwrap_or_else(|| panic!("`{slug}` is a terminal"))
    };
    // (the record the runner writes, the terminal it must ready). A
    // null exit is what the runner sends for "no exit" (ops-runner.sh
    // `exit_code: (if $rc == "" then null ...)`), which the merge door
    // reads as a delete — so it, an absent key and an empty string all
    // say the same thing: the verb was invoked and no exit is known.
    let cases: [(Value, &str); 12] = [
        (
            json!({"disposition": "answered", "exit_code": "0"}),
            "answered",
        ),
        // EX_TEMPFAIL, the estate's not-yet (decided 2026-11-18 on the
        // item): an answer about the world, never a failed execution.
        (
            json!({"disposition": "answered", "exit_code": "75"}),
            "not-yet",
        ),
        // THE CASE THAT FILED THIS.
        (
            json!({"disposition": "answered", "exit_code": "1",
                "output": "move: FAILED after 1204s — new replica still rebuilding\n"}),
            "failed",
        ),
        (
            json!({"disposition": "answered", "exit_code": "124",
                "output": "x\n\n[ops-runner: command killed at 300s timeout]\n"}),
            "failed",
        ),
        (
            json!({"disposition": "answered", "exit_code": "137"}),
            "failed",
        ),
        (
            json!({"disposition": "answered", "exit_code": "255"}),
            "failed",
        ),
        (json!({"disposition": "answered"}), "indeterminate"),
        (
            json!({"disposition": "answered", "exit_code": null}),
            "indeterminate",
        ),
        (
            json!({"disposition": "answered", "exit_code": ""}),
            "indeterminate",
        ),
        // Not a status the runner can write ($? is digits). The row
        // cannot read digits, so it closes this `failed` where the
        // classifier says indeterminate: both are outcome_kind failed,
        // which is what is pinned — never success, never nothing-ran.
        (
            json!({"disposition": "answered", "exit_code": "abc"}),
            "failed",
        ),
        (
            json!({"disposition": "refused", "output": "outside the allowlist"}),
            "refused",
        ),
        // A refusal says nothing ran; one carrying an exit contradicts
        // itself and is read closed.
        (
            json!({"disposition": "refused", "exit_code": "1"}),
            "indeterminate",
        ),
    ];
    let (app, _) = fixture(row.clone());
    for (record, want) in cases {
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
        let mut answer = json!({"output": "ran\n", "runner_host": "forge"});
        for (k, v) in record.as_object().expect("a record") {
            answer[k] = v.clone();
        }
        let (status, body) = send(
            &app,
            "PATCH",
            &merge_door(&job, &execute),
            "automation:ops-runner",
            Some(FORGE),
            Some(answer),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{record}: {body}");
        let (status, body) = send(
            &app,
            "PUT",
            &step_put(&job, &execute),
            "automation:ops-runner",
            Some(FORGE),
            Some(json!({"status": "completed"})),
        )
        .await;
        assert!(status.is_success(), "{record}: {status} {body}");

        let after = steps(&app, &job).await;
        let execute = after
            .iter()
            .find(|s| s["spec_slug"] == "execute")
            .expect("execute");
        assert_eq!(execute["status"], "completed", "{record}: {execute}");
        let ready: Vec<&Value> = after
            .iter()
            .filter(|s| {
                let slug = s["spec_slug"].as_str().unwrap_or_default();
                step(&row, slug).terminal.is_some() && s["status"] == "ready"
            })
            .collect();
        assert_eq!(
            ready.len(),
            1,
            "{record}: exactly one terminal is readied, got {ready:#?}"
        );
        let terminal = ready[0];
        assert_eq!(terminal["spec_slug"], want, "{record}: {terminal}");
        let reading = classify(&execute["metadata"]);
        assert_eq!(
            terminal["metadata"]["outcome_kind"],
            reading.outcome_kind(),
            "{record}: the row's terminal and the classifier's reading ({reading:?}) disagree"
        );
        // A verb that ran never closes on a terminal that says nothing
        // did.
        if record["disposition"] == "answered" {
            assert_ne!(outcome_of(want), "refused", "{record}");
        }
    }
    // The not-yet closes on the `answered` outcome — every rule that
    // hears an answer hears it, as it did before — and says not_yet.
    assert_eq!(outcome_of("not-yet"), "answered");
    assert_eq!(outcome_of("failed"), "failed");
    assert_eq!(outcome_of("indeterminate"), "indeterminate");
}
