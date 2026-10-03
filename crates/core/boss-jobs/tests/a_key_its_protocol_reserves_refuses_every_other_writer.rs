//! A key its protocol reserves to one writer refuses every other
//! writer, at every step door that can change it (design f623e425,
//! David 2026-09-25; backlog 6c9183de).
//!
//! THE CLAIM, measured on origin/main 21561380 (2026-09-25) and again at
//! 604ed86f (2026-09-26): an ops-request's `approve` step carries the
//! keys the runner renders on the host — `plan`, `verb`, `host`, `args`,
//! `rendered_plan_sha256` — and any caller with Update on the step could
//! merge them, so the passkey could be asked to sign a plan the runner
//! never rendered (extend c reproduced exactly that: `PLAN wipe`).
//!
//! THE SHAPE TESTED is that step reduced to its keys: every runner key
//! declares `writer = "runner:ops"` on its field, one key (`comment`)
//! declares none, and the packet's `host` is `forge`. The forged caller
//! is the one the review named: the machine door's self-asserted
//! `x-boss-user` spelling the runner's own id with platform-admin, which
//! the policy admits — so a refusal here is the writer rule's, not the
//! policy's. The credentialed caller is a `CredentialedCaller` request
//! extension, set either by a layer standing in for the credential door
//! or, in the last section, by the door itself
//! (`boss_jobs::runner_credential`, reading a slot directory shaped like
//! the Secret mount); a client cannot set an extension, so only a
//! credential the door resolves can reach it.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Step, StepField, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
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

const KIND: &str = "approval-carrier";
const RUNNER_KEYS: [&str; 5] = ["plan", "verb", "host", "args", "rendered_plan_sha256"];

fn field(name: &str, writer: Option<&str>) -> StepField {
    StepField {
        filled_by: Default::default(),
        writer: writer.map(str::to_string),
        ..StepField::new(name, "string")
    }
}

/// The same step with no key reserved: the control for the host rule.
const PLAIN_KIND: &str = "plain-carrier";

fn spec() -> WorkflowSpec {
    let mut fields: Vec<StepField> = RUNNER_KEYS
        .iter()
        .map(|k| field(k, Some("runner:ops")))
        .collect();
    fields.push(field("comment", None));
    spec_of(KIND, fields)
}

fn plain_spec() -> WorkflowSpec {
    spec_of(
        PLAIN_KIND,
        RUNNER_KEYS.iter().map(|k| field(k, None)).collect(),
    )
}

fn spec_of(kind: &str, fields: Vec<StepField>) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        "Carry an approval",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "approve".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Approve the plan".into(),
            authority_role: Some("platform-admin".into()),
            fields,
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

fn runner_credential(host: &str) -> CredentialedCaller {
    CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some(host.into()),
    }
}

/// A fresh store and the router over it.
fn app(caller: Option<CredentialedCaller>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    (app_with_jobs(jobs.clone(), caller), jobs)
}

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes).into_owned()));
    (status, json)
}

/// File the packet for `host`, and return its approve step.
async fn file(app: &Router, jobs: &InMemoryJobs, host: &str) -> Step {
    file_kind(app, jobs, KIND, host).await
}

/// File a packet of `kind` for `host`, and return its approve step.
async fn file_kind(app: &Router, jobs: &InMemoryJobs, kind: &str, host: &str) -> Step {
    let mut job = Job::new(
        kind,
        Subject::new("custom", "disk-1"),
        "Commission a disk",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
    );
    job.metadata = json!({ "host": host, "verb": "commission-a-disk" });
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
    let id = body["id"].as_str().unwrap();
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
    jobs.list_steps(&job_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some("approve"))
        .expect("the approve step")
}

async fn merge(app: &Router, step: &Step, patch: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/metadata",
                    step.job_id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(patch.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn put(app: &Router, step: &Step, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", step.job_id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn stored(jobs: &InMemoryJobs, step: &Step) -> Value {
    jobs.get_step(&step.id).await.unwrap().unwrap().metadata
}

fn assert_refused(status: StatusCode, body: &Value, key: &str) {
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write to `{key}` by anyone but its declared writer must be refused: {body}"
    );
    let refused = body["refused_keys"].as_array().expect("refused_keys");
    assert!(
        refused
            .iter()
            .any(|r| r["key"] == key && r["writer"] == "runner:ops"),
        "the refusal names the key and its declared writer: {body}"
    );
    assert_eq!(
        body["asked_by"], "automation:ops-runner",
        "the refusal names who asked: {body}"
    );
}

/// THE CLAIM, at the merge door: the forged runner cannot plant any of
/// the five runner keys, and the stored step does not move.
#[tokio::test]
async fn the_merge_door_refuses_a_forged_runner_every_reserved_key() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "forge").await;
    let before = stored(&jobs, &step).await;
    for key in RUNNER_KEYS {
        let (status, body) = merge(&app, &step, json!({ key: "PLAN wipe" })).await;
        assert_refused(status, &body, key);
    }
    assert_eq!(stored(&jobs, &step).await, before, "nothing was written");
}

