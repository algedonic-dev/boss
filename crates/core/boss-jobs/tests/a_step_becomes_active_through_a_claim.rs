//! The claim door starts a step the way the PUT did, and says who may
//! start one for someone else.
//!
//! Design 611fbffd ("A step becomes Active only through a claim",
//! answered by David 2026-09-26), clause (2) of backlog 6ef4a36b. Every
//! surface's Start and the sim's workforce take a Ready step to Active
//! with a step PUT naming the holder — so the record cannot tell "X took
//! this work" from "someone assigned X and started the clock". The claim
//! door is the one that records a claim, and two things kept the
//! surfaces off it:
//!
//! - THE CLAIM DOOR DID NOT RESERVE. `calendar_hook::apply_step_transition`
//!   ran only in `update_step`, so moving the Scheduling surface's Start to
//!   the claim door would have silently stopped reserving the holder's
//!   time. Both doors now start a step through one function
//!   (`start_hold`), and a claim the calendar refuses, or the CAS refuses
//!   after the calendar said yes, holds nothing. (Since backlog e639899a
//!   the PUT, which can no longer start a step, calls it no longer, and
//!   a POSTed step cannot be born Active: one door starts a step.)
//! - THE CLAIM DOOR COULD ONLY CLAIM FOR ITS CALLER. Q1 decided who may
//!   claim FOR someone else: the executor the step's own audience names
//!   (the automation a Workflow row declares), or a holder of the
//!   `step-assign` authority (platform-admin today). The claim records
//!   both: the event is signed by the caller, the step is held by the
//!   nominee, and the assignment marker names both (`claimed_by`,
//!   `claimed_for`). Everyone else claims only for themselves.
//!
//! THE REWORK (the adversarial review of this car, 2026-09-26):
//!
//! - H1: the declared executor was read off the step's LIVE metadata,
//!   and `audience` was a key any step writer could PATCH — so a caller
//!   with no `step-assign` wrote `{"audience":{"individual":<self>}}`
//!   (204) and then claimed for anyone (200). The executor is now read
//!   from the PROTOCOL the packet is pinned to (the Workflow row at
//!   `job.workflow_version`, the step by `spec_slug`), and `audience` is
//!   a protocol key neither write door lets a writer change.
//! - S1: the claim CAS judges status and holder, not the row, so a
//!   schedule PATCHed between the claim's read and its CAS landed with
//!   the hold still on the OLD window. A landed claim whose stored
//!   schedule moved hands its reservation back and holds the time as
//!   stored.
//! - S2: `claimed_for` is resolved to the one id the registries know
//!   (an agent login to its registered id) BEFORE anything is reserved
//!   or recorded, and a nominee no registry knows is refused.
//! - S3: every `individual` audience in the platform bundle names an
//!   automation, because that audience is claim-for authority.
//!
//! The calendar here keeps what it holds (one hard hold per subject per
//! overlapping window), because the defect is in what the calendar HOLDS
//! after a claim, which a call-recording fake cannot show.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_calendar_client::{CalendarClient, CalendarClientError};
use boss_core::calendar::{
    BusinessCalendar, Reservation, ReservationId, ReservationRequest, TimeWindow,
};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::types::{AgentInput, AgentRow, AgentsBatchOutcome};
use boss_jobs::agents::{AgentsError, AgentsRegistry, InMemoryAgents};
use boss_jobs::audience::Audience;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// A step write to land between a claim's read and its CAS: the
/// calendar's `reserve` is the one call the claim makes in that gap, so
/// the calendar lands it there — the S1 race, made deterministic.
type MoveOnReserve = (Arc<InMemoryJobs>, StepId, &'static str);

/// A rival's claim to land between a claim's read and its CAS — the
/// rival (the last field) takes the step and holds its time, as its own
/// claim would have: the claim race of backlog d0cabe6e, made
/// deterministic the same way.
type ClaimOnReserve = (Arc<InMemoryJobs>, StepId, &'static str);

/// A new holder handed a still-READY step between a claim's read and its
/// CAS — a nomination, not a claim: the last field becomes the holder
/// and the step stays Ready (backlog ce8b7d66, the review of car
/// eb2f9b0f, finding 1).
type HandOnReserve = (Arc<InMemoryJobs>, StepId, &'static str);

#[derive(Default)]
struct HeldCalendar {
    held: Mutex<Vec<Reservation>>,
    move_on_reserve: Mutex<Option<MoveOnReserve>>,
    claim_on_reserve: Mutex<Option<ClaimOnReserve>>,
    hand_on_reserve: Mutex<Option<HandOnReserve>>,
}

impl HeldCalendar {
    fn live(&self) -> Vec<Reservation> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.cancelled_at.is_none())
            .cloned()
            .collect()
    }

    fn live_for(&self, ref_id: &str) -> Vec<Reservation> {
        self.live()
            .into_iter()
            .filter(|r| r.reason_ref_id == ref_id)
            .collect()
    }
}

#[async_trait]
impl CalendarClient for HeldCalendar {
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarClientError> {
        let racing = self.move_on_reserve.lock().unwrap().take();
        if let Some((jobs, step_id, scheduled_at)) = racing {
            let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
            step.metadata["scheduled_at"] = serde_json::json!(scheduled_at);
            jobs.update_step(&step).await.unwrap();
        }
        let handed = self.hand_on_reserve.lock().unwrap().take();
        if let Some((jobs, step_id, holder)) = handed {
            let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
            step.assignee_id = Some(holder.to_string());
            jobs.update_step(&step).await.unwrap();
        }
        let rival = self.claim_on_reserve.lock().unwrap().take();
        if let Some((jobs, step_id, winner)) = rival {
            let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
            step.status = StepStatus::Active;
            step.assignee_id = Some(winner.to_string());
            jobs.update_step(&step).await.unwrap();
            self.held.lock().unwrap().push(Reservation {
                id: ReservationId::new(),
                subject: Subject::new("employee", winner),
                window: req.window,
                reason_kind: req.reason_kind.clone(),
                reason_ref_id: req.reason_ref_id.clone(),
                strength: req.strength,
                notes: None,
                created_by: winner.to_string(),
                created_at: chrono::Utc::now(),
                cancelled_at: None,
            });
        }
        let mut held = self.held.lock().unwrap();
        let clashing: Vec<Reservation> = held
            .iter()
            .filter(|r| {
                r.cancelled_at.is_none()
                    && r.subject == req.subject
                    && r.window.overlaps(&req.window)
            })
            .cloned()
            .collect();
        if !clashing.is_empty() {
            return Err(CalendarClientError::Conflict { existing: clashing });
        }
        let id = ReservationId::new();
        held.push(Reservation {
            id,
            subject: req.subject,
            window: req.window,
            reason_kind: req.reason_kind,
            reason_ref_id: req.reason_ref_id,
            strength: req.strength,
            notes: req.notes,
            created_by: req.created_by,
            created_at: chrono::Utc::now(),
            cancelled_at: None,
        });
        Ok(id)
    }

    async fn list(
        &self,
        _subject: &Subject,
        _window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarClientError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, id: ReservationId, _actor: &str) -> Result<(), CalendarClientError> {
        for r in self
            .held
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|r| r.id == id && r.cancelled_at.is_none())
        {
            r.cancelled_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        _actor: &str,
    ) -> Result<usize, CalendarClientError> {
        let mut n = 0;
        for r in self.held.lock().unwrap().iter_mut().filter(|r| {
            r.cancelled_at.is_none()
                && r.reason_kind == reason_kind
                && r.reason_ref_id == reason_ref_id
        }) {
            r.cancelled_at = Some(chrono::Utc::now());
            n += 1;
        }
        Ok(n)
    }

    async fn get_business_calendar(
        &self,
        _code: &str,
    ) -> Result<Option<BusinessCalendar>, CalendarClientError> {
        Ok(None)
    }
}

const TECH: &str = "emp-tech";
const OTHER: &str = "emp-other";
const LEAD: &str = "emp-lead";
const EXECUTOR: &str = "automation:boss-step";
const AGENT: &str = "agent-claude";
const AGENT_LOGIN: &str = "claude@algedonic.dev";
const KIND: &str = "service-visit";
/// The step of [`KIND`] that declares no executor.
const VISIT: &str = "visit";
/// The step of [`KIND`] whose Workflow row names [`EXECUTOR`].
const RUN: &str = "run";

/// The protocol the packets here are pinned to: `visit` declares no
/// audience, `run` declares [`EXECUTOR`] as its `individual`.
fn spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        KIND,
        "Service visit",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: VISIT.into(),
                kind: "scheduling".into(),
                ready_when: "true".into(),
                title_template: "Visit".into(),
                ..Default::default()
            },
            StepSpec {
                title: RUN.into(),
                kind: "scheduling".into(),
                ready_when: "true".into(),
                title_template: "Run".into(),
                audience: Some(Audience::Individual(EXECUTOR.into())),
                ..Default::default()
            },
        ],
    )
}

