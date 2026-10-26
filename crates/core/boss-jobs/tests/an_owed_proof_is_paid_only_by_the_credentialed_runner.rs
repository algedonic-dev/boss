//! An owed proof is paid only by the credentialed runner — at both job
//! doors that can write job metadata (backlog 82946cd9, F3 of review
//! 9daaa7a9 / e7bf6b14 on car c8ecf66a).
//!
//! THE CLAIM, measured on origin/main cfae8594e (2026-10-17) and again
//! at 75d9ab9da (2026-10-18): a car that closed landed with its proof
//! owed is paid by ONE metadata write — `proof_owed = "paid"` beside the
//! proof under `owed_proof` — and the job metadata PATCH reserved only
//! `corrections`, `repins` and `opened_by`. So any caller holding Update
//! on job could write a payment no reader could tell from the forge's,
//! except by the audit actor, which is the self-asserted `x-boss-user`
//! id every machine-door caller can type.
//!
//! THE SHAPE TESTED is the forged caller the review named: the machine
//! door's `x-boss-user` spelling the runner's own id with platform-admin,
//! which the policy admits — so a refusal here is the payment rule's, not
//! the policy's — writing exactly the patch the forge door writes
//! (`boss-cli` prove.rs `owed_paid_patch`: the marker and the proof in
//! one object). The credentialed runner is a `CredentialedCaller` request
//! extension, set by a layer standing in for the credential door
//! (`boss_jobs::runner_credential`), which a client cannot set.

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

/// The forged runner: the machine door's header, spelling the runner's
/// own id, exactly as infra/ops/ops-runner.sh builds it.
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
            .allow(role, Action::Read, Resource::job(), Scope::All)
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

/// File a merged car and return its id.
async fn file(app: &Router) -> JobId {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", "change-1"),
        "Ship a change",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(),
    );
    job.metadata = json!({ "merged": "true" });
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

/// PUT the stored job back with `metadata` in place of its own.
async fn put_metadata(
    app: &Router,
    jobs: &InMemoryJobs,
    id: &JobId,
    metadata: Value,
) -> (StatusCode, Value) {
    let mut job = jobs.get_job(id).await.unwrap().unwrap();
    job.metadata = metadata;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{id}"))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn stored(jobs: &InMemoryJobs, id: &JobId) -> Value {
    jobs.get_job(id).await.unwrap().unwrap().metadata
}

/// The forge door's payment, shaped as `owed_paid_patch` writes it.
fn payment(verified: &str) -> Value {
    json!({
        PROOF_OWED: "paid",
        OWED_PROOF: {
            "verified": verified,
            "proof": "{\"exit\":0}",
            "completed_at": "2026-10-18T05:00:00Z",
            "proven_by": "run-car-probe",
        },
    })
}

fn assert_refused(status: StatusCode, body: &Value, key: &str) {
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a payment written by anyone but the credentialed runner must be refused: {body}"
    );
    let refused = body["refused_keys"].as_array().expect("refused_keys");
    assert!(
        refused.iter().any(|k| k == key),
        "the refusal names `{key}`: {body}"
    );
    assert_eq!(
        body["asked_by"], "automation:ops-runner",
        "the refusal names who asked, and does not believe it: {body}"
    );
}

/// THE CLAIM, at the merge door: the forged runner cannot pay a debt,
/// and the stored car does not move. Marking the debt is not a payment
/// and is admitted, so the refusal is the payment's, not the key's.
#[tokio::test]
async fn the_merge_door_refuses_a_forged_payment() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = app_with_jobs(jobs.clone(), None);
    let id = file(&app).await;
    let (status, body) = patch(&app, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "marking a debt is admitted: {body}");

    let (status, body) = patch(&app, &id, payment("forged")).await;
    assert_refused(status, &body, PROOF_OWED);
    assert_refused(status, &body, OWED_PROOF);
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "true", "the debt still reads owed: {md}");
    assert!(md.get(OWED_PROOF).is_none(), "no proof was planted: {md}");

    // Half a payment is a payment too: the proof alone, or the marker alone.
    let (status, body) = patch(&app, &id, json!({ OWED_PROOF: {"verified": "forged"} })).await;
    assert_refused(status, &body, OWED_PROOF);
    let (status, body) = patch(&app, &id, json!({ PROOF_OWED: "paid" })).await;
    assert_refused(status, &body, PROOF_OWED);
    assert_eq!(stored(&jobs, &id).await[PROOF_OWED], "true");
}

