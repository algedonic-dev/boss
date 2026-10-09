//! A step's `assurance_required` holds on EVERY path that completes it.
//!
//! THE DEFECT (backlog 148549c5), measured live on packet d5efbb3c,
//! 2026-09-22. The first presence-assured step in the system was
//! completed with no passkey ceremony, no stamp and no refusal:
//!
//! ```text
//!   step `approve`
//!     assurance_required = presence     <- still declared
//!     status             = completed
//!     completed_by       = emp-david
//!     sign_offs          = []           <- no stamp at all
//! ```
//!
//! `execute` then went ready and the host ran the verb. The whole
//! guarantee of design 17835005 — that a destructive ops verb cannot
//! run until a passkey has signed the rendered plan — rests on that
//! step, and an ordinary `PUT /api/jobs/{id}/steps/{step_id}` walked
//! past it.
//!
//! WHY, counted rather than described: the entire check lived in
//! `post_step_sign_off`. Inside `update_step` the string "assurance"
//! appeared ZERO times. The control was OPT-IN — it applied on the
//! path a step reaches only when it also declares a required sign-off
//! role, and not on the path everything else uses.
//!
//! One function below the hole the code already said the right thing:
//! "NO BYPASS, which is the point David settled in Q3: an assurance
//! level with a bypass is a comment, not a control." True of the
//! sign-off path; false of the system.
//!
//! WHAT THE FIX IS, and what it deliberately is NOT. `assurance_required`
//! is a property of the STEP, so every path that completes one owes it
//! the same refusal — one judgement, called from both (§9a). It is NOT
//! a refusal of every write: a metadata write to a step that stays
//! ready needs no ceremony, and refusing those would break the filer
//! that puts the plan on the step in the first place. The line is
//! COMPLETION.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{
    Assurance, Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject,
};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-00000000c001";
const GUARDED: &str = "00000000-0000-0000-0000-00000000d001";
const ORDINARY: &str = "00000000-0000-0000-0000-00000000d002";

fn operator() -> User {
    User {
        id: "emp-david".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    )
}

fn step(id: &str, assurance: Option<Assurance>) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        spec_slug: Some("approve".into()),
        assignee_id: Some("emp-david".into()),
        status: StepStatus::Ready,
        assurance_required: assurance,
        metadata: serde_json::json!({ "plan": "{\"verb\":\"commission-a-disk\"}" }),
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
            "generic",
            "Approve the plan",
            1,
        )
    }
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    jobs.create_job(&Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        workflow_version: 2,
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "ops-request",
            Subject::new("custom", "forge"),
            "df on forge",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        )
    })
    .await
    .unwrap();
    jobs.add_step(&step(GUARDED, Some(Assurance::Presence)))
        .await
        .unwrap();
    jobs.add_step(&step(ORDINARY, None)).await.unwrap();
    (router(state), jobs)
}

async fn put(app: &Router, step_id: &str, body: &str) -> (StatusCode, String) {
    send(app, "PUT", format!("/api/jobs/{JOB}/steps/{step_id}"), body).await
}

/// The step merge door — the one metadata write there is, since the PUT
/// writes none (e39a9d2a).
async fn merge(app: &Router, step_id: &str, body: &str) -> (StatusCode, String) {
    let uri = format!("/api/jobs/{JOB}/steps/{step_id}/metadata");
    send(app, "PATCH", uri, body).await
}

async fn send(app: &Router, method: &str, uri: String, body: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&operator()).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn status_of(jobs: &InMemoryJobs, id: &str) -> StepStatus {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
        .status
}

/// THE BUG, in the shape it was measured: a status flip completed a
/// presence-required step with no ceremony.
#[tokio::test]
async fn a_plain_put_cannot_complete_a_presence_required_step() {
    let (app, jobs) = seed().await;

    let (status, body) = put(&app, GUARDED, r#"{"status":"completed"}"#).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a step declaring presence must refuse a completion carrying none — this is the \
         gate a destructive ops verb waits behind; body: {body}"
    );
    assert!(
        body.contains("assurance"),
        "the refusal must name what is missing, the way the sign-off path does: {body}"
    );
    assert_eq!(
        status_of(&jobs, GUARDED).await,
        StepStatus::Ready,
        "the refused write must NOT have completed the step"
    );
}