/// The people roster: three active employees.
struct Roster;

#[async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(vec![])
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok([TECH, OTHER, LEAD].contains(&id))
    }
}

fn user(id: &str, role: &str) -> User {
    User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// Every role here may write steps; only `lead` holds `step-assign`.
fn build_app() -> (Router, Arc<InMemoryJobs>, Arc<HeldCalendar>) {
    build_app_with_agents(Arc::new(
        InMemoryAgents::new().with_agent(AGENT, [AGENT_LOGIN]),
    ))
}

/// [`build_app`], reading the agents registry `agents`.
fn build_app_with_agents(
    agents: Arc<dyn AgentsRegistry>,
) -> (Router, Arc<InMemoryJobs>, Arc<HeldCalendar>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let calendar = Arc::new(HeldCalendar::default());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("technician", Action::Update, Resource::step(), Scope::All)
            .allow("lead", Action::Update, Resource::step(), Scope::All)
            .allow("lead", Action::Update, Resource::step_assign(), Scope::All)
            .allow("system", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec()).expect("seed the kind");
    let state = JobsApiState {
        calendar: Some(calendar.clone() as Arc<dyn CalendarClient>),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        agent_budget: Some(Arc::new(BudgetDoor {
            agents,
            runs: Arc::new(InMemoryAgentRuns::new(vec![])),
        })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, calendar)
}

/// A scheduled Ready `visit` step, held by `assignee` (or nobody), with
/// `extra` merged into its metadata.
async fn scheduled(
    jobs: &InMemoryJobs,
    assignee: Option<&str>,
    extra: serde_json::Value,
) -> (Job, Step) {
    scheduled_as(jobs, VISIT, assignee, extra).await
}

/// [`scheduled`], as the step of the pinned protocol named `slug`.
async fn scheduled_as(
    jobs: &InMemoryJobs,
    slug: &str,
    assignee: Option<&str>,
    extra: serde_json::Value,
) -> (Job, Step) {
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            KIND,
            Subject::new("custom", "visit"),
            "A visit started through the claim door",
            TECH,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        )
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "scheduling", "Visit", 1);
    step.status = StepStatus::Ready;
    step.assignee_id = assignee.map(str::to_string);
    step.spec_slug = Some(slug.to_string());
    let mut metadata = serde_json::json!({
        "scheduled_at": "2026-09-27T10:00:00Z",
        "duration_minutes": 90,
    });
    if let (Some(m), Some(e)) = (metadata.as_object_mut(), extra.as_object()) {
        m.extend(e.clone());
    }
    step.metadata = metadata;
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

async fn claim(
    app: &Router,
    job: &Job,
    step: &Step,
    as_user: &User,
    claimed_for: Option<&str>,
) -> (StatusCode, String) {
    let query = claimed_for
        .map(|who| format!("?claimed_for={who}"))
        .unwrap_or_default();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/claim{query}",
                    job.id, step.id
                ))
                .header("x-boss-user", serde_json::to_string(as_user).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_claim_door_reserves_the_holders_time_as_the_put_did() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(
        holds.len(),
        1,
        "a claimed scheduled step holds its holder's time, as a PUT start did"
    );
    assert_eq!(holds[0].subject, Subject::new("employee", TECH));
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some(TECH));
}