/// The same claim at the step PUT, which used to be the other door that
/// overlaid step keys. Since e39a9d2a (Stage 2's last car) the PUT writes
/// no metadata at all: it refuses the body before the writer rule is
/// reached, naming the merge door — where the rule is asserted above —
/// so the forged plan has one door left, and that door refuses it.
#[tokio::test]
async fn the_step_put_refuses_a_forged_runner_any_metadata_body() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "forge").await;
    let mut md = stored(&jobs, &step).await;
    md["plan"] = json!("PLAN wipe");
    let (status, body) = put(&app, &step, json!({ "metadata": md })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["merge_door"],
        format!("/api/jobs/{}/steps/{}/metadata", step.job_id, step.id),
        "{body}"
    );
    assert_eq!(
        body["hint"],
        boss_jobs::step_metadata_write::METADATA_BODY_HINT,
        "{body}"
    );
    assert!(stored(&jobs, &step).await.get("plan").is_none());
}

/// Deleting a reserved key is a change too: a planted plan must not be
/// replaceable by a removal the runner then has to notice.
#[tokio::test]
async fn deleting_a_reserved_key_is_refused_like_writing_it() {
    let (app, jobs) = app(Some(runner_credential("forge")));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the runner writes its plan: {body}");

    let app_forged = app_with_jobs(jobs.clone(), None);
    let (status, body) = merge(&app_forged, &step, json!({ "plan": null })).await;
    assert_refused(status, &body, "plan");
    assert_eq!(stored(&jobs, &step).await["plan"], "PLAN a");
}

/// CONTROLS. A key with no declared writer is untouched by the rule,
/// and so is an unchanged re-send of a reserved one — a caller sending
/// the stored value back is not writing it. The re-send used to ride a
/// step PUT's whole metadata body; since e39a9d2a the PUT writes no
/// metadata, so it is asserted at the merge door, over a plan the
/// runner really wrote.
#[tokio::test]
async fn undeclared_keys_and_unchanged_re_sends_are_admitted() {
    let jobs = Arc::new(InMemoryJobs::new());
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential("forge")));
    let step = file(&runner, &jobs, "forge").await;
    let (status, body) = merge(&runner, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the runner writes its plan: {body}");

    let app = app_with_jobs(jobs.clone(), None);
    let (status, body) = merge(&app, &step, json!({ "comment": "looks right" })).await;
    assert!(
        status.is_success(),
        "an undeclared key is not reserved: {body}"
    );

    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN a" })).await;
    assert!(
        status.is_success(),
        "sending the stored plan back changes no reserved key: {body}"
    );
    assert_eq!(stored(&jobs, &step).await["plan"], "PLAN a");
}

/// THE WRITER ITSELF: a caller the server resolved from a credential for
/// `runner:ops`, bound to this packet's host, writes every reserved key.
#[tokio::test]
async fn the_declared_writer_writes_its_keys() {
    let (app, jobs) = app(Some(runner_credential("forge")));
    let step = file(&app, &jobs, "forge").await;
    let patch: serde_json::Map<String, Value> = RUNNER_KEYS
        .iter()
        .map(|k| (k.to_string(), json!(format!("{k} as rendered"))))
        .collect();
    let (status, body) = merge(&app, &step, Value::Object(patch)).await;
    assert!(status.is_success(), "the runner writes its keys: {body}");
    assert_eq!(stored(&jobs, &step).await["plan"], "plan as rendered");
}

/// A runner for host h writes only requests whose host is h: the
/// credential of another host's runner is refused, naming both hosts.
#[tokio::test]
async fn another_hosts_runner_is_refused() {
    let (app, jobs) = app(Some(runner_credential("boss-gcp")));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN wipe" })).await;
    assert_refused(status, &body, "plan");
    let why = body["refused_keys"][0]["why"].as_str().unwrap_or_default();
    assert!(
        why.contains("boss-gcp") && why.contains("forge"),
        "the refusal names both hosts: {body}"
    );
}

