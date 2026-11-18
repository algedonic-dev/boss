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

async fn resource_fixture(
    capacity: serde_json::Value,
    duration_hours: Option<f64>,
) -> (
    boss_testing::TestDb,
    PgJobs,
    SelectionContext,
    NativeSelectionStore,
    boss_core::publisher::EventStamp,
) {
    use boss_jobs::pools::PoolRegistry;
    let (db, repo, mut context, store) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let spec:boss_jobs::registry::WorkflowSpec=serde_json::from_value(serde_json::json!({
        "kind":"claim-fixture","label":"Flow fixture","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},{"title":"work","kind":"task","ready_when":"steps.start.done","duration_hours":duration_hours,"resources":{"vcpu":"1"}},{"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}]})).unwrap();
    let workflows = PgWorkflows::for_fixture(db.pool.clone());
    workflows
        .publish_authored(
            spec,
            boss_core::job::JobId::new(),
            stamp.actor(),
            stamp.timestamp,
        )
        .await
        .unwrap();
    let pools = boss_jobs::pools::PgPools::new(db.pool.clone());
    pools.publish_declared(serde_json::from_value(serde_json::json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":capacity,"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    context.active_workflows = workflows
        .list_active(None)
        .await
        .unwrap()
        .into_iter()
        .map(|w| (w.kind.clone(), w))
        .collect();
    context.native_authority = None;
    (db, repo, context, store, stamp)
}

#[tokio::test(flavor = "multi_thread")]
async fn native_station_resource_refusal_is_an_original_receipt_not_a_claim_and_rebuilds() {
    let (db, repo, mut context, store, stamp) =
        resource_fixture(serde_json::json!({"state":"unknown"}), None).await;
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let SelectionResult::Refused { receipt } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("resource refusal must not be called claimed/replayed")
    };
    assert_eq!(receipt["outcome"], "resource-refused");
    assert_eq!(receipt["refusal"]["reason"], "capacity-unknown");
    assert!(receipt.get("claimed_step").is_none());
    let step = repo
        .get_step(&context.validated_step.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.status, StepStatus::Ready);
    assert!(step.assignee_id.is_none());
    let claimed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind='jobs.station.claimed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(claimed, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM step_resource_claims")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let bus: Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    let restarted = PgJobs::for_native_selection(store);
    assert_eq!(
        restarted
            .station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .unwrap()
            .json(),
        receipt.json()
    );
    context.native_authority = Some(prepared(&restarted, context.request_id).await);
    let SelectionResult::Replayed(original) = restarted
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("original refusal must replay")
    };
    assert_eq!(original.json(), receipt.json());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resource_task_without_a_resolved_duration_refuses_before_inserting_the_header() {
    let (db, repo, mut context, _, stamp) =
        resource_fixture(serde_json::json!({"state":"configured","amount":"1"}), None).await;
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let SelectionResult::Refused { receipt } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("an unknown duration cannot acquire a resource lease")
    };
    assert_eq!(receipt["refusal"]["reason"], "duration-unknown");
    let headers: i64 = sqlx::query_scalar("SELECT count(*) FROM step_resource_claims")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(headers, 0);
    let step = repo
        .get_step(&context.validated_step.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.status, StepStatus::Ready);
    assert!(step.assignee_id.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn native_resource_statements_spend_the_same_original_budget_after_a_real_lock_wait() {
    // A resource-bearing task needs an authored duration before 2b can
    // acquire its lease. The unknown-duration case above must refuse before
    // the header; this case must reach the real writer and spend one budget.
    let (db, repo, mut context, _, stamp) = resource_fixture(
        serde_json::json!({"state":"configured","amount":"1"}),
        Some(1.0),
    )
    .await;
    sqlx::raw_sql("CREATE TABLE resource_budget_samples(stage TEXT PRIMARY KEY,timeout_ms BIGINT NOT NULL,observed_at TIMESTAMPTZ NOT NULL); CREATE FUNCTION capture_resource_budget() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO resource_budget_samples VALUES(TG_ARGV[0],(SELECT setting::bigint FROM pg_settings WHERE name='statement_timeout'),clock_timestamp()); IF TG_ARGV[0]='header' THEN PERFORM pg_advisory_xact_lock(21761,5192); END IF; RETURN NULL; END $$; CREATE TRIGGER resource_header_budget AFTER INSERT ON step_resource_claims FOR EACH STATEMENT EXECUTE FUNCTION capture_resource_budget('header'); CREATE TRIGGER resource_allocation_budget BEFORE DELETE ON step_resource_allocations FOR EACH STATEMENT EXECUTE FUNCTION capture_resource_budget('allocation');").execute(&db.pool).await.unwrap();
    let mut held = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(21761,5192)")
        .execute(&mut *held)
        .await
        .unwrap();
    // Authenticate this request once, after fixture setup, and never renew
    // it while it executes. It runs under the answer fixture's budget: the
    // subject is that later SQL receives less of ONE absolute budget, which
    // holds for any length, and a stalled drive must not decide the claim.
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let selection = repo.claim_station_head_at(&context, ACTOR, &[], &stamp, &[]);
    let release = async {
        tokio::time::timeout(std::time::Duration::from_secs(1),async {
            loop {
                let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND locktype='advisory' AND classid=21761 AND objid=5192 AND NOT granted)").fetch_one(&db.pool).await.unwrap();
                if waiting{break}
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("observe the actual native writer blocked at the resource header");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        held.rollback().await.unwrap();
    };
    let (result, ()) = tokio::join!(selection, release);
    assert!(matches!(result.unwrap(), SelectionResult::Claimed { .. }));
    let samples: Vec<(String, i64, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT stage,timeout_ms,observed_at FROM resource_budget_samples ORDER BY stage",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    eprintln!(
        "actual native resource statement budget samples={samples:?}; advisory wait observed before release"
    );
    let header = samples.iter().find(|row| row.0 == "header").unwrap().1;
    let allocation = samples.iter().find(|row| row.0 == "allocation").unwrap().1;
    assert!(
        header > 0 && allocation > 0 && allocation < header,
        "the later resource SQL must receive less of the SAME absolute budget, not its stale inherited timeout: {samples:?}"
    );
}

/// A PRIVATE OPEN RUN NEVER REACHES A NATIVE PG RECEIPT (backlog 3e927996).
/// This test pinned the per-actor run cap's copied counter rows: their job
/// bindings under All, and a refusal before commit under Self where a
/// counted row was outside the reader's scope. The cap is retired — a robot
/// is bounded by its `robots:<model>` pool alone — so nothing is counted or
/// copied: under the narrow Self scope the claim is admitted and the private
/// run's identity appears nowhere in the receipt.
#[tokio::test(flavor = "multi_thread")]
async fn a_private_open_run_never_reaches_a_native_pg_receipt() {
    let (db, repo, mut context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    boss_jobs::agents::AgentsRegistry::publish(
        &boss_jobs::agents::PgAgents::new(db.pool.clone()),
        &[boss_jobs::agents::AgentInput {
            factory_rotation: Some(true),
            id: "agent-pg".into(),
            display_name: "PG fixture".into(),
            default_model: "opus-5[1m]".into(),
            model_class: None,
            role: Some("worker".into()),
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(3),
            stations: None,
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
    boss_policy_client::PolicyRepository::upsert_rule(
        &policy,
        &boss_policy_client::PolicyRule {
            id: boss_policy_client::types::rule_id("worker", &read.resource(), read.action()),
            role: "worker".into(),
            action: read.action(),
            resource: read.resource(),
            scope: boss_policy_client::Scope::Self_,
            active: true,
        },
        "counter-scope-regression",
    )
    .await
    .unwrap();
    context.scope = JobScope::OwnerIs(ACTOR.into());
    assert!(!context.scope.admits(&private));
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let result = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await;
    let SelectionResult::Claimed { receipt, .. } =
        result.expect("no run is counted, so no out-of-scope row can refuse the claim")
    else {
        panic!("fresh claim")
    };
    assert!(
        receipt.pointer("/owner_consumption/concurrency").is_none(),
        "no concurrency reading is copied: {}",
        receipt.json()
    );
    assert!(
        !receipt.json().contains(&private.id.to_string()),
        "the private run's identity reached the receipt"
    );
}

/// The ingress a test that judges an ANSWER runs under: the one the
/// credential door admitted, having already lost [`MEASURED_STALL`], under
/// [`FIXTURE_BUDGET`]. Whatever the gate's drive does, such a test answers
/// for the selection, never the drive (backlog d7e5b016). A test whose
/// subject is the deadline takes [`admitted_ingress`] and production's.
async fn ingress() -> boss_jobs::station_claim::AuthenticatedSelectionIngress {
    as_a_fixture(admitted_ingress().await)
}

/// The io.full stall gate-run 7563df44 recorded in its test phase (334 s
/// of 1572 s, node w-1) when this file's HTTP retry test answered 409
/// "selection ingress deadline expired during preparation": a stalled
/// drive, not a selection. An ingress that reports it has already spent
/// this much states the stall instead of waiting for one (backlog d7e5b016).
const MEASURED_STALL: std::time::Duration = std::time::Duration::from_secs(334);
/// An answer fixture's pre-commit budget: three times the measured stall.
/// It bounds a hung fixture and claims nothing about production; the
/// production budget's refusal of a request that outlasted it is pinned by
/// an_http_selection_that_outlasts_the_production_budget_is_refused here
/// and by native_selection_store's
/// the_production_budget_refuses_a_preparation_that_lost_the_measured_stall.
const FIXTURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(1002);

fn as_a_fixture(
    admitted: boss_jobs::station_claim::AuthenticatedSelectionIngress,
) -> boss_jobs::station_claim::AuthenticatedSelectionIngress {
    admitted
        .with_time_already_spent(MEASURED_STALL)
        .with_budget(FIXTURE_BUDGET)
}

/// The same admitted request with production's budget, having lost the
/// measured stall: what the gate's drive made of the HTTP retry test.
fn after_the_measured_stall(
    admitted: boss_jobs::station_claim::AuthenticatedSelectionIngress,
) -> boss_jobs::station_claim::AuthenticatedSelectionIngress {
    admitted.with_time_already_spent(MEASURED_STALL)
}

/// The ingress the credential door admits, under the production
/// `SELECTION_PRE_COMMIT_BUDGET` and with no time stated as spent.
async fn admitted_ingress() -> boss_jobs::station_claim::AuthenticatedSelectionIngress {
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

// Count real PostgreSQL SELECT statements, independently of row count. The
// initplan runs once per statement, so a loop of per-job reads cannot pass
// merely because all rows happened to fit in the adapter's cache (f999363f).
async fn count_population_reads(db: &boss_testing::TestDb) {
    sqlx::raw_sql("CREATE SEQUENCE selection_step_reads; ALTER TABLE steps RENAME TO selection_steps; CREATE VIEW steps AS SELECT selection_steps.*, xmin FROM selection_steps WHERE (SELECT nextval('selection_step_reads')) > 0")
        .execute(&db.pool).await.unwrap();
}

async fn seed_population(repo: &PgJobs, context: &SelectionContext, count: usize) {
    for index in 0..count {
        let mut job = context.validated_job.clone();
        job.id = boss_core::job::JobId::new();
        job.title = format!("Population {index}");
        // Keep the original head older. All rows, even packets with no
        // currently claimable step, must remain in the observed population.
        let now = chrono::Utc::now();
        job.opened_at = Some(now);
        repo.create_job_at(&job, now, &[]).await.unwrap();
        let mut step = Step::new(job.id, "task", "Not ready", 0);
        step.spec_slug = Some("work".into());
        repo.add_step_at(&step, now, &[]).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_selection_reads_a_complete_population_without_per_packet_queries() {
    let (db, repo, context, _) = fixture().await;
    seed_population(&repo, &context, 1025).await;
    count_population_reads(&db).await;
    let app = http_app(&db, repo, as_a_fixture);
    let started = std::time::Instant::now();
    let (status, bytes) = claim_next(&app, context.request_id).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let statements: i64 = sqlx::query_scalar("SELECT last_value FROM selection_step_reads")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    eprintln!(
        "station population: 1026 packets, {statements} step SELECTs, {:?}",
        started.elapsed()
    );
    assert_eq!(
        body["owner_consumption"]["population_read"]["open_packets"], 1026,
        "population must not stop at one page"
    );
    assert_eq!(
        body["owner_consumption"]["competing_population"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "only the packet that competed is copied (f999363f)"
    );
    assert!(
        statements < 16,
        "HTTP preparation and native revalidation must batch steps: {statements}"
    );
    let (replayed_status, replayed) = claim_next(&app, context.request_id).await;
    assert_eq!(replayed_status, axum::http::StatusCode::CREATED);
    assert_eq!(
        replayed, bytes,
        "the original receipt must replay byte-for-byte"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn native_selection_revalidates_a_population_without_per_packet_step_queries() {
    let (db, repo, mut context, _) = fixture().await;
    seed_population(&repo, &context, 64).await;
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    count_population_reads(&db).await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let started = std::time::Instant::now();
    let SelectionResult::Claimed { receipt, step } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("the unchanged oldest head must still be selected");
    };
    assert_eq!(step.id, context.validated_step.id);
    assert_eq!(
        receipt.pointer("/owner_consumption/population_read/open_packets"),
        Some(&serde_json::json!(65)),
        "batching must preserve the complete population the head was judged over"
    );
    let statements: i64 = sqlx::query_scalar("SELECT last_value FROM selection_step_reads")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    eprintln!(
        "native population: 65 packets, {statements} step SELECTs, {:?}",
        started.elapsed()
    );
    // The claim's own selected-row/CAS reads remain. Only the population
    // loop goes; the bound must not grow with the other 64 packets.
    assert!(
        statements < 16,
        "native revalidation still reads each packet separately: {statements}"
    );
}

/// `count` open packets in production's measured shape (2026-11-18: 1,492
/// open, a mean 18.4 KB each with its steps), none with a ready step, so
/// none competes for the fixture's head. Written in one statement: the
/// repository door would spend most of the test seeding them.
async fn seed_production_shaped_bystanders(
    db: &boss_testing::TestDb,
    context: &SelectionContext,
    count: i32,
) {
    sqlx::query(
        "WITH bystanders AS (
            INSERT INTO jobs SELECT (jsonb_populate_record(NULL::jobs, to_jsonb(j) || jsonb_build_object(
                'id', gen_random_uuid(), 'title', 'Bystander ' || g, 'subject_id', 'bystander-' || g,
                'metadata', jsonb_build_object('description', repeat('measured packet prose ', 300))))).*
            FROM jobs j, generate_series(1, $2) g WHERE j.id = $1 RETURNING id)
         INSERT INTO steps SELECT (jsonb_populate_record(NULL::steps, to_jsonb(s) || jsonb_build_object(
                'id', gen_random_uuid(), 'job_id', b.id, 'status', 'pending', 'sort_order', k,
                'spec_slug', 'elsewhere',
                'metadata', jsonb_build_object('procedure', repeat('repeated procedure ', 80))))).*
         FROM steps s, bystanders b, generate_series(0, 7) k WHERE s.id = $3",
    )
    .bind(*context.validated_job.id.inner().as_uuid())
    .bind(count)
    .bind(*context.validated_step.id.inner().as_uuid())
    .execute(&db.pool)
    .await
    .unwrap();
}

// Measured 2026-11-18 at production's shape and budget (1,501 open
// packets): the native transaction read the population in ~0.1 s, then
// spent the rest of the five seconds serializing and hashing a copy of
// every packet into the receipt and refused "during evidence
// serialization" (f999363f). Packets that did not compete for the head
// are counted, not copied, so the receipt is the size of the competition.
#[tokio::test(flavor = "multi_thread")]
async fn a_native_receipt_does_not_grow_with_packets_that_did_not_compete() {
    let (db, repo, mut context, _) = fixture().await;
    seed_production_shaped_bystanders(&db, &context, 300).await;
    context.native_authority = Some(prepared(&repo, context.request_id).await);
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { receipt, step } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("the unchanged head must still be selected");
    };
    assert_eq!(step.id, context.validated_step.id);
    let bytes = receipt.json().len();
    assert!(
        bytes < 256 * 1024,
        "the receipt copied packets that never competed for the head: {bytes} bytes for one competitor"
    );
    let competing = receipt
        .pointer("/owner_consumption/competing_population")
        .and_then(serde_json::Value::as_array)
        .expect("the packets that competed must be copied whole, not reduced to a digest");
    assert_eq!(competing.len(), 1);
    assert_eq!(
        competing[0][0]["id"],
        serde_json::json!(context.validated_job.id)
    );
    assert_eq!(
        receipt.pointer("/owner_consumption/population_read/open_packets"),
        Some(&serde_json::json!(301)),
        "the receipt must still name how many open packets the head was judged over"
    );
}

async fn fixture() -> (
    boss_testing::TestDb,
    PgJobs,
    SelectionContext,
    NativeSelectionStore,
) {
    let (db, repo, context, native, _) = fixture_admitted(as_a_fixture).await;
    (db, repo, context, native)
}

/// The fixture whose prepared request carries production's budget: for the
/// tests whose subject is the ingress deadline itself.
async fn fixture_with_ingress_time() -> (
    boss_testing::TestDb,
    PgJobs,
    SelectionContext,
    NativeSelectionStore,
    std::time::Instant,
) {
    fixture_admitted(|admitted| admitted).await
}

/// What a test makes of the ingress the credential door admitted.
type Admit = fn(
    boss_jobs::station_claim::AuthenticatedSelectionIngress,
) -> boss_jobs::station_claim::AuthenticatedSelectionIngress;

async fn fixture_admitted(
    admit: Admit,
) -> (
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
            access_tier: None,
            station: None,
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
        StationKind::Group,
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
    let ingress = admit(admitted_ingress().await);
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
        tree_selection: None,
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
        .pointer("/owner_consumption/competing_population")
        .and_then(serde_json::Value::as_array)
        .expect("the competing population must be reconstructible, not a digest alone");
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
    // A competitor is copied whole, so its own prose spans several chunks
    // (an unrelated packet is counted, not copied, since f999363f).
    context.validated_job.metadata["complete_observation"] =
        serde_json::json!("evidence".repeat(60000));
    repo.update_job(&context.validated_job).await.unwrap();
    context.validated_job = repo
        .get_job(&context.validated_job.id)
        .await
        .unwrap()
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
    // Reconstruction of the multi-chunk receipt must use its COPY.
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        repo.station_claim_receipt(context.request_id)
            .await
            .unwrap()
            .unwrap()
            .json(),
        receipt.json()
    );
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
    // A COMPETITOR the narrowed reader cannot read: since f999363f only the
    // packets that competed for the head are copied, so this one must
    // compete — a ready step at the same station, ranked behind the head.
    let mut other = Job::new(
        "claim-fixture",
        Subject::new("custom", "private-other"),
        "Other actor's private input",
        "emp-other",
        Priority::Scheduled,
        chrono::Utc::now().date_naive(),
    );
    other.status = boss_core::job::JobStatus::Open;
    other.metadata["private_observation"] = serde_json::json!("other actor's immutable content");
    repo.create_job_at(&other, chrono::Utc::now(), &[])
        .await
        .unwrap();
    let mut rival = Step::new(other.id, "task", "Work", 0);
    rival.spec_slug = Some("work".into());
    rival.status = StepStatus::Ready;
    repo.add_step_at(&rival, chrono::Utc::now(), &[])
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

#[tokio::test]
async fn selection_commits_the_owned_step_and_replays_the_same_server_receipt() {
    struct ResumeClock;
    impl Drop for ResumeClock {
        fn drop(&mut self) {
            tokio::time::resume();
        }
    }
    tokio::time::pause();
    let _clock = ResumeClock;
    // Real PostgreSQL I/O must not make Tokio auto-advance to the request's
    // timer. This companion is a scoped future, not a task that can escape.
    tokio::select! {
        () = selection_receipt_answer_under_controlled_clock() => {},
        () = async { loop { tokio::task::yield_now().await; } } => unreachable!(),
    }
}

async fn selection_receipt_answer_under_controlled_clock() {
    let (db, repo, context, store, ingress_started) = fixture_with_ingress_time().await;
    // Ingress stores std::Instant; enforcement converts that origin to Tokio's
    // clock. Align once after setup, then let this answer test own elapsed time.
    let logical_ingress = tokio::time::Instant::from_std(ingress_started);
    tokio::time::advance(logical_ingress.saturating_duration_since(tokio::time::Instant::now()))
        .await;
    assert_eq!(tokio::time::Instant::now(), logical_ingress);
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
    // A controlled host gap reproduces the old ingress being spent by later
    // receipt assertions (09145add); this is not a performance upper bound.
    std::thread::sleep(
        boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET
            + std::time::Duration::from_millis(25),
    );
    assert!(ingress_started.elapsed() >= boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET);
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
    assert_eq!(tokio::time::Instant::now(), logical_ingress);
    tokio::time::advance(
        boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET + std::time::Duration::from_millis(2),
    )
    .await;
    assert!(matches!(
        restarted
            .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
            .await,
        Err(boss_jobs::port::JobsError::SelectionConflict(message))
            if message == "selection ingress deadline expired before commit dispatch"
    ));
    // A new authenticated request may replay the same idempotency UUID. The
    // expired request above must never receive this new request's lifetime.
    let fresh_context = SelectionContext {
        native_authority: Some(prepared(&restarted, context.request_id).await),
        ..context.clone()
    };
    let SelectionResult::Replayed(fresh_copy) = restarted
        .claim_station_head_at(&fresh_context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("fresh ingress must copy the original receipt")
    };
    assert_eq!(fresh_context.request_id, context.request_id);
    assert_eq!(fresh_copy.json().as_bytes(), receipt.json().as_bytes());
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

/// The jobs router behind the credential door and the machine gate, with
/// `admit` applied, inside the door, to the ingress it resolved: the HTTP
/// tests judge the same door under the budget and stall they name.
fn http_app(db: &boss_testing::TestDb, repo: PgJobs, admit: Admit) -> axum::Router {
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
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
    let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| async move {
            if let Some(ingress) = request
                .extensions_mut()
                .remove::<boss_jobs::station_claim::AuthenticatedSelectionIngress>(
            ) {
                request.extensions_mut().insert(admit(ingress));
            }
            next.run(request).await
        },
    ));
    let app = boss_jobs::runner_credential::mount(app, keys);
    boss_core::machine_gate::gated(
        app,
        Arc::new(boss_core::machine_gate::MachineGate::new(
            "jobs",
            &["/health"],
            boss_core::machine_gate::Reading::new(
                boss_core::machine_gate::Mode::Off,
                Default::default(),
            ),
        )),
    )
}

/// One `claim-next` for `request_id`, as the fixture's credentialed worker.
async fn claim_next(
    app: &axum::Router,
    request_id: uuid::Uuid,
) -> (axum::http::StatusCode, axum::body::Bytes) {
    use http_body_util::BodyExt;
    let user = serde_json::to_string(&boss_policy_client::types::User {
        id: "emp-worker".into(),
        role: "ceo".into(),
        access_tier: boss_policy_client::types::AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    })
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::post("/api/stations/claim-fixture/claim-next")
                .header("content-type", "application/json")
                .header("x-boss-user", &user)
                .header(
                    boss_jobs::runner_credential::HEADER,
                    "station-pg-fixture-key",
                )
                .body(axum::body::Body::from(
                    serde_json::json!({"request_id": request_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        response.into_body().collect().await.unwrap().to_bytes(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn http_retries_copy_the_exact_postgres_receipt_bytes() {
    use axum::http::StatusCode;
    let (db, repo, context, _) = fixture().await;
    // Each request reaches the handler having lost the stall gate-run
    // 7563df44 measured, under the answer fixture's budget: the bytes it
    // copies are the selection's, whatever the drive does (d7e5b016).
    let app = http_app(&db, repo, as_a_fixture);
    let mut copies = Vec::new();
    for _ in 0..2 {
        let (status, body) = claim_next(&app, context.request_id).await;
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

/// The production budget still refuses the request the retry test above
/// now states: the same door, the same stall, production's five seconds.
/// Stated at ingress, the stall is refused by the first deadline reader
/// (the native read), before any preparation read; the gate's "during
/// preparation" spelling is the same deadline landing mid-preparation,
/// pinned in native_selection_store (backlog d7e5b016).
#[tokio::test(flavor = "multi_thread")]
async fn an_http_selection_that_outlasts_the_production_budget_is_refused() {
    let (db, repo, context, _) = fixture().await;
    let before = selection_effects(&db.pool).await;
    let app = http_app(&db, repo, after_the_measured_stall);
    let (status, body) = claim_next(&app, context.request_id).await;
    assert_eq!(
        (status, String::from_utf8_lossy(&body).into_owned()),
        (
            axum::http::StatusCode::CONFLICT,
            "station selection refused: selection ingress deadline expired before commit dispatch"
                .to_string()
        )
    );
    assert_eq!(
        selection_effects(&db.pool).await,
        before,
        "a refused selection claims nothing, writes no receipt and emits nothing"
    );
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
async fn claims_history_reads_the_actual_station_producer_group_and_replays_it() {
    use boss_jobs::claims_console::{ClaimsHistory, PgClaimsHistory};
    let (db, repo, context, _) = fixture().await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("ops-runner"),
    );
    let SelectionResult::Claimed { step, .. } = repo
        .claim_station_head_at(&context, ACTOR, &[], &stamp, &[])
        .await
        .unwrap()
    else {
        panic!("fresh native request must acquire its step")
    };
    let bus: std::sync::Arc<dyn boss_core::port::EventBus> = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    // A fold fixture, not production's 2 s: this judges what the history
    // folds, and a loaded gate's drive is not that (backlog ebbf38c6).
    let history = PgClaimsHistory::for_fold_fixture(db.pool.clone());
    while !boss_jobs::claims_console::source::advance_native_once(&history, &db.pool, 1024)
        .await
        .unwrap()
    {}
    let before = history.read(None, 24, chrono::Utc::now()).await.unwrap();
    let wire = serde_json::to_value(&before).unwrap();
    let claim = wire["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["step_id"] == step.id.to_string())
        .unwrap();
    assert_eq!(claim["station"], "claim-fixture");
    assert_eq!(claim["station_unavailable"], serde_json::Value::Null);
    boss_jobs::claims_console::source::rebuild_native_with(&history, &db.pool)
        .await
        .unwrap();
    assert_eq!(
        history
            .read(None, 24, chrono::Utc::now())
            .await
            .unwrap()
            .claims,
        before.claims
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
    if bounded_selection_refusal(&error) == Some("ingress-budget") {
        assert!(
            ingress_started.elapsed() >= boss_jobs::station_claim::SELECTION_PRE_COMMIT_BUDGET,
            "an ingress-budget refusal must spend the original authenticated budget"
        );
    }
    // Setup and lock acquisition already consume the original ingress budget.
    // An expired ingress correctly refuses immediately; a scheduled task may
    // also resume late after either timeout. Neither implies a wall-clock
    // lower or upper bound on this call. The exact timeout class, unchanged
    // rows/outbox/receipt, and fresh request below are the safety evidence.
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