#[tokio::test]
async fn a_claim_the_calendar_refuses_is_not_made() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;
    // Someone else's booking already holds the technician's morning.
    let start_at = chrono::DateTime::parse_from_rfc3339("2026-09-27T09:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    calendar
        .reserve(ReservationRequest {
            subject: Subject::new("employee", TECH),
            window: TimeWindow::new(start_at, start_at + chrono::Duration::minutes(60)).unwrap(),
            reason_kind: boss_core::calendar::reason::JOB_STEP.to_string(),
            reason_ref_id: "another-step".into(),
            strength: boss_core::calendar::ReservationStrength::Hard,
            notes: None,
            created_by: "emp-dispatch".into(),
        })
        .await
        .unwrap();

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("calendar conflict"), "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready, "nothing was claimed");
    assert_eq!(stored.assignee_id, None);
}

#[tokio::test]
async fn a_claim_the_cas_refuses_hands_its_reservation_back() {
    let (app, jobs, calendar) = build_app();
    // Nominated to someone else: the CAS refuses the technician's claim
    // AFTER the calendar said yes to it.
    let (job, step) = scheduled(&jobs, Some(OTHER), serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("not claimable"), "{body}");
    assert!(
        calendar.live_for(&step.id.to_string()).is_empty(),
        "a refused claim says nothing was written; the calendar must agree: {:?}",
        calendar.live()
    );
}

#[tokio::test]
async fn the_loser_of_a_claim_race_holds_none_of_the_winners_time() {
    // Backlog d0cabe6e: the loser reserved its own time, a rival's claim
    // landed before the loser's CAS, and the CAS refused the loser. The
    // step is Active over the same schedule — held by the RIVAL — so
    // the loser's reservation is not that step's hold and goes back; the
    // rival's stays. The holder is compared through `scheduling_fields`,
    // whose third element is the assignee.
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;
    *calendar.claim_on_reserve.lock().unwrap() = Some((jobs.clone(), step.id, OTHER));

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "the rival won: {body}");

    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(
        holds.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec![Subject::new("employee", OTHER)],
        "one hold, the winner's; the loser's handed back: {holds:?}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));
}

