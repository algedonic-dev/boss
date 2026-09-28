//! A step write lands only inside the caller's scope, and a claim only
//! for a caller who holds the step's authority.
//!
//! Backlog 0a8a2463 (the re-review of car 94469495, 2026-09-25): the
//! step write doors checked only `(Update, step)` and threw away the
//! scope that grant carries. `update_step`, the merge door and
//! `claim_step` never asked whether the PARENT packet was inside it, and
//! the claim never asked for the step's `authority_role`. Since car
//! 94469495 a step that is the caller's own work opens its packet
//! (`step_is_callers`), so a `self`-scoped caller who knew a job id (the
//! live board lists them) and a step id could PUT `assignee_id` to
//! themselves on another owner's Ready step — or release an Active one
//! and claim it — and read the whole packet: taking a step was a way to
//! read it, and to take another team's work.
//!
//! Backlog aba2bb26 (same review, one car): the ROLE half of
//! `step_is_callers` ignored the packet's status, so a role-claimable
//! Ready step left on a closed or cancelled packet opened it to every
//! holder of that role forever — the queue's SQL already asks for an
//! open packet. And `all_assigned=true` from any scoped caller made the
//! store read its 50,000-row ceiling before the scope cut.
//!
//! Each write now judges the stored step and its packet the way the
//! reads do: the packet is inside the scope the `(Update, step)` grant
//! carries, or the step is already the caller's own work. A step outside
//! both answers exactly as a step that does not exist. The claim for
//! oneself also needs the step's authority: the role, as the caller
//! holds it, or the policy's `step-signoff:<role>`, or `step-assign`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// Owned by the brewer — inside its `self` scope.
const OWN: &str = "0a8a0001-0000-0000-0000-000000000000";
/// Owned by somebody else, carrying cellar-tech work only.
const OTHERS: &str = "0a8a0002-0000-0000-0000-000000000000";
/// Owned by somebody else, with a step claimable by the brewer's role.
const CLAIMABLE: &str = "0a8a0003-0000-0000-0000-000000000000";
/// Owned by somebody else and CLOSED, with a leftover Ready step whose
/// authority is the brewer's role.
const CLOSED: &str = "0a8a0004-0000-0000-0000-000000000000";
/// Owned by somebody else and CLOSED, with a step the brewer completed.
const CLOSED_HANDED: &str = "0a8a0005-0000-0000-0000-000000000000";

const BREWER: &str = "emp-brewer";
const HELPER: &str = "emp-helper";

fn id(s: &str) -> JobId {
    JobId::from_uuid(Uuid::parse_str(s).expect("uuid"))
}

fn user(id: &str, role: &str, tier: AccessTier) -> User {
    User {
        id: id.into(),
        role: role.into(),
        access_tier: tier,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// `self` on job and step: the packets it owns, and its own work.
fn brewer() -> User {
    user(BREWER, "brewer", AccessTier::User)
}
fn cellar_tech() -> User {
    user("emp-cellar", "cellar-tech", AccessTier::User)
}
/// The control: every packet, and `step-assign`.
fn operator() -> User {
    user("emp-david", "platform-admin", AccessTier::Operator)
}

fn policy() -> Arc<dyn PolicyClient> {
    let mut b = FakePolicyClient::builder();
    for role in ["brewer", "cellar-tech"] {
        b = b
            .allow(role, Action::Read, Resource::job(), Scope::Self_)
            .allow(role, Action::Update, Resource::step(), Scope::Self_);
    }
    Arc::new(
        b.allow("platform-admin", Action::Read, Resource::job(), Scope::All)
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
    )
}

fn packet(job: &str, owner: &str, status: JobStatus) -> Job {
    Job {
        id: id(job),
        kind: "brew-day".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "FV-1"),
        title: format!("packet {job}"),
        owner_id: owner.into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).expect("day"),
        opened_at: None,
        due_on: None,
        closed_on: (status != JobStatus::Open)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 26).expect("day")),
        metadata: Value::Null,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn step(job: &str, title: &str, status: StepStatus, holder: Option<&str>, role: &str) -> Step {
    let mut s = Step::new(id(job), "task", title, 0);
    s.status = status;
    s.assignee_id = holder.map(str::to_string);
    s.metadata = json!({ "authority_role": role });
    s
}

