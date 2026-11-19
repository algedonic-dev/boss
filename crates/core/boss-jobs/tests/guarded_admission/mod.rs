//! The guarded-admission scenarios, written once against the PORT and
//! run on both adapters (backlog 84a010cb, car 3; design f1d66249), so
//! the in-memory and Postgres adapters are held to one contract rather
//! than to two copies of it. Included by
//! `a_guarded_admission_is_one_jobs_transaction.rs` (in-memory) and
//! `a_guarded_admission_is_one_jobs_transaction_pg.rs` (Postgres, plus
//! the writer races only a real database can show).
//!
//! The world is the packet's own consumer, built from data: an inquiry
//! (`receive-an-inquiry`) whose `sent` step is the answer signal, and a
//! `delayed-admission` intent captured seven days before its due
//! instant, whose envelope pins a `follow-up-a-prospect` child.

#![allow(dead_code)]

use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::events::{JOB_CREATED, STEP_CREATED, STEP_UPDATED, step_state_payload};
use boss_jobs::guarded_admission::{Outcome, Request};
use chrono::{DateTime, NaiveDate, TimeDelta, TimeZone, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

pub const SOURCE: u128 = 0x84a0_10cb_0000_0000_0000_0000_0000_00a1;
pub const INTENT: u128 = 0x84a0_10cb_0000_0000_0000_0000_0000_00b1;
pub const CHILD: u128 = 0x84a0_10cb_0000_0000_0000_0000_0000_00c1;

pub fn id(n: u128) -> JobId {
    JobId::from_uuid(Uuid::from_u128(n))
}

/// The source fact's own timestamp; the due instant is exactly 604800
/// seconds later.
pub fn anchor() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap()
}

pub fn due_at() -> DateTime<Utc> {
    anchor() + TimeDelta::seconds(604_800)
}

pub fn stamp_at(at: DateTime<Utc>) -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("dispatcher".into())).with_timestamp(at)
}

fn job(n: u128, kind: &str, title: &str, metadata: Value) -> Job {
    Job {
        id: id(n),
        status: JobStatus::Open,
        metadata,
        ..Job::new(
            kind,
            Subject::new("custom", "inquiry-1"),
            title,
            "automation:dispatcher",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
        )
    }
}

fn step(job: &Job, slug: &str, status: StepStatus, order: i32) -> Step {
    let mut s = Step::new(job.id, "task", slug, order);
    s.spec_slug = Some(slug.into());
    s.status = status;
    s
}

pub fn envelope() -> Value {
    json!({
        "contract_version": 1,
        "intent_id": Uuid::from_u128(INTENT).to_string(),
        "child_id": Uuid::from_u128(CHILD).to_string(),
        "child": {"kind": "follow-up-a-prospect", "workflow_version": 1, "partition": "real"},
        "source_event": {"id": "e-1", "kind": "jobs.job.opened", "timestamp": "2026-11-08T12:00:00Z"},
        "anchor": "2026-11-08T12:00:00Z",
        "delay_seconds": 604_800,
        "due_at": "2026-11-15T12:00:00Z",
        "rule": {"name": "follow-up-an-unanswered-inquiry", "version": 1},
        "guard": {
            "source_job": Uuid::from_u128(SOURCE).to_string(),
            "source_kind": "receive-an-inquiry",
            "steps": [{"step": "sent", "allowed": ["pending", "ready", "active"]}],
        },
    })
}

pub struct World {
    pub source: Job,
    pub sent: Step,
    pub triage: Step,
    pub intent: Job,
    pub due: Step,
}

async fn admit(repo: &dyn JobsRepository, job: &Job, steps: &[Step], at: DateTime<Utc>) {
    let stamp = stamp_at(at);
    let job_events = vec![stamp.event(JOB_CREATED, serde_json::to_value(job).unwrap())];
    let step_events: Vec<Event> = steps
        .iter()
        .map(|s| stamp.event(STEP_CREATED, step_state_payload(s)))
        .collect();
    let admission = repo
        .create_job_with_steps_at(job, steps, at, &job_events, &step_events)
        .await
        .expect("planted");
    assert_eq!(admission, boss_jobs::Admission::Admitted, "planted once");
}