/// A packet whose kind the registry holds no version of — the case
/// `add_step`'s registry guard falls through on (an unwired registry,
/// an experiment arm's discarded draft) — and a step body for it.
async fn unpinned_job_and_step(jobs: &InMemoryJobs) -> (Job, Step) {
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        metadata: serde_json::json!({}),
        ..Job::new(
            "ad-hoc",
            Subject::new("custom", "ad-hoc"),
            "A packet no protocol row describes",
            TECH,
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
        )
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "scheduling", "Posted", 1);
    step.metadata = serde_json::json!({
        "scheduled_at": "2026-09-27T10:00:00Z",
        "duration_minutes": 90,
    });
    (job, step)
}

async fn post_step(app: &Router, job: &Job, step: &Step, as_user: &User) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{}/steps", job.id))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(as_user).unwrap())
                .body(Body::from(serde_json::to_vec(step).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_step_is_not_born_active() {
    // Backlog e639899a (1): the registry guard refuses a posted step only
    // when the packet's pinned version READS, and the status check below
    // it refused only a resolved one — so on an unreadable version a step
    // was born Active naming anyone, past the claim CAS, the claim-for
    // rule, the station gate, the budget and the calendar.
    let (app, jobs, calendar) = build_app();
    let (job, mut step) = unpinned_job_and_step(&jobs).await;
    step.status = StepStatus::Active;
    step.assignee_id = Some(OTHER.into());

    let (status, body) = post_step(&app, &job, &step, &user(TECH, "technician")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("/claim"),
        "the refusal names the claim door: {body}"
    );
    assert!(
        jobs.list_steps(&job.id).await.unwrap().is_empty(),
        "nothing was stored"
    );
    assert!(calendar.live().is_empty(), "{:?}", calendar.live());

    // Born open, then claimed: the one way a step becomes Active.
    step.status = StepStatus::Ready;
    step.assignee_id = None;
    let (status, body) = post_step(&app, &job, &step, &user(TECH, "technician")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_step_is_not_born_holding_a_blank() {
    // Backlog d88d9601 (b): a posted `assignee_id: ""` was stored as
    // `Some("")`, which both claim CASes read as a holder — a Ready step
    // no one could ever claim. The step PUT stores a blank as nobody
    // (6ef4a36b); this door refuses one, so the caller learns the rule.
    let (app, jobs, _) = build_app();
    for blank in ["", "  \t"] {
        let (job, mut step) = unpinned_job_and_step(&jobs).await;
        step.status = StepStatus::Ready;
        step.assignee_id = Some(blank.into());
        let (status, body) = post_step(&app, &job, &step, &user(TECH, "technician")).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{blank:?}: {body}"
        );
        assert!(body.contains("assignee_id"), "{body}");
        assert!(jobs.list_steps(&job.id).await.unwrap().is_empty());
    }
    // Nobody, spelled as nobody, is an ordinary open step.
    let (job, mut step) = unpinned_job_and_step(&jobs).await;
    step.status = StepStatus::Ready;
    let (status, body) = post_step(&app, &job, &step, &user(TECH, "technician")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn a_claim_for_someone_else_is_refused_to_anyone_without_the_authority() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("claimed_for"),
        "the refusal names the rule: {body}"
    );
    assert!(body.contains("step-assign"), "and the authority: {body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

#[tokio::test]
async fn a_claim_for_yourself_by_name_is_an_ordinary_claim() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(TECH)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(TECH));
}

/// The STEP_UPDATED and assignment marker a claim recorded for `step`.
fn claim_events(jobs: &InMemoryJobs, step: &Step) -> (serde_json::Value, serde_json::Value) {
    let events = jobs.recorded_events();
    let sid = step.id.to_string();
    let updated = events
        .iter()
        .rev()
        .find(|e| e.kind == "jobs.step.updated" && e.payload["id"] == sid.as_str())
        .map(|e| e.payload.clone())
        .expect("the claim recorded its STEP_UPDATED");
    let assigned = events
        .iter()
        .rev()
        .find(|e| e.kind.starts_with("step.assigned.") && e.payload["step_id"] == sid.as_str())
        .map(|e| e.payload.clone())
        .expect("a claim for someone else records its assignment marker");
    (updated, assigned)
}

#[tokio::test]
async fn a_holder_of_step_assign_claims_for_someone_else_and_the_record_names_both() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(
        stored.assignee_id.as_deref(),
        Some(OTHER),
        "the nominee holds it"
    );

    let (updated, assigned) = claim_events(&jobs, &step);
    assert_eq!(updated["_actor"], LEAD, "the event is signed by the caller");
    assert_eq!(updated["assignee_id"], OTHER);
    assert_eq!(assigned["claimed_by"], LEAD);
    assert_eq!(assigned["claimed_for"], OTHER);

    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(holds.len(), 1, "the nominee's time is held");
    assert_eq!(holds[0].subject, Subject::new("employee", OTHER));
    assert_eq!(holds[0].created_by, LEAD);
}

/// The `run` step AS MATERIALISATION LEAVES IT, made Ready: an
/// `individual` audience is born assigned (`materialize_steps` sets
/// `assignee_id` to it), so every real executor-declared step is Ready
/// AND held by its executor (backlog 5d1c0b7a). The holder is read from
/// the materialiser, not typed here — the fixture this replaced seeded
/// `assignee_id = None`, a state no real step is in, and its test passed
/// while the route it tested 409'd on every live step.
async fn materialised_run(jobs: &InMemoryJobs) -> (Job, Step) {
    let (job, _) = scheduled(jobs, None, serde_json::json!({})).await;
    let mut step = boss_jobs::registry::materialize_steps(
        &spec(),
        &job.subject,
        job.id,
        &job.metadata,
        StepId::new,
    )
    .into_iter()
    .find(|s| s.spec_slug.as_deref() == Some(RUN))
    .expect("the protocol has a run step");
    assert_eq!(
        step.assignee_id.as_deref(),
        Some(EXECUTOR),
        "materialisation hands an individual-audience step to its executor"
    );
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({
        "scheduled_at": "2026-09-27T10:00:00Z",
        "duration_minutes": 90,
    });
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

#[tokio::test]
async fn the_executor_a_step_declares_claims_for_someone_else() {
    let (app, jobs, _calendar) = build_app();
    // The PINNED protocol names the executor; the step's own metadata
    // says nothing about it. The step is in the state materialisation
    // leaves it: Ready, and held by the executor itself.
    let (job, step) = materialised_run(&jobs).await;

    let (status, body) = claim(&app, &job, &step, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));
    assert_eq!(stored.status, StepStatus::Active);
    let (updated, assigned) = claim_events(&jobs, &step);
    assert_eq!(updated["_actor"], EXECUTOR, "signed by the executor");
    assert_eq!(assigned["claimed_by"], EXECUTOR);
    assert_eq!(assigned["claimed_for"], OTHER);

    // The same automation on a step its protocol does NOT declare it
    // for: refused.
    let (job2, step2) = scheduled(&jobs, None, serde_json::json!({})).await;
    let (status, body) = claim(&app, &job2, &step2, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// A holder of `step-assign` starts a materialised executor step for
/// someone else too: the step's declared executor is a holder the claim
/// may displace whoever makes it (backlog 5d1c0b7a).
#[tokio::test]
async fn a_holder_of_step_assign_claims_an_executor_held_step_for_someone_else() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = materialised_run(&jobs).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));
}

/// The negatives the displacement must not open. A caller who is neither
/// the executor nor a holder of `step-assign` is still 403 on a
/// materialised step; and a Ready step held by someone OTHER than the
/// executor or the caller is still 409 — to the executor too — and that
/// 409 does not call a Ready step "already claimed".
#[tokio::test]
async fn a_claim_for_displaces_only_the_executor_and_only_when_authorised() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = materialised_run(&jobs).await;
    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(EXECUTOR));
    assert_eq!(stored.status, StepStatus::Ready);

    // A plain claim does not take the executor's step either.
    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(!body.contains("already claimed"), "{body}");

    // Someone else holds this Ready step: nobody's claim-for takes it.
    let (job2, step2) = scheduled_as(&jobs, RUN, Some(LEAD), serde_json::json!({})).await;
    for caller in [user(EXECUTOR, "system"), user(TECH, "technician")] {
        let (status, body) = claim(&app, &job2, &step2, &caller, Some(OTHER)).await;
        if caller.id == EXECUTOR {
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert!(
                !body.contains("already claimed"),
                "a Ready step is not already claimed: {body}"
            );
            assert!(body.contains(LEAD), "the refusal names the holder: {body}");
        } else {
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        }
    }
    let stored = jobs.get_step(&step2.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(LEAD));
    assert_eq!(stored.status, StepStatus::Ready);
    assert!(calendar.live_for(&step2.id.to_string()).is_empty());
}