/// A credential for a different principal is not the declared writer.
#[tokio::test]
async fn a_credential_for_another_principal_is_refused() {
    let (app, jobs) = app(Some(CredentialedCaller {
        principal: "runner:other".into(),
        ..runner_credential("forge")
    }));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN wipe" })).await;
    assert_refused(status, &body, "plan");
}

/// THE MERGE DOOR'S RACE (review S3 of car 1e603cfd, 2026-09-26). The
/// door judges a patch against the row it READ and the adapter applies
/// it to the row as it STANDS, so a non-writer that re-sends the plan it
/// read — "unchanged", so admitted — reverted the runner's newer plan
/// when the runner wrote between the two. The shape tested is that
/// window: the runner's write lands straight after the forged caller's
/// read (`merge_after_next_read`), and the forged caller re-sends the
/// old plan beside a key it may write. The old plan must not come back,
/// and the key it may write must land.
#[tokio::test]
async fn an_unchanged_re_send_cannot_revert_the_writers_newer_value() {
    let jobs = Arc::new(InMemoryJobs::new());
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential("forge")));
    let step = file(&runner, &jobs, "forge").await;
    let (status, body) = merge(&runner, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the runner writes its plan: {body}");

    let mut newer = serde_json::Map::new();
    newer.insert("plan".into(), json!("PLAN b"));
    jobs.merge_after_next_read(&step.id, newer);
    let forged = app_with_jobs(jobs.clone(), None);
    let (status, body) = merge(
        &forged,
        &step,
        json!({ "plan": "PLAN a", "comment": "looks right" }),
    )
    .await;
    assert!(
        status.is_success(),
        "an unchanged re-send is admitted: {body}"
    );
    let md = stored(&jobs, &step).await;
    assert_eq!(
        md["plan"], "PLAN b",
        "the runner's newer plan survives a stale re-send"
    );
    assert_eq!(md["comment"], "looks right", "the undeclared key lands");
}

