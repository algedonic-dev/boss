//! Selection and its copied receipt survive another repository instance.
use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
use boss_jobs::native_selection_store::NativeSelectionStore;
use boss_jobs::port::JobScope;
use boss_jobs::station_claim::{SelectionContext, SelectionResult};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::{
    JobsRepository, PgJobs, PgStations, PgWorkflows, StationKind, StationRegistry, StationSpec,
    WorkflowRegistry,
};
use std::sync::Arc;
use tower::ServiceExt;

const ACTOR: &str = boss_jobs::runner_credential::ACTOR;

#[tokio::test(flavor = "multi_thread")]
async fn counted_native_pg_runs_conserve_bindings_and_require_read_scope_before_commit() {
    let (db, repo, mut context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    boss_jobs::agents::AgentsRegistry::publish(
        &boss_jobs::agents::PgAgents::new(db.pool.clone()),
        &[boss_jobs::agents::AgentInput {
            id: "agent-pg".into(),
            display_name: "PG fixture".into(),
            default_model: "opus-5[1m]".into(),
            role: Some("worker".into()),
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(3),
            aliases: vec![ACTOR.into()],
        }],
        boss_core::publish::PublishMode::InsertIfAbsent,
        &stamp,
    )
    .await
    .unwrap();
    context.validated_job.owner_id = ACTOR.into();
    repo.update_job(&context.validated_job).await.unwrap();
    context.validated_job = repo
        .get_job(&context.validated_job.id)
        .await
        .unwrap()
        .unwrap();
    context.validated_step.metadata[boss_jobs::agent_spec::MODEL_KEY] =
        serde_json::json!("opus-5[1m]");
    repo.update_step_at(&context.validated_step, stamp.timestamp, &[])
        .await
        .unwrap();
    context.validated_step = repo
        .get_step(&context.validated_step.id)
        .await
        .unwrap()
        .unwrap();
    let mut private = Job::new(
        "agent-run",
        Subject::new("custom", "private-counted"),
        "Counted private run",
        "emp-other",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    private.status = boss_core::job::JobStatus::Open;
    private.metadata["agent"] = serde_json::json!("agent-pg");
    repo.create_job_at(&private, stamp.timestamp, &[])
        .await
        .unwrap();
    let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
    let read = boss_policy_client::controls::READ_JOB;
    for scope in [
        boss_policy_client::Scope::Self_,
        boss_policy_client::Scope::All,
    ] {
        boss_policy_client::PolicyRepository::upsert_rule(
            &policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id("worker", &read.resource(), read.action()),
                role: "worker".into(),
                action: read.action(),
                resource: read.resource(),
                scope: scope.clone(),
                active: true,
            },
            "counter-scope-regression",
        )
        .await
        .unwrap();
        context.scope = if scope == boss_policy_client::Scope::All {
            JobScope::All
        } else {
            JobScope::OwnerIs(ACTOR.into())
        };
        context.native_authority = Some(prepared(&repo, context.request_id).await);
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let result = repo
            .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
            .await;
        if scope != boss_policy_client::Scope::All {
            assert!(
                matches!(
                    result,
                    Err(boss_jobs::port::JobsError::SelectionConflict(_))
                ),
                "private counted row must refuse before commit"
            );
            assert_eq!(
                repo.get_step(&context.validated_step.id)
                    .await
                    .unwrap()
                    .unwrap(),
                context.validated_step
            );
            assert!(
                repo.station_claim_receipt(context.request_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            assert_eq!(before, after);
        } else {
            let SelectionResult::Claimed { receipt, .. } = result.unwrap() else {
                panic!("fresh claim")
            };
            let row = &receipt["owner_consumption"]["concurrency"]["runs"][0];
            assert_eq!(row["id"], serde_json::json!(private.id));
            assert_eq!(row["owner_id"], serde_json::json!(private.owner_id));
            assert_eq!(row["subject"], serde_json::json!(private.subject));
        }
    }
}

async fn ingress() -> boss_jobs::station_claim::AuthenticatedSelectionIngress {
    let dir = boss_testing::scratch_dir("station-pg-authenticated-ingress");
    boss_testing::write_file(&dir.join("forge.current"), "station-pg-fixture-key");
    let captured = Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    let app = axum::Router::new().route(
        "/fixture",
        axum::routing::get(
            move |proof: axum::Extension<
                boss_jobs::station_claim::AuthenticatedSelectionIngress,
            >| {
                let sink = sink.clone();
                async move {
                    *sink.lock().unwrap() = Some(proof.0);
                    axum::http::StatusCode::NO_CONTENT
                }
            },
        ),
    );
    let app = boss_jobs::runner_credential::mount(app, dir);
    let app = boss_core::machine_gate::gated(
        app,
        Arc::new(boss_core::machine_gate::MachineGate::new(
            "jobs",
            &["/health"],
            boss_core::machine_gate::Reading::new(
                boss_core::machine_gate::Mode::Off,
                Default::default(),
            ),
        )),
    );
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/fixture")
                .header(
                    boss_jobs::runner_credential::HEADER,
                    "station-pg-fixture-key",
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    captured.lock().unwrap().take().unwrap()
}

async fn prepared(
    repo: &PgJobs,
    request: uuid::Uuid,
) -> Arc<boss_jobs::station_claim::NativeAuthorityObservation> {
    Arc::new(
        repo.prepare_native_selection(ingress().await, request, "claim-fixture")
            .await
            .unwrap()
            .unwrap(),
    )
}

async fn fixture() -> (
    boss_testing::TestDb,
    PgJobs,
    SelectionContext,
    NativeSelectionStore,
) {
    let (db, repo, context, native, _) = fixture_with_ingress_time().await;
    (db, repo, context, native)
}

async fn fixture_with_ingress_time() -> (
    boss_testing::TestDb,
    PgJobs,
    SelectionContext,
    NativeSelectionStore,
    std::time::Instant,
) {
    let db = boss_testing::TestDb::new().await;
    let native = NativeSelectionStore::new(db.pool.clone());
    let repo = PgJobs::for_native_selection(native.clone());
    let owner_stamp = boss_core::publisher::EventStamp::new(
        "agents",
        boss_core::actor::ActorId::automation("test"),
    );
    boss_jobs::agents::AgentsRegistry::declare_automations(
        &boss_jobs::agents::PgAgents::new(db.pool.clone()),
        &[boss_jobs::agents::automations::AutomationActor {
            id: ACTOR.into(),
            role: "worker".into(),
            description: "authenticated integration fixture".into(),
            signs_for: None,
        }],
        &owner_stamp,
    )
    .await
    .unwrap();
    let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
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
            &policy,
            &boss_policy_client::PolicyRule {
                id: boss_policy_client::types::rule_id("worker", &resource, action),
                role: "worker".into(),
                action,
                resource,
                scope: boss_policy_client::Scope::All,
                active: true,
            },
            "actual-integration-owner",
        )
        .await
        .unwrap();
    }
    let mut job = Job::new(
        "claim-fixture",
        Subject::new("custom", "fixture"),
        "Select it",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.status = boss_core::job::JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.spec_slug = Some("work".into());
    step.status = StepStatus::Ready;
    let birth = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("test"),
    );
    repo.create_job_at(
        &job,
        birth.timestamp,
        &[birth.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&job).unwrap(),
        )],
    )
    .await
    .unwrap();
    repo.add_step_at(
        &step,
        birth.timestamp,
        &[birth.event(
            boss_jobs::events::STEP_CREATED,
            boss_jobs::events::step_state_payload(&step),
        )],
    )
    .await
    .unwrap();
    let job = repo.get_job(&job.id).await.unwrap().unwrap();
    let step = repo.get_step(&step.id).await.unwrap().unwrap();
    let mut station = StationSpec::draft(
        "claim-fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate {
            kind: Some("claim-fixture".into()),
            step: Some(StepMatch {
                slug: Some("work".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    station.version = 1;
    let stations = PgStations::new(db.pool.clone());
    stations
        .create_draft(
            station,
            &boss_core::actor::ActorId::automation("test"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let station = stations
        .publish(
            "claim-fixture",
            &boss_core::actor::ActorId::automation("test"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let active_workflows = PgWorkflows::new(db.pool.clone())
        .list_active(None)
        .await
        .unwrap()
        .into_iter()
        .map(|workflow| (workflow.kind.clone(), workflow))
        .collect();
    let request_id = uuid::Uuid::new_v4();
    let ingress = ingress().await;
    let ingress_started = ingress.started();
    let context = SelectionContext {
        native_authority: Some(Arc::new(
            repo.prepare_native_selection(ingress, request_id, "claim-fixture")
                .await
                .unwrap()
                .unwrap(),
        )),
        actor_observation: None,
        prepared_policy: None,
        definition_fence: None,
        request_id,
        station,
        scope: JobScope::default(),
        active_workflows,
        validated_job: job.clone(),
        validated_step: step.clone(),
    };
    (db, repo, context, native, ingress_started)
}

#[tokio::test(flavor = "multi_thread")]
async fn native_selection_conserves_complete_observed_rows_after_restart_and_rebuild() {
    let (db, repo, mut context, store) = fixture().await;
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { receipt, .. } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("unchanged native owners must claim")
    };
    let population = receipt
        .pointer("/owner_consumption/open_population")
        .and_then(serde_json::Value::as_array)
        .expect("complete observed population must be reconstructible, not a digest alone");
    assert!(
        population
            .iter()
            .any(|row| row[0]["id"] == serde_json::json!(context.validated_job.id))
    );
    let definitions = receipt
        .pointer("/owner_consumption/definitions/stations")
        .and_then(serde_json::Value::as_array)
        .expect("actual station rows must survive, not just their digest");
    assert!(definitions.iter().any(|row| row["name"] == "claim-fixture"));
    let restarted = PgJobs::for_native_selection(store);
    assert_eq!(
        restarted
            .station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        restarted
            .station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
}

#[tokio::test]
async fn a_later_chunk_stage_failure_rolls_back_claim_receipt_and_all_earlier_chunks() {
    let (db, repo, mut context, _) = fixture().await;
    let mut other = Job::new(
        "unrelated",
        Subject::new("custom", "chunk-rollback"),
        "Other input",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    other.status = boss_core::job::JobStatus::Open;
    other.metadata["complete_observation"] = serde_json::json!("evidence".repeat(60000));
    repo.create_job_at(&other, chrono::Utc::now(), &[])
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION refuse_second_selection_chunk() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind = 'jobs.station.selection_evidence' AND NEW.payload->>'index' = '1' THEN RAISE EXCEPTION 'second evidence stage refused'; END IF; RETURN NEW; END $$; CREATE TRIGGER refuse_second_selection_chunk BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION refuse_second_selection_chunk();")
        .execute(&db.pool).await.unwrap();
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let refused = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .err()
        .expect("the injected second-stage failure must refuse");
    assert!(
        refused
            .to_string()
            .contains("second evidence stage refused"),
        "{refused}"
    );
    assert_eq!(
        repo.get_step(&context.validated_step.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        StepStatus::Ready
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    let recorded:i64=sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind IN ('jobs.station.claimed','jobs.station.selection_evidence')").fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        recorded, 0,
        "no staged chunk or claim may escape transaction rollback"
    );
    sqlx::raw_sql("DROP TRIGGER refuse_second_selection_chunk ON event_outbox; DROP FUNCTION refuse_second_selection_chunk();").execute(&db.pool).await.unwrap();
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let SelectionResult::Claimed { receipt, .. } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("same request may succeed after refusal")
    };
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind='jobs.station.selection_evidence'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        count > 1,
        "positive control must actually stage several chunks"
    );
    let bus: Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    // The fixture's unrelated row is itself a real observed input, even though
    // this rebuild has no birth fact for it. Reconstruction must use its COPY.
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .unwrap()
            .json(),
        receipt.json()
    );
    assert!(repo.get_job(&other.id).await.unwrap().is_none());
}

#[tokio::test]
async fn original_v1_inline_receipt_keeps_its_exact_whitespace_after_rebuild() {
    let (db, repo, context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { receipt, .. } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("claim")
    };
    let mut historical = stamp.event(boss_jobs::events::STATION_CLAIMED, serde_json::json!({}));
    let request = uuid::Uuid::new_v4();
    let mut value = receipt.value().clone();
    value["version"] = serde_json::json!(1);
    value["event_id"] = serde_json::json!(historical.id);
    value["request_id"] = serde_json::json!(request);
    let exact = format!(" \n{}\n ", serde_json::to_string(&value).unwrap());
    historical.payload["receipt_json"] = serde_json::json!(exact);
    historical.payload["receipt"] = value;
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &historical)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    for _ in 0..2 {
        boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
        assert_eq!(
            repo.station_claim_receipt(request)
                .await
                .unwrap()
                .unwrap()
                .json()
                .as_bytes(),
            exact.as_bytes()
        );
    }
}

#[tokio::test]
async fn original_receipt_replay_requires_current_read_scope_for_every_copied_job() {
    let (db, repo, mut context, store) = fixture().await;
    let mut selected = context.validated_job.clone();
    selected.owner_id = ACTOR.into();
    repo.update_job(&selected).await.unwrap();
    context.validated_job = repo.get_job(&selected.id).await.unwrap().unwrap();
    let mut other = Job::new(
        "unrelated",
        Subject::new("custom", "private-other"),
        "Other actor's private input",
        "emp-other",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    other.status = boss_core::job::JobStatus::Open;
    other.metadata["private_observation"] = serde_json::json!("other actor's immutable content");
    repo.create_job_at(&other, chrono::Utc::now(), &[])
        .await
        .unwrap();
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { receipt, .. } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("initial All scope must claim")
    };
    let policy = boss_policy_client::postgres::PgPolicy::new(db.pool.clone());
    boss_policy_client::PolicyRepository::upsert_rule(
        &policy,
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
        "scope-narrowing-control",
    )
    .await
    .unwrap();
    let refused = store
        .read_selection(&ingress().await, context.request_id, "claim-fixture")
        .await;
    assert!(
        refused.is_err(),
        "selected job is still readable, but exact receipt includes an unreadable other job"
    );
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .unwrap()
            .json(),
        receipt.json(),
        "read refusal must not redact or rewrite original bytes"
    );
    boss_policy_client::PolicyRepository::upsert_rule(
        &policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id(
                "worker",
                &boss_policy_client::Resource::job(),
                boss_policy_client::Action::Read,
            ),
            role: "worker".into(),
            action: boss_policy_client::Action::Read,
            resource: boss_policy_client::Resource::job(),
            scope: boss_policy_client::Scope::All,
            active: true,
        },
        "scope-restoration-control",
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .read_selection(&ingress().await, context.request_id, "claim-fixture")
            .await
            .unwrap()
            .original
            .unwrap()
            .json(),
        receipt.json()
    );
    let claims: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind='jobs.station.claimed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        claims, 1,
        "read refusal and restoration must not issue another claim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_commits_the_owned_step_and_replays_the_same_server_receipt() {
    let (db, repo, context, store) = fixture().await;
    let step = context.validated_step.clone();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let mut unpublished = context.clone();
    unpublished.request_id = uuid::Uuid::new_v4();
    unpublished.native_authority = Some(prepared(&repo, unpublished.request_id).await);
    unpublished.station.title = "definition never observed in the registry".into();
    assert!(
        matches!(
            repo.claim_station_head_at(&unpublished, ACTOR, &[], &stamp, &[])
                .await,
            Err(boss_jobs::port::JobsError::SelectionConflict(_))
        ),
        "unpublished station observations cannot claim a packet"
    );
    let mut invented_stamp = context.clone();
    invented_stamp.request_id = uuid::Uuid::new_v4();
    invented_stamp.native_authority = Some(prepared(&repo, invented_stamp.request_id).await);
    invented_stamp.station.created_at += chrono::Duration::seconds(1);
    assert!(
        matches!(
            repo.claim_station_head_at(&invented_stamp, ACTOR, &[], &stamp, &[])
                .await,
            Err(boss_jobs::port::JobsError::SelectionConflict(_))
        ),
        "an authored registry row must conserve its actual stamp"
    );
    let selected = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap();
    let SelectionResult::Claimed {
        step: claimed,
        receipt,
    } = selected
    else {
        panic!("first request must acquire its READY step")
    };
    assert_eq!(claimed.status, StepStatus::Active);
    assert_eq!(claimed.assignee_id.as_deref(), Some(ACTOR));
    let stored_text: String = sqlx::query_scalar(
        "SELECT receipt::text FROM station_claim_receipts WHERE request_id = $1",
    )
    .bind(context.request_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        stored_text.as_bytes(),
        serde_json::to_vec(&receipt).unwrap(),
        "the durable artifact must conserve the exact server JSON bytes"
    );
    use sha2::Digest;
    let recorded_definition = PgStations::new(db.pool.clone())
        .get_version(&context.station.name, context.station.version)
        .await
        .unwrap();
    let digest = hex::encode(sha2::Sha256::digest(boss_core::job::canonical_json_bytes(
        &serde_json::to_value(recorded_definition).unwrap(),
    )));
    assert_eq!(
        receipt["station_hash"], digest,
        "the witness must bind the full published definition"
    );
    assert_eq!(
        serde_json::json!(receipt["validated"]["workflows"].as_object().unwrap().len()),
        serde_json::json!(context.active_workflows.len()),
        "the registry evidence must bind every observed workflow"
    );
    let restarted = PgJobs::for_native_selection(store.clone());
    assert_eq!(
        restarted
            .station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    PgStations::new(db.pool.clone())
        .retire(
            &context.station.name,
            &boss_core::actor::ActorId::automation("test"),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert!(
        PgStations::new(db.pool.clone())
            .list_active()
            .await
            .unwrap()
            .iter()
            .all(|station| station.name != context.station.name)
    );
    let native = boss_jobs::native_selection_store::NativeSelectionStore::new(db.pool.clone());
    let foreign = boss_jobs::native_selection_store::NativeSelectionStore::new(db.pool.clone());
    assert!(matches!(
        native
            .acquire_selection_for(&foreign, uuid::Uuid::new_v4(), ACTOR, &context.station.name)
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    assert!(matches!(
        native
            .acquire_selection_for(
                &foreign,
                context.request_id,
                "emp-other",
                &context.station.name
            )
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    assert!(matches!(
        native
            .acquire_selection_for(&foreign, context.request_id, ACTOR, "another-station")
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    let replay = native
        .acquire_selection_for(&foreign, context.request_id, ACTOR, &context.station.name)
        .await
        .expect("original replay must precede unavailable fresh owners");
    let boss_jobs::native_selection_store::NativeSelectionAcquisition::Replayed(original) = replay
    else {
        panic!("original request must replay without fresh acquisition");
    };
    assert_eq!(
        serde_json::to_vec(&original).unwrap(),
        serde_json::to_vec(&receipt).unwrap()
    );
    let repeated = restarted
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap();
    let SelectionResult::Replayed(copy) = repeated else {
        panic!("retry must not acquire again")
    };
    assert_eq!(copy, receipt);
    assert_eq!(
        restarted.get_step(&step.id).await.unwrap().unwrap(),
        *claimed
    );
    assert!(matches!(
        restarted
            .claim_station_head_at(&context, "emp-other", &[], &stamp, &[])
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    let mut other_station = context.clone();
    other_station.station.name = "another-station".into();
    assert!(matches!(
        restarted
            .claim_station_head_at(&other_station, ACTOR, &[], &stamp, &[])
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM station_claim_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "foreign identity replays must not mint receipts");
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.station.claimed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(events, 1, "replay must not emit another selection event");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_higher_priority_intake_before_selection_refuses_the_prepared_lower_head() {
    let (db, repo, context, _) = fixture().await;
    let mut urgent = context.validated_job.clone();
    urgent.id = boss_core::job::JobId::new();
    urgent.priority = Priority::Urgent;
    let mut urgent_step = context.validated_step.clone();
    urgent_step.id = boss_core::job::StepId::new();
    urgent_step.job_id = urgent.id;
    repo.create_job(&urgent).await.unwrap();
    repo.add_step(&urgent_step).await.unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    assert!(matches!(
        repo.claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(_))
    ));
    for id in [context.validated_step.id, urgent_step.id] {
        assert_eq!(
            repo.get_step(&id).await.unwrap().unwrap().status,
            StepStatus::Ready
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM station_claim_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "stale selection cannot leave a receipt");
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_same_request_claims_once_and_copies_one_receipt() {
    let (db, repo, context, store) = fixture().await;
    let second = PgJobs::for_native_selection(store);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let (left, right) = tokio::join!(
        repo.claim_station_head_at(&context, ACTOR, &[], &stamp, &[]),
        second.claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
    );
    let (claim, replay) = match (left.unwrap(), right.unwrap()) {
        (SelectionResult::Claimed { receipt, .. }, SelectionResult::Replayed(copy))
        | (SelectionResult::Replayed(copy), SelectionResult::Claimed { receipt, .. }) => {
            (receipt, copy)
        }
        _ => panic!("concurrent retry must make exactly one acquisition"),
    };
    assert_eq!(claim, replay);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.station.claimed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn competing_distinct_requests_cannot_both_acquire_the_prepared_head() {
    let (db, repo, context, store) = fixture().await;
    let second = PgJobs::for_native_selection(store);
    let mut competing = context.clone();
    competing.request_id = uuid::Uuid::new_v4();
    competing.native_authority = Some(prepared(&second, competing.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let (left, right) = tokio::join!(
        repo.claim_station_head_at(&context, ACTOR, &[], &stamp, &[]),
        second.claim_station_head_at(&competing, ACTOR, &[], &stamp, &[])
    );
    let claimed = match (left, right) {
        (
            Ok(SelectionResult::Claimed { step, .. }),
            Err(boss_jobs::port::JobsError::SelectionConflict(_)),
        )
        | (
            Err(boss_jobs::port::JobsError::SelectionConflict(_)),
            Ok(SelectionResult::Claimed { step, .. }),
        ) => step,
        _ => panic!("distinct selectors must conserve one acquisition"),
    };
    assert_eq!(repo.get_step(&claimed.id).await.unwrap().unwrap(), *claimed);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM station_claim_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.station.claimed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((receipts, events), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn http_retries_copy_the_exact_postgres_receipt_bytes() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    let (db, repo, context, _) = fixture().await;
    let bus = boss_testing::RecordingEventBus::new();
    let event_bus: Arc<dyn boss_core::port::EventBus> = bus.clone();
    let state = boss_jobs::http::JobsApiState {
        agent_budget: Some(Arc::new(boss_jobs::agent_budget::BudgetDoor {
            agents: Arc::new(boss_jobs::agents::PgAgents::new(db.pool.clone())),
            runs: Arc::new(boss_jobs::agent_runs::PgAgentRuns::new(db.pool.clone())),
        })),
        stations: Some(Arc::new(PgStations::new(db.pool.clone()))),
        kind_registry: Some(Arc::new(PgWorkflows::new(db.pool.clone()))),
        ..boss_jobs::http::JobsApiState::minimal(
            Arc::new(repo),
            bus,
            boss_core::publisher::DomainPublisher::new(event_bus, "jobs"),
            Arc::new(
                FakePolicyClient::builder()
                    .allow("ceo", Action::Read, Resource::job(), Scope::All)
                    .allow("ceo", Action::Update, Resource::step(), Scope::All)
                    .build(),
            ),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let keys = boss_testing::scratch_dir("station-pg-http-ingress");
    boss_testing::write_file(&keys.join("forge.current"), "station-pg-fixture-key");
    let app = boss_jobs::runner_credential::mount(boss_jobs::http::router(state), keys);
    let app = boss_core::machine_gate::gated(
        app,
        Arc::new(boss_core::machine_gate::MachineGate::new(
            "jobs",
            &["/health"],
            boss_core::machine_gate::Reading::new(
                boss_core::machine_gate::Mode::Off,
                Default::default(),
            ),
        )),
    );
    let user = serde_json::to_string(&boss_policy_client::types::User {
        id: "emp-worker".into(),
        role: "ceo".into(),
        access_tier: boss_policy_client::types::AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    })
    .unwrap();
    let mut copies = Vec::new();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/stations/claim-fixture/claim-next")
                    .header("content-type", "application/json")
                    .header("x-boss-user", &user)
                    .header(
                        boss_jobs::runner_credential::HEADER,
                        "station-pg-fixture-key",
                    )
                    .body(Body::from(
                        serde_json::json!({"request_id": context.request_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        copies.push(body);
    }
    let stored: String =
        sqlx::query_scalar("SELECT receipt FROM station_claim_receipts WHERE request_id = $1")
            .bind(context.request_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(copies[0], copies[1]);
    assert_eq!(copies[0].as_ref(), stored.as_bytes());
}

async fn wait_for_table_waiter(db: &boss_testing::TestDb, mode: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = 'jobs'::regclass AND mode = $1 AND NOT granted)")
                .bind(mode).fetch_one(&db.pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.expect("observe the actual blocked writer, not just a spawned future");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_intake_waiting_first_is_observed_before_the_blocked_selection() {
    let (db, _repo, context, store) = fixture().await;
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs, steps IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .unwrap();
    let writer = PgJobs::new(db.pool.clone());
    let mut urgent = context.validated_job.clone();
    urgent.id = boss_core::job::JobId::new();
    urgent.priority = Priority::Urgent;
    let mut urgent_step = context.validated_step.clone();
    urgent_step.id = boss_core::job::StepId::new();
    urgent_step.job_id = urgent.id;
    let intake = tokio::spawn(async move {
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("test"),
        );
        let job_event = stamp.event(
            boss_jobs::events::JOB_CREATED,
            serde_json::to_value(&urgent).unwrap(),
        );
        let step_event = stamp.event(
            boss_jobs::events::STEP_CREATED,
            boss_jobs::events::step_state_payload(&urgent_step),
        );
        writer
            .create_job_with_steps_at(
                &urgent,
                &[urgent_step],
                stamp.timestamp,
                &[job_event],
                &[step_event],
            )
            .await
    });
    wait_for_table_waiter(&db, "RowExclusiveLock").await;
    let selector = PgJobs::for_native_selection(store);
    let prepared = context.clone();
    let selected = tokio::spawn(async move {
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("ops-runner"),
        );
        selector
            .claim_station_head_at(&prepared, ACTOR, &[], &stamp, &[])
            .await
    });
    wait_for_table_waiter(&db, "ShareRowExclusiveLock").await;
    held.commit().await.unwrap();
    assert_eq!(
        intake.await.unwrap().unwrap(),
        boss_jobs::port::Admission::Admitted
    );
    assert!(
        matches!(
            selected.await.unwrap(),
            Err(boss_jobs::port::JobsError::SelectionConflict(_))
        ),
        "selection must observe the completed earlier intake, then refuse its stale lower head"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM station_claim_receipts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_rebuild_recovers_the_owned_step_and_exact_selection_receipt() {
    let (db, repo, context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { step, receipt } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("fresh request must acquire its step")
    };
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    sqlx::query("DELETE FROM station_claim_receipts")
        .execute(&db.pool)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), *step);
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), *step);
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_owned_actor_in_the_audit_receipt_refuses_rebuild_without_changing_projections() {
    let (db, repo, context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { step, receipt } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("fresh request must acquire its step")
    };
    let mut event = stamp.event(boss_jobs::events::STATION_CLAIMED, serde_json::json!({}));
    let mut corrupt = receipt.value().clone();
    corrupt["actor"] = serde_json::json!("emp-other");
    corrupt["event_id"] = serde_json::json!(event.id);
    event.payload["receipt_json"] = serde_json::json!(serde_json::to_string(&corrupt).unwrap());
    event.payload["receipt"] = corrupt;
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    let refused = boss_jobs::rebuild_jobs_and_steps(&db.pool)
        .await
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("disagrees with its immutable event"),
        "corruption must be refused by its provenance check: {refused}"
    );
    assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), *step);
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
}

async fn selection_effects(pool: &sqlx::PgPool) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'jobs', (SELECT coalesce(jsonb_agg(to_jsonb(j) ORDER BY id), '[]') FROM jobs j),
            'steps', (SELECT coalesce(jsonb_agg(to_jsonb(s) ORDER BY id), '[]') FROM steps s),
            'receipts', (SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY request_id), '[]') FROM station_claim_receipts r),
            'outbox', (SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY id), '[]') FROM event_outbox e))",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn bounded_selection_refusal(error: &boss_jobs::port::JobsError) -> Option<&'static str> {
    use boss_jobs::port::JobsError;
    match error {
        JobsError::Storage(message)
            if matches!(
                message.as_str(),
                "error returned from database: canceling statement due to lock timeout"
                    | "error returned from database: canceling statement due to statement timeout"
                    | "pre-commit statement budget expired"
            ) =>
        {
            Some("sql-budget")
        }
        JobsError::SelectionConflict(message)
            if message == "selection ingress deadline expired before commit dispatch" =>
        {
            Some("ingress-budget")
        }
        _ => None,
    }
}

#[test]
fn contention_refusal_does_not_accept_unrelated_errors_or_the_wrong_variant() {
    use boss_jobs::port::JobsError;
    for error in [
        JobsError::SelectionConflict("native current read identity or scope changed".into()),
        JobsError::Storage("connection closed".into()),
        JobsError::Storage("selection ingress deadline expired before commit dispatch".into()),
        JobsError::SelectionConflict("pre-commit statement budget expired".into()),
        JobsError::Storage("not a lock timeout".into()),
    ] {
        assert_eq!(bounded_selection_refusal(&error), None, "{error:?}");
    }
}

async fn fresh_request_can_claim_after_refusal(repo: &PgJobs, context: &SelectionContext) {
    // This is a distinct authenticated request after a proven non-mutating
    // refusal, not a renewed deadline or uncertain-commit retry of the first.
    let mut fresh = context.clone();
    fresh.request_id = uuid::Uuid::new_v4();
    fresh.native_authority = Some(prepared(repo, fresh.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { step, receipt } = repo
        .claim_station_head_at(&fresh, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("the newly authenticated request must claim after the lock is released")
    };
    assert_eq!(step.id, context.validated_step.id);
    assert_eq!(step.status, StepStatus::Active);
    assert_eq!(
        repo.station_claim_receipt(fresh.request_id).await.unwrap(),
        Some(receipt)
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn original_ingress_expiration_refuses_without_dispatching_the_contended_claim() {
    let (db, repo, context, _, ingress_started) = fixture_with_ingress_time().await;
    let before = selection_effects(&db.pool).await;
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs, steps IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .unwrap();
    // Retain the authenticated ingress, including time already spent before
    // the repository call. This must never become a new five-second budget.
    tokio::time::sleep(
        boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET
            + std::time::Duration::from_millis(100),
    )
    .await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let started = std::time::Instant::now();
    let refused = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await;
    eprintln!(
        "expired original ingress actual_error={:?}; elapsed={:?}; original_ingress_age={:?}",
        refused.as_ref().err(),
        started.elapsed(),
        ingress_started.elapsed()
    );
    let error = refused
        .err()
        .expect("an expired ingress cannot claim or replay");
    assert_eq!(
        bounded_selection_refusal(&error),
        Some("ingress-budget"),
        "{error:?}"
    );
    assert!(ingress_started.elapsed() >= boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET);
    // Do not add another wallclock race: exact pre-dispatch expiry and the
    // retained rows prove refusal without relying on a sub-second scheduler.
    held.rollback().await.unwrap();
    assert_eq!(selection_effects(&db.pool).await, before);
    fresh_request_can_claim_after_refusal(&repo, &context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn contention_refuses_with_a_bounded_wait_and_leaves_the_step_and_receipt_untouched() {
    let (db, repo, context, _, ingress_started) = fixture_with_ingress_time().await;
    let before = selection_effects(&db.pool).await;
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs, steps IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let started = std::time::Instant::now();
    let refused = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await;
    let elapsed = started.elapsed();
    eprintln!(
        "selection contention actual_error={:?}; elapsed={elapsed:?}; original_ingress_age={:?}",
        refused.as_ref().err(),
        ingress_started.elapsed()
    );
    let error = refused
        .err()
        .expect("contention must refuse instead of selecting without its fence");
    // SQL's remaining statement timeout and the outer absolute ingress timeout
    // guard the same operation. Either can report its own exact refusal first.
    assert!(
        bounded_selection_refusal(&error).is_some(),
        "refusal must name its actual bounded wait: {error:?}"
    );
    assert!(elapsed >= std::time::Duration::from_millis(1500));
    assert!(elapsed < std::time::Duration::from_millis(5700));
    eprintln!("selection lock refusal elapsed={elapsed:?}; actual total ingress budget is 5s");
    held.commit().await.unwrap();
    assert_eq!(selection_effects(&db.pool).await, before);
    assert_eq!(
        repo.get_step(&context.validated_step.id)
            .await
            .unwrap()
            .unwrap(),
        context.validated_step
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    fresh_request_can_claim_after_refusal(&repo, &context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_event_identity_definition_hash_or_raw_copy_cannot_rebuild_as_a_receipt() {
    for field in ["event_id", "event_actor", "station_hash", "receipt_json"] {
        let (db, repo, context, _) = fixture().await;
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("ops-runner"),
        );
        let SelectionResult::Claimed { step, receipt } = repo
            .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
            .await
            .unwrap()
        else {
            panic!("fresh request must acquire its step")
        };
        let mut event = stamp.event(boss_jobs::events::STATION_CLAIMED, serde_json::json!({}));
        let mut corrupt = receipt.value().clone();
        corrupt["event_id"] = serde_json::json!(event.id);
        match field {
            "event_id" => corrupt[field] = serde_json::json!(uuid::Uuid::new_v4()),
            "event_actor" => corrupt[field] = serde_json::json!("automation:other"),
            "station_hash" => corrupt[field] = serde_json::json!("invented"),
            "receipt_json" => (),
            _ => unreachable!(),
        }
        event.payload["receipt_json"] = serde_json::json!(serde_json::to_string(&corrupt).unwrap());
        event.payload["receipt"] = corrupt;
        if field == "receipt_json" {
            event.payload["receipt_json"] = serde_json::json!("{}");
        }
        let mut tx = db.pool.begin().await.unwrap();
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let bus: std::sync::Arc<dyn boss_core::port::EventBus> =
            boss_testing::RecordingEventBus::new();
        boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
            .await
            .unwrap();
        let refused = boss_jobs::rebuild_jobs_and_steps(&db.pool)
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("immutable event"),
            "{field} must fail its provenance binding, not a later duplicate check: {refused}"
        );
        assert_eq!(repo.get_step(&step.id).await.unwrap().unwrap(), *step);
        assert_eq!(
            repo.station_claim_receipt(context.request_id)
                .await
                .unwrap(),
            Some(receipt)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pg_selector_without_native_authority_owners_refuses_before_effects() {
    let (db, _configured, mut context, _) = fixture().await;
    let repo = PgJobs::new(db.pool.clone());
    context.native_authority = None;
    assert!(context.actor_observation.is_none());
    assert!(context.definition_fence.is_none());
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("emp-worker"),
    );
    let result = repo
        .claim_station_head_at(&context, "emp-worker", &[], &stamp, &[])
        .await;
    assert!(
        matches!(
            result,
            Err(boss_jobs::port::JobsError::SelectionConflict(_))
        ),
        "native Pg mutation must consume actual authority-owner evidence, not merely local definition SQL locks"
    );
    assert_eq!(
        repo.get_step(&context.validated_step.id)
            .await
            .unwrap()
            .unwrap(),
        context.validated_step
    );
    assert!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .is_none()
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, before);
}