/// The claim-for marker names the holder the claim took the step FROM
/// (backlog ce8b7d66, S3 of the review of car 5d1c0b7a): `claimed_by`
/// and `claimed_for` said who acted and for whom, and nothing said that
/// the declared executor's hold was ended by it. A claim that displaced
/// no one says so by leaving the key out.
#[tokio::test]
async fn the_claim_for_marker_names_the_holder_it_displaced() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = materialised_run(&jobs).await;
    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, assigned) = claim_events(&jobs, &step);
    assert_eq!(assigned["claimed_for"], OTHER);
    assert_eq!(
        assigned["displaced"], EXECUTOR,
        "the marker names whose hold the claim ended: {assigned}"
    );

    let (job2, step2) = scheduled(&jobs, None, serde_json::json!({})).await;
    let (status, body) = claim(&app, &job2, &step2, &user(LEAD, "lead"), Some(TECH)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, assigned) = claim_events(&jobs, &step2);
    assert!(
        assigned.get("displaced").is_none(),
        "a step nobody held displaces no one: {assigned}"
    );
}

/// A Ready step held by the CALLER's own login is the caller's to hand
/// on (backlog ce8b7d66, S4 of the review of car 5d1c0b7a): the claim
/// may displace the caller, and the caller is every spelling the agents
/// registry ties to it — not only the id the login door signs with. It
/// answered 409 "held by someone else", naming the caller's own login.
#[tokio::test]
async fn a_claim_for_takes_a_step_held_by_the_callers_own_login() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, Some(AGENT_LOGIN), serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(AGENT, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));
    assert_eq!(stored.status, StepStatus::Active);
    let (_, assigned) = claim_events(&jobs, &step);
    assert_eq!(assigned["claimed_by"], AGENT);
    assert_eq!(assigned["displaced"], AGENT_LOGIN, "{assigned}");

    // Someone else's login is still someone else's hold.
    let (job2, step2) = scheduled(&jobs, Some(LEAD), serde_json::json!({})).await;
    let (status, body) = claim(&app, &job2, &step2, &user(AGENT, "lead"), Some(TECH)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains(LEAD), "{body}");
}

