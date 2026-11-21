//! An annotate write resolves the id a reader actually holds.
//!
//! THE DEFECT (packet cd7b0054), and it is a door that teaches the
//! wrong move rather than an operator being careless. Every surface in
//! this system prints EIGHT characters of an id — `boss orient`, the
//! yard, `boss triage`'s own output, a packet's references, a packet
//! title. Every verb takes eight (`boss triage`, `boss step complete`,
//! `boss prove`, `boss rerail` each document "8+ characters of its
//! id"). `GET /api/jobs/{id}` has resolved eight since c2e6a703.
//!
//! `PATCH /api/jobs/{id}/metadata` — the door CLAUDE.md names as THE
//! way to annotate a packet — demanded thirty-six, and answered
//! `invalid job id`.
//!
//! WHAT THE REFUSAL TAUGHT, measured three times in one day
//! (2026-09-21, on packets dd8120ba, 445c1494 and 5994de6d): a reader
//! holding eight characters, refused, CONSTRUCTED a thirty-six
//! character id by padding — `5994de6d-9f3a-…` — and sent that. All
//! three 404'd, which is the lucky outcome, because a padded uuid
//! cannot collide with a real one. The habit is the unlucky part, and
//! this project already carries the rule it breaks: never write a sha
//! you did not read. A refusal that says only "invalid" names neither
//! the shape it wants nor the route one door over that would have
//! answered.
//!
//! THIS REVERSES A DELIBERATE EARLIER DECISION, and says so out loud.
//! `get_job_by_prefix.rs` recorded: "WRITES ARE DELIBERATELY NOT
//! WIDENED. A prefix that resolves to the wrong job on a PUT/PATCH is
//! a silently-corrupted packet; a write holds the full id it just read
//! back." Both halves turned out not to hold.
//!
//!   - "resolves to the wrong job" CANNOT happen. `resolve_path_job_id`
//!     refuses an ambiguous prefix with 409 rather than picking one —
//!     `an_ambiguous_prefix_is_refused_on_a_write_too` below pins that
//!     on the write path. The silent corruption the decision guarded
//!     against has no route to occur.
//!   - "a write holds the full id it just read back" is false of the
//!     actual readers. They hold what the surface printed, which is
//!     eight. Measured, three times.
//!
//! So the decision traded a failure that cannot happen for one that
//! did, three times in a day, to the same person who had just read the
//! packet describing the trap.
//!
//! THE STEP DOORS ARE WIDENED TOO — their JOB segment (backlog
//! 97f98011). This paragraph used to exclude every door under
//! `/api/jobs/{id}/steps/...` because "nothing prints a step id short
//! ... no measured slip". That reasoning was about the STEP id, and it
//! still holds for it. The JOB segment of a step door is a different
//! currency: it is whatever id the packet page was opened with, and
//! `/jobs/<8 hex>` loads since car cafa573e, so every step surface on
//! that page wrote with the eight characters in its route. Two measured
//! slips in the step-write refusal record: an agent's metadata write on
//! 2026-11-12, and row 27765 on 2026-11-20 — David's ratify of the
//! doc-flatten review 06170f1c, `PATCH /api/jobs/06170f1c/steps/
//! 544e70e3-…/metadata` answered `400 invalid job id` before its body
//! was judged at all. So every step door now resolves the job segment
//! through the same resolver the job doors use: a unique prefix
//! resolves, an ambiguous one is 409, nothing is 404.
//!
//! WHAT IS STILL NOT WIDENED: the STEP id. Nothing prints one short — a
//! caller gets it from a step listing, which carries it in full — so it
//! stays a full uuid, and the step-belongs-to-this-job check stays a
//! 404: a short job id can only ever reach a step of the one packet it
//! resolves to.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// Two packets share the prefix `aaaaaaaa`; one owns `bcbcbcbc`.
/// Controlled ids make ambiguity deterministic — random v4 ids never
/// collide at eight characters in a test-sized set.
const AMBIG_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const AMBIG_B: &str = "aaaaaaaa-2222-4222-8222-222222222222";
const UNIQUE: &str = "bcbcbcbc-3333-4333-8333-333333333333";

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-op",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn job_at(id: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        status: JobStatus::Open,
        metadata: serde_json::json!({ "area": "boss-jobs" }),
        ..Job::new(
            "backlog-item",
            Subject::new("custom", "bosspipeline"),
            title,
            "emp-op",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
        )
    }
}