/// The inquiry, still unanswered, and its captured intent.
pub async fn plant(repo: &dyn JobsRepository) -> World {
    let source = job(SOURCE, "receive-an-inquiry", "an inquiry", json!({}));
    let sent = step(&source, "sent", StepStatus::Ready, 0);
    let triage = step(&source, "triage", StepStatus::Ready, 1);
    admit(repo, &source, &[sent.clone(), triage.clone()], anchor()).await;
    let intent = job(
        INTENT,
        "delayed-admission",
        "follow up an unanswered inquiry",
        json!({"envelope": envelope()}),
    );
    let captured = step(&intent, "captured", StepStatus::Completed, 0);
    let due = step(&intent, "due", StepStatus::Ready, 1);
    admit(repo, &intent, &[captured, due.clone()], anchor()).await;
    World {
        source,
        sent,
        triage,
        intent,
        due,
    }
}

pub fn child() -> Job {
    let mut j = job(
        CHILD,
        "follow-up-a-prospect",
        "follow up a prospect",
        json!({}),
    );
    j.workflow_version = 1;
    j
}

pub fn child_steps() -> Vec<Step> {
    let c = child();
    vec![
        step(&c, "write", StepStatus::Ready, 0),
        step(&c, "send", StepStatus::Pending, 1),
    ]
}

pub fn request(now: DateTime<Utc>) -> Request {
    let stamp = stamp_at(now);
    let child = child();
    let child_steps = child_steps();
    Request {
        intent: id(INTENT),
        child_job_events: vec![stamp.event(JOB_CREATED, serde_json::to_value(&child).unwrap())],
        child_step_events: child_steps
            .iter()
            .map(|s| stamp.event(STEP_CREATED, step_state_payload(s)))
            .collect(),
        child,
        child_steps,
        now,
        actor: ActorId::Automation("dispatcher".into()),
    }
}

pub fn due_events(now: DateTime<Utc>) -> impl Fn(&Step) -> Vec<Event> + Send + Sync {
    move |s: &Step| vec![stamp_at(now).event(STEP_UPDATED, step_state_payload(s))]
}

pub async fn attempt(repo: &dyn JobsRepository, now: DateTime<Utc>) -> Outcome {
    repo.guarded_admission_at(&request(now), &due_events(now))
        .await
        .expect("the attempt runs")
}

/// The answer, through the real step write door.
pub async fn answer(repo: &dyn JobsRepository, sent: &Step, at: DateTime<Utc>) {
    let (mut s, version) = repo
        .get_step_versioned(&sent.id)
        .await
        .unwrap()
        .expect("the sent step");
    s.status = StepStatus::Completed;
    s.completed_at = Some(at);
    s.completed_on = Some(at.date_naive());
    s.completed_by = Some(ActorId::human("emp-sales"));
    let event = stamp_at(at).event(STEP_UPDATED, step_state_payload(&s));
    repo.update_step_if_unchanged_at(&s, version, at, &[event])
        .await
        .expect("the answer lands");
}

/// The source's close, through the real close door.
pub async fn close_source(repo: &dyn JobsRepository, at: DateTime<Utc>) {
    let mut owned = serde_json::Map::new();
    owned.insert("closed_at".into(), json!(at.to_rfc3339()));
    repo.close_job_at(
        &id(SOURCE),
        at.date_naive(),
        &owned,
        &stamp_at(at),
        &|_: &Job| Vec::new(),
    )
    .await
    .expect("the close runs")
    .expect("the source was open");
}

pub async fn due_now(repo: &dyn JobsRepository, w: &World) -> Step {
    repo.get_step(&w.due.id).await.unwrap().expect("due step")
}

