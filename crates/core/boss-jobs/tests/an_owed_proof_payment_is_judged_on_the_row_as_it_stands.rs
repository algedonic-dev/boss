//! An owed proof's payment is judged on the row as it STANDS at the
//! write, not only on the row the door read (backlog 87fe090d, review
//! 7d92ec03 of car 5351793c).
//!
//! Car 5351793c reserved the payment (`proof_owed` / `owed_proof`) to the
//! credentialed runner at both job doors, judging each write with
//! `boss_jobs::car::payment_moves` against the row the door READ. The
//! merge then lands on the row as it stands, and the PUT saves a whole
//! row guarded only on status — so a writer landing between the read
//! and the write turned an admitted no-op into an erasure or an undo.
//!
//! THE SHAPE TESTED is the review's: the car's own in-memory adapter,
//! the race planted with `InMemoryJobs::change_job_after_next_read` (the
//! other writer commits straight after the door's read), and the forged
//! caller the car's tests use — the machine door's `x-boss-user`
//! spelling the runner's id with platform-admin, which the policy
//! admits, so what is judged here is the payment rule and nothing else.
//! Measured on origin/main e589a41de (2026-10-19), every sequence below
//! marked RACE answered 204 and left the debt erased or the payment
//! undone. The Postgres adapter's half of the precondition is pinned in
//! `an_owed_proof_payment_is_judged_under_the_row_lock_pg.rs`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::car::{OWED_PROOF, PROOF_OWED};
use boss_jobs::field_writer::CredentialedCaller;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "ship-a-change";

fn spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        KIND,
        "Ship a change",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "proven".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Prove it".into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }],
    )
}

fn user(id: &str, role: &str) -> String {
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

/// The forged runner: the machine door's header spelling the runner's id.
fn forged_runner() -> String {
    user("automation:ops-runner", "platform-admin")
}

fn runner_credential() -> CredentialedCaller {
    CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some("forge".into()),
    }
}

fn app_with_jobs(jobs: Arc<InMemoryJobs>, caller: Option<CredentialedCaller>) -> Router {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec()).unwrap();
    let mut policy = FakePolicyClient::builder();
    for role in ["system", "platform-admin"] {
        policy = policy
            .allow(role, Action::Create, Resource::job(), Scope::All)
            .allow(role, Action::Update, Resource::step(), Scope::All)
            .allow(role, Action::Update, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let router = router(state);
    match caller {
        Some(c) => router.layer(axum::Extension(c)),
        None => router,
    }
}

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes).into_owned()));
    (status, json)
}

/// File a merged car carrying `metadata`, and return its id.
async fn file(app: &Router, metadata: Value) -> JobId {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", "change-1"),
        "Ship a change",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
    );
    job.metadata = metadata;
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user("automation:filer", "system"))
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    JobId::from_uuid(uuid::Uuid::parse_str(body["id"].as_str().unwrap()).unwrap())
}

async fn patch(app: &Router, id: &JobId, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

/// PUT `job` whole, as the forged runner.
async fn put(app: &Router, job: &Job) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}", job.id))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(serde_json::to_vec(job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn stored(jobs: &InMemoryJobs, id: &JobId) -> Value {
    jobs.get_job(id).await.unwrap().unwrap().metadata
}

/// The forge door's payment, as `owed_paid_patch` writes it.
fn proof() -> Value {
    json!({
        "verified": "proven on the forge",
        "proof": "{\"exit\":0}",
        "completed_at": "2026-10-19T05:00:00Z",
        "proven_by": "run-car-probe",
    })
}

/// The other writer, committing straight after the door's read.
fn after_the_read(jobs: &InMemoryJobs, id: &JobId, write: Value) {
    jobs.change_job_after_next_read(id, move |job| {
        if let Some(md) = job.metadata.as_object_mut() {
            for (k, v) in write.as_object().cloned().unwrap_or_default() {
                md.insert(k, v);
            }
        }
    });
}

/// The refusal a write raced by a payment move gets: nothing written,
/// and the caller told to re-read — the re-send is then judged on the
/// row as it stands, by `payment_moves`.
fn assert_changed_under_it(status: StatusCode, body: &Value) {
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write judged on a payment that moved since its read is refused: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("changed since")),
        "the refusal says the row moved under the write: {body}"
    );
}

/// RACE (1), the merge door: a non-runner's `null` for `proof_owed`, read
/// while the row held none, was no move — and was not stripped, because
/// the strip fired only when the READ row held the key. A debt marked in
/// between was erased. A null for a key the row does not hold is an
/// unchanged re-send like any other, so it never reaches the merge; the
/// rest of the patch lands, and the debt stands.
#[tokio::test]
async fn a_racing_null_does_not_erase_a_debt_marked_after_the_read() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    after_the_read(&jobs, &id, json!({ PROOF_OWED: "true" }));
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: null, "note": "seen" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let md = stored(&jobs, &id).await;
    assert_eq!(
        md[PROOF_OWED], "true",
        "the debt survives a racing null: {md}"
    );
    assert_eq!(md["note"], "seen", "the rest of the patch lands: {md}");
}

