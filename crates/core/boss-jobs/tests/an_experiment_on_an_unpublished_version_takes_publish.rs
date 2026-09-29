//! An experiment that admits packets to a version never published is
//! opened by whoever may publish one (backlog ce8b7d66).
//!
//! MEASURED (the adversarial review of car 5d1c0b7a, 2026-09-26). A
//! draft needs only workflow Create/Update, and an OPEN
//! `protocol-experiment` packet whose metadata declares a split pins
//! every packet admitted to the kind under test to its arm's version —
//! a draft included, because admission reads the arm through
//! `get_version`, which serves drafts. Opening that packet needed only
//! Create on job. So a Create-only author could write a draft declaring
//! `individual = self`, open an experiment at split 100, and every new
//! packet of the kind materialised held by that author as its declared
//! executor: the draft had become the live protocol without anyone who
//! may publish one saying so.
//!
//! THE CALL (made on the build, not by design d8771dec, which set only
//! WHEN an experiment starts): the experiment stays the one sanctioned
//! way a draft meets traffic — a candidate is a draft until a promote
//! publishes it — but putting such a split in force is a publish-level
//! act, so it takes Publish on `workflow`, the permission that makes a
//! version live. Every job write door asks it: the admission, the PUT
//! and the metadata PATCH. A version that does not exist yet counts as
//! unpublished, because the draft that later takes the number would be
//! admitted to without a second look. So does a RETIRED version (the
//! review of car 06973644, finding C): at split 100 it rolls the kind
//! back to a superseded protocol. Only a split whose arms are the
//! active version needs nothing beyond the job write.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::experiments::EXPERIMENT_KIND;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    KindReconcileStats, StepSpec, Terminal, WorkflowError, WorkflowRegistry, WorkflowSpec,
    WorkflowStatus, seedable_platform_workflows,
};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "versioned";

fn user(id: &str, role: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// May publish a protocol version.
fn publisher() -> String {
    user("emp-publisher", "publisher")
}

/// Writes jobs and drafts, and may not publish.
fn author() -> String {
    user("emp-author", "author")
}

fn spec(version: i32, status: WorkflowStatus) -> WorkflowSpec {
    let mut s = WorkflowSpec::platform_seed(
        KIND,
        "Versioned",
        "test",
        vec!["system".into()],
        vec![
            StepSpec {
                title: "open".into(),
                kind: "trigger".into(),
                ready_when: "true".into(),
                title_template: "Opened".into(),
                ..Default::default()
            },
            StepSpec {
                title: "closed".into(),
                kind: "outcome".into(),
                ready_when: "steps.open.done".into(),
                title_template: "Closed".into(),
                terminal: Some(Terminal {
                    outcome: "completed".into(),
                }),
                ..Default::default()
            },
        ],
    );
    s.version = version;
    s.status = status;
    s
}

/// v1 ran and was superseded, v2 is live, v3 is a draft nobody published.
fn seeded() -> Arc<InMemoryWorkflows> {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for s in seedable_platform_workflows()
        .into_iter()
        .filter(|s| s.kind == EXPERIMENT_KIND)
    {
        kinds.seed(s).expect("seed protocol-experiment");
    }
    kinds.seed(spec(1, WorkflowStatus::Retired)).unwrap();
    kinds.seed(spec(2, WorkflowStatus::Active)).unwrap();
    kinds.seed(spec(3, WorkflowStatus::Draft)).unwrap();
    kinds
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    app_over(seeded())
}

fn app_over(kinds: Arc<dyn WorkflowRegistry>) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for role in ["publisher", "author"] {
        for action in [Action::Create, Action::Read, Action::Update] {
            policy = policy.allow(role, action, Resource::job(), Scope::All);
        }
        for action in [Action::Create, Action::Update] {
            policy = policy.allow(role, action, Resource::workflow(), Scope::All);
        }
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        policy
            .allow(
                "publisher",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

fn experiment(control: i32, candidate: i32, split: u8) -> Job {
    let mut e = Job::new(
        EXPERIMENT_KIND,
        Subject::new("custom", "proto-versioned"),
        "Experiment: versioned",
        "emp-publisher",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
    );
    e.status = JobStatus::Open;
    e.metadata = serde_json::json!({
        "kind_under_test": KIND,
        "control_version": control,
        "candidate_version": candidate,
        "split": split,
    });
    e
}

async fn send(
    app: &axum::Router,
    who: &str,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-boss-user", who)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn open(app: &axum::Router, who: &str, e: &Job) -> (StatusCode, String) {
    send(
        app,
        who,
        "POST",
        "/api/jobs",
        serde_json::to_value(e).unwrap(),
    )
    .await
}

/// THE MEASURED CHAIN, cut at its second link: a Create-only author
/// cannot put a split onto a draft in force, and nothing is admitted.
#[tokio::test]
async fn an_author_who_cannot_publish_may_not_open_an_experiment_on_a_draft() {
    let (app, jobs) = app();
    let e = experiment(2, 3, 100);
    let (status, body) = open(&app, &author(), &e).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"version\":3"), "names the version: {body}");
    assert!(body.contains("draft"), "says what it is: {body}");
    assert!(
        jobs.get_job(&e.id).await.unwrap().is_none(),
        "a refused admission writes nothing"
    );
}

/// The experiment is still the sanctioned way a draft meets traffic —
/// for whoever may make a version live.
#[tokio::test]
async fn a_publisher_opens_an_experiment_on_a_draft() {
    let (app, jobs) = app();
    let e = experiment(2, 3, 100);
    let (status, body) = open(&app, &publisher(), &e).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(jobs.get_job(&e.id).await.unwrap().is_some());
}

/// A RETIRED arm is not the live protocol either (the review of car
/// 06973644, finding C). It ran once, but it was superseded, and a
/// split of 100 onto it rolls the kind back to it for every new packet
/// — the same act as publishing it again, so it asks the same
/// permission. Only the active version is exempt.
#[tokio::test]
async fn a_retired_arm_takes_publish_too() {
    let (app, jobs) = app();
    let e = experiment(2, 1, 100);
    let (status, body) = open(&app, &author(), &e).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"version\":1"), "names the version: {body}");
    assert!(body.contains("retired"), "says what it is: {body}");
    assert!(
        jobs.get_job(&e.id).await.unwrap().is_none(),
        "a refused admission writes nothing"
    );

    let (status, body) = open(&app, &publisher(), &experiment(2, 1, 100)).await;
    assert_eq!(status, StatusCode::CREATED, "a publisher may: {body}");
}