/// The assignment markers recorded for `step`, if any.
fn assigned_markers(jobs: &InMemoryJobs, step: &Step) -> Vec<serde_json::Value> {
    let sid = step.id.to_string();
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind.starts_with("step.assigned.") && e.payload["step_id"] == sid.as_str())
        .map(|e| e.payload)
        .collect()
}

/// A CLAIM FOR SOMEONE ELSE ENDS ONLY THE HOLD IT READ (backlog
/// ce8b7d66, the review of car eb2f9b0f, finding 1). The marker names the
/// holder the claim read as `displaced`, so the CAS may displace that
/// holder and no other: a step handed, between the claim's read and its
/// CAS, to another holder the claim would have been AUTHORISED to take
/// it from — the declared executor, or the caller itself — must not be
/// taken, or the marker names the wrong hold. Two shapes: the step was
/// free when read, and it was the executor's. Each answers a 409 naming
/// the new holder, says the claim may be sent again rather than calling
/// the caller's own hold someone else's, leaves the step with that
/// holder, records no assignment marker, and holds none of the
/// nominee's time. The width the CAS had before this car
/// (`declared` + caller) answered 200 to both.
#[tokio::test]
async fn a_claim_for_ends_only_the_hold_it_read() {
    for (read, handed_to) in [(None, EXECUTOR), (Some(EXECUTOR), LEAD)] {
        let (app, jobs, calendar) = build_app();
        let (job, step) = scheduled_as(&jobs, RUN, read, serde_json::json!({})).await;
        *calendar.hand_on_reserve.lock().unwrap() = Some((jobs.clone(), step.id, handed_to));

        let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
        assert_eq!(status, StatusCode::CONFLICT, "read {read:?}: {body}");
        let answer: serde_json::Value = serde_json::from_str(&body).expect("a JSON body");
        assert_eq!(
            answer["holder"], handed_to,
            "names who holds it now: {body}"
        );
        let error = answer["error"].as_str().unwrap_or_default();
        assert!(error.contains("send it again"), "retryable: {body}");
        assert!(!error.contains("someone else"), "{body}");

        let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(stored.assignee_id.as_deref(), Some(handed_to));
        assert_eq!(stored.status, StepStatus::Ready);
        assert!(
            assigned_markers(&jobs, &step).is_empty(),
            "read {read:?}: a refused claim records no marker"
        );
        assert!(
            calendar.live_for(&step.id.to_string()).is_empty(),
            "read {read:?}: the nominee's time is handed back"
        );
    }
}

/// An agents registry whose `call`-th `list` fails and every other
/// answers as `inner` does — a registry that goes dark for one read.
struct ListFailsOnCall {
    inner: InMemoryAgents,
    calls: std::sync::atomic::AtomicUsize,
    fail_on: usize,
}