/// THE WRITER RACING ITSELF (review of car f3365343, 2026-09-28,
/// follow-up a). The S3 strip above dropped an unchanged re-send from
/// EVERY caller, the declared writer included: a runner pass that read
/// `PLAN a`, lost a race to another pass writing `PLAN b`, and then sent
/// `PLAN a` was answered 204 and its write thrown away — the one caller
/// whose value the key exists to hold. The strip guards the writer
/// AGAINST other callers; the writer's own write is the record, so it
/// lands as sent.
#[tokio::test]
async fn the_declared_writers_own_re_send_is_applied_not_dropped() {
    let jobs = Arc::new(InMemoryJobs::new());
    let runner = app_with_jobs(jobs.clone(), Some(runner_credential("forge")));
    let step = file(&runner, &jobs, "forge").await;
    let (status, body) = merge(&runner, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the runner writes its plan: {body}");

    let mut newer = serde_json::Map::new();
    newer.insert("plan".into(), json!("PLAN b"));
    jobs.merge_after_next_read(&step.id, newer);
    let (status, body) = merge(&runner, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the writer's re-send lands: {body}");
    assert_eq!(
        stored(&jobs, &step).await["plan"],
        "PLAN a",
        "the declared writer's own write is applied as sent, never dropped as unchanged"
    );
}

async fn patch_job(app: &Router, job_id: &str, patch: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{job_id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(patch.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn put_job(app: &Router, job: &Job) -> (StatusCode, Value) {
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

async fn stored_job(jobs: &InMemoryJobs, step: &Step) -> Job {
    jobs.get_job(&step.job_id).await.unwrap().unwrap()
}

/// THE HOST A CREDENTIAL IS BOUND TO IS FIXED AT ADMISSION (review S2 of
/// car 1e603cfd, 2026-09-26). A host-bound credential writes only a
/// packet whose job-metadata `host` is its host — and that `host` was
/// PATCHable by any caller with Update on the job, so a forge runner
/// could author a boss-gcp request's plan by flipping `host` to forge,
/// writing, and flipping it back. On a packet whose steps declare a
/// writer, both job doors refuse a change to `host` — a new value or a
/// removal — naming the key; an unchanged re-send lands.
#[tokio::test]
async fn the_host_a_writer_is_bound_to_is_fixed_after_admission() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "boss-gcp").await;
    let id = step.job_id.to_string();
    for patch in [json!({ "host": "forge" }), json!({ "host": null })] {
        let (status, body) = patch_job(&app, &id, patch.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{patch}: {body}");
        assert!(body["refused_keys"] == json!(["host"]), "{body}");
    }
    let (status, body) = patch_job(&app, &id, json!({ "host": "boss-gcp", "note": "x" })).await;
    assert!(status.is_success(), "an unchanged re-send lands: {body}");

    let mut job = stored_job(&jobs, &step).await;
    job.metadata["host"] = json!("forge");
    let (status, body) = put_job(&app, &job).await;
    assert_eq!(status, StatusCode::CONFLICT, "the job PUT too: {body}");
    assert!(body["refused_keys"] == json!(["host"]), "{body}");
    let now = stored_job(&jobs, &step).await;
    assert_eq!(now.metadata["host"], "boss-gcp");
    assert_eq!(now.metadata["note"], "x");
}

/// CONTROL: a packet whose protocol declares no writer is untouched by
/// the rule — its `host` is an ordinary key, as it always was.
#[tokio::test]
async fn a_packet_that_declares_no_writer_keeps_an_ordinary_host() {
    let (app, jobs) = app(None);
    let step = file_kind(&app, &jobs, PLAIN_KIND, "boss-gcp").await;
    let (status, body) =
        patch_job(&app, &step.job_id.to_string(), json!({ "host": "forge" })).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(stored_job(&jobs, &step).await.metadata["host"], "forge");
}

// ---------------------------------------------------------------------
// THE CREDENTIAL DOOR ITSELF (design f623e425 Q1, option A, as David
// decided it 2026-09-25). The tests above stand a layer in for the door;
// these mount the real one, `boss_jobs::runner_credential::mount`, over a
// slot directory laid out the way kubelet lays out a mounted Secret: the
// values under `..data/`, and one symlink per key at the top. The forged
// caller is the same — the runner's own id in `x-boss-user`, which the
// policy admits — now with or without a credential header beside it.
// ---------------------------------------------------------------------

/// Fake, fixture-only credential values, shaped like the 32 random bytes
/// base64url the door is written for.
const FORGE_RC: &str = "rcFORGEforgeFORGEforgeFORGEforgeFORGEforge0";
const GCP_RC: &str = "rcGCPgcpGCPgcpGCPgcpGCPgcpGCPgcpGCPgcpGCP00";

/// The slot directory as a Secret mount presents it.
fn slot_dir(slots: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = boss_testing::scratch_dir("runner-credential-door");
    let data = dir.join("..data");
    boss_testing::create_dir(&data);
    for (name, value) in slots {
        boss_testing::write_file(&data.join(name), value);
        std::os::unix::fs::symlink(format!("..data/{name}"), dir.join(name)).unwrap();
    }
    dir
}

/// The router over `jobs` behind the real credential door, reading `dir`.
fn door_app(jobs: Arc<InMemoryJobs>, dir: std::path::PathBuf) -> Router {
    boss_jobs::runner_credential::mount(app_with_jobs(jobs, None), dir)
}

async fn merge_presenting(
    app: &Router,
    step: &Step,
    patch: Value,
    credential: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/api/jobs/{}/steps/{}/metadata",
            step.job_id, step.id
        ))
        .header("content-type", "application/json")
        .header("x-boss-user", forged_runner());
    if let Some(c) = credential {
        req = req.header(boss_jobs::runner_credential::HEADER, c);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(patch.to_string())).unwrap())
        .await
        .unwrap();
    read(resp).await
}

async fn whoami(app: &Router, credential: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::get(boss_jobs::runner_credential::WHOAMI_PATH)
        .header("x-boss-user", forged_runner());
    if let Some(c) = credential {
        req = req.header(boss_jobs::runner_credential::HEADER, c);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// THE CLAIM THE DOOR EXISTS FOR: a runner that presents its host's
/// credential writes that host's reserved keys, and nothing else it
/// sends — the `x-boss-user` id least of all — is what admitted it.
#[tokio::test]
async fn a_presented_runner_credential_writes_its_own_hosts_keys() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(jobs.clone(), slot_dir(&[("forge.current", FORGE_RC)]));
    let step = file(&app, &jobs, "forge").await;
    for key in RUNNER_KEYS {
        let (status, body) =
            merge_presenting(&app, &step, json!({ key: "rendered" }), Some(FORGE_RC)).await;
        assert!(status.is_success(), "{key}: {body}");
    }
    let md = stored(&jobs, &step).await;
    assert!(RUNNER_KEYS.iter().all(|k| md[*k] == "rendered"), "{md}");
}

/// THE FORGERY THE REVIEW NAMED, at the real door: the runner's id and
/// no credential, or a credential no slot holds, is refused every
/// reserved key — and the door itself refuses NOTHING (DR rule
/// 62dac114): a key no protocol reserves lands with a wrong credential
/// exactly as it does with none.
#[tokio::test]
async fn the_forged_runner_is_refused_at_the_real_door_and_nothing_else_is() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(jobs.clone(), slot_dir(&[("forge.current", FORGE_RC)]));
    let step = file(&app, &jobs, "forge").await;
    for credential in [None, Some(GCP_RC), Some(""), Some("rcFORGE")] {
        let (status, body) =
            merge_presenting(&app, &step, json!({ "plan": "PLAN wipe" }), credential).await;
        assert_refused(status, &body, "plan");
        assert!(body["credential"].is_null(), "{credential:?}: {body}");
        let (status, body) =
            merge_presenting(&app, &step, json!({ "comment": "fine" }), credential).await;
        assert!(status.is_success(), "{credential:?}: {body}");
    }
    assert!(stored(&jobs, &step).await.get("plan").is_none());
}

#[tokio::test]
async fn a_forge_credential_cannot_write_a_boss_gcp_request() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(
        jobs.clone(),
        slot_dir(&[("forge.current", FORGE_RC), ("boss-gcp.current", GCP_RC)]),
    );
    let step = file(&app, &jobs, "boss-gcp").await;
    let (status, body) =
        merge_presenting(&app, &step, json!({ "plan": "PLAN wipe" }), Some(FORGE_RC)).await;
    assert_refused(status, &body, "plan");
    assert_eq!(body["credential"]["host"], "forge", "{body}");
    let (status, body) =
        merge_presenting(&app, &step, json!({ "plan": "PLAN gcp" }), Some(GCP_RC)).await;
    assert!(status.is_success(), "{body}");
}

/// A ROTATION NEVER LEAVES THE RUNNER WITHOUT A CREDENTIAL: `next` is
/// accepted before the host holds it and `previous` after it has moved
/// on, the machine token's three slots (design 6805c764).
#[tokio::test]
async fn every_slot_of_a_rotation_resolves() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(
        jobs.clone(),
        slot_dir(&[("forge.next", FORGE_RC), ("forge.previous", GCP_RC)]),
    );
    let step = file(&app, &jobs, "forge").await;
    for (credential, slot) in [(FORGE_RC, "next"), (GCP_RC, "previous")] {
        let (status, body) =
            merge_presenting(&app, &step, json!({ "plan": slot }), Some(credential)).await;
        assert!(status.is_success(), "{slot}: {body}");
        let (_, who) = whoami(&app, Some(credential)).await;
        let who: Value = serde_json::from_str(&who).unwrap();
        assert_eq!(who["slot"], slot, "{who}");
    }
}