/// A split whose arms are both the live protocol puts nothing new in
/// force, so it asks nothing beyond the job write.
#[tokio::test]
async fn an_author_may_open_an_experiment_whose_arms_are_the_active_version() {
    let (app, _) = app();
    let (status, body) = open(&app, &author(), &experiment(2, 2, 50)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// A number no row holds yet is not safe either: the next draft takes
/// it, and the open experiment would start admitting to that draft
/// with no one asked.
#[tokio::test]
async fn a_version_that_does_not_exist_yet_counts_as_unpublished() {
    let (app, _) = app();
    let (status, body) = open(&app, &author(), &experiment(2, 9, 50)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"version\":9"), "{body}");
}

/// The PATCH door: moving a running experiment's arm onto a draft is
/// the same act as opening one there. An annotation is not.
#[tokio::test]
async fn the_metadata_patch_may_not_move_an_arm_onto_a_draft() {
    let (app, jobs) = app();
    let e = experiment(2, 1, 50);
    let (status, body) = open(&app, &publisher(), &e).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let uri = format!("/api/jobs/{}/metadata", e.id);

    let (status, body) = send(
        &app,
        &author(),
        "PATCH",
        &uri,
        serde_json::json!({ "candidate_version": 3 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_job(&e.id).await.unwrap().expect("stored");
    assert_eq!(stored.metadata["candidate_version"], 1, "the arm stays");

    let (status, body) = send(
        &app,
        &author(),
        "PATCH",
        &uri,
        serde_json::json!({ "note": "halfway" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an annotation lands: {body}"
    );
}

/// THE PATCH DOOR MERGES ONLY INTO THE ROW IT JUDGED (the review of car
/// 06973644, finding B; pinned at the door by the review of car
/// bd5c8d9e). An author may widen a split whose arms are both the
/// active version — judged harmless. Another writer moves the candidate
/// onto the draft between the door's read and its merge. Merged into
/// the row as it then stands, the author's `split: 100` would put that
/// draft in force for every new packet with no one asked; the door
/// hands the adapter the row it judged, and the merge refuses: 409,
/// the retry named, nothing written and nothing recorded.
#[tokio::test]
async fn the_metadata_patch_refuses_a_row_that_moved_under_its_judgement() {
    let (app, jobs) = app();
    let e = experiment(2, 2, 50);
    let (status, body) = open(&app, &author(), &e).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Straight after the door reads the packet, the arm moves to v3.
    jobs.change_job_after_next_read(&e.id, |row| {
        row.metadata["candidate_version"] = serde_json::json!(3);
    });
    let recorded = jobs.recorded_events().len();
    let (status, body) = send(
        &app,
        &author(),
        "PATCH",
        &format!("/api/jobs/{}/metadata", e.id),
        serde_json::json!({ "split": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("changed since this merge was judged"),
        "says what happened: {body}"
    );
    assert!(
        body.contains("send the same patch again"),
        "names the retry: {body}"
    );
    let stored = jobs.get_job(&e.id).await.unwrap().expect("stored");
    assert_eq!(stored.metadata["split"], 50, "the split stays");
    assert_eq!(
        stored.metadata["candidate_version"], 3,
        "the other write stands"
    );
    assert_eq!(
        jobs.recorded_events().len(),
        recorded,
        "a refused merge records nothing"
    );

    // Re-sent, the door judges the row as it now stands: a draft arm.
    let (status, body) = send(
        &app,
        &author(),
        "PATCH",
        &format!("/api/jobs/{}/metadata", e.id),
        serde_json::json!({ "split": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// The PUT door: a publisher opened an idle split (0% to the draft);
/// an author widening it to all of the kind's traffic is refused.
#[tokio::test]
async fn the_put_may_not_widen_a_split_onto_a_draft() {
    let (app, jobs) = app();
    let e = experiment(2, 3, 0);
    let (status, body) = open(&app, &publisher(), &e).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let mut widened = jobs.get_job(&e.id).await.unwrap().expect("stored");
    widened.metadata["split"] = serde_json::json!(100);
    let (status, body) = send(
        &app,
        &author(),
        "PUT",
        &format!("/api/jobs/{}", e.id),
        serde_json::to_value(&widened).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_job(&e.id).await.unwrap().expect("stored");
    assert_eq!(stored.metadata["split"], 0, "the split stays");
}

/// The seeded registry, except that one version's row cannot be read —
/// the registry answers a storage error for it, as a Pg registry does
/// when its database is unreachable or the row will not parse.
struct OneRowUnreadable {
    inner: Arc<InMemoryWorkflows>,
    version: i32,
}

#[async_trait::async_trait]
impl WorkflowRegistry for OneRowUnreadable {
    async fn get_active(&self, kind: &str) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.get_active(kind).await
    }
    async fn get_version(&self, kind: &str, version: i32) -> Result<WorkflowSpec, WorkflowError> {
        if kind == KIND && version == self.version {
            return Err(WorkflowError::Storage("connection reset".into()));
        }
        self.inner.get_version(kind, version).await
    }
    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        self.inner.list_active(category).await
    }
    async fn list_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        self.inner.list_versions(kind).await
    }
    async fn create_draft(
        &self,
        spec: WorkflowSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.create_draft(spec, actor, now).await
    }
    async fn publish(
        &self,
        kind: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.publish(kind, actor, now).await
    }
    async fn retire(
        &self,
        kind: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        self.inner.retire(kind, actor, now).await
    }
    async fn discard_draft(
        &self,
        kind: &str,
        version: i32,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        self.inner.discard_draft(kind, version, actor, now).await
    }
    async fn publish_authored(
        &self,
        spec: WorkflowSpec,
        authoring_job_id: JobId,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner
            .publish_authored(spec, authoring_job_id, actor, now)
            .await
    }
    async fn bootstrap_reconcile(
        &self,
        defaults: &[WorkflowSpec],
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<KindReconcileStats, WorkflowError> {
        self.inner.bootstrap_reconcile(defaults, actor, now).await
    }
}

/// FAIL CLOSED (the review of car 06973644, finding D): an arm whose
/// row the registry cannot read is judged not active, so a caller who
/// may not publish is refused and told why, and nothing is admitted.
/// Read as "not a draft", a dark registry would wave the split through.
#[tokio::test]
async fn an_arm_the_registry_cannot_read_takes_publish() {
    let (app, jobs) = app_over(Arc::new(OneRowUnreadable {
        inner: seeded(),
        version: 3,
    }));
    let e = experiment(2, 3, 100);
    let (status, body) = open(&app, &author(), &e).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"version\":3"), "names the version: {body}");
    assert!(body.contains("unreadable"), "says why: {body}");
    assert!(
        jobs.get_job(&e.id).await.unwrap().is_none(),
        "a refused admission writes nothing"
    );
}