#[async_trait]
impl AgentsRegistry for ListFailsOnCall {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        self.inner.resolve_login(login).await
    }
    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == self.fail_on {
            return Err(AgentsError::Storage("connection reset".into()));
        }
        self.inner.list().await
    }
    async fn publish(
        &self,
        rows: &[AgentInput],
        mode: boss_core::publish::PublishMode,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        self.inner.publish(rows, mode, stamp).await
    }
    async fn list_automations(
        &self,
    ) -> Result<Vec<boss_jobs::agents::AutomationActor>, AgentsError> {
        self.inner.list_automations().await
    }
    async fn declare_automations(
        &self,
        rows: &[boss_jobs::agents::AutomationActor],
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<boss_jobs::agents::AutomationsSeedOutcome, AgentsError> {
        self.inner.declare_automations(rows, stamp).await
    }
}

/// S4's "a registry that cannot answer is judged as before" (the review
/// of car eb2f9b0f, finding 3). A claim for someone else reads the
/// registry three times — to resolve the nominee, to name the caller's
/// logins, and for the holder's row — and the SECOND going dark must
/// neither refuse the claim nor widen it: the caller's login is not
/// displaceable (409, the step untouched), and the declared executor
/// still is (200).
#[tokio::test]
async fn a_claim_for_whose_caller_logins_cannot_be_read_is_judged_as_before() {
    let dark_second_read = || -> Arc<dyn AgentsRegistry> {
        Arc::new(ListFailsOnCall {
            inner: InMemoryAgents::new().with_agent(AGENT, [AGENT_LOGIN]),
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail_on: 2,
        })
    };

    let (app, jobs, _calendar) = build_app_with_agents(dark_second_read());
    let (job, step) = scheduled(&jobs, Some(AGENT_LOGIN), serde_json::json!({})).await;
    let (status, body) = claim(&app, &job, &step, &user(AGENT, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::CONFLICT, "not widened: {body}");
    assert!(body.contains(AGENT_LOGIN), "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(AGENT_LOGIN));
    assert_eq!(stored.status, StepStatus::Ready);

    let (app, jobs, _calendar) = build_app_with_agents(dark_second_read());
    let (job, step) = materialised_run(&jobs).await;
    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "not refused: {body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));
    let (_, assigned) = claim_events(&jobs, &step);
    assert_eq!(assigned["displaced"], EXECUTOR);
}

/// A caller naming ITS OWN login as `claimed_for` is claiming for
/// itself: the nominee is judged after it is resolved, not on the raw
/// spelling — which judged an agent's own login a claim for someone
/// else and refused it 403 (backlog 5d1c0b7a).
#[tokio::test]
async fn a_claim_for_your_own_login_is_an_ordinary_claim() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(
        &app,
        &job,
        &step,
        &user(AGENT, "technician"),
        Some(AGENT_LOGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(AGENT));
    let marker = jobs.recorded_events().into_iter().find(|e| {
        e.kind.starts_with("step.assigned.") && e.payload["step_id"] == step.id.to_string().as_str()
    });
    if let Some(marker) = marker {
        assert!(
            marker.payload.get("claimed_for").is_none(),
            "a claim for oneself records no claim-for: {}",
            marker.payload
        );
    }
}

/// H1: an audience in the step's METADATA is not a declaration — only
/// the protocol the packet is pinned to declares an executor. Stored
/// straight into the row, as a writer that got round the doors would
/// leave it.
#[tokio::test]
async fn an_audience_in_step_metadata_makes_no_one_an_executor() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(
        &jobs,
        None,
        serde_json::json!({ "audience": { "individual": TECH } }),
    )
    .await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

/// H1, in the shape of the review's probe: a caller with no
/// `step-assign` PATCHes itself in as the step's audience, then claims
/// the step for someone else. The PATCH is refused (`audience` is the
/// protocol's), and the claim is refused whatever the PATCH did.
#[tokio::test]
async fn a_writer_cannot_patch_itself_into_the_executor() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&user(TECH, "technician")).unwrap(),
                )
                .body(Body::from(
                    serde_json::json!({ "audience": { "individual": TECH } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("audience"),
        "the refusal names the key: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(stored.metadata.get("audience").is_none());

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None);
}

/// S1: the claim CAS judges status and holder, not the row, so a
/// schedule PATCHed between the claim's read and its CAS lands under
/// it. The hold follows the time as STORED, and the reservation made
/// for the old time is handed back.
#[tokio::test]
async fn a_schedule_moved_under_a_claim_is_held_as_stored() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;
    *calendar.move_on_reserve.lock().unwrap() =
        Some((jobs.clone(), step.id, "2026-09-27T14:00:00Z"));

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["scheduled_at"], "2026-09-27T14:00:00Z");
    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(
        holds.len(),
        1,
        "one hold, not the old one beside the new: {holds:?}"
    );
    let at = chrono::DateTime::parse_from_rfc3339("2026-09-27T14:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(holds[0].window.start, at, "the hold is on the stored time");
}

/// S2: a nominee is resolved to the id the agents registry knows before
/// anything is reserved or recorded — an agent's login becomes its
/// registered id on the step, the event and the marker alike.
#[tokio::test]
async fn a_claim_for_an_agents_login_holds_its_registered_id() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(AGENT_LOGIN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(AGENT));

    let (updated, assigned) = claim_events(&jobs, &step);
    assert_eq!(updated["assignee_id"], AGENT);
    assert_eq!(assigned["claimed_for"], AGENT);
    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(holds.len(), 1, "{holds:?}");
    assert_eq!(holds[0].subject, Subject::new("employee", AGENT));
}