struct Fixture {
    app: Router,
    jobs: Arc<InMemoryJobs>,
    /// OTHERS: Ready, nobody holds it, cellar-tech's authority.
    for_cellar: StepId,
    /// OTHERS: Active, held by the helper, cellar-tech's authority.
    held_by_helper: StepId,
    /// OWN: Ready, nobody holds it, qa-tech's authority.
    needs_qa: StepId,
    /// CLAIMABLE: Ready, nobody holds it, the brewer's authority.
    claimable_by_brewer: StepId,
}

async fn fixture() -> Fixture {
    let jobs = Arc::new(InMemoryJobs::new());
    for (job, owner, status) in [
        (OWN, BREWER, JobStatus::Open),
        (OTHERS, "emp-other", JobStatus::Open),
        (CLAIMABLE, "emp-other", JobStatus::Open),
        (CLOSED, "emp-other", JobStatus::Closed),
        (CLOSED_HANDED, "emp-other", JobStatus::Closed),
    ] {
        jobs.create_job(&packet(job, owner, status))
            .await
            .expect("packet");
    }
    let for_cellar = step(OTHERS, "for-cellar", StepStatus::Ready, None, "cellar-tech");
    let held_by_helper = step(
        OTHERS,
        "held-by-helper",
        StepStatus::Active,
        Some(HELPER),
        "cellar-tech",
    );
    let needs_qa = step(OWN, "needs-qa", StepStatus::Ready, None, "qa-tech");
    let claimable_by_brewer = step(
        CLAIMABLE,
        "claimable-by-brewer",
        StepStatus::Ready,
        None,
        "brewer",
    );
    let leftover = step(CLOSED, "leftover", StepStatus::Ready, None, "brewer");
    let done_by_brewer = step(
        CLOSED_HANDED,
        "done-by-brewer",
        StepStatus::Completed,
        Some(BREWER),
        "brewer",
    );
    for s in [
        &for_cellar,
        &held_by_helper,
        &needs_qa,
        &claimable_by_brewer,
        &leftover,
        &done_by_brewer,
    ] {
        jobs.add_step(s).await.expect("step");
    }

    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        step_registry: Arc::new(StepRegistry::v1()),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy(),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    Fixture {
        app: router(state),
        jobs,
        for_cellar: for_cellar.id,
        held_by_helper: held_by_helper.id,
        needs_qa: needs_qa.id,
        claimable_by_brewer: claimable_by_brewer.id,
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    who: &User,
    body: Option<Value>,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", serde_json::to_string(who).expect("user"))
        .header("content-type", "application/json");
    let req = match body {
        Some(b) => req.body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn step_uri(job: &str, step: &StepId) -> String {
    format!("/api/jobs/{job}/steps/{step}")
}

/// A step id no row carries, on the same packet: what "does not exist"
/// answers, to compare a refusal against.
fn absent_step() -> StepId {
    Step::new(id(OTHERS), "task", "absent", 0).id
}

async fn stored(jobs: &InMemoryJobs, s: &StepId) -> Step {
    jobs.get_step(s).await.expect("read").expect("step")
}

// ---------------------------------------------------------------------
// Outside scope: a step write answers as a step that does not exist.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_scoped_caller_cannot_take_another_owners_step_by_put() {
    let f = fixture().await;
    let before = stored(&f.jobs, &f.for_cellar).await;
    let taken = send(
        &f.app,
        "PUT",
        &step_uri(OTHERS, &f.for_cellar),
        &brewer(),
        Some(json!({ "assignee_id": BREWER })),
    )
    .await;
    let absent = send(
        &f.app,
        "PUT",
        &step_uri(OTHERS, &absent_step()),
        &brewer(),
        Some(json!({ "assignee_id": BREWER })),
    )
    .await;
    assert_eq!(taken.0, StatusCode::NOT_FOUND, "{}", taken.1);
    assert_eq!(taken, absent, "a refusal must not tell a real step apart");
    assert_eq!(
        stored(&f.jobs, &f.for_cellar).await,
        before,
        "nothing written"
    );

    // And so the packet stays unreadable.
    let (status, body) = send(
        &f.app,
        "GET",
        &format!("/api/jobs/{OTHERS}"),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_scoped_caller_cannot_release_another_owners_active_step() {
    let f = fixture().await;
    let before = stored(&f.jobs, &f.held_by_helper).await;
    let (status, body) = send(
        &f.app,
        "PUT",
        &step_uri(OTHERS, &f.held_by_helper),
        &brewer(),
        Some(json!({ "status": "ready", "assignee_id": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(stored(&f.jobs, &f.held_by_helper).await, before);
}

#[tokio::test]
async fn a_scoped_caller_cannot_write_another_owners_step_through_the_merge_door() {
    let f = fixture().await;
    let before = stored(&f.jobs, &f.for_cellar).await;
    let (status, body) = send(
        &f.app,
        "PATCH",
        &format!("{}/metadata", step_uri(OTHERS, &f.for_cellar)),
        &brewer(),
        Some(json!({ "note": "mine now" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(stored(&f.jobs, &f.for_cellar).await, before);
}

#[tokio::test]
async fn a_scoped_caller_cannot_claim_another_owners_step() {
    let f = fixture().await;
    let before = stored(&f.jobs, &f.for_cellar).await;
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("{}/claim", step_uri(OTHERS, &f.for_cellar)),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(stored(&f.jobs, &f.for_cellar).await, before);
}

// ---------------------------------------------------------------------
// The claim asks for the step's authority, even inside scope.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_claim_needs_the_steps_authority_even_on_the_callers_own_packet() {
    let f = fixture().await;
    let before = stored(&f.jobs, &f.needs_qa).await;
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("{}/claim", step_uri(OWN, &f.needs_qa)),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("qa-tech"),
        "the refusal names the authority: {body}"
    );
    assert_eq!(stored(&f.jobs, &f.needs_qa).await, before);
}

// ---------------------------------------------------------------------
// Controls: the caller's own work, and the operator, are unchanged.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_role_holder_still_claims_and_completes_its_pool_on_a_packet_it_does_not_own() {
    let f = fixture().await;
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("{}/claim", step_uri(CLAIMABLE, &f.claimable_by_brewer)),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(
        &f.app,
        "PUT",
        &step_uri(CLAIMABLE, &f.claimable_by_brewer),
        &brewer(),
        Some(json!({ "status": "completed" })),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(
        stored(&f.jobs, &f.claimable_by_brewer).await.status,
        StepStatus::Completed
    );

    // The other role's pool, likewise, for its own holder.
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("{}/claim", step_uri(OTHERS, &f.for_cellar)),
        &cellar_tech(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_operator_with_step_assign_claims_any_step() {
    let f = fixture().await;
    for (job, s) in [(OTHERS, f.for_cellar), (OWN, f.needs_qa)] {
        let (status, body) = send(
            &f.app,
            "POST",
            &format!("{}/claim", step_uri(job, &s)),
            &operator(),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{job}: {body}");
    }
}

// ---------------------------------------------------------------------
// aba2bb26: a closed packet is nobody's claimable pool.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_leftover_role_step_does_not_open_a_closed_packet() {
    let f = fixture().await;
    for route in ["", "/steps", "/events"] {
        let (status, body) = send(
            &f.app,
            "GET",
            &format!("/api/jobs/{CLOSED}{route}"),
            &brewer(),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}: {body}");
    }
    // Control: a step HANDED to the caller still opens its packet after
    // the packet closes — the page re-reads what the caller completed.
    let (status, body) = send(
        &f.app,
        "GET",
        &format!("/api/jobs/{CLOSED_HANDED}"),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_whole_backlog_is_refused_to_a_scoped_caller() {
    let f = fixture().await;
    let (status, body) = send(
        &f.app,
        "GET",
        "/api/jobs/assignments?all_assigned=true&limit=1",
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("assignee_id"),
        "names the door that works: {body}"
    );

    // Control: the caller's own queue still answers.
    let (status, body) = send(
        &f.app,
        "GET",
        &format!("/api/jobs/assignments?assignee_id={BREWER}&roles=brewer"),
        &brewer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("claimable-by-brewer"), "{body}");
    assert!(!body.contains("leftover"), "{body}");

    // And the operator, whose scope cuts nothing, reads the backlog.
    let (status, body) = send(
        &f.app,
        "GET",
        "/api/jobs/assignments?all_assigned=true",
        &operator(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