/// A debt is not erased, and a payment not unpaid or re-owed, by any
/// writer but the runner: each would silence or re-open the obligation
/// on a record that looks the runner's.
#[tokio::test]
async fn a_debt_is_not_erased_nor_a_payment_undone_by_another_writer() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged).await;
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "{body}");
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: null })).await;
    assert_refused(status, &body, PROOF_OWED);
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "false" })).await;
    assert_refused(status, &body, PROOF_OWED);
    assert_eq!(stored(&jobs, &id).await[PROOF_OWED], "true");

    let runner = app_with_jobs(jobs.clone(), Some(runner_credential()));
    let (status, body) = patch(&runner, &id, payment("proven on the forge")).await;
    assert!(status.is_success(), "the runner pays: {body}");

    for undo in [
        json!({ OWED_PROOF: null }),
        json!({ PROOF_OWED: null }),
        json!({ PROOF_OWED: "true" }),
    ] {
        let (status, body) = patch(&forged, &id, undo.clone()).await;
        let key = undo.as_object().unwrap().keys().next().unwrap().clone();
        assert_refused(status, &body, &key);
    }
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "paid", "{md}");
    assert_eq!(md[OWED_PROOF]["verified"], "proven on the forge", "{md}");
}

/// The job PUT REPLACES metadata, so it is the other door a payment
/// could ride: a forged one is refused there too, and a body built
/// without the payment (a stale round-trip) keeps the stored one rather
/// than erasing it.
#[tokio::test]
async fn the_job_put_neither_forges_nor_drops_a_payment() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged).await;
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "{body}");

    let mut md = stored(&jobs, &id).await;
    md[PROOF_OWED] = json!("paid");
    md[OWED_PROOF] = payment("forged")[OWED_PROOF].clone();
    let (status, body) = put_metadata(&forged, &jobs, &id, md).await;
    assert_refused(status, &body, PROOF_OWED);
    assert!(stored(&jobs, &id).await.get(OWED_PROOF).is_none());

    let runner = app_with_jobs(jobs.clone(), Some(runner_credential()));
    let (status, body) = patch(&runner, &id, payment("proven on the forge")).await;
    assert!(status.is_success(), "the runner pays: {body}");

    let (status, body) = put_metadata(&forged, &jobs, &id, json!({ "merged": "true" })).await;
    assert!(
        status.is_success(),
        "a body that omits the payment is not a write to it: {body}"
    );
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "paid", "the stored payment survives: {md}");
    assert_eq!(md[OWED_PROOF]["verified"], "proven on the forge", "{md}");
}

/// THE MERGE DOOR'S RACE (the S3 shape of car 1e603cfd, at the job
/// door). The door judges against the row it READ and merges onto the
/// row as it STANDS, so a stale re-send of the debt — unchanged when
/// read, so admitted — would put `"true"` back over a payment the runner
/// wrote in between, and the obligation would run a paid car again. The
/// runner's payment lands straight after the forged caller's read; the
/// re-send must not undo it, and the key beside it must still land.
#[tokio::test]
async fn a_stale_re_send_of_the_debt_cannot_undo_a_payment() {
    let jobs = Arc::new(InMemoryJobs::new());
    let forged = app_with_jobs(jobs.clone(), None);
    let id = file(&forged).await;
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "{body}");

    jobs.change_job_after_next_read(&id, |job| {
        if let Some(md) = job.metadata.as_object_mut() {
            md.insert(PROOF_OWED.into(), json!("paid"));
        }
    });
    let (status, body) = patch(&forged, &id, json!({ PROOF_OWED: "true", "note": "seen" })).await;
    assert!(
        status.is_success(),
        "an unchanged re-send is admitted: {body}"
    );
    let md = stored(&jobs, &id).await;
    assert_eq!(
        md[PROOF_OWED], "paid",
        "the payment survives a stale re-send: {md}"
    );
    assert_eq!(md["note"], "seen", "the key beside it lands: {md}");
}

/// CONTROLS. The credentialed runner pays; an unchanged re-send of the
/// stored payment by anyone changes nothing and is admitted; a key that
/// is not the payment's is untouched by the rule.
#[tokio::test]
async fn the_runner_pays_and_unchanged_re_sends_are_admitted() {
    let jobs = Arc::new(InMemoryJobs::new());
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential()));
    let id = file(&runner).await;
    let (status, body) = patch(&runner, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "{body}");
    let (status, body) = patch(&runner, &id, payment("proven on the forge")).await;
    assert!(status.is_success(), "the runner pays: {body}");

    let forged = app_with_jobs(jobs.clone(), None);
    let (status, body) = patch(&forged, &id, payment("proven on the forge")).await;
    assert!(
        status.is_success(),
        "sending the stored payment back changes nothing: {body}"
    );
    let (status, body) = patch(&forged, &id, json!({ "note": "seen" })).await;
    assert!(
        status.is_success(),
        "an unrelated key is not reserved: {body}"
    );
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "paid", "{md}");
    assert_eq!(md["note"], "seen", "{md}");
}