/// RACE (2), same door, same null, against a payment the runner wrote in
/// between: the marker was erased and the proof kept, so the car read as
/// never having owed.
#[tokio::test]
async fn a_racing_null_does_not_erase_a_payment_written_after_the_read() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    after_the_read(
        &jobs,
        &id,
        json!({ PROOF_OWED: "paid", OWED_PROOF: proof() }),
    );
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: null, OWED_PROOF: null })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "paid", "the payment survives: {md}");
    assert_eq!(md[OWED_PROOF], proof(), "{md}");
}

/// RACE (4), the one write a non-runner may make to the marker: marking
/// a debt, absent to "true", judged on a row with none. Landed on a row
/// the runner paid in between, it re-owed a paid car and left its proof
/// beside the debt. The mark is conditioned on the payment as read: the
/// row moved, so it is refused, and the re-send is judged as a re-owe.
#[tokio::test]
async fn a_mark_is_refused_over_a_payment_written_after_the_read() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    after_the_read(
        &jobs,
        &id,
        json!({ PROOF_OWED: "paid", OWED_PROOF: proof() }),
    );
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert_changed_under_it(status, &body);
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "paid", "the payment stands: {md}");

    // The re-send the refusal asks for is judged on the row as it stands.
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["refused_keys"], json!([PROOF_OWED]), "{body}");
}

/// CONTROL for (4): the precondition reads only the payment keys, so a
/// mark raced by any OTHER key's writer — the conductor's markers land
/// on a car all day — still lands, and so does that writer's key.
#[tokio::test]
async fn a_mark_raced_by_an_unrelated_write_still_lands() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    after_the_read(&jobs, &id, json!({ "merge_ref": "abc123" }));
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "true", "{md}");
    assert_eq!(md["merge_ref"], "abc123", "{md}");
}

/// RACE (3), the job PUT: a body built while the car owed, saved after
/// the runner paid. The carry-forward put the READ row's `"true"` back
/// and the read row held no proof, so the whole-row save undid the
/// payment. The save is now conditioned on the payment as read.
#[tokio::test]
async fn a_stale_put_does_not_undo_a_payment_written_after_the_read() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true", PROOF_OWED: "true" })).await;
    let mut job = jobs.get_job(&id).await.unwrap().unwrap();
    job.metadata = json!({ "merged": "true", "note": "seen" });
    after_the_read(
        &jobs,
        &id,
        json!({ PROOF_OWED: "paid", OWED_PROOF: proof() }),
    );
    let (status, body) = put(&forged, &job).await;
    assert_changed_under_it(status, &body);
    let md = stored(&jobs, &id).await;
    assert_eq!(
        md[PROOF_OWED], "paid",
        "the payment survives a stale PUT: {md}"
    );
    assert_eq!(md[OWED_PROOF], proof(), "{md}");
    assert!(
        md.get("note").is_none(),
        "nothing of the stale body was written: {md}"
    );
}

/// RACE (5), the PUT's twin of (1): a body built while the car owed
/// nothing, saved after the debt was marked. The read row held no
/// marker to carry forward, so the whole-row save erased the debt.
#[tokio::test]
async fn a_stale_put_does_not_erase_a_debt_marked_after_the_read() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    let mut job = jobs.get_job(&id).await.unwrap().unwrap();
    job.metadata = json!({ "merged": "true", "note": "seen" });
    after_the_read(&jobs, &id, json!({ PROOF_OWED: "true" }));
    let (status, body) = put(&forged, &job).await;
    assert_changed_under_it(status, &body);
    assert_eq!(stored(&jobs, &id).await[PROOF_OWED], "true");
}

/// CONTROL for the PUT: a stale body whose payment did NOT move under it
/// saves as it always did — the precondition is the payment, not the row.
#[tokio::test]
async fn a_put_raced_by_an_unrelated_write_saves_and_keeps_the_payment() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged, json!({ "merged": "true" })).await;
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential()));
    let (status, body) = patch(
        &runner,
        &id,
        json!({ PROOF_OWED: "paid", OWED_PROOF: proof() }),
    )
    .await;
    assert!(status.is_success(), "the runner pays: {body}");
    let mut job = jobs.get_job(&id).await.unwrap().unwrap();
    job.metadata = json!({ "merged": "true", "note": "seen" });
    after_the_read(&jobs, &id, json!({ "merge_ref": "abc123" }));
    let (status, body) = put(&forged, &job).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let md = stored(&jobs, &id).await;
    assert_eq!(md["note"], "seen", "{md}");
    assert_eq!(md[PROOF_OWED], "paid", "the omitted payment is kept: {md}");
    assert_eq!(md[OWED_PROOF], proof(), "{md}");
}