/// THE CONTROL that keeps the fix from being "refuse everything": an
/// ordinary step, declaring no assurance, still completes by PUT. That
/// is how almost every step in the system is completed — the
/// dispatcher, the conductor, `boss step complete`, the page march —
/// and breaking it would stop the pipeline.
#[tokio::test]
async fn an_ordinary_step_still_completes_by_put() {
    let (app, jobs) = seed().await;
    let (status, body) = put(&app, ORDINARY, r#"{"status":"completed"}"#).await;
    assert!(
        status.is_success(),
        "a step demanding nothing must still complete normally: {status} {body}"
    );
    assert_eq!(status_of(&jobs, ORDINARY).await, StepStatus::Completed);
}

/// THE SECOND CONTROL, and the one that keeps the filer working: a
/// write that does NOT complete the step is untouched. The plan is put
/// onto the approve step by an ordinary metadata write before anyone
/// signs it; refusing that would make the guarded step unusable rather
/// than guarded. That write goes through the merge door: the PUT writes
/// no metadata since e39a9d2a.
#[tokio::test]
async fn a_write_that_does_not_complete_the_step_needs_no_assurance() {
    let (app, jobs) = seed().await;
    let (status, body) = merge(
        &app,
        GUARDED,
        r#"{"plan":"{\"verb\":\"df\"}","note":"re-rendered"}"#,
    )
    .await;
    assert!(
        status.is_success(),
        "a metadata write to a step that stays ready needs no ceremony: {status} {body}"
    );
    assert_eq!(
        status_of(&jobs, GUARDED).await,
        StepStatus::Ready,
        "and it leaves the step ready"
    );
}

/// A SKIP IS A COMPLETION TOO, as far as the downstream predicate is
/// concerned: `execute` waits on `steps.approve.done`, and a skipped
/// step is done. So the guard has to cover it, or the bypass simply
/// moves one word over.
#[tokio::test]
async fn a_skip_cannot_walk_past_the_requirement_either() {
    let (app, jobs) = seed().await;
    let (status, body) = put(&app, GUARDED, r#"{"status":"skipped"}"#).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "skipping a presence-required step is completing it by another name; body: {body}"
    );
    assert_eq!(status_of(&jobs, GUARDED).await, StepStatus::Ready);
}

// ---------------------------------------------------------------------
// A STAMPED PRESENCE STEP COMPLETES ON ITS LIVE STAMPS (design 1ce67f7e,
// decided 2026-10-07; backlog 570c66e9).
//
// WHAT WAS TRUE BEFORE. The judgement above read assurance off the
// completing REQUEST alone and never consulted the stamps the step
// held, so a step a passkey had just stamped refused a bare completion
// 422. The surfaces hid it by keeping the stamp's ticket client-side
// and sending it again (`presenceTicketHeld`, sign-off.js) — a
// credential held between two requests, a two-minute race, and a
// failure whenever the stamp and the completion happened on different
// surfaces.
//
// THE RULE. A presence step that declares sign-off roles leaves the
// open states when EVERY required role holds a live stamp a passkey
// produced over the step's current shape, none older than
// `PRESENCE_STAMP_COMPLETES_FOR_HOURS`. The completing request then
// needs a session only, and may come from any actor the existing
// step-update policy admits. A presence step with NO roles keeps the
// rule above: its completion carries the ticket, because that
// completion is the only act.
//
// EVERY DOOR, ONE ANSWER (§9a). Two requests can move a step to
// completed: the status PUT and `POST .../complete-if`. Both run
// `update_step_with_condition`, and every table below is driven through
// both. `POST .../steps` refuses a presence step born resolved
// (a_posted_step_carries_no_evidence.rs), the sign-off door completes
// nothing, and `boss step complete`, the dispatcher's handlers and the
// conductor all send the status PUT.
// ---------------------------------------------------------------------

use boss_core::job::{PRESENCE_STAMP_COMPLETES_FOR_HOURS, SignOffStamp};
use boss_core::presence::PresenceTicket;
use boss_jobs::http::PresenceKey;
use chrono::{DateTime, Duration, Utc};

const STAMPED_JOB: &str = "00000000-0000-0000-0000-00000000c002";
const STAMPED: &str = "00000000-0000-0000-0000-00000000d101";
const NO_ROLES: &str = "00000000-0000-0000-0000-00000000d103";
const OTHER: &str = "00000000-0000-0000-0000-00000000d104";
const GATEWAY_KEY: &[u8] = b"presence-test-key-0123456789abcdef";

/// The admin of the decision's own example ("think about an admin
/// grabbing 5 sign-offs"): holds step-update, signed nothing.
fn admin() -> User {
    User {
        id: "emp-ada".to_string(),
        ..operator()
    }
}

/// An automation: `complete-if` is the door automations use.
fn automation() -> User {
    User {
        id: "automation:observer".to_string(),
        access_tier: AccessTier::User,
        ..operator()
    }
}

/// The people registry: David and Ada are active employees; nobody else.
struct Roster;

#[async_trait::async_trait]
impl boss_jobs::owner_resolution::RosterLookup for Roster {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(vec!["emp-david".to_string()])
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-david" || id == "emp-ada")
    }
}

fn stamped_step(id: &str, roles: &[&str]) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        spec_slug: Some("approve".into()),
        assignee_id: None,
        status: StepStatus::Ready,
        assurance_required: Some(Assurance::Presence),
        sign_offs_required: roles.iter().map(|r| r.to_string()).collect(),
        metadata: serde_json::json!({ "plan": "{\"verb\":\"wipe-a-disk\"}" }),
        ..Step::new(
            JobId::from_uuid(Uuid::parse_str(STAMPED_JOB).unwrap()),
            "task",
            "Approve the plan",
            1,
        )
    }
}

/// A stamp as the sign-off door writes one for a verified ticket.
fn stamp(step: &Step, role: &str, at: DateTime<Utc>) -> SignOffStamp {
    SignOffStamp {
        authority_id: "emp-david".into(),
        role: role.into(),
        stamped_at: at,
        shape_hash: step.shape_hash(),
        assurance: Assurance::Presence,
        presence_nonce: Some(format!("nonce-{role}-{}", at.timestamp_micros())),
        voided_at: None,
        voided_by_event: None,
    }
}

fn hours_ago(h: i64) -> DateTime<Utc> {
    Utc::now() - Duration::hours(h)
}

/// The jobs API with the gateway's key, holding `steps` on one packet.
async fn seed_stamped(steps: Vec<Step>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:platform-admin"),
                Scope::All,
            )
            .build(),
    );
    let state = JobsApiState {
        presence_key: Some(Arc::new(PresenceKey::fixed(GATEWAY_KEY.to_vec()))),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    jobs.create_job(&Job {
        id: JobId::from_uuid(Uuid::parse_str(STAMPED_JOB).unwrap()),
        workflow_version: 2,
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "ops-request",
            Subject::new("custom", "forge"),
            "wipe a disk on forge",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
        )
    })
    .await
    .unwrap();
    for s in &steps {
        jobs.add_step(s).await.unwrap();
    }
    (router(state), jobs)
}

