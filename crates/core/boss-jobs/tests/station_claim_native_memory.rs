use std::sync::Arc;

use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_jobs::agents::{AgentInput, AgentsRegistry, InMemoryAgents};
use boss_jobs::memory_selection_store::NativeMemorySelectionStore;
use boss_jobs::port::{JobsError, JobsRepository};
use boss_jobs::station_claim::{AuthenticatedSelectionIngress, SelectionContext, SelectionResult};
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationKind, StationRegistry, StationSpec,
};
use tower::ServiceExt;

#[tokio::test]
async fn native_memory_resource_refusal_retains_the_exact_original_without_claiming() {
    use boss_jobs::{WorkflowRegistry, pools::PoolRegistry};
    let mut f = fixture_with_actor(None, true).await;
    let workflows = Arc::new(InMemoryWorkflows::for_fixture());
    let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
    let spec:boss_jobs::registry::WorkflowSpec=serde_json::from_value(serde_json::json!({
        "kind":"fixture","label":"Flow fixture","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":f.stamp.timestamp,
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},{"title":"work","kind":"task","ready_when":"steps.start.done","resources":{"vcpu":"1"}},{"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}]})).unwrap();
    workflows
        .publish_authored(
            spec,
            boss_core::job::JobId::new(),
            f.stamp.actor(),
            f.stamp.timestamp,
        )
        .await
        .unwrap();
    pools.publish_declared(serde_json::from_value(serde_json::json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":{"state":"unknown"},"created_at":f.stamp.timestamp})).unwrap(),f.stamp.actor(),f.stamp.timestamp).await.unwrap();
    f.repo = InMemoryJobs::for_native_selection(NativeMemorySelectionStore::new(
        workflows.clone(),
        f.stations.clone(),
        f.agents.clone(),
        Arc::new(boss_policy_client::FakePolicyClient::from_repository(
            f.policy.clone(),
        )),
    ))
    .with_resource_pools(pools);
    f.repo
        .create_job_at(&f.context.validated_job, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.repo
        .add_step_at(&f.step, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.workflows = workflows;
    f.context.active_workflows = f
        .workflows
        .list_active(None)
        .await
        .unwrap()
        .into_iter()
        .map(|w| (w.kind.clone(), w))
        .collect();
    admit(&mut f).await.unwrap();
    let SelectionResult::Refused { receipt } = claim_answer(&mut f).await.unwrap() else {
        panic!("resource refusal must not be a claimed fact")
    };
    assert_eq!(receipt["refusal"]["reason"], "capacity-unknown");
    assert!(receipt.get("claimed_step").is_none());
    assert_eq!(
        f.repo.get_step(&f.step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
    assert!(
        !f.repo
            .recorded_events()
            .iter()
            .any(|e| e.kind == boss_jobs::events::STATION_CLAIMED)
    );
    let SelectionResult::Replayed(original) = claim_answer(&mut f).await.unwrap() else {
        panic!("original")
    };
    assert_eq!(original.json(), receipt.json());
}

async fn ingress() -> AuthenticatedSelectionIngress {
    let dir = boss_testing::scratch_dir("memory-native-ingress");
    boss_testing::write_file(&dir.join("forge.current"), "memory-native-fixture-key");
    let captured = Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    let app = axum::Router::new().route(
        "/fixture-ingress",
        axum::routing::get(
            move |proof: axum::Extension<AuthenticatedSelectionIngress>| {
                let sink = sink.clone();
                async move {
                    *sink.lock().unwrap() = Some(proof.0);
                    axum::http::StatusCode::NO_CONTENT
                }
            },
        ),
    );
    let app = boss_jobs::runner_credential::mount(app, dir);
    let gate = Arc::new(boss_core::machine_gate::MachineGate::new(
        "jobs",
        &["/health"],
        boss_core::machine_gate::Reading::new(
            boss_core::machine_gate::Mode::Off,
            Default::default(),
        ),
    ));
    let app = boss_core::machine_gate::gated(app, gate);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/fixture-ingress")
                .header(
                    boss_jobs::runner_credential::HEADER,
                    "memory-native-fixture-key",
                )
                .header(
                    "x-boss-user",
                    serde_json::to_string(&boss_policy_client::User::service("forged-owner"))
                        .unwrap(),
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    let proof = captured.lock().unwrap().take().unwrap();
    assert_eq!(proof.actor_id(), "automation:ops-runner");
    proof
}

struct Fixture {
    calendar: Option<Arc<boss_calendar::InMemoryCalendar>>,
    admitted_at: std::time::Instant,
    repo: InMemoryJobs,
    stations: Arc<InMemoryStations>,
    workflows: Arc<InMemoryWorkflows>,
    agents: Arc<InMemoryAgents>,
    policy: Arc<boss_policy_client::InMemoryPolicy>,
    context: SelectionContext,
    step: Step,
    stamp: boss_core::publisher::EventStamp,
}

/// Admit a fresh authenticated ingress for the fixture's own request and
/// prepare its native authority. `admitted_at` follows it, because the
/// production budget runs from the ingress, not from the claim.
async fn admit(f: &mut Fixture) -> Result<(), JobsError> {
    let proof = ingress().await;
    f.admitted_at = proof.started();
    let observation = f
        .repo
        .prepare_native_selection(proof, f.context.request_id, &f.context.station.name)
        .await?
        .expect("native memory owners prepare an observation");
    f.context.native_authority = Some(Arc::new(observation));
    Ok(())
}

/// Admissions an answer test spends before a run of deadline refusals is
/// its verdict rather than the load's.
const ANSWER_ADMISSIONS: u32 = 3;

/// Every owner a refused claim must leave as it found it.
#[derive(Debug, PartialEq)]
struct Owners {
    events: serde_json::Value,
    step: Option<Step>,
    receipt: Option<String>,
    calendar: Option<serde_json::Value>,
}

impl Owners {
    async fn read(f: &Fixture) -> Self {
        Self {
            events: serde_json::to_value(f.repo.recorded_events()).unwrap(),
            step: f.repo.get_step(&f.step.id).await.unwrap(),
            receipt: f
                .repo
                .station_claim_receipt(f.context.request_id)
                .await
                .unwrap()
                .map(|receipt| receipt.json().to_owned()),
            calendar: f.calendar.as_ref().map(|calendar| {
                serde_json::json!([
                    calendar.active_count(),
                    serde_json::to_value(calendar.recorded_events()).unwrap()
                ])
            }),
        }
    }
}

/// Claim for a test whose subject is the ANSWER — what a claim records,
/// or what it replays — and not the ingress deadline (cb503277). The
/// budget stays the production five seconds, read on the std clock the
/// memory owners read it on; a test that moved only tokio's clock would
/// miss that reader (car 4f6ec932's review, B1). A thread stalled past it
/// is refused before commit dispatch, which is the system working: red
/// train d1251135's gate ran this binary in 21.5 s against 6 s on an idle
/// pod, with three gates on one node, and this file's calendar answer
/// lost its five seconds there on a tree no car had changed it on.
/// So a deadline refusal is JUDGED, never unwrapped: the deadline really
/// passed and the refused attempt left every owner as it found it; then
/// the same request is admitted afresh, as its client would retry it. A
/// deadline refusal before the deadline, a refusal of any other kind, or
/// one on every admission is still this test's failure.
async fn claim_answer(f: &mut Fixture) -> Result<SelectionResult, JobsError> {
    let mut admission = 1;
    loop {
        let before = Owners::read(f).await;
        let admitted = if admission == 1 {
            Ok(())
        } else {
            admit(f).await
        };
        let attempt = match admitted {
            Ok(()) => {
                f.repo
                    .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
                    .await
            }
            Err(refusal) => Err(refusal),
        };
        match attempt {
            Err(JobsError::SelectionConflict(reason))
                if reason.contains("deadline expired") && admission < ANSWER_ADMISSIONS =>
            {
                assert!(
                    std::time::Instant::now()
                        >= f.admitted_at + boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET,
                    "a deadline refusal before its deadline is a defect, not load: {reason}"
                );
                assert_eq!(
                    Owners::read(f).await,
                    before,
                    "a deadline refusal must leave every owner as it found it: {reason}"
                );
                eprintln!(
                    "admission {admission} refused at its ingress deadline ({reason}); admitting afresh"
                );
                admission += 1;
            }
            other => return other,
        }
    }
}

#[tokio::test]
async fn complete_native_memory_population_is_copied_not_omitted() {
    let mut f = fixture().await;
    let SelectionResult::Claimed { receipt, .. } = claim_answer(&mut f).await.unwrap() else {
        panic!("unchanged owners must claim")
    };
    // The head packet is copied in full; every competitor is counted and
    // digested beside it (a8e91ac5).
    let head = receipt
        .pointer("/owner_consumption/competing_head")
        .and_then(serde_json::Value::as_array)
        .expect("the packet that heads the memory queue must be copied in full");
    assert_eq!(head[0]["id"], serde_json::json!(f.context.validated_job.id));
    assert!(
        receipt
            .pointer("/owner_consumption/competitor_digest/sha256")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|digest| digest.len() == 64),
        "the competitor set must be pinned by its digest"
    );
}

#[tokio::test]
async fn native_memory_self_read_never_copies_an_unrelated_private_run() {
    for (run_actor, readable) in [("agent-private-other", true), ("agent-memory", false)] {
        let mut f = fixture().await;
        let actor = "automation:ops-runner";
        f.agents
            .publish(
                &[AgentInput {
                    factory_rotation: Some(true),
                    id: "agent-memory".into(),
                    display_name: "Memory fixture".into(),
                    default_model: "test-model".into(),
                    model_class: None,
                    role: Some("worker".into()),
                    department: None,
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: Some(3),
                    stations: None,
                    aliases: vec![actor.into()],
                }],
                boss_core::publish::PublishMode::Take,
                &f.stamp,
            )
            .await
            .unwrap();
        let mut job = f.context.validated_job.clone();
        job.owner_id = actor.into();
        f.repo.update_job(&job).await.unwrap();
        f.context.validated_job = f.repo.get_job(&job.id).await.unwrap().unwrap();
        f.step.metadata[boss_jobs::agent_spec::MODEL_KEY] = serde_json::json!("test-model");
        f.repo
            .update_step_at(&f.step, f.stamp.timestamp, &[])
            .await
            .unwrap();
        f.context.validated_step = f.step.clone();
        let mut private = Job::new(
            "agent-run",
            Subject::new("custom", "private-other"),
            "Private unrelated run",
            "emp-other",
            Priority::Standard,
            f.stamp.timestamp.date_naive(),
        );
        private.status = JobStatus::Open;
        private.metadata["agent"] = serde_json::json!(run_actor);
        f.repo
            .create_job_at(&private, f.stamp.timestamp, &[])
            .await
            .unwrap();
        boss_policy_client::PolicyRepository::upsert_rule(
            &*f.policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id(
                    "worker",
                    &boss_policy_client::Resource::job(),
                    boss_policy_client::Action::Read,
                ),
                role: "worker".into(),
                action: boss_policy_client::Action::Read,
                resource: boss_policy_client::Resource::job(),
                scope: boss_policy_client::Scope::Self_,
                active: true,
            },
            "private-run-scope-regression",
        )
        .await
        .unwrap();
        let observation = f
            .repo
            .prepare_native_selection(
                ingress().await,
                f.context.request_id,
                &f.context.station.name,
            )
            .await
            .unwrap()
            .unwrap();
        f.context.scope = boss_jobs::port::JobScope::OwnerIs(actor.into());
        f.context.native_authority = Some(Arc::new(observation));
        assert!(!f.context.scope.admits(&private));
        let outcome = f
            .repo
            .claim_station_head_at(&f.context, actor, &[], &f.stamp, &[])
            .await;
        // NO RUN IS COUNTED (backlog 3e927996): the per-actor run cap that
        // copied the actor's open runs into the receipt is retired, so a
        // private run reaches the receipt under neither actor's name, and
        // the claim is admitted whether or not that run would have counted.
        let SelectionResult::Claimed { receipt, .. } = outcome.unwrap() else {
            panic!("fresh claim ({run_actor}, readable {readable})")
        };
        assert!(
            receipt
                .pointer("/owner_consumption/run_population")
                .is_none(),
            "no run population is copied"
        );
        assert!(
            !receipt.json().contains(&private.id.to_string()),
            "Self Read received a private unrelated agent-run identity"
        );
        let replay = f
            .repo
            .native_selection_read(
                &ingress().await,
                f.context.request_id,
                &f.context.station.name,
            )
            .await
            .unwrap()
            .unwrap()
            .original
            .unwrap();
        assert_eq!(replay.json(), receipt.json());
    }
}

/// `rows` packets compete for the head, each carrying `copies` of prose:
/// the head itself and `rows - 1` rivals ranked behind it. Since a8e91ac5
/// only the head is copied whole; the rivals are digested, so the receipt
/// no longer grows with them. A lone oversized head must still be refused
/// at the deadline (`head_is_oversized`). Many large rivals have two
/// honest answers, and neither may escape the original deadline: a claim
/// whose receipt stayed bounded, or a deadline refusal that left nothing
/// behind. Which one depends on how fast this machine hashes their
/// revisions, which is not the contract (the gate claimed, a slower
/// worker refused, gate fca1b9f9).
async fn observation_deadline(rows: usize, copies: usize, head_is_oversized: bool) {
    let mut f = fixture().await;
    let prose = serde_json::json!("complete evidence ".repeat(copies));
    let mut head = f.context.validated_job.clone();
    head.metadata["original_observation"] = prose.clone();
    f.repo.update_job(&head).await.unwrap();
    f.context.validated_job = f.repo.get_job(&head.id).await.unwrap().unwrap();
    for _ in 1..rows {
        let mut rival = Job::new(
            "fixture",
            Subject::new("custom", "large-observation"),
            "Complete immutable evidence",
            "emp-owner",
            Priority::Scheduled,
            f.stamp.timestamp.date_naive(),
        );
        rival.status = JobStatus::Open;
        rival.metadata["original_observation"] = prose.clone();
        f.repo
            .create_job_at(&rival, f.stamp.timestamp, &[])
            .await
            .unwrap();
        let mut step = Step::new(rival.id, "task", "Work", 0);
        step.status = StepStatus::Ready;
        step.spec_slug = Some("work".into());
        f.repo.add_step(&step).await.unwrap();
    }
    let admitted = ingress().await;
    let deadline = admitted.started() + boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET;
    f.context.native_authority = Some(Arc::new(
        f.repo
            .prepare_native_selection(admitted, f.context.request_id, &f.context.station.name)
            .await
            .unwrap()
            .unwrap(),
    ));
    let before = f.repo.recorded_events().len();
    let start = std::time::Instant::now();
    let outcome = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    let elapsed = start.elapsed();
    println!(
        "full native observation preparation elapsed={elapsed:?}, outcome={:?}",
        outcome.as_ref().err()
    );
    if !head_is_oversized && let Ok(SelectionResult::Claimed { receipt, .. }) = &outcome {
        let bytes = receipt.json().len();
        assert!(
            bytes < 1024 * 1024,
            "{rows} large competitors must be digested, not copied: {bytes} receipt bytes"
        );
        let counted = receipt
            .pointer("/owner_consumption/competitor_digest/count")
            .and_then(serde_json::Value::as_u64);
        assert!(
            counted.is_some_and(|count| count >= rows as u64),
            "every large competitor must be counted in the digest: {counted:?} of {rows}"
        );
        return;
    }
    assert!(
        matches!(&outcome, Err(JobsError::SelectionConflict(reason)) if reason.contains("deadline expired")),
        "deadline must refuse the oversized attempt"
    );
    // A scheduler pause can delay returning the refusal. The contract is the
    // original ingress deadline and no committed effects, not CPU availability.
    assert!(
        std::time::Instant::now() >= deadline,
        "the refusal must come from the original ingress deadline"
    );
    assert_eq!(
        f.repo.recorded_events().len(),
        before,
        "no evidence fact may escape refusal"
    );
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.repo.get_step(&f.step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
}

#[tokio::test]
async fn full_observation_serialization_obeys_the_original_total_ingress_deadline() {
    observation_deadline(1, 5_000_000, true).await;
}
#[tokio::test]
async fn many_observed_rows_cannot_escape_the_original_total_ingress_deadline() {
    observation_deadline(1000, 5000, false).await;
}

#[tokio::test]
async fn narrow_memory_read_refuses_private_copied_jobs_without_rewriting_original() {
    let mut f = fixture().await;
    let actor = "automation:ops-runner";
    let mut job = f.context.validated_job.clone();
    job.owner_id = actor.into();
    f.repo.update_job(&job).await.unwrap();
    f.context.validated_job = f.repo.get_job(&job.id).await.unwrap().unwrap();
    // A private COMPETITOR, ranked behind the head: only packets that
    // competed are copied since f999363f.
    let mut other = Job::new(
        "fixture",
        Subject::new("custom", "private-other"),
        "Private input",
        "emp-other",
        Priority::Scheduled,
        f.stamp.timestamp.date_naive(),
    );
    other.status = JobStatus::Open;
    f.repo
        .create_job_at(&other, f.stamp.timestamp, &[])
        .await
        .unwrap();
    let mut rival = Step::new(other.id, "task", "Work", 0);
    rival.status = StepStatus::Ready;
    rival.spec_slug = Some("work".into());
    f.repo.add_step(&rival).await.unwrap();
    f.context.native_authority = Some(Arc::new(
        f.repo
            .prepare_native_selection(
                ingress().await,
                f.context.request_id,
                &f.context.station.name,
            )
            .await
            .unwrap()
            .unwrap(),
    ));
    let SelectionResult::Claimed { receipt, .. } = f
        .repo
        .claim_station_head_at(&f.context, actor, &[], &f.stamp, &[])
        .await
        .unwrap()
    else {
        panic!("claim")
    };
    let before = f.repo.recorded_events().len();
    for (scope, allowed) in [
        (boss_policy_client::Scope::Self_, false),
        (boss_policy_client::Scope::All, true),
    ] {
        boss_policy_client::PolicyRepository::upsert_rule(
            &*f.policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id(
                    "worker",
                    &boss_policy_client::Resource::job(),
                    boss_policy_client::Action::Read,
                ),
                role: "worker".into(),
                action: boss_policy_client::Action::Read,
                resource: boss_policy_client::Resource::job(),
                scope,
                active: true,
            },
            "current-read-scope-control",
        )
        .await
        .unwrap();
        let read = f
            .repo
            .native_selection_read(
                &ingress().await,
                f.context.request_id,
                &f.context.station.name,
            )
            .await;
        if allowed {
            assert_eq!(
                read.unwrap().unwrap().original.unwrap().json(),
                receipt.json()
            );
        } else {
            assert!(
                read.is_err(),
                "selected job remains readable but copied other job does not"
            );
        }
    }
    assert_eq!(
        f.repo.recorded_events().len(),
        before,
        "read/refusal/restoration must not mint another claim or fact"
    );
}

async fn fixture() -> Fixture {
    fixture_with_calendar(None).await
}

#[tokio::test]
async fn an_actual_automation_row_prepares_without_becoming_an_agent_candidate() {
    let agents = Arc::new(InMemoryAgents::new());
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    agents
        .declare_automations(
            &[boss_jobs::agents::automations::AutomationActor {
                id: "automation:ops-runner".into(),
                role: "worker".into(),
                description: "actual separate automation owner".into(),
                signs_for: Some("automation:ops-runner:".into()),
                access_tier: None,
                station: None,
            }],
            &stamp,
        )
        .await
        .unwrap();
    let policy = Arc::new(boss_policy_client::InMemoryPolicy::new());
    for (action, resource) in [
        (
            boss_policy_client::Action::Read,
            boss_policy_client::Resource::job(),
        ),
        (
            boss_policy_client::Action::Update,
            boss_policy_client::Resource::step(),
        ),
    ] {
        boss_policy_client::PolicyRepository::upsert_rule(
            &*policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id("worker", &resource, action),
                role: "worker".into(),
                resource,
                action,
                scope: boss_policy_client::Scope::All,
                active: true,
            },
            "automation-owner-control",
        )
        .await
        .unwrap();
    }
    let repo = InMemoryJobs::for_native_selection(NativeMemorySelectionStore::new(
        Arc::new(InMemoryWorkflows::new()),
        Arc::new(InMemoryStations::new()),
        agents.clone(),
        Arc::new(boss_policy_client::FakePolicyClient::from_repository(
            policy,
        )),
    ));
    let prepared = repo
        .prepare_native_selection(ingress().await, uuid::Uuid::new_v4(), "fixture")
        .await;
    assert!(
        prepared.is_ok(),
        "actual automation owner must prepare: {:?}",
        prepared.as_ref().err()
    );
    assert!(prepared.unwrap().is_some());
    assert!(
        agents.list().await.unwrap().is_empty(),
        "automations must never enter executor nominations"
    );
}

async fn fixture_with_calendar(calendar: Option<Arc<boss_calendar::InMemoryCalendar>>) -> Fixture {
    fixture_with_actor(calendar, false).await
}

async fn fixture_with_actor(
    calendar: Option<Arc<boss_calendar::InMemoryCalendar>>,
    automation: bool,
) -> Fixture {
    let stations = Arc::new(InMemoryStations::new());
    let workflows = Arc::new(InMemoryWorkflows::new());
    let agents = Arc::new(InMemoryAgents::new());
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    if automation {
        agents
            .declare_automations(
                &[boss_jobs::agents::automations::AutomationActor {
                    id: "automation:ops-runner".into(),
                    role: "worker".into(),
                    description: "separate automation owner".into(),
                    signs_for: Some("automation:ops-runner:".into()),
                    access_tier: None,
                    station: None,
                }],
                &stamp,
            )
            .await
            .unwrap();
    } else {
        agents
            .publish(
                &[AgentInput {
                    factory_rotation: Some(true),
                    id: "agent-memory".into(),
                    display_name: "Memory fixture".into(),
                    default_model: "test-model".into(),
                    model_class: None,
                    role: Some("worker".into()),
                    department: None,
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: None,
                    stations: None,
                    aliases: vec!["automation:ops-runner".into()],
                }],
                boss_core::publish::PublishMode::InsertIfAbsent,
                &stamp,
            )
            .await
            .unwrap();
    }
    let policy = Arc::new(boss_policy_client::InMemoryPolicy::new());
    for (action, resource) in [
        (
            boss_policy_client::Action::Read,
            boss_policy_client::Resource::job(),
        ),
        (
            boss_policy_client::Action::Update,
            boss_policy_client::Resource::step(),
        ),
    ] {
        boss_policy_client::PolicyRepository::upsert_rule(
            &*policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id("worker", &resource, action),
                role: "worker".into(),
                resource,
                action,
                scope: boss_policy_client::Scope::All,
                active: true,
            },
            "fixture",
        )
        .await
        .unwrap();
    }
    let policy_client = Arc::new(boss_policy_client::FakePolicyClient::from_repository(
        policy.clone(),
    ));
    let mut store = NativeMemorySelectionStore::new(
        workflows.clone(),
        stations.clone(),
        agents.clone(),
        policy_client,
    );
    if let Some(calendar) = &calendar {
        store = store.with_calendar(calendar.clone());
    }
    let repo = InMemoryJobs::for_native_selection(store);
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
        StationKind::Group,
        boss_jobs::station_queue::StationPredicate {
            kind: Some("fixture".into()),
            step: Some(boss_jobs::station_queue::StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    stations.seed(station.clone()).unwrap();
    let request_id = uuid::Uuid::new_v4();
    let proof = ingress().await;
    let admitted_at = proof.started();
    let observation = repo
        .prepare_native_selection(proof, request_id, "fixture")
        .await
        .unwrap()
        .unwrap();
    let context = SelectionContext {
        native_authority: Some(Arc::new(observation)),
        actor_observation: None,
        prepared_policy: None,
        definition_fence: None,
        request_id,
        station,
        scope: boss_jobs::port::JobScope::All,
        active_workflows: Default::default(),
        tree_selection: None,
        validated_job: job,
        validated_step: step.clone(),
    };
    Fixture {
        calendar,
        admitted_at,
        repo,
        stations,
        workflows,
        agents,
        policy,
        context,
        step,
        stamp,
    }
}

#[tokio::test]
async fn a_real_automation_owner_is_conserved_through_claim_and_original_replay() {
    let mut f = fixture_with_actor(None, true).await;
    let SelectionResult::Claimed { receipt, .. } = claim_answer(&mut f).await.unwrap() else {
        panic!("automation must claim");
    };
    assert!(f.agents.list().await.unwrap().is_empty());
    let original = claim_answer(&mut f).await.unwrap();
    let SelectionResult::Replayed(replayed) = original else {
        panic!("original");
    };
    assert_eq!(replayed, receipt);
    let value = serde_json::to_value(receipt).unwrap();
    let actual = f.agents.list_automations().await.unwrap();
    assert_eq!(
        value.pointer("/owner_consumption/prepared_inputs/automations"),
        Some(&serde_json::to_value(actual).unwrap())
    );
}

#[tokio::test]
async fn an_unobserved_historical_budget_is_copied_as_unknown_never_zero() {
    let mut f = fixture().await;
    f.step.metadata[boss_jobs::agent_spec::BUDGET_KEY] = serde_json::json!(1.0);
    f.repo
        .update_step_at(&f.step, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.context.validated_step = f.step.clone();
    let SelectionResult::Claimed { receipt, .. } = claim_answer(&mut f).await.unwrap() else {
        panic!("claim");
    };
    let value = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        value.pointer("/owner_consumption/budget_hour/status"),
        Some(&serde_json::json!("unknown"))
    );
    assert_eq!(
        value.pointer("/owner_consumption/budget_hour/inputs"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        value.pointer("/owner_consumption/budget_hour/observed_at"),
        Some(&serde_json::Value::Null)
    );
}

#[tokio::test]
async fn cancellation_while_a_real_owner_is_busy_leaves_no_detached_memory_effect() {
    let f = fixture().await;
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let stations = f.stations.clone();
    let writer = std::thread::spawn(move || {
        let _guard = stations.lock_active_selection_rows().unwrap();
        held_tx.send(()).unwrap();
        release_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let repo = Arc::new(f.repo);
    let task_repo = repo.clone();
    let context = f.context.clone();
    let stamp = f.stamp.clone();
    let task = tokio::spawn(async move {
        task_repo
            .claim_station_head_at(&context, "automation:ops-runner", &[], &stamp, &[])
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !task.is_finished(),
        "actual station owner keeps mutation waiting"
    );
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        repo.station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.recorded_events().is_empty());
}

#[tokio::test]
async fn native_memory_retains_declared_executor_principal_and_host() {
    for (declaration, host, allowed) in [
        (
            serde_json::json!(boss_jobs::runner_credential::PRINCIPAL),
            "forge",
            true,
        ),
        (
            serde_json::json!(boss_jobs::runner_credential::PRINCIPAL),
            "boss-gcp",
            false,
        ),
        (serde_json::json!("runner:other"), "forge", false),
        (serde_json::json!({"invalid":"declaration"}), "forge", false),
    ] {
        let mut f = fixture().await;
        f.step.metadata[boss_jobs::credential_executor::KEY] = declaration;
        f.context.validated_job.metadata[boss_jobs::field_writer::HOST_KEY] =
            serde_json::json!(host);
        f.repo.update_job(&f.context.validated_job).await.unwrap();
        f.repo
            .update_step_at(&f.step, f.stamp.timestamp, &[])
            .await
            .unwrap();
        f.context.validated_step = f.step.clone();
        let result = f
            .repo
            .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
            .await;
        if allowed {
            assert!(
                matches!(result, Ok(SelectionResult::Claimed { .. })),
                "matching real ingress executor must claim"
            );
        } else {
            assert!(
                matches!(result, Err(JobsError::SelectionConflict(_))),
                "declared executor host/principal/malformed constraint cannot be bypassed"
            );
            assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
            assert!(
                f.repo
                    .station_claim_receipt(f.context.request_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(f.repo.recorded_events().is_empty());
        }
    }
}

async fn scheduled_fixture() -> Fixture {
    let mut f = fixture_with_calendar(Some(Arc::new(boss_calendar::InMemoryCalendar::new()))).await;
    let control = boss_policy_client::controls::CREATE_SCHEDULE;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &control.resource(), control.action()),
            role: "worker".into(),
            resource: control.resource(),
            action: control.action(),
            scope: boss_policy_client::Scope::All,
            active: true,
        },
        "fixture-calendar",
    )
    .await
    .unwrap();
    f.step.metadata["scheduled_at"] =
        serde_json::json!((chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339());
    f.step.metadata["duration_minutes"] = serde_json::json!(30.0);
    f.repo
        .update_step_at(&f.step, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.context.validated_step = f.step.clone();
    admit(&mut f).await.unwrap();
    f
}

fn calendar_request(f: &Fixture, reason_ref: String) -> boss_core::calendar::ReservationRequest {
    let start =
        chrono::DateTime::parse_from_rfc3339(f.step.metadata["scheduled_at"].as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
    boss_core::calendar::ReservationRequest {
        subject: Subject::new("employee", "automation:ops-runner"),
        window: boss_core::calendar::TimeWindow::new(start, start + chrono::Duration::minutes(30))
            .unwrap(),
        reason_kind: boss_core::calendar::reason::JOB_STEP.into(),
        reason_ref_id: reason_ref,
        strength: boss_core::calendar::ReservationStrength::Hard,
        notes: None,
        created_by: "automation:ops-runner".into(),
    }
}

#[tokio::test]
async fn existing_own_hold_is_retained_without_a_second_calendar_fact() {
    let mut f = scheduled_fixture().await;
    let calendar = f.calendar.clone().unwrap();
    let id = boss_calendar::CalendarClient::reserve_at(
        &*calendar,
        calendar_request(&f, f.step.id.to_string()),
        f.stamp.timestamp,
        &f.stamp,
    )
    .await
    .unwrap();
    let original = boss_calendar::CalendarClient::get(&*calendar, id)
        .await
        .unwrap()
        .unwrap();
    let calendar_events = serde_json::to_value(calendar.recorded_events()).unwrap();
    let SelectionResult::Claimed { receipt, .. } = claim_answer(&mut f).await.unwrap() else {
        panic!("claim");
    };
    assert_eq!(calendar.active_count(), 1);
    assert_eq!(
        serde_json::to_value(calendar.recorded_events()).unwrap(),
        calendar_events
    );
    assert_eq!(
        receipt.pointer("/owner_consumption/calendar/consumed/inserted"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        receipt.pointer("/owner_consumption/calendar/consumed/row"),
        Some(&serde_json::to_value(original).unwrap())
    );
}

#[tokio::test]
async fn an_actual_foreign_calendar_hold_refuses_every_joined_effect() {
    let f = scheduled_fixture().await;
    let calendar = f.calendar.as_ref().unwrap();
    let id = boss_calendar::CalendarClient::reserve_at(
        &**calendar,
        calendar_request(&f, "different-step".into()),
        f.stamp.timestamp,
        &f.stamp,
    )
    .await
    .unwrap();
    let original = boss_calendar::CalendarClient::get(&**calendar, id)
        .await
        .unwrap()
        .unwrap();
    let events = serde_json::to_value(calendar.recorded_events()).unwrap();
    let result = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    assert!(
        matches!(result, Err(JobsError::SelectionCalendarConflict { existing }) if existing.len()==1 && existing[0]==original)
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
    assert_eq!(
        serde_json::to_value(calendar.recorded_events()).unwrap(),
        events
    );
}

#[tokio::test]
async fn failed_job_validation_drops_the_staged_calendar_row_and_fact() {
    let f = scheduled_fixture().await;
    f.repo.fail_steps_read(&f.step.job_id);
    assert!(
        f.repo
            .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
            .await
            .is_err()
    );
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
    let calendar = f.calendar.as_ref().unwrap();
    assert_eq!(calendar.active_count(), 0);
    assert!(calendar.recorded_events().is_empty());
}

#[tokio::test]
async fn a_real_calendar_writer_wait_cannot_escape_the_original_ingress_budget() {
    let f = scheduled_fixture().await;
    let calendar = f.calendar.as_ref().unwrap().clone();
    let req = calendar_request(&f, "writer-only-plan".into());
    let stamp = f.stamp.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _plan = calendar.try_stage_claim(req, &stamp).unwrap().unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
        // No apply: retaining the real writer need not create any row/fact.
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let started = std::time::Instant::now();
    let result = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    let elapsed = started.elapsed();
    writer.join().unwrap();
    assert!(
        matches!(result, Err(JobsError::SelectionConflict(_)))
            && elapsed < std::time::Duration::from_millis(5700),
        "real calendar owner wait {elapsed:?}"
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
    assert_eq!(f.calendar.as_ref().unwrap().active_count(), 0);
    assert!(f.calendar.as_ref().unwrap().recorded_events().is_empty());
}

#[tokio::test]
async fn a_real_actor_writer_wait_cancels_before_any_memory_effect() {
    let f = fixture().await;
    let agents = f.agents.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _guard = agents.lock_selection_rows().unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let started = std::time::Instant::now();
    let result = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    let elapsed = started.elapsed();
    writer.join().unwrap();
    assert!(
        matches!(result, Err(JobsError::SelectionConflict(_)))
            && elapsed < std::time::Duration::from_millis(5700),
        "real actor owner wait {elapsed:?}"
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
}

#[tokio::test]
async fn actual_authenticated_memory_claim_commits_and_replays_one_record() {
    let mut f = fixture().await;
    let result = claim_answer(&mut f).await.unwrap();
    let SelectionResult::Claimed { step, receipt } = result else {
        panic!("new claim");
    };
    assert_eq!(step.status, StepStatus::Active);
    assert_eq!(
        receipt
            .pointer("/owner_consumption/ingress/actor_id")
            .and_then(serde_json::Value::as_str),
        Some("automation:ops-runner")
    );
    let events = serde_json::to_value(f.repo.recorded_events()).unwrap();
    let replay = claim_answer(&mut f).await.unwrap();
    assert!(
        matches!(replay, SelectionResult::Replayed(original) if original.json() == receipt.json())
    );
    assert_eq!(
        serde_json::to_value(f.repo.recorded_events()).unwrap(),
        events
    );
}

#[tokio::test]
async fn a_real_policy_writer_revocation_refuses_without_effects() {
    let f = fixture().await;
    let resource = boss_policy_client::Resource::step();
    let action = boss_policy_client::Action::Update;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &resource, action),
            role: "worker".into(),
            resource,
            action,
            scope: boss_policy_client::Scope::All,
            active: false,
        },
        "fixture-revocation",
    )
    .await
    .unwrap();
    assert!(
        f.repo
            .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
            .await
            .is_err()
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
}

#[tokio::test]
async fn a_scheduled_postclaim_assignee_requires_a_real_calendar_owner() {
    let mut f = fixture().await;
    let control = boss_policy_client::controls::CREATE_SCHEDULE;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &control.resource(), control.action()),
            role: "worker".into(),
            resource: control.resource(),
            action: control.action(),
            scope: boss_policy_client::Scope::All,
            active: true,
        },
        "fixture-calendar",
    )
    .await
    .unwrap();
    f.step.metadata["scheduled_at"] =
        serde_json::json!((chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339());
    f.step.metadata["duration_minutes"] = serde_json::json!(30.0);
    assert!(
        f.step.assignee_id.is_none(),
        "the assignee is created by the actual claim"
    );
    f.repo
        .update_step_at(&f.step, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.context.validated_step = f.step.clone();
    f.context.native_authority = Some(Arc::new(
        f.repo
            .prepare_native_selection(ingress().await, f.context.request_id, "fixture")
            .await
            .unwrap()
            .unwrap(),
    ));
    let result = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    assert!(
        result.is_err(),
        "a schedule cannot silently disappear because its preclaim assignee was empty"
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
}

#[tokio::test]
async fn a_real_memory_calendar_joins_the_stored_assignee_and_one_fact() {
    calendar_join(std::time::Duration::ZERO).await;
}

/// The same answer, with the test's thread stalled past the production
/// ingress budget before its first claim — what red train d1251135's
/// gate did to the test above, under three gates on one node (cb503277).
#[tokio::test]
async fn a_real_memory_calendar_join_survives_a_stall_past_its_ingress_budget() {
    calendar_join(
        boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET
            + std::time::Duration::from_millis(100),
    )
    .await;
}

async fn calendar_join(stall: std::time::Duration) {
    let mut f = fixture_with_calendar(Some(Arc::new(boss_calendar::InMemoryCalendar::new()))).await;
    let control = boss_policy_client::controls::CREATE_SCHEDULE;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &control.resource(), control.action()),
            role: "worker".into(),
            resource: control.resource(),
            action: control.action(),
            scope: boss_policy_client::Scope::All,
            active: true,
        },
        "fixture-calendar",
    )
    .await
    .unwrap();
    f.step.metadata["scheduled_at"] =
        serde_json::json!((chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339());
    f.step.metadata["duration_minutes"] = serde_json::json!(30.0);
    f.repo
        .update_step_at(&f.step, f.stamp.timestamp, &[])
        .await
        .unwrap();
    f.context.validated_step = f.step.clone();
    admit(&mut f).await.unwrap();
    std::thread::sleep(stall);
    let SelectionResult::Claimed { step, receipt } = claim_answer(&mut f).await.unwrap() else {
        panic!("new claim");
    };
    assert_eq!(step.assignee_id.as_deref(), Some("automation:ops-runner"));
    let calendar = f.calendar.as_ref().unwrap();
    assert_eq!(calendar.active_count(), 1);
    assert_eq!(calendar.recorded_events().len(), 1);
    let row: boss_core::calendar::Reservation =
        serde_json::from_value(calendar.recorded_events()[0].payload.clone()).unwrap();
    assert_eq!(row.subject.id, step.assignee_id.unwrap());
    assert_eq!(
        receipt
            .pointer("/owner_consumption/calendar/consumed/row")
            .unwrap(),
        &serde_json::to_value(&row).unwrap()
    );
}

#[tokio::test]
async fn matching_owner_configuration_does_not_bind_a_different_jobs_state() {
    let f = fixture().await;
    let other = InMemoryJobs::for_native_selection(NativeMemorySelectionStore::new(
        f.workflows.clone(),
        f.stations.clone(),
        f.agents.clone(),
        Arc::new(boss_policy_client::FakePolicyClient::from_repository(
            f.policy.clone(),
        )),
    ));
    other.create_job(&f.context.validated_job).await.unwrap();
    other.add_step(&f.step).await.unwrap();
    assert!(matches!(
        other
            .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
            .await,
        Err(JobsError::SelectionConflict(_))
    ));
    assert_eq!(other.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        other
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(other.recorded_events().is_empty());
}

#[tokio::test]
async fn original_replay_survives_update_retirement_but_not_read_retirement() {
    let mut f = fixture().await;
    let SelectionResult::Claimed { receipt, .. } = claim_answer(&mut f).await.unwrap() else {
        panic!("new claim");
    };
    let events = serde_json::to_value(f.repo.recorded_events()).unwrap();
    let resource = boss_policy_client::Resource::step();
    let action = boss_policy_client::Action::Update;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &resource, action),
            role: "worker".into(),
            resource,
            action,
            scope: boss_policy_client::Scope::All,
            active: false,
        },
        "fixture-retirement",
    )
    .await
    .unwrap();
    let replay = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await
        .unwrap();
    assert!(
        matches!(replay, SelectionResult::Replayed(original) if original.json() == receipt.json())
    );
    let resource = boss_policy_client::Resource::job();
    let action = boss_policy_client::Action::Read;
    boss_policy_client::PolicyRepository::upsert_rule(
        &*f.policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &resource, action),
            role: "worker".into(),
            resource,
            action,
            scope: boss_policy_client::Scope::All,
            active: false,
        },
        "fixture-read-retirement",
    )
    .await
    .unwrap();
    assert!(
        f.repo
            .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
            .await
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(f.repo.recorded_events()).unwrap(),
        events
    );
}

#[tokio::test]
async fn actual_ingress_budget_bounds_a_real_memory_station_writer() {
    let f = fixture().await;
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let stations = f.stations.clone();
    let writer = std::thread::spawn(move || {
        let _owner = stations.lock_active_selection_rows().unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // Time already spent after authenticated ingress cannot become a fresh
    // five seconds at the repository or at any subsequent owner.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let started = std::time::Instant::now();
    let observed = f
        .repo
        .claim_station_head_at(&f.context, "automation:ops-runner", &[], &f.stamp, &[])
        .await;
    let elapsed = started.elapsed();
    let total_since_ingress = f.admitted_at.elapsed();
    eprintln!(
        "memory ingress budget: repository_wait_ms={} total_since_ingress_ms={}",
        elapsed.as_millis(),
        total_since_ingress.as_millis()
    );
    writer.join().unwrap();
    assert!(
        matches!(observed, Err(JobsError::SelectionConflict(_)))
            && elapsed < std::time::Duration::from_millis(4500)
            && total_since_ingress < std::time::Duration::from_millis(5700),
        "actual ingress owner wait {elapsed:?}, total {total_since_ingress:?}"
    );
    assert_eq!(f.repo.get_step(&f.step.id).await.unwrap().unwrap(), f.step);
    assert!(
        f.repo
            .station_claim_receipt(f.context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.repo.recorded_events().is_empty());
    assert!(
        boss_jobs::WorkflowRegistry::try_lock_active_selection_rows(&*f.workflows)
            .unwrap()
            .is_some()
    );
}