/// A value two hosts' slots both hold names no one host, so it resolves
/// to neither — a broker fault is read as no credential, never as the
/// first host a directory listing happened to reach.
///
/// The packet is filed on `boss-gcp` BECAUSE it sorts first: filed on
/// `forge`, a door that picked the first host would resolve to boss-gcp
/// and still be refused, so the test could not tell it from this one
/// (review of 8e5de104, finding 2; backlog 1e50e66b). The whoami read
/// says the same thing without leaning on the order at all.
#[tokio::test]
async fn a_value_two_hosts_hold_resolves_to_neither() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(
        jobs.clone(),
        slot_dir(&[("forge.current", FORGE_RC), ("boss-gcp.previous", FORGE_RC)]),
    );
    let step = file(&app, &jobs, "boss-gcp").await;
    let (status, body) =
        merge_presenting(&app, &step, json!({ "plan": "PLAN a" }), Some(FORGE_RC)).await;
    assert_refused(status, &body, "plan");
    let (_, who) = whoami(&app, Some(FORGE_RC)).await;
    let who: Value = serde_json::from_str(&who).unwrap();
    assert_eq!(who["resolved"], false, "{who}");
}

/// What a handler behind the door can see of the credential: whether the
/// header reached it, how many copies, and which host the door resolved
/// — never the value itself.
async fn echo_the_header(
    headers: axum::http::HeaderMap,
    caller: Option<axum::Extension<CredentialedCaller>>,
) -> axum::Json<Value> {
    let header = boss_jobs::runner_credential::HEADER;
    axum::Json(json!({
        "header_present": headers.contains_key(header),
        "copies": headers.get_all(header).iter().count(),
        "resolved_host": caller.map(|axum::Extension(c)| c.host),
    }))
}