/// A ticket the gateway would mint for `who` over `step` as it stands.
fn ticket_for(step: &Step, who: &str, nonce: &str) -> String {
    PresenceTicket {
        i: who.into(),
        s: step.id.to_string(),
        h: step.shape_hash(),
        n: nonce.into(),
        e: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs")
}

async fn call(
    app: &Router,
    who: &User,
    method: &str,
    uri: String,
    presence: Option<&str>,
    body: String,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(who).unwrap());
    if let Some(p) = presence {
        req = req.header("x-boss-presence", p);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The two requests that can move a step to completed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Door {
    StatusPut,
    CompleteIf,
}

const DOORS: [Door; 2] = [Door::StatusPut, Door::CompleteIf];

/// Complete `id` through `door`, as an actor who signed nothing, with
/// whatever header the case presents and NO content change.
async fn complete_through(
    app: &Router,
    door: Door,
    id: &str,
    presence: Option<&str>,
) -> (StatusCode, String) {
    match door {
        Door::StatusPut => {
            call(
                app,
                &admin(),
                "PUT",
                format!("/api/jobs/{STAMPED_JOB}/steps/{id}"),
                presence,
                r#"{"status":"completed"}"#.into(),
            )
            .await
        }
        Door::CompleteIf => {
            call(
                app,
                &automation(),
                "POST",
                format!("/api/jobs/{STAMPED_JOB}/steps/{id}/complete-if"),
                presence,
                serde_json::json!({
                    "operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd",
                    "expected": {"status": "ready", "assignee_id": null},
                    "evidence": {},
                })
                .to_string(),
            )
            .await
        }
    }
}

async fn stored(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
}

/// THE RULE, through the real doors end to end: a passkey ticket stamps
/// the step at the sign-off door, and a DIFFERENT actor then completes it
/// with a bare status PUT — no header, no ceremony of their own. The
/// record says what the completion stood on.
#[tokio::test]
async fn a_stamped_presence_step_is_completed_by_another_actor_with_no_ticket() {
    let step = stamped_step(STAMPED, &["platform-admin"]);
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let sign_offs = format!("/api/jobs/{STAMPED_JOB}/steps/{STAMPED}/sign-offs");
    let role = r#"{"role":"platform-admin"}"#;

    // WRITING the stamp still takes the ticket: this car changes only
    // what completing needs.
    let (status, body) = call(
        &app,
        &operator(),
        "POST",
        sign_offs.clone(),
        None,
        role.into(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a presence stamp is still written only on a verified ticket: {body}"
    );
    assert!(stored(&jobs, STAMPED).await.sign_offs.is_empty());

    let t = ticket_for(&step, "emp-david", "ceremony-nonce-1");
    let (status, body) = call(&app, &operator(), "POST", sign_offs, Some(&t), role.into()).await;
    assert_eq!(status, StatusCode::OK, "the ticketed stamp lands: {body}");

    let (status, body) = complete_through(&app, Door::StatusPut, STAMPED, None).await;
    assert!(
        status.is_success(),
        "every required role holds a live presence stamp over this content, so a bare \
         completion by another actor lands: {status} {body}"
    );
    let done = stored(&jobs, STAMPED).await;
    assert_eq!(done.status, StepStatus::Completed);
    assert_eq!(
        done.completed_by.as_ref().map(|a| a.to_string()).as_deref(),
        Some("emp-ada"),
        "the completion is credited to the actor that sent it, not to the signer"
    );

    // PROVENANCE: the log says how this completion was assured.
    let events = jobs.recorded_events();
    let completed = events
        .iter()
        .find(|e| e.kind == "jobs.step.completed")
        .expect("the completion marker was recorded");
    let assured = &completed.payload["assured"];
    assert_eq!(assured["required"], "presence", "{assured}");
    assert_eq!(assured["by"], "stamps", "{assured}");
    assert_eq!(assured["ticket_presented"], false, "{assured}");
    assert_eq!(assured["stamps"][0]["role"], "platform-admin", "{assured}");
    assert_eq!(
        assured["stamps"][0]["authority_id"], "emp-david",
        "{assured}"
    );
    assert_eq!(
        assured["stamps"][0]["presence_nonce"], "ceremony-nonce-1",
        "{assured}"
    );
}

/// EVERY DOOR ANSWERS ALIKE on a step whose stamps carry it.
#[tokio::test]
async fn stamps_that_carry_the_step_complete_it_through_every_door() {
    for door in DOORS {
        let mut step = stamped_step(STAMPED, &["platform-admin", "security"]);
        // 71 hours: inside the bound by its declared unit (a bound read
        // in minutes or seconds refuses this).
        step.sign_offs = vec![
            stamp(&step, "platform-admin", hours_ago(71)),
            stamp(&step, "security", hours_ago(1)),
        ];
        let (app, jobs) = seed_stamped(vec![step]).await;
        let (status, body) = complete_through(&app, door, STAMPED, None).await;
        assert!(status.is_success(), "{door:?}: {status} {body}");
        assert_eq!(
            stored(&jobs, STAMPED).await.status,
            StepStatus::Completed,
            "{door:?}"
        );
    }
}

/// What a case's stamps look like, and the reason the refusal must give.
struct Short {
    name: &'static str,
    roles: &'static [&'static str],
    stamps: fn(&Step) -> Vec<SignOffStamp>,
    says: &'static [&'static str],
}

fn shortfalls() -> Vec<Short> {
    vec![
        Short {
            name: "no stamp at all",
            roles: &["platform-admin"],
            stamps: |_| Vec::new(),
            says: &["platform-admin", "no live presence stamp"],
        },
        Short {
            name: "a stamp over a DIFFERENT shape (the content changed after it)",
            roles: &["platform-admin"],
            stamps: |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.shape_hash = boss_core::job::step_shape_hash(
                    &s.title,
                    &serde_json::json!({ "plan": "{\"verb\":\"df\"}" }),
                );
                vec![st]
            },
            says: &["platform-admin", "no live presence stamp"],
        },
        Short {
            name: "a VOIDED stamp on this very shape (A-B-A)",
            roles: &["platform-admin"],
            stamps: |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.voided_at = Some(hours_ago(0));
                st.voided_by_event = Some(Uuid::new_v4());
                vec![st]
            },
            says: &["platform-admin", "no live presence stamp"],
        },
        Short {
            name: "a live stamp a SESSION wrote",
            roles: &["platform-admin"],
            stamps: |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.assurance = Assurance::Session;
                st.presence_nonce = None;
                vec![st]
            },
            says: &["platform-admin", "emp-david", "session"],
        },
        Short {
            name: "ONE of two required roles",
            roles: &["platform-admin", "security"],
            stamps: |s| vec![stamp(s, "platform-admin", hours_ago(1))],
            says: &["security", "no live presence stamp"],
        },
        Short {
            name: "the other one of two required roles",
            roles: &["platform-admin", "security"],
            stamps: |s| vec![stamp(s, "security", hours_ago(1))],
            says: &["platform-admin", "no live presence stamp"],
        },
        Short {
            name: "a stamp of a role the step does not require",
            roles: &["platform-admin"],
            stamps: |s| vec![stamp(s, "security", hours_ago(1))],
            says: &["platform-admin", "no live presence stamp"],
        },
        Short {
            // 73 hours: outside the bound by its declared unit (a bound
            // read in days, or removed, completes this).
            name: "a stamp 73 hours old",
            roles: &["platform-admin"],
            stamps: |s| vec![stamp(s, "platform-admin", hours_ago(73))],
            says: &["platform-admin", "emp-david", "72 hours", "sign"],
        },
        Short {
            name: "one fresh role and one aged out",
            roles: &["platform-admin", "security"],
            stamps: |s| {
                vec![
                    stamp(s, "platform-admin", hours_ago(1)),
                    stamp(s, "security", hours_ago(73)),
                ]
            },
            says: &["security", "72 hours"],
        },
        Short {
            name: "a stamp dated a day AFTER the clock",
            roles: &["platform-admin"],
            stamps: |s| vec![stamp(s, "platform-admin", hours_ago(-24))],
            says: &["platform-admin", "after this service's clock"],
        },
    ]
}

