fn native_policy_observation(
    request_id: uuid::Uuid,
) -> std::sync::Arc<boss_jobs::station_claim::PolicySelectionObservation> {
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
    let owner = std::sync::Arc::new(
        FakePolicyClient::builder()
            .allow("worker", Action::Read, Resource::job(), Scope::All)
            .allow("worker", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    std::sync::Arc::new(
        boss_jobs::station_claim::PolicySelectionObservation::prepare(
            owner,
            native_user(),
            request_id,
        )
        .unwrap(),
    )
}
fn native_user() -> boss_policy_client::User {
    boss_policy_client::User {
        id: "emp-worker".into(),
        role: "worker".into(),
        access_tier: boss_policy_client::types::AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_jobs::port::{JobScope, JobsError};
use boss_jobs::station_claim::SelectionContext;
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, JobsRepository, StationKind,
    StationRegistry, StationSpec,
};

struct FailedDefinitionOwner;
impl boss_jobs::station_claim::SelectionFence for FailedDefinitionOwner {
    fn lock_verified<'a>(
        &'a self,
        _context: &'a SelectionContext,
        _actor: &str,
        _at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Box<dyn boss_jobs::station_claim::SelectionGuard + 'a>, JobsError> {
        Err(JobsError::Storage("definition owner failure".into()))
    }
}

#[tokio::test]
async fn an_unfenced_definition_snapshot_is_not_an_atomic_claim_authority() {
    let request_id = uuid::Uuid::new_v4();
    let repo = InMemoryJobs::for_selection_fixture();
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "fixture"),
        "Fixture",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    step.spec_slug = Some("work".into());
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let mut context = SelectionContext {
        native_authority: None,
        prepared_policy: None,
        actor_observation: None,
        definition_fence: None,
        request_id,
        station,
        scope: JobScope::All,
        active_workflows: Default::default(),
        validated_job: job,
        validated_step: step.clone(),
    };
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("test"),
    );
    assert!(
        matches!(
            repo.claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
                .await,
            Err(JobsError::Storage(_))
        ),
        "a bare context has no fence against registry publication"
    );
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    let stations = std::sync::Arc::new(InMemoryStations::new());
    stations.seed(context.station.clone()).unwrap();
    let workflows = std::sync::Arc::new(InMemoryWorkflows::new());
    context.definition_fence = Some(std::sync::Arc::new(
        boss_jobs::station_claim::RegistrySelectionFence {
            workflows,
            stations: stations.clone(),
            policy: native_policy_observation(request_id),
        },
    ));
    assert!(
        matches!(
            repo.claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
                .await,
            Err(JobsError::SelectionConflict(_))
        ),
        "the repository commit door must refuse a missing actor owner witness"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), step);
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    context.actor_observation = Some(boss_jobs::station_claim::ActorSelectionObservation {
        owner: std::sync::Arc::new(boss_jobs::agents::InMemoryAgents::new()),
        holder: "emp-worker".into(),
        row: None,
        requested_user: serde_json::json!({"id": "emp-worker"}),
    });
    let real_fence = context.definition_fence.take();
    context.definition_fence = Some(std::sync::Arc::new(FailedDefinitionOwner));
    assert!(matches!(
        repo.claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
            .await,
        Err(JobsError::Storage(_))
    ));
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap(),
        step,
        "definition owner failure must not escape with a committed claim"
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    context.definition_fence = real_fence;
    repo.fail_steps_read(&step.job_id);
    assert!(
        matches!(
            repo.claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
                .await,
            Err(JobsError::Storage(_))
        ),
        "unreadable step census must not become an empty or successful selection"
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    stations
        .retire(
            "fixture",
            &boss_core::actor::ActorId::automation("test"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            repo.claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
                .await,
            Err(JobsError::SelectionConflict(_))
        ),
        "a fenced read must observe retirement before committing"
    );
    assert_eq!(
        repo.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
}

struct WaitBeforeOwners {
    inner: std::sync::Arc<boss_jobs::station_claim::RegistrySelectionFence>,
    paused: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

#[tokio::test]
async fn a_busy_station_writer_releases_earlier_definition_owners() {
    use boss_jobs::station_claim::{ActorSelectionObservation, RegistrySelectionFence};
    let request_id = uuid::Uuid::new_v4();
    let repo = InMemoryJobs::for_selection_fixture();
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "bounded-owner"),
        "Bounded owner",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    step.spec_slug = Some("work".into());
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let stations = std::sync::Arc::new(InMemoryStations::new());
    stations.seed(station.clone()).unwrap();
    let workflows = std::sync::Arc::new(InMemoryWorkflows::new());
    let context = SelectionContext {
        native_authority: None,
        prepared_policy: None,
        actor_observation: Some(ActorSelectionObservation {
            owner: std::sync::Arc::new(boss_jobs::agents::InMemoryAgents::new()),
            holder: "emp-worker".into(),
            row: None,
            requested_user: serde_json::json!({"id": "emp-worker"}),
        }),
        definition_fence: Some(std::sync::Arc::new(RegistrySelectionFence {
            workflows: workflows.clone(),
            stations: stations.clone(),
            policy: native_policy_observation(request_id),
        })),
        request_id,
        station,
        scope: JobScope::All,
        active_workflows: Default::default(),
        validated_job: job,
        validated_step: step.clone(),
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        // This is the actual station writer boundary, not a delayed callback
        // standing in for exclusion. Its release is deliberately later than
        // the selector's whole five-second admission budget.
        let _guard = stations.lock_active_selection_rows().unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("emp-worker"),
    );
    let started = std::time::Instant::now();
    let attempt = context
        .definition_fence
        .as_ref()
        .unwrap()
        .try_lock_verified(&context, "emp-worker", stamp.timestamp)
        .unwrap();
    assert!(
        attempt.is_none(),
        "the real station writer makes the attempt busy"
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    assert!(
        boss_jobs::WorkflowRegistry::try_lock_active_selection_rows(&*workflows)
            .unwrap()
            .is_some(),
        "a busy later owner must release the earlier workflow owner"
    );
    writer.join().unwrap();
    // The old repository-stopwatch RED is retained in the run record. The
    // actual ingress deadline is proved by station_claim_native_memory's
    // actual_ingress_budget_bounds_a_real_memory_station_writer control.
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), step);
    assert!(
        repo.station_claim_receipt(request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.recorded_events().is_empty());
}
impl boss_jobs::station_claim::SelectionFence for WaitBeforeOwners {
    fn lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Box<dyn boss_jobs::station_claim::SelectionGuard + 'a>, JobsError> {
        // No owner lock has been acquired: this is the competing-record
        // seam after the repository's first receipt lookup.
        self.paused.send(()).unwrap();
        self.resume.lock().unwrap().recv().unwrap();
        self.inner.lock_verified(context, actor, at)
    }
}