/// THE HOST BINDING (still owed on 82946cd9 after car 5351793c): the
/// runner credential resolves on BOTH runner hosts, and only the forge
/// runs a car's probe (infra/ops/verbs/run-car-probe.json `hosts`). So
/// the boss-gcp runner's credential — the right principal on a host
/// that never ran the probe — cannot pay, and the refusal names the
/// verb whose hosts it was judged against.
#[tokio::test]
async fn a_runner_on_a_host_that_runs_no_probe_cannot_pay() {
    let jobs = Arc::new(InMemoryJobs::new());
    let elsewhere = CredentialedCaller {
        host: Some("boss-gcp".into()),
        ..runner_credential()
    };
    let app = app_with_jobs(jobs.clone(), Some(elsewhere));
    let id = file(&app).await;
    let (status, body) = patch(&app, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "marking a debt is admitted: {body}");

    let (status, body) = patch(&app, &id, payment("paid from boss-gcp")).await;
    assert_refused(status, &body, PROOF_OWED);
    assert!(
        body["why"]
            .as_str()
            .is_some_and(|w| w.contains("run-car-probe")),
        "the refusal names the verb whose hosts judged it: {body}"
    );
    assert_eq!(body["credential"]["host"], "boss-gcp", "{body}");
    let md = stored(&jobs, &id).await;
    assert_eq!(md[PROOF_OWED], "true", "the debt still reads owed: {md}");
    assert!(md.get(OWED_PROOF).is_none(), "no proof was planted: {md}");
}

/// The packet as `GET /api/jobs/{id}` hands it to every reader.
async fn get(app: &Router, id: &JobId) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/jobs/{id}"))
                .header("x-boss-user", user("automation:reader", "platform-admin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn proven_step(job: &Value) -> &Value {
    job["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "proven")
        .expect("the car's proven step")
}

/// F4 OF REVIEWS 9daaa7a9 / e7bf6b14 (backlog 82946cd9): NOTHING READ
/// THE PAYMENT. A landing close skips `proven`, the runner pays the
/// debt on the CAR (a closed packet's steps are history), and a reader
/// of the proven step saw a skipped step and no pointer to the proof
/// that paid it. The job GET now hands the payment beside that step, as
/// it hands a correction beside the step it corrects (design 4105b020):
/// the reader is given it rather than asked to search job metadata.
#[tokio::test]
async fn the_job_get_hands_a_paid_owed_proof_beside_the_proven_step() {
    let jobs = Arc::new(InMemoryJobs::new());
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential()));
    let id = file(&runner).await;
    let (status, body) = patch(&runner, &id, json!({ PROOF_OWED: "true" })).await;
    assert!(status.is_success(), "{body}");

    // CONTROL: owed and unpaid, the step carries nothing extra.
    let job = get(&runner, &id).await;
    assert!(
        proven_step(&job).get(OWED_PROOF).is_none(),
        "an unpaid debt has no payment to hand: {job}"
    );

    let (status, body) = patch(&runner, &id, payment("proven on the forge")).await;
    assert!(status.is_success(), "the runner pays: {body}");
    let job = get(&runner, &id).await;
    let beside = &proven_step(&job)[OWED_PROOF];
    assert_eq!(
        beside["verified"], "proven on the forge",
        "the proven step carries the payment beside it: {job}"
    );
    assert_eq!(beside["proven_by"], "run-car-probe", "{job}");
    assert_eq!(
        *beside,
        stored(&jobs, &id).await[OWED_PROOF],
        "handed exactly as stored, never re-shaped"
    );
}

/// ADMISSION IS A DOOR TOO. Once a reader is HANDED the payment (above),
/// a packet born carrying one would read as paid by the runner from its
/// first byte: the metadata PATCH and the job PUT judge a move against
/// the stored row, and a create has none. So admission judges the body
/// against an empty row by the same rule — marking a debt stays open to
/// anyone, a payment does not.
#[tokio::test]
async fn a_packet_is_not_admitted_already_paid() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = app_with_jobs(jobs.clone(), None);
    let packet = |metadata: Value| {
        let mut job = Job::new(
            KIND,
            Subject::new("custom", "change-2"),
            "Ship a change",
            "automation:ops-runner",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
        );
        job.metadata = metadata;
        job
    };
    let admit = |job: Job| {
        let app = app.clone();
        async move {
            let resp = app
                .oneshot(
                    Request::post("/api/jobs")
                        .header("content-type", "application/json")
                        .header("x-boss-user", forged_runner())
                        .body(Body::from(serde_json::to_vec(&job).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            read(resp).await
        }
    };
    let mut born_paid = payment("forged at birth");
    born_paid["merged"] = json!("true");
    let born_paid = packet(born_paid);
    let (status, body) = admit(born_paid.clone()).await;
    assert_refused(status, &body, PROOF_OWED);
    assert_refused(status, &body, OWED_PROOF);
    assert!(
        jobs.get_job(&born_paid.id).await.unwrap().is_none(),
        "nothing was admitted"
    );

    // CONTROL: a packet born owing its proof is admitted.
    let (status, body) = admit(packet(json!({ "merged": "true", PROOF_OWED: "true" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