/// Nothing decided and no child: what every non-writing outcome leaves.
pub async fn nothing_written(repo: &dyn JobsRepository, w: &World) {
    assert!(
        repo.get_job(&id(CHILD)).await.unwrap().is_none(),
        "no child"
    );
    let due = due_now(repo, w).await;
    assert_eq!(due.status, StepStatus::Ready, "the intent is undecided");
    assert!(due.metadata.get("result").is_none(), "{:?}", due.metadata);
}

// ---------------------------------------------------------------------------
// The scenarios
// ---------------------------------------------------------------------------

/// At the exact due instant, an unanswered inquiry admits its child: the
/// child under the envelope's id with every step, and the intent's `due`
/// step decided `admitted` naming that child.
pub async fn a_due_unanswered_intent_admits_its_child_and_decides(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let outcome = attempt(repo, due_at()).await;
    let Outcome::Admitted {
        child,
        reconciled,
        due,
    } = outcome
    else {
        panic!("admitted: {outcome:?}");
    };
    assert_eq!(child, id(CHILD));
    assert!(!reconciled);
    let stored = repo.get_job(&id(CHILD)).await.unwrap().expect("the child");
    assert_eq!(stored.kind, "follow-up-a-prospect");
    assert_eq!(repo.list_steps(&id(CHILD)).await.unwrap().len(), 2);
    let decided = due_now(repo, &w).await;
    assert_eq!(decided.status, StepStatus::Completed);
    assert_eq!(decided.metadata["result"], "admitted");
    assert_eq!(decided.metadata["child_id"], json!(id(CHILD)));
    assert_eq!(decided.metadata["due_at"], "2026-11-15T12:00:00Z");
    assert_eq!(decided.completed_at, Some(due_at()));
    assert_eq!(decided.metadata, due.metadata, "the answer is the row");
}

/// One second early nothing is judged and nothing is written.
pub async fn nothing_is_written_before_the_due_instant(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    assert_eq!(
        attempt(repo, due_at() - TimeDelta::seconds(1)).await,
        Outcome::NotDue { due_at: due_at() }
    );
    nothing_written(repo, &w).await;
}

/// The inquiry was answered before the bound: suppressed, no child, and
/// the decision names what it saw.
pub async fn an_answered_inquiry_suppresses_and_admits_no_child(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    answer(repo, &w.sent, due_at() - TimeDelta::days(1)).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(&outcome, Outcome::Suppressed { reason, .. } if reason.contains("completed")),
        "{outcome:?}"
    );
    assert!(
        repo.get_job(&id(CHILD)).await.unwrap().is_none(),
        "no child"
    );
    let decided = due_now(repo, &w).await;
    assert_eq!(decided.status, StepStatus::Completed);
    assert_eq!(decided.metadata["result"], "suppressed");
    let observed: Value =
        serde_json::from_str(decided.metadata["observed"].as_str().unwrap()).unwrap();
    assert_eq!(observed["steps"][0]["status"], "completed");
}

/// The source closed before the bound: suppressed.
pub async fn a_closed_source_suppresses(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    close_source(repo, due_at() - TimeDelta::hours(1)).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(&outcome, Outcome::Suppressed { reason, .. } if reason.contains("closed")),
        "{outcome:?}"
    );
    assert!(repo.get_job(&id(CHILD)).await.unwrap().is_none());
    assert_eq!(due_now(repo, &w).await.metadata["result"], "suppressed");
}

/// Unrelated work on the source — triage completed the day before the
/// bound — neither moves the anchor nor suppresses.
pub async fn unrelated_work_does_not_move_the_anchor(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let (mut t, v) = repo
        .get_step_versioned(&w.triage.id)
        .await
        .unwrap()
        .unwrap();
    let at = due_at() - TimeDelta::days(1);
    t.status = StepStatus::Completed;
    t.completed_at = Some(at);
    repo.update_step_if_unchanged_at(&t, v, at, &[])
        .await
        .unwrap();
    assert_eq!(
        attempt(repo, due_at() - TimeDelta::seconds(1)).await,
        Outcome::NotDue { due_at: due_at() },
        "the bound is the envelope's, not the newest completion's"
    );
    assert!(matches!(
        attempt(repo, due_at()).await,
        Outcome::Admitted { .. }
    ));
}