/// EVERY DOOR REFUSES ALIKE, naming the role, when the stamps do not
/// carry the step — and a ticket-less request is told to SIGN, not to
/// send a ticket with its completion.
#[tokio::test]
async fn stamps_that_do_not_carry_the_step_refuse_it_through_every_door() {
    for case in shortfalls() {
        let mut answers = Vec::new();
        for door in DOORS {
            let mut step = stamped_step(STAMPED, case.roles);
            step.sign_offs = (case.stamps)(&step);
            let (app, jobs) = seed_stamped(vec![step]).await;
            let (status, body) = complete_through(&app, door, STAMPED, None).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{}, {door:?}: {body}",
                case.name
            );
            for word in case.says {
                assert!(
                    body.contains(word),
                    "{}, {door:?}: the refusal must say `{word}`: {body}",
                    case.name
                );
            }
            assert!(
                body.contains("completes on its sign-off stamps"),
                "{}, {door:?}: the refusal says the new truth: {body}",
                case.name
            );
            assert_eq!(
                stored(&jobs, STAMPED).await.status,
                StepStatus::Ready,
                "{}, {door:?}: the refused write completed nothing",
                case.name
            );
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            answers.push((parsed["error"].clone(), parsed["owed"].clone()));
        }
        // The stamp's own instant differs by the microsecond between the
        // two seeds, so the doors are compared on the reason, not on it.
        let reasons = |v: &serde_json::Value| -> Vec<(String, String)> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|o| {
                    (
                        o["role"].as_str().unwrap().to_string(),
                        o["why"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        };
        assert_eq!(answers[0].0, answers[1].0, "{}: one error", case.name);
        assert_eq!(
            reasons(&answers[0].1),
            reasons(&answers[1].1),
            "{}: the two doors name the same roles for the same reasons",
            case.name
        );
    }
}

/// THE BOUND IS THE ONE DECLARED VALUE, read to the minute on either
/// side of it, and the decision's own figure.
#[tokio::test]
async fn the_age_bound_is_the_declared_seventy_two_hours() {
    assert_eq!(
        PRESENCE_STAMP_COMPLETES_FOR_HOURS, 72,
        "design 1ce67f7e, stamp-age: 72 hours, decided by David 2026-10-07"
    );
    let minutes = PRESENCE_STAMP_COMPLETES_FOR_HOURS * 60;
    for (age, completes) in [(minutes - 2, true), (minutes + 2, false)] {
        for door in DOORS {
            let mut step = stamped_step(STAMPED, &["platform-admin"]);
            step.sign_offs = vec![stamp(
                &step,
                "platform-admin",
                Utc::now() - Duration::minutes(age),
            )];
            let at = step.sign_offs[0].stamped_at;
            let (app, jobs) = seed_stamped(vec![step]).await;
            let (status, body) = complete_through(&app, door, STAMPED, None).await;
            assert_eq!(
                status.is_success(),
                completes,
                "{door:?}, a stamp {age} minutes old: {status} {body}"
            );
            if !completes {
                // The refusal names the stale stamp: role, authority,
                // stamped_at — and asks for a fresh signature.
                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
                let owed = &parsed["owed"][0];
                assert_eq!(owed["role"], "platform-admin", "{body}");
                assert_eq!(owed["why"], "too-old", "{body}");
                assert_eq!(owed["authority_id"], "emp-david", "{body}");
                let said: DateTime<Utc> =
                    serde_json::from_value(owed["stamped_at"].clone()).unwrap();
                assert_eq!(said, at, "{body}");
                assert_eq!(parsed["max_stamp_age_hours"], 72, "{body}");
                assert!(body.contains("sign again"), "{body}");
                assert_eq!(stored(&jobs, STAMPED).await.status, StepStatus::Ready);
            }
        }
    }
}

/// A PRESENCE STEP WITH NO ROLES KEEPS ITS RULE EXACTLY: the completion
/// carries the ticket. Stray stamps on it stand for nothing — no role
/// asked for them — so "every required role is stamped" is not read as
/// true of a step that requires none.
#[tokio::test]
async fn a_presence_step_with_no_roles_still_completes_only_on_a_ticket() {
    for door in DOORS {
        let mut step = stamped_step(NO_ROLES, &[]);
        step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;

        let (status, body) = complete_through(&app, door, NO_ROLES, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{door:?}: {body}");
        assert!(
            body.contains("requires proof of presence"),
            "{door:?}: today's refusal, word for word: {body}"
        );
        assert_eq!(stored(&jobs, NO_ROLES).await.status, StepStatus::Ready);

        // ...and with the completer's own ticket it completes, as before.
        let who = if door == Door::StatusPut {
            "emp-ada"
        } else {
            "automation:observer"
        };
        let t = ticket_for(&step, who, "completion-nonce");
        let (status, body) = complete_through(&app, door, NO_ROLES, Some(&t)).await;
        assert!(status.is_success(), "{door:?}: {status} {body}");
        let events = jobs.recorded_events();
        let completed = events
            .iter()
            .find(|e| e.kind == "jobs.step.completed")
            .expect("the completion marker");
        assert_eq!(completed.payload["assured"]["by"], "ticket");
        assert_eq!(completed.payload["assured"]["ticket_presented"], true);
    }
}

/// A HEADER THAT DOES NOT VERIFY IS REFUSED EVEN WHEN THE STAMPS WOULD
/// SUFFICE. A forged header is never ignored: a claim this service
/// cannot check is a forgery or a fault, on every path.
#[tokio::test]
async fn an_unverified_ticket_is_refused_even_when_the_stamps_carry_the_step() {
    for door in DOORS {
        let mut step = stamped_step(STAMPED, &["platform-admin"]);
        step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;
        let forged = serde_json::json!({
            "employee_id": "emp-ada",
            "step_id": step.id.to_string(),
            "shape_hash": step.shape_hash(),
            "nonce": "forged",
        })
        .to_string();
        let (status, body) = complete_through(&app, door, STAMPED, Some(&forged)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{door:?}: {body}");
        assert!(body.contains("did not verify"), "{door:?}: {body}");
        assert_eq!(
            stored(&jobs, STAMPED).await.status,
            StepStatus::Ready,
            "{door:?}"
        );
    }
}

/// THE COMPLETING REQUEST CHANGES NOTHING THE STAMP COVERS, NOR NOTES OR
/// HOLDER, and a presence step is never skipped — all as before, now
/// that no ticket rides the request (backlogs c0b56fd9, 42e7c6b9).
#[tokio::test]
async fn a_completion_on_stamps_still_moves_nothing_and_never_skips() {
    let bodies = [
        (
            r#"{"status":"completed","title":"Approve another plan"}"#,
            "title",
        ),
        (
            r#"{"status":"completed","metadata":{"plan":"{\"verb\":\"df\"}"}}"#,
            "metadata",
        ),
        (r#"{"status":"completed","notes":"looks fine"}"#, "notes"),
        (
            r#"{"status":"completed","assignee_id":"emp-ada"}"#,
            "assignee_id",
        ),
        (r#"{"status":"skipped"}"#, "never skipped"),
    ];
    for (body, names) in bodies {
        let mut step = stamped_step(STAMPED, &["platform-admin"]);
        step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;
        let uri = format!("/api/jobs/{STAMPED_JOB}/steps/{STAMPED}");
        let (status, answer) = call(&app, &admin(), "PUT", uri, None, body.into()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}: {answer}");
        assert!(answer.contains(names), "{body}: names `{names}`: {answer}");
        assert_eq!(
            stored(&jobs, STAMPED).await,
            step,
            "{body}: the refused write changed nothing"
        );
    }
    // The conditional door's evidence is a metadata write in the same
    // act: it moves the shape the stamps signed, and is refused.
    let mut step = stamped_step(STAMPED, &["platform-admin"]);
    step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let (status, answer) = call(
        &app,
        &automation(),
        "POST",
        format!("/api/jobs/{STAMPED_JOB}/steps/{STAMPED}/complete-if"),
        None,
        serde_json::json!({
            "operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd",
            "expected": {"status": "ready", "assignee_id": null},
            "evidence": {"plan": "{\"verb\":\"df\"}"},
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(stored(&jobs, STAMPED).await, step);
}

/// AN AGED-OUT SIGNATURE CAN BE GIVEN AGAIN. The sign-off door answered
/// "already signed" to any live stamp, which would leave a role whose
/// stamp is past the bound no way to sign at all. On a step still open,
/// a fresh ceremony writes a fresh stamp beside the old one, and the
/// newest carries the role.
#[tokio::test]
async fn a_role_whose_stamp_aged_out_signs_again_and_the_step_completes() {
    let mut step = stamped_step(STAMPED, &["platform-admin"]);
    step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(73))];
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let sign_offs = format!("/api/jobs/{STAMPED_JOB}/steps/{STAMPED}/sign-offs");
    let role = r#"{"role":"platform-admin"}"#;

    let (status, body) = complete_through(&app, Door::StatusPut, STAMPED, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Still only on a ticket.
    let (status, body) = call(
        &app,
        &operator(),
        "POST",
        sign_offs.clone(),
        None,
        role.into(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(stored(&jobs, STAMPED).await.sign_offs.len(), 1);

    let t = ticket_for(&step, "emp-david", "second-ceremony");
    let (status, body) = call(
        &app,
        &operator(),
        "POST",
        sign_offs.clone(),
        Some(&t),
        role.into(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let now = stored(&jobs, STAMPED).await;
    assert_eq!(
        now.sign_offs.len(),
        2,
        "the old stamp stays; a new one joins it"
    );
    assert_eq!(
        now.sign_offs[1].presence_nonce.as_deref(),
        Some("second-ceremony")
    );

    // A role that IS carried stays idempotent: no third stamp.
    let t = ticket_for(&step, "emp-david", "third-ceremony");
    let (status, body) = call(&app, &operator(), "POST", sign_offs, Some(&t), role.into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored(&jobs, STAMPED).await.sign_offs.len(), 2);

    let (status, body) = complete_through(&app, Door::StatusPut, STAMPED, None).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(stored(&jobs, STAMPED).await.status, StepStatus::Completed);
}

/// ...AND ONLY WHILE THE STEP IS OPEN. A finished step's stamps are its
/// record: the door stays idempotent there, exactly as before, so an
/// approval that aged out unrun is not refreshed onto a completed
/// approve step (the ops runner reads that stamp's `stamped_at`).
#[tokio::test]
async fn a_completed_steps_aged_stamp_is_not_renewed() {
    let mut step = stamped_step(STAMPED, &["platform-admin"]);
    step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(73))];
    step.status = StepStatus::Completed;
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let t = ticket_for(&step, "emp-david", "late-ceremony");
    let (status, body) = call(
        &app,
        &operator(),
        "POST",
        format!("/api/jobs/{STAMPED_JOB}/steps/{STAMPED}/sign-offs"),
        Some(&t),
        r#"{"role":"platform-admin"}"#.into(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored(&jobs, STAMPED).await.sign_offs,
        step.sign_offs,
        "no stamp was added to the finished step"
    );
}

/// A genuine ticket minted for ANOTHER step is not presence here — and
/// is not needed: the stamps carry the step, so the request is judged as
/// the session it is. (Only a header that does not VERIFY is refused.)
#[tokio::test]
async fn a_genuine_ticket_for_another_step_neither_helps_nor_blocks() {
    let mut step = stamped_step(STAMPED, &["platform-admin"]);
    step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
    let other = stamped_step(OTHER, &["platform-admin"]);
    let (app, jobs) = seed_stamped(vec![step, other.clone()]).await;
    let t = ticket_for(&other, "emp-ada", "elsewhere");

    // On the unstamped step it is its own step's ticket and still does
    // not complete it: the role has not signed.
    let (status, body) = complete_through(&app, Door::StatusPut, OTHER, Some(&t)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("no live presence stamp"), "{body}");

    let (status, body) = complete_through(&app, Door::StatusPut, STAMPED, Some(&t)).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(stored(&jobs, STAMPED).await.status, StepStatus::Completed);
}

// ---------------------------------------------------------------------
// A HUMAN-ONLY STEP COMPLETES ON A PASSKEY (David, 2026-10-07, item
// 570c66e9 `human_only_by_passkey_20261007`: "human-only step completion
// should use passkey for enforcement", and "we may also be able to update
// jobs such that I stamp the instructions so the step no longer requires
// human completion if it doesn't make sense").
//
// WHAT WAS TRUE BEFORE. `human_only` was enforced by `person_check`: the
// ASSERTED actor id must be human-shaped and on the roster. The machine
// door trusts the id a caller asserts, so every holder of the machine
// token could complete a human-only step as any employee (review
// 3f7b70bc measured emp-ghost and emp-david, without presence, writing
// one). And once a stamped presence step completes on its stamps from
// any actor, `human_only` was the one guard left on three of the eight
// presence steps — a guard an asserted id walks past.
//
// THE RULE. `human_only` raises the step's required assurance to
// presence, and the roster check stays beside the passkey:
//   (a) with sign-off roles: every required role's live passkey stamp,
//       each BY A PERSON — and then any actor may send the completion
//       (he stamps the instructions; a machine may finish);
//   (b) with no roles: a verifying ticket on the completing request, for
//       this step, shape and actor, and the actor is a person.
// ---------------------------------------------------------------------

const HUMAN: &str = "00000000-0000-0000-0000-00000000d201";

/// Human-shaped and absent from the roster: the id review 3f7b70bc
/// asserted at the machine door.
fn ghost() -> User {
    User {
        id: "emp-ghost".to_string(),
        ..operator()
    }
}

/// A human-only step that declares NO assurance of its own — the shape
/// of all ten plain human-only steps on the live registry.
fn human_only_step(roles: &[&str]) -> Step {
    Step {
        assurance_required: None,
        metadata: serde_json::json!({ "human_only": true, "why": "widen to everyone" }),
        ..stamped_step(HUMAN, roles)
    }
}

async fn complete_as(
    app: &Router,
    door: Door,
    who: &User,
    id: &str,
    presence: Option<&str>,
) -> (StatusCode, String) {
    match door {
        Door::StatusPut => {
            call(
                app,
                who,
                "PUT",
                format!("/api/jobs/{STAMPED_JOB}/steps/{id}"),
                presence,
                r#"{"status":"completed"}"#.into(),
            )
            .await
        }
        Door::CompleteIf => {
            call(
                app,
                who,
                "POST",
                format!("/api/jobs/{STAMPED_JOB}/steps/{id}/complete-if"),
                presence,
                serde_json::json!({
                    "operation_id": "63f6e276-4a5e-40f8-84c6-38d8315d27cd",
                    "expected": {"status": "ready", "assignee_id": null},
                    "evidence": {},
                })
                .to_string(),
            )
            .await
        }
    }
}

/// ARM (b), THE REFUSAL the operator will meet first: a person's bare
/// completion of a human-only step — `boss step complete`, the machine
/// door, any asserted id — is refused at both doors, and the body says
/// the step is human-only, that a passkey is the proof, and both ways
/// out.
#[tokio::test]
async fn a_human_only_step_with_no_roles_refuses_a_bare_completion_and_says_how() {
    let mut answers = Vec::new();
    for door in DOORS {
        let step = human_only_step(&[]);
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;
        let (status, body) = complete_as(&app, door, &operator(), HUMAN, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{door:?}: {body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["required"], "presence",
            "{door:?}: the key a surface answers with a passkey tap: {body}"
        );
        assert_eq!(parsed["human_only"], true, "{door:?}: {body}");
        for word in [
            "human_only",
            "passkey",
            "surface that runs the passkey ceremony",
            "sign-off role",
            "Workflow row",
        ] {
            assert!(body.contains(word), "{door:?}: says `{word}`: {body}");
        }
        assert_eq!(
            parsed["ways_out"].as_array().map(Vec::len),
            Some(2),
            "{body}"
        );
        assert_eq!(
            stored(&jobs, HUMAN).await,
            step,
            "{door:?}: nothing written"
        );
        answers.push(parsed);
    }
    assert_eq!(answers[0], answers[1], "the two doors answer identically");
}

/// ARM (b): the person's own verifying ticket completes it, and the log
/// records what the completion stood on.
#[tokio::test]
async fn a_human_only_step_with_no_roles_completes_on_the_persons_own_ticket() {
    for door in DOORS {
        let step = human_only_step(&[]);
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;
        let t = ticket_for(&step, "emp-david", "human-nonce");
        let (status, body) = complete_as(&app, door, &operator(), HUMAN, Some(&t)).await;
        assert!(status.is_success(), "{door:?}: {status} {body}");
        assert_eq!(stored(&jobs, HUMAN).await.status, StepStatus::Completed);
        let events = jobs.recorded_events();
        let assured = &events
            .iter()
            .find(|e| e.kind == "jobs.step.completed")
            .expect("the completion marker")
            .payload["assured"];
        assert_eq!(assured["required"], "presence", "{door:?}: {assured}");
        assert_eq!(assured["by"], "ticket", "{door:?}: {assured}");
        assert_eq!(assured["human_only"], true, "{door:?}: {assured}");
        assert_eq!(assured["person"], "emp-david", "{door:?}: {assured}");
        assert_eq!(
            assured["presence_nonce"], "human-nonce",
            "{door:?}: {assured}"
        );
    }
}

/// ARM (b): a ticket is THIS actor's or it is nothing; and a passkey
/// does not make a person — the roster check stays beside it.
#[tokio::test]
async fn a_human_only_ticket_is_the_actors_own_and_the_actor_is_on_the_roster() {
    for door in DOORS {
        // Someone else's genuine ticket for this very step and shape.
        let step = human_only_step(&[]);
        let (app, jobs) = seed_stamped(vec![step.clone()]).await;
        let t = ticket_for(&step, "emp-david", "davids");
        let (status, body) = complete_as(&app, door, &admin(), HUMAN, Some(&t)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{door:?}: {body}");
        assert!(body.contains("did not match"), "{door:?}: {body}");
        assert_eq!(stored(&jobs, HUMAN).await.status, StepStatus::Ready);

        // A verifying ticket of their own, for an id the roster does not
        // list, and for a machine-shaped id.
        for (who, why) in [
            (ghost(), "does not list an active employee"),
            (automation(), "machine-shaped"),
        ] {
            let (app, jobs) = seed_stamped(vec![step.clone()]).await;
            let t = ticket_for(&step, &who.id, "own");
            let (status, body) = complete_as(&app, door, &who, HUMAN, Some(&t)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{door:?} {}: {body}", who.id);
            assert!(body.contains(why), "{door:?} {}: {body}", who.id);
            assert!(body.contains(&who.id), "{door:?}: names the actor: {body}");
            assert_eq!(stored(&jobs, HUMAN).await.status, StepStatus::Ready);
        }
    }
}

/// ARM (a), statement (2): he stamps the instructions, a machine may
/// finish. The step declares no assurance of its own — `human_only`
/// alone raises it — and an automation's bare completion lands on the
/// person's live passkey stamp.
#[tokio::test]
async fn a_human_only_step_a_person_stamped_is_completed_by_a_machine() {
    for door in DOORS {
        let mut step = human_only_step(&["platform-admin"]);
        step.sign_offs = vec![stamp(&step, "platform-admin", hours_ago(1))];
        let (app, jobs) = seed_stamped(vec![step]).await;
        let (status, body) = complete_as(&app, door, &automation(), HUMAN, None).await;
        assert!(status.is_success(), "{door:?}: {status} {body}");
        let done = stored(&jobs, HUMAN).await;
        assert_eq!(done.status, StepStatus::Completed);
        let events = jobs.recorded_events();
        let assured = &events
            .iter()
            .find(|e| e.kind == "jobs.step.completed")
            .expect("the completion marker")
            .payload["assured"];
        assert_eq!(assured["by"], "stamps", "{door:?}: {assured}");
        assert_eq!(assured["human_only"], true, "{door:?}: {assured}");
        assert_eq!(
            assured["stamps"][0]["authority_id"], "emp-david",
            "{assured}"
        );
    }
}

/// ARM (a): the stamp is a passkey's, and its authority is a person.
#[tokio::test]
async fn a_human_only_step_is_carried_only_by_a_persons_passkey_stamps() {
    type Make = fn(&Step) -> SignOffStamp;
    let cases: [(&str, Make, StatusCode, &str); 4] = [
        (
            "a stamp a session wrote",
            |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.assurance = Assurance::Session;
                st.presence_nonce = None;
                st
            },
            StatusCode::UNPROCESSABLE_ENTITY,
            "session",
        ),
        (
            "a passkey stamp by an id the roster does not list",
            |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.authority_id = "emp-ghost".into();
                st
            },
            StatusCode::FORBIDDEN,
            "emp-ghost",
        ),
        (
            "a passkey stamp by a machine-shaped id",
            |s| {
                let mut st = stamp(s, "platform-admin", hours_ago(1));
                st.authority_id = "automation:observer".into();
                st
            },
            StatusCode::FORBIDDEN,
            "automation:observer",
        ),
        (
            "a stamp 73 hours old",
            |s| stamp(s, "platform-admin", hours_ago(73)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "72 hours",
        ),
    ];
    for (name, make, want, says) in cases {
        let mut answers = Vec::new();
        for door in DOORS {
            let mut step = human_only_step(&["platform-admin"]);
            step.sign_offs = vec![make(&step)];
            let (app, jobs) = seed_stamped(vec![step]).await;
            // Sent by a PERSON: who sends it does not rescue the stamp.
            let (status, body) = complete_as(&app, door, &operator(), HUMAN, None).await;
            assert_eq!(status, want, "{name}, {door:?}: {body}");
            assert!(
                body.contains(says),
                "{name}, {door:?}: says `{says}`: {body}"
            );
            assert!(body.contains("human_only"), "{name}, {door:?}: {body}");
            assert_eq!(stored(&jobs, HUMAN).await.status, StepStatus::Ready);
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            answers.push(parsed["error"].clone());
        }
        assert_eq!(answers[0], answers[1], "{name}: one answer at both doors");
    }
}

/// WRITING the stamp on a human-only step takes a passkey too, though
/// the step declares no assurance: a session stamp could never carry it.
#[tokio::test]
async fn a_human_only_steps_stamp_is_written_only_on_a_ticket() {
    let step = human_only_step(&["platform-admin"]);
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let uri = format!("/api/jobs/{STAMPED_JOB}/steps/{HUMAN}/sign-offs");
    let role = r#"{"role":"platform-admin"}"#;
    let (status, body) = call(&app, &operator(), "POST", uri.clone(), None, role.into()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(stored(&jobs, HUMAN).await.sign_offs.is_empty());
    let t = ticket_for(&step, "emp-david", "stamp-nonce");
    let (status, body) = call(&app, &operator(), "POST", uri, Some(&t), role.into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stored(&jobs, HUMAN).await.sign_offs[0].assurance,
        Assurance::Presence
    );
}

/// A SKIP satisfies `steps.x.done` as a completion does, and a ticket
/// carries no verb: a human-only step is completed, never skipped.
#[tokio::test]
async fn a_human_only_step_is_not_skipped() {
    let step = human_only_step(&[]);
    let (app, jobs) = seed_stamped(vec![step.clone()]).await;
    let uri = format!("/api/jobs/{STAMPED_JOB}/steps/{HUMAN}");
    let skip = r#"{"status":"skipped"}"#;
    let (status, body) = call(&app, &operator(), "PUT", uri.clone(), None, skip.into()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let t = ticket_for(&step, "emp-david", "skip-nonce");
    let (status, body) = call(&app, &operator(), "PUT", uri, Some(&t), skip.into()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("never skipped"), "{body}");
    assert_eq!(stored(&jobs, HUMAN).await, step);
}

/// THE CONTROL: a step that declares `human_only = false`, or nothing,
/// is asked for no passkey.
#[tokio::test]
async fn a_step_that_is_not_human_only_is_asked_for_no_passkey() {
    for declared in [
        serde_json::json!(false),
        serde_json::json!("false"),
        serde_json::Value::Null,
    ] {
        for door in DOORS {
            let mut step = human_only_step(&[]);
            step.metadata["human_only"] = declared.clone();
            let (app, jobs) = seed_stamped(vec![step]).await;
            let (status, body) = complete_as(&app, door, &automation(), HUMAN, None).await;
            assert!(status.is_success(), "{declared} {door:?}: {status} {body}");
            assert_eq!(stored(&jobs, HUMAN).await.status, StepStatus::Completed);
        }
    }
}