#[tokio::test]
async fn a_competing_original_receipt_survives_owner_retirement_before_lock_acquisition() {
    use boss_jobs::station_claim::{ActorSelectionObservation, RegistrySelectionFence};
    let request_id = uuid::Uuid::new_v4();
    let repo = std::sync::Arc::new(InMemoryJobs::for_selection_fixture());
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "race"),
        "Race",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    step.spec_slug = Some("work".into());
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let stations = std::sync::Arc::new(InMemoryStations::new());
    stations.seed(station.clone()).unwrap();
    let fence = std::sync::Arc::new(RegistrySelectionFence {
        workflows: std::sync::Arc::new(InMemoryWorkflows::new()),
        stations: stations.clone(),
        policy: native_policy_observation(request_id),
    });
    let context = SelectionContext {
        native_authority: None,
        prepared_policy: None,
        actor_observation: Some(ActorSelectionObservation {
            owner: std::sync::Arc::new(boss_jobs::agents::InMemoryAgents::new()),
            holder: "emp-worker".into(),
            row: None,
            requested_user: serde_json::json!({"id": "emp-worker"}),
        }),
        definition_fence: Some(fence.clone()),
        request_id,
        station,
        scope: JobScope::All,
        active_workflows: Default::default(),
        validated_job: job,
        validated_step: step.clone(),
    };
    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let mut waiting_context = context.clone();
    waiting_context.definition_fence = Some(std::sync::Arc::new(WaitBeforeOwners {
        inner: fence,
        paused: paused_tx,
        resume: std::sync::Mutex::new(resume_rx),
    }));
    let waiting_repo = repo.clone();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("emp-worker"),
    );
    let waiting_stamp = stamp.clone();
    let waiter = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(waiting_repo.claim_station_head_at(
                &waiting_context,
                "emp-worker",
                &[],
                &waiting_stamp,
                &[],
            ))
    });
    paused_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let winner = repo
        .claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
        .await
        .unwrap();
    let original = match winner {
        boss_jobs::station_claim::SelectionResult::Claimed { receipt, .. } => receipt,
        _ => panic!("the competing writer must create the first record"),
    };
    let events = serde_json::to_value(repo.recorded_events()).unwrap();
    stations
        .retire(
            "fixture",
            &boss_core::actor::ActorId::human("emp-owner"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    resume_tx.send(()).unwrap();
    let observed = waiter.join().unwrap();
    assert!(
        matches!(&observed, Ok(boss_jobs::station_claim::SelectionResult::Replayed(receipt)) if receipt.json() == original.json()),
        "a real competing claim already committed its original receipt before the owner retired"
    );
    assert_eq!(
        serde_json::to_value(repo.recorded_events()).unwrap(),
        events
    );
}

struct NativeRepositoryPolicy(std::sync::Arc<boss_policy_client::InMemoryPolicy>);
#[async_trait::async_trait]
impl boss_policy_client::PolicyClient for NativeRepositoryPolicy {
    fn lock_selection_rows(
        &self,
    ) -> Result<
        Box<dyn boss_policy_client::port::PolicySelectionRows + '_>,
        boss_policy_client::port::PolicyError,
    > {
        boss_policy_client::PolicyRepository::lock_selection_rows(&*self.0)
    }
    async fn check(
        &self,
        _: &boss_policy_client::User,
        _: boss_policy_client::Action,
        _: boss_policy_client::Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        panic!("native test uses actual owner rows only")
    }
    async fn scope_predicate(
        &self,
        _: &boss_policy_client::User,
        _: boss_policy_client::Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        panic!("native test uses actual owner rows only")
    }
}
struct WaitAfterOwners {
    inner: std::sync::Arc<boss_jobs::station_claim::RegistrySelectionFence>,
    expires: chrono::DateTime<chrono::Utc>,
}
impl boss_jobs::station_claim::SelectionFence for WaitAfterOwners {
    fn lock_verified<'a>(
        &'a self,
        context: &'a SelectionContext,
        actor: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Box<dyn boss_jobs::station_claim::SelectionGuard + 'a>, JobsError> {
        let guard = self.inner.lock_verified(context, actor, at)?;
        while chrono::Utc::now() < self.expires {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(guard)
    }
}
#[tokio::test]
async fn an_override_expiring_after_owner_acquisition_refuses_at_actual_commit() {
    use boss_jobs::station_claim::{ActorSelectionObservation, RegistrySelectionFence};
    let request_id = uuid::Uuid::new_v4();
    let repo = std::sync::Arc::new(InMemoryJobs::for_selection_fixture());
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "race"),
        "Race",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    step.spec_slug = Some("work".into());
    repo.create_job(&job).await.unwrap();
    repo.add_step(&step).await.unwrap();
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let stations = std::sync::Arc::new(InMemoryStations::new());
    stations.seed(station.clone()).unwrap();
    let expires = chrono::Utc::now() + chrono::Duration::seconds(2);
    let policy_repo = std::sync::Arc::new(boss_policy_client::InMemoryPolicy::with_rules([
        boss_policy_client::PolicyRule::new(
            "worker",
            boss_policy_client::Resource::job(),
            boss_policy_client::Action::Read,
            boss_policy_client::Scope::All,
        ),
    ]));
    boss_policy_client::PolicyRepository::upsert_user_override(
        &*policy_repo,
        &boss_policy_client::UserOverride {
            id: "temporary-claim".into(),
            user_id: "emp-worker".into(),
            resource: boss_policy_client::Resource::step(),
            action: boss_policy_client::Action::Update,
            scope: boss_policy_client::Scope::All,
            reason: "bounded test grant".into(),
            expires_at: Some(expires),
        },
        "fixture",
    )
    .await
    .unwrap();
    let policy = std::sync::Arc::new(
        boss_jobs::station_claim::PolicySelectionObservation::prepare(
            std::sync::Arc::new(NativeRepositoryPolicy(policy_repo)),
            native_user(),
            request_id,
        )
        .unwrap(),
    );
    let fence = std::sync::Arc::new(RegistrySelectionFence {
        workflows: std::sync::Arc::new(InMemoryWorkflows::new()),
        stations: stations.clone(),
        policy,
    });
    let context = SelectionContext {
        native_authority: None,
        prepared_policy: None,
        actor_observation: Some(ActorSelectionObservation {
            owner: std::sync::Arc::new(boss_jobs::agents::InMemoryAgents::new()),
            holder: "emp-worker".into(),
            row: None,
            requested_user: serde_json::json!({"id": "emp-worker"}),
        }),
        definition_fence: Some(std::sync::Arc::new(WaitAfterOwners {
            inner: fence.clone(),
            expires,
        })),
        request_id,
        station,
        scope: JobScope::All,
        active_workflows: Default::default(),
        validated_job: job,
        validated_step: step.clone(),
    };

    let before_events = serde_json::to_value(repo.recorded_events()).unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("emp-worker"),
    );
    let result = repo
        .claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
        .await;
    assert!(
        matches!(result, Err(JobsError::SelectionConflict(_))),
        "actual expiry must be consumed before commit"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), step);
    assert_eq!(
        serde_json::to_value(repo.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_run_population_change_after_observation_refuses_under_the_actual_jobs_owner() {
    use boss_jobs::station_claim::{ActorSelectionObservation, RegistrySelectionFence};
    let request_id = uuid::Uuid::new_v4();
    let repo = std::sync::Arc::new(InMemoryJobs::for_selection_fixture());
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "race"),
        "Race",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    step.spec_slug = Some("work".into());
    repo.create_job(&job).await.unwrap();
    step.metadata[boss_jobs::agent_spec::MODEL_KEY] = serde_json::json!("test-model");
    repo.add_step(&step).await.unwrap();
    let mut station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    let stations = std::sync::Arc::new(InMemoryStations::new());
    stations.seed(station.clone()).unwrap();
    let fence = std::sync::Arc::new(RegistrySelectionFence {
        workflows: std::sync::Arc::new(InMemoryWorkflows::new()),
        stations: stations.clone(),
        policy: native_policy_observation(request_id),
    });
    let agents = std::sync::Arc::new(boss_jobs::agents::InMemoryAgents::new());
    let owner_stamp = boss_core::publisher::EventStamp::new(
        "agents",
        boss_core::actor::ActorId::human("fixture"),
    );
    boss_jobs::agents::AgentsRegistry::publish(
        &*agents,
        &[boss_jobs::agents::AgentInput {
            id: "agent-worker".into(),
            display_name: "Worker".into(),
            default_model: "test-model".into(),
            role: None,
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(1),
            aliases: vec![],
        }],
        boss_core::publish::PublishMode::InsertIfAbsent,
        &owner_stamp,
    )
    .await
    .unwrap();
    let agent_row = boss_jobs::agents::AgentsRegistry::list(&*agents)
        .await
        .unwrap()
        .remove(0);
    let context = SelectionContext {
        native_authority: None,
        prepared_policy: None,
        actor_observation: Some(ActorSelectionObservation {
            owner: agents,
            holder: "agent-worker".into(),
            row: Some(agent_row),
            requested_user: serde_json::json!({"id": "emp-worker"}),
        }),
        definition_fence: Some(fence.clone()),
        request_id,
        station,
        scope: JobScope::All,
        active_workflows: Default::default(),
        validated_job: job,
        validated_step: step.clone(),
    };

    let filter = boss_jobs::port::JobFilter {
        kind: Some(boss_jobs::agent_budget::RUN_KIND.into()),
        status: Some(JobStatus::Open),
        ..Default::default()
    };
    let (observed, _) = repo.list_jobs(&filter, 100, 0).await.unwrap();
    assert_eq!(
        boss_jobs::agent_budget::in_flight_runs(&observed, &["agent-worker".into()]),
        0
    );
    let mut run = Job::new(
        boss_jobs::agent_budget::RUN_KIND,
        Subject::new("custom", "new-run"),
        "New run",
        "agent-worker",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    run.status = JobStatus::Open;
    run.metadata[boss_jobs::agent_budget::RUN_AGENT_KEY] = serde_json::json!("agent-worker");
    repo.create_job(&run).await.unwrap();
    let (current, _) = repo.list_jobs(&filter, 100, 0).await.unwrap();
    assert_eq!(
        boss_jobs::agent_budget::in_flight_runs(&current, &["agent-worker".into()]),
        1
    );
    let before_events = serde_json::to_value(repo.recorded_events()).unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("agent-worker"),
    );
    let result = repo
        .claim_station_head_at(&context, "agent-worker", &[], &stamp, &[])
        .await;
    assert!(
        matches!(result, Err(JobsError::SelectionConflict(_))),
        "same-store run writer reached cap after observed zero"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), step);
    assert_eq!(
        serde_json::to_value(repo.recorded_events()).unwrap(),
        before_events
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    run.status = JobStatus::Closed;
    repo.update_job(&run).await.unwrap();
    let claimed = repo
        .claim_station_head_at(&context, "agent-worker", &[], &stamp, &[])
        .await
        .unwrap();
    let receipt = match claimed {
        boss_jobs::station_claim::SelectionResult::Claimed { receipt, .. } => receipt,
        _ => panic!("drained population must admit fresh claim"),
    };
    run.id = boss_core::job::JobId::new();
    run.status = JobStatus::Open;
    repo.create_job(&run).await.unwrap();
    let replayed = repo
        .claim_station_head_at(&context, "agent-worker", &[], &stamp, &[])
        .await
        .unwrap();
    assert!(
        matches!(replayed, boss_jobs::station_claim::SelectionResult::Replayed(original) if original.json() == receipt.json()),
        "replayed original is not a fresh population acquisition"
    );
}

#[test]
fn prepared_policy_inputs_are_bound_to_the_actual_owner_request_and_complete_user() {
    use boss_policy_client::{Action, Decision, FakePolicyClient, Resource, Scope};
    let client = || -> std::sync::Arc<dyn boss_policy_client::PolicyClient> {
        std::sync::Arc::new(
            FakePolicyClient::builder()
                .allow("worker", Action::Read, Resource::job(), Scope::All)
                .allow("worker", Action::Update, Resource::step(), Scope::All)
                .build(),
        )
    };
    let owner = client();
    let same_inputs_other_owner = client();
    let request = uuid::Uuid::new_v4();
    let user = native_user();
    let observed = boss_jobs::station_claim::PolicySelectionObservation::prepare(
        owner.clone(),
        user.clone(),
        request,
    )
    .unwrap();
    let expected = Decision::Allow { scope: Scope::All };
    assert!(observed.matches_preflight(&owner, &user, &expected, request));
    assert!(
        !observed.matches_preflight(&same_inputs_other_owner, &user, &expected, request),
        "identical policy rows do not bind another actual owner"
    );
    assert!(!observed.matches_preflight(&owner, &user, &expected, uuid::Uuid::new_v4()));
    let mut changed_user = user.clone();
    changed_user
        .territory_account_ids
        .push("later-session-scope".into());
    assert!(
        !observed.matches_preflight(&owner, &changed_user, &expected, request),
        "complete caller context is bound, not only role or id"
    );
    assert!(!observed.matches_preflight(
        &owner,
        &user,
        &Decision::Allow {
            scope: Scope::Self_
        },
        request
    ));
}