/// A retry after the decision committed answers the recorded decision and
/// writes nothing more — one child, one decision.
pub async fn a_retry_after_the_decision_writes_nothing_more(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let first = attempt(repo, due_at()).await;
    let Outcome::Admitted { due, .. } = first else {
        panic!("{first:?}");
    };
    let later = due_at() + TimeDelta::days(2);
    assert_eq!(
        attempt(repo, later).await,
        Outcome::AlreadyDecided { due: due.clone() }
    );
    assert_eq!(due_now(repo, &w).await.completed_at, Some(due_at()));
    assert_eq!(repo.list_steps(&id(CHILD)).await.unwrap().len(), 2);
}

/// The child already exists under the envelope's id — and is CLOSED —
/// while the intent is undecided: the attempt reconciles that exact
/// child rather than admitting another graph. The planted child is the
/// request's on every field admission compares, steps included (review
/// d6774dfd: this scenario once planted one of its two steps, so it
/// pinned only the kind/version/partition check).
pub async fn an_existing_closed_child_is_reconciled_not_admitted_twice(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let mut closed = child();
    closed.status = JobStatus::Closed;
    closed.closed_on = Some(due_at().date_naive());
    admit(repo, &closed, &child_steps(), due_at()).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(
            outcome,
            Outcome::Admitted {
                reconciled: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        repo.list_steps(&id(CHILD)).await.unwrap().len(),
        2,
        "no second graph hung on the existing child"
    );
    assert_eq!(
        repo.get_job(&id(CHILD)).await.unwrap().unwrap().status,
        JobStatus::Closed
    );
    assert_eq!(due_now(repo, &w).await.metadata["reconciled"], true);
}

/// A packet of another kind under the child id is a conflict: refused,
/// nothing written over it and nothing decided.
pub async fn a_stranger_under_the_child_id_is_refused(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let mut stranger = child();
    stranger.kind = "backlog-item".into();
    admit(repo, &stranger, &[], due_at()).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
    let due = due_now(repo, &w).await;
    assert_eq!(due.status, StepStatus::Ready);
}

/// A packet of the child's own kind, version and partition, planted
/// under the child id by another writer days before the due instant —
/// but about another subject, under another title and owner — is not
/// this intent's child. Refused: the intent's decision never vouches for
/// a packet the delayed rule did not write (review d6774dfd, B1), and
/// nothing is written over it or decided.
pub async fn a_same_kind_packet_about_another_subject_under_the_child_id_is_refused(
    repo: &dyn JobsRepository,
) {
    let w = plant(repo).await;
    let mut planted = child();
    planted.subject = Subject::new("custom", "someone-else-entirely");
    planted.title = "planted before the due instant".into();
    planted.owner_id = "emp-mallory".into();
    planted.metadata = json!({"planted_by": "mallory"});
    admit(repo, &planted, &[], due_at() - TimeDelta::days(6)).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(&outcome, Outcome::Refused { reason } if reason.contains("subject")),
        "{outcome:?}"
    );
    let due = due_now(repo, &w).await;
    assert_eq!(due.status, StepStatus::Ready, "the intent is undecided");
    assert!(due.metadata.get("result").is_none(), "{:?}", due.metadata);
    let kept = repo.get_job(&id(CHILD)).await.unwrap().expect("planted");
    assert_eq!(kept.subject.id, "someone-else-entirely", "nothing over it");
    assert!(repo.list_steps(&id(CHILD)).await.unwrap().is_empty());
}

/// The requested child's job row stands under the child id but its step
/// graph does not — here none of its steps. Reconciling it would complete
/// the intent `admitted` for a child that never got the graph the
/// transaction promises, and consume the intent for good (review b12f08eb,
/// P1). Refused: nothing written over it, nothing decided.
pub async fn a_child_without_its_step_graph_is_refused_not_reconciled(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    admit(repo, &child(), &[], due_at() - TimeDelta::days(1)).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(&outcome, Outcome::Refused { reason } if reason.contains("step graph")),
        "{outcome:?}"
    );
    let due = due_now(repo, &w).await;
    assert_eq!(due.status, StepStatus::Ready, "the intent is undecided");
    assert!(due.metadata.get("result").is_none(), "{:?}", due.metadata);
    assert!(
        repo.list_steps(&id(CHILD)).await.unwrap().is_empty(),
        "no graph hung on it"
    );
}