/// S2: a nominee neither the roster nor the agents registry knows is
/// refused, with nothing reserved or stored.
#[tokio::test]
async fn a_claim_for_someone_no_registry_knows_is_refused() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some("emp-nobody")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("emp-nobody"),
        "the refusal names the nominee: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

/// S3: an `individual` audience is claim-for authority — its executor
/// may start the step for anyone with no `step-assign` grant. So every
/// one the platform bundle declares names an automation, never a person
/// or an agent login, who would hold that authority by virtue of a
/// protocol row. Kept here, beside the rule it guards, rather than on
/// the tail of `platform_bundle.rs` (§9a's contended tail line).
#[test]
fn every_individual_executor_in_the_platform_bundle_is_an_automation() {
    let rows = boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the platform bundle parses");
    let declared: Vec<String> = rows
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(move |s| match &s.audience {
                Some(Audience::Individual(id)) => {
                    Some(format!("{} v{} step {}: {id}", w.kind, w.version, s.title))
                }
                _ => None,
            })
        })
        .collect();
    assert!(
        !declared.is_empty(),
        "a bundle with no individual audience proves nothing"
    );
    let offenders: Vec<&String> = declared
        .iter()
        .filter(|d| !d.contains(": automation:"))
        .collect();
    assert!(
        offenders.is_empty(),
        "an individual audience names someone who is not an automation: {offenders:?}"
    );
}

/// BOTH DOORS START A STEP THROUGH ONE FUNCTION (design 611fbffd). The
/// reservation moved out of `update_step` so the claim could make it;
/// a later edit that inlined it back into one door would leave the other
/// starting steps with no hold, which is the defect this pins. Read from
/// the handler source, because "calls the same function" is a property
/// of the code, not of any one request.
/// ONE DOOR STARTS A STEP, AND IT RESERVES (backlog e639899a (2)). This
/// pin held BOTH the step PUT and the claim door to `start_hold` while
/// both could start a step. The PUT's move to Active from Ready or
/// Pending is refused before it reaches the calendar (`opens_by_put`),
/// so its `start_hold` could reserve nothing — dead code the old pin
/// kept alive. The claim door calls the start and settles it; the PUT
/// calls neither, and settles only the release a skip owes
/// (`after_step_written`).
#[test]
fn only_the_claim_door_starts_a_step_and_reserves() {
    let src = include_str!("../src/http/steps.rs");
    let body_of = |name: &str| -> &str {
        let head = format!("async fn {name}<");
        let start = src.find(&head).unwrap_or_else(|| panic!("no fn {name}"));
        let rest = &src[start + head.len()..];
        let end = ["\npub(super) async fn ", "\nasync fn "]
            .into_iter()
            .filter_map(|boundary| rest.find(boundary))
            .min()
            .unwrap_or(rest.len());
        &rest[..end]
    };
    let claim = body_of("claim_step");
    for call in ["start_hold(", "settle_start_hold("] {
        assert!(claim.contains(call), "claim_step must call {call}");
    }
    let put = body_of("update_step");
    assert!(
        put.contains("update_step_with_condition("),
        "the PUT must use the shared status-write handler"
    );
    let write = body_of("update_step_with_condition");
    for call in ["start_hold(", "settle_start_hold("] {
        assert!(
            !put.contains(call) && !write.contains(call),
            "update_step must not call {call}: a PUT never starts a step"
        );
    }
    assert!(
        write.contains("calendar_hook::after_step_written("),
        "a PUT's skip of an Active step still releases its hold"
    );
    for door in ["add_step", "update_step", "update_step_with_condition"] {
        assert!(
            !body_of(door).contains("apply_step_transition("),
            "{door} must not reserve"
        );
    }
    assert_eq!(
        src.matches("calendar_hook::apply_step_transition(").count(),
        1,
        "the reservation is made in one place, the shared start"
    );
}