async fn seed() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    for (id, title) in [
        (AMBIG_A, "first sharer"),
        (AMBIG_B, "second sharer"),
        (UNIQUE, "the unique one"),
    ] {
        jobs.create_job(&job_at(id, title)).await.unwrap();
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

async fn patch_metadata(
    app: &axum::Router,
    id: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn metadata_of(jobs: &InMemoryJobs, id: &str) -> serde_json::Value {
    jobs.get_job(&JobId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the packet is there")
        .metadata
}

/// THE BUG: the annotate door refused the id the reader was holding.
#[tokio::test]
async fn the_annotate_door_takes_the_eight_characters_a_surface_printed() {
    let (app, jobs) = seed().await;

    let (status, body) = patch_metadata(&app, "bcbcbcbc", r#"{"note":"annotated"}"#).await;
    assert!(
        status.is_success(),
        "the prefix every surface prints must reach the door CLAUDE.md names for annotating; \
         {status} {body}"
    );

    let md = metadata_of(&jobs, UNIQUE).await;
    assert_eq!(
        md["note"], "annotated",
        "the write landed on the packet the prefix names"
    );
    assert_eq!(
        md["area"], "boss-jobs",
        "and it MERGED — the existing keys survive, which is the whole contract of this door"
    );
}

/// A hyphenated prefix is still a prefix, the way a reader copies one
/// out of a longer id.
#[tokio::test]
async fn a_longer_hyphenated_prefix_reaches_the_door_too() {
    let (app, jobs) = seed().await;
    let (status, body) = patch_metadata(&app, "bcbcbcbc-3333", r#"{"note":"longer"}"#).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(metadata_of(&jobs, UNIQUE).await["note"], "longer");
}

/// THE HALF THAT MAKES THE WIDENING SAFE, and the reason the earlier
/// decision's stated risk cannot occur: two packets matching means the
/// caller asked a two-answer question, and a write is the last place
/// to guess. Neither packet may move.
#[tokio::test]
async fn an_ambiguous_prefix_is_refused_on_a_write_too() {
    let (app, jobs) = seed().await;

    let (status, _) = patch_metadata(&app, "aaaaaaaa", r#"{"note":"which one?"}"#).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an ambiguous prefix is refused, not resolved to whichever row came back first"
    );

    for id in [AMBIG_A, AMBIG_B] {
        let md = metadata_of(&jobs, id).await;
        assert!(
            md.get("note").is_none(),
            "{id} must not have been written to by a refused, ambiguous write"
        );
    }
}

/// A prefix matching nothing is absent, not malformed — the distinction
/// a caller needs to tell "I mistyped it" from "it is gone".
#[tokio::test]
async fn a_prefix_matching_nothing_is_absent_not_malformed() {
    let (app, _) = seed().await;
    let (status, _) = patch_metadata(&app, "ffffffff", r#"{"note":"nobody"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// THE REFUSAL THAT REMAINS MUST TEACH. Too short to resolve safely is
/// still a 400 — but "invalid job id" is what sent three writes off to
/// construct an id nobody had read. It has to name the shape and the
/// route.
#[tokio::test]
async fn what_is_still_refused_says_what_would_work() {
    let (app, _) = seed().await;

    for garbage in ["not-a-real-id-xyz", "bcb"] {
        let (status, body) = patch_metadata(&app, garbage, r#"{"note":"x"}"#).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{garbage} is not an id or a resolvable prefix"
        );
        let text = body.as_str().unwrap_or_default();
        assert!(
            text.contains('8'),
            "the refusal must name the shape that works — 8 or more characters. \
             `{garbage}` got: {text}"
        );
    }
}

/// THE CONTROL. A full uuid resolves exactly as it always did, on the
/// fast path, with no lookup — without this the tests above would pass
/// against a door that had merely stopped parsing ids.
#[tokio::test]
async fn a_full_uuid_still_takes_the_fast_path() {
    let (app, jobs) = seed().await;
    let (status, body) = patch_metadata(&app, UNIQUE, r#"{"note":"by full id"}"#).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(metadata_of(&jobs, UNIQUE).await["note"], "by full id");
}

/// A BODY WRAPPED IN `metadata` IS REFUSED, naming the mistake (backlog
/// 0727a117). This door merges TOP-LEVEL keys, so `{"metadata": {...}}`
/// stored one key named `metadata` that each later wrapped write replaced,
/// every call answering 204: on 2026-11-05/06 nine builder reports to one
/// conductor session landed that way and were read as two and a half
/// hours of silence, and on 2026-11-07 a report landed the same way
/// again. A wrong shape that answers is the silent failure; it is refused
/// 422, and nothing is written.
#[tokio::test]
async fn a_body_wrapped_in_metadata_is_refused_and_writes_nothing() {
    let (app, jobs) = seed().await;
    let (status, body) =
        patch_metadata(&app, UNIQUE, r#"{"metadata":{"report_1":"swallowed"}}"#).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a wrapped body must be refused, not stored as one nested key: {body}"
    );
    let text = body.to_string();
    assert!(
        text.contains("metadata") && text.contains("top-level"),
        "the refusal names the wrapper and the shape that works: {text}"
    );
    let md = metadata_of(&jobs, UNIQUE).await;
    assert!(md.get("metadata").is_none(), "nothing was written: {md}");

    // Beside an ordinary key it is the same mistake, and still nothing lands.
    let (status, _) = patch_metadata(
        &app,
        UNIQUE,
        r#"{"note":"kept out","metadata":{"report_2":"swallowed"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(metadata_of(&jobs, UNIQUE).await.get("note").is_none());

    // The unwrapped body is the door's contract, and lands.
    let (status, body) = patch_metadata(&app, UNIQUE, r#"{"report_1":"read"}"#).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(metadata_of(&jobs, UNIQUE).await["report_1"], "read");
}

// ---------------------------------------------------------------------
// THE STEP DOORS (backlog 97f98011). The fixture is the shape row 27765
// refused: doc-flatten v6's `review` step, a `sign-off` that declares
// ONE field, `decision`, no `sign_offs_required`, authority
// platform-admin, ready — on the packet whose ratify was refused.
// ---------------------------------------------------------------------

/// The packet David was ratifying (doc-flatten 06170f1c).
const DOC: &str = "06170f1c-f7f7-4b08-8aea-e7860f989b73";
/// Its review step (row 27765's step segment).
const REVIEW: &str = "544e70e3-3949-42f1-8d22-83473c405f3e";
/// A step on AMBIG_A — reachable by a short id only when that id is
/// unique, which `aaaaaaaa` is not.
const AMBIG_STEP: &str = "dddddddd-4444-4444-8444-444444444444";

async fn seed_steps() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    for (id, title) in [
        (AMBIG_A, "first sharer"),
        (AMBIG_B, "second sharer"),
        (UNIQUE, "the unique one"),
    ] {
        jobs.create_job(&job_at(id, title)).await.unwrap();
    }
    let doc = Job {
        id: JobId::from_uuid(Uuid::parse_str(DOC).unwrap()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "doc-flatten",
            Subject::new("custom", "docs"),
            "Flatten the settled design docs",
            "emp-op",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
        )
    };
    jobs.create_job(&doc).await.unwrap();

    let review = Step {
        id: StepId::from_uuid(Uuid::parse_str(REVIEW).unwrap()),
        spec_slug: Some("review".into()),
        status: StepStatus::Ready,
        fields: vec![
            serde_json::from_value(serde_json::json!({
                "name": "decision",
                "field_type": "pending|approved|rejected|changes-requested",
                "required": true,
            }))
            .unwrap(),
        ],
        metadata: serde_json::json!({ "authority_role": "platform-admin" }),
        ..Step::new(doc.id, "sign-off", "Ratify the fold", 5)
    };
    jobs.add_step(&review).await.unwrap();

    let other = Step {
        id: StepId::from_uuid(Uuid::parse_str(AMBIG_STEP).unwrap()),
        spec_slug: Some("work".into()),
        status: StepStatus::Ready,
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(AMBIG_A).unwrap()),
            "task",
            "some work",
            0,
        )
    };
    jobs.add_step(&other).await.unwrap();

    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step_assign(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn step_of(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
}

/// THE BUG, as row 27765 recorded it: the ratify the sign-off surface
/// sends, addressed by the eight characters the page was opened with,
/// answered 400 before its body was judged. Both legs of the ratify —
/// the merge door's decision and the completion PUT — and the step
/// list the surface reads back must take the job's short id.
#[tokio::test]
async fn a_ratify_through_an_eight_character_job_id_records_and_completes() {
    let (app, jobs) = seed_steps().await;

    // The body sign-off.js sends: the decision, its time, and a null
    // comment when the reviewer typed none.
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/06170f1c/steps/{REVIEW}/metadata"),
        serde_json::json!({
            "decision": "approved",
            "decided_at": "2026-11-20T20:34:54Z",
            "comment": null,
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "the merge door must resolve the job segment the page was opened with \
         (row 27765 answered 400 invalid job id here): {status} {body}"
    );
    assert_eq!(
        step_of(&jobs, REVIEW).await.metadata["decision"],
        "approved"
    );

    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/06170f1c/steps/{REVIEW}"),
        serde_json::json!({ "status": "completed" }),
    )
    .await;
    assert!(
        status.is_success(),
        "the completion PUT must resolve the same short id: {status} {body}"
    );

    let review = step_of(&jobs, REVIEW).await;
    assert_eq!(review.status, StepStatus::Completed, "{review:?}");
    assert_eq!(review.metadata["decision"], "approved");

    // The read-back the surfaces make after every write.
    let (status, body) = send(
        &app,
        "GET",
        "/api/jobs/06170f1c/steps",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the step list by short id: {body}");
    let listed = body
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["id"] == REVIEW))
        .unwrap_or_else(|| panic!("the review step is listed: {body}"));
    assert_eq!(listed["status"], "completed");
    assert_eq!(listed["metadata"]["decision"], "approved");
}

/// THE HALF THAT KEEPS IT SAFE: a short job id reaches only steps of
/// the ONE packet it resolves to. Another packet's step under it is the
/// same 404 a full id gets, and nothing moves.
#[tokio::test]
async fn a_short_job_id_cannot_reach_another_packets_step() {
    let (app, jobs) = seed_steps().await;

    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/bcbcbcbc/steps/{REVIEW}/metadata"),
        serde_json::json!({ "decision": "approved" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a step of 06170f1c is not a step of bcbcbcbc: {body}"
    );
    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/bcbcbcbc/steps/{REVIEW}"),
        serde_json::json!({ "status": "completed" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let review = step_of(&jobs, REVIEW).await;
    assert_eq!(review.status, StepStatus::Ready, "nothing moved");
    assert!(review.metadata.get("decision").is_none(), "nothing written");
}

/// An ambiguous job prefix on a step door is refused 409, exactly as on
/// the job doors — never resolved to whichever packet came back first.
#[tokio::test]
async fn an_ambiguous_job_prefix_is_refused_on_a_step_door() {
    let (app, jobs) = seed_steps().await;

    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/aaaaaaaa/steps/{AMBIG_STEP}/metadata"),
        serde_json::json!({ "note": "which one?" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/aaaaaaaa/steps/{AMBIG_STEP}"),
        serde_json::json!({ "status": "completed" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let step = step_of(&jobs, AMBIG_STEP).await;
    assert_eq!(step.status, StepStatus::Ready, "nothing moved");
    assert!(step.metadata.get("note").is_none(), "nothing written");
}

/// The STEP id is not widened: a short step id is still malformed.
#[tokio::test]
async fn the_step_id_stays_a_full_uuid() {
    let (app, _) = seed_steps().await;
    let (status, body) = send(
        &app,
        "PATCH",
        "/api/jobs/06170f1c/steps/544e70e3/metadata",
        serde_json::json!({ "decision": "approved" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