/// A packet of another kind stands under the child id and the guard says
/// suppress: the occupied identity is judged BEFORE either terminal
/// decision, so the conflict is refused rather than recorded `suppressed`
/// beside it (review b12f08eb, P2).
pub async fn a_conflicting_child_is_refused_even_when_the_guard_suppresses(
    repo: &dyn JobsRepository,
) {
    let w = plant(repo).await;
    let mut stranger = child();
    stranger.kind = "backlog-item".into();
    admit(repo, &stranger, &[], due_at() - TimeDelta::days(2)).await;
    answer(repo, &w.sent, due_at() - TimeDelta::days(1)).await;
    let outcome = attempt(repo, due_at()).await;
    assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
    let due = due_now(repo, &w).await;
    assert_eq!(due.status, StepStatus::Ready, "the intent is undecided");
    assert!(due.metadata.get("result").is_none(), "{:?}", due.metadata);
}

/// A child step whose job id is not the child's — here the guard's own
/// source — is refused before anything is judged: the port writes the
/// child graph and nothing else (review d6774dfd, B2).
pub async fn a_child_step_of_another_job_is_refused(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let before = repo.list_steps(&id(SOURCE)).await.unwrap().len();
    let mut r = request(due_at());
    let mut stray = Step::new(id(SOURCE), "task", "sent", 9);
    stray.spec_slug = Some("sent".into());
    stray.status = StepStatus::Completed;
    r.child_steps.push(stray.clone());
    r.child_step_events
        .push(stamp_at(due_at()).event(STEP_CREATED, step_state_payload(&stray)));
    let outcome = repo
        .guarded_admission_at(&r, &due_events(due_at()))
        .await
        .unwrap();
    assert!(
        matches!(&outcome, Outcome::Refused { reason } if reason.contains(&stray.id.to_string())),
        "{outcome:?}"
    );
    assert_eq!(repo.list_steps(&id(SOURCE)).await.unwrap().len(), before);
    nothing_written(repo, &w).await;
}

/// The guard names a step the source does not have: held, nothing
/// written — absence is not a negative fact.
pub async fn a_missing_guard_step_is_held(repo: &dyn JobsRepository) {
    // The source carries no `sent` step at all.
    let source = job(SOURCE, "receive-an-inquiry", "an inquiry", json!({}));
    let triage = step(&source, "triage", StepStatus::Ready, 0);
    admit(repo, &source, std::slice::from_ref(&triage), anchor()).await;
    let intent = job(
        INTENT,
        "delayed-admission",
        "follow up",
        json!({"envelope": envelope()}),
    );
    let due = step(&intent, "due", StepStatus::Ready, 1);
    admit(
        repo,
        &intent,
        &[
            step(&intent, "captured", StepStatus::Completed, 0),
            due.clone(),
        ],
        anchor(),
    )
    .await;
    let outcome = attempt(repo, due_at()).await;
    assert!(
        matches!(&outcome, Outcome::Held { reason, .. } if reason.contains("sent")),
        "{outcome:?}"
    );
    let w = World {
        sent: triage.clone(),
        triage,
        source,
        intent,
        due,
    };
    nothing_written(repo, &w).await;
}

/// A child the envelope does not name is refused; the caller cannot
/// substitute one.
pub async fn a_child_the_envelope_does_not_name_is_refused(repo: &dyn JobsRepository) {
    let w = plant(repo).await;
    let mut r = request(due_at());
    r.child.workflow_version = 2;
    let outcome = repo
        .guarded_admission_at(&r, &due_events(due_at()))
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
    nothing_written(repo, &w).await;
}