/// THE STRIP (review of 8e5de104, finding 1; backlog 1e50e66b): the module
/// promises that no handler, log or proxy downstream of the door ever
/// holds the value, and deleting `headers_mut().remove(HEADER)` survived
/// every test. Here a handler behind the real door echoes whether the
/// header reached it, for every way the door can read a presented value:
/// resolved, unmatched, ambiguous and multi-valued. The resolved host is
/// echoed too, so a case that stops resolving cannot pass for a strip.
#[tokio::test]
async fn no_handler_behind_the_door_ever_sees_the_credential_header() {
    const SHARED: &str = "rcSHAREDsharedSHAREDsharedSHAREDsharedSHAR";
    let app = boss_jobs::runner_credential::mount(
        Router::new().route("/api/jobs/echo", axum::routing::get(echo_the_header)),
        slot_dir(&[
            ("forge.current", FORGE_RC),
            ("boss-gcp.current", SHARED),
            ("w-1.current", SHARED),
        ]),
    );
    let cases: [(&str, &[&str], Value); 4] = [
        ("resolved", &[FORGE_RC], json!("forge")),
        ("unmatched", &[GCP_RC], Value::Null),
        ("ambiguous", &[SHARED], Value::Null),
        ("multi-valued", &[FORGE_RC, FORGE_RC], Value::Null),
    ];
    for (case, presented, host) in cases {
        let mut req = Request::get("/api/jobs/echo").header("x-boss-user", forged_runner());
        for value in presented {
            req = req.header(boss_jobs::runner_credential::HEADER, *value);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (status, seen) = read(resp).await;
        assert_eq!(status, StatusCode::OK, "{case}: {seen}");
        assert_eq!(
            seen,
            json!({ "header_present": false, "copies": 0, "resolved_host": host }),
            "{case}: the handler behind the door saw the credential header"
        );
    }
}

/// The broker's verify-by-effect read: what the door made of the header
/// presented, and never the value — nor anything derived from it.
#[tokio::test]
async fn whoami_names_the_resolution_and_never_the_value() {
    let jobs = Arc::new(InMemoryJobs::new());
    let app = door_app(jobs, slot_dir(&[("forge.current", FORGE_RC)]));
    let (status, body) = whoami(&app, Some(FORGE_RC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let who: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        who,
        json!({
            "resolved": true,
            "principal": "runner:ops",
            "actor_id": "automation:ops-runner",
            "host": "forge",
            "slot": "current",
        })
    );
    assert!(!body.contains(FORGE_RC), "{body}");
    for credential in [None, Some(GCP_RC)] {
        let (status, body) = whoami(&app, credential).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let who: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(who["resolved"], false, "{credential:?}: {who}");
        assert!(!body.contains(GCP_RC), "{body}");
    }
}

/// TODAY'S STATE, until the broker's Secret is mounted: no slot
/// directory at all. Every caller reads as uncredentialed, which is what
/// every caller was before this door — nothing is refused that was not.
#[tokio::test]
async fn with_no_slot_directory_every_caller_is_as_before() {
    let jobs = Arc::new(InMemoryJobs::new());
    let missing = boss_testing::scratch_dir("runner-credential-absent").join("not-mounted");
    let app = door_app(jobs.clone(), missing);
    let step = file(&app, &jobs, "forge").await;
    let (status, body) =
        merge_presenting(&app, &step, json!({ "plan": "PLAN a" }), Some(FORGE_RC)).await;
    assert_refused(status, &body, "plan");
    let (status, body) =
        merge_presenting(&app, &step, json!({ "comment": "x" }), Some(FORGE_RC)).await;
    assert!(status.is_success(), "{body}");
}

/// The router over `jobs`, and — when `caller` is given — a layer that
/// inserts it as the server-resolved credential, standing in for the
/// credential door (the tests above mount the real one). Over an existing store,
/// so one test can write as the credentialed runner and then try the
/// forged caller on the same row.
fn app_with_jobs(jobs: Arc<InMemoryJobs>, caller: Option<CredentialedCaller>) -> Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).unwrap();
    kinds.seed(plain_spec()).unwrap();
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
