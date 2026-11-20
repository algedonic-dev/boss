use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, Priority, Step, StepStatus, Subject};
use boss_core::partition::Partition;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::pools::{PgPools, PoolRegistry, PoolSpec};
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowSpec};
use boss_jobs::{JobsRepository, PgJobs};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{NaiveDate, Utc};
use serde_json::json;

struct Fixture<J> {
    jobs: Arc<J>,
    pools: Arc<dyn PoolRegistry>,
    workflows: Arc<dyn WorkflowRegistry>,
    /// The PostgreSQL adapter's database, so a case can carry its
    /// committed outbox into `audit_log` before reading the log through
    /// the port; `None` for the in-memory adapter, whose log is direct.
    log: Option<sqlx::PgPool>,
}

/// The committed log as the port reads it: the PostgreSQL adapter's
/// outbox drained into `audit_log` first, as the relay would.
async fn committed_log<J>(f: &Fixture<J>) {
    if let Some(pool) = &f.log {
        let bus: Arc<dyn EventBus> = RecordingEventBus::new();
        boss_events::outbox::drain_outbox_once(pool, &bus, 500)
            .await
            .unwrap();
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => {
            let pools=Arc::new(boss_jobs::pools::InMemoryPools::default());
            let workflows=Arc::new(boss_jobs::registry::InMemoryWorkflows::for_fixture());
            let jobs=Arc::new(boss_jobs::InMemoryJobs::new().with_workflows(workflows.clone()).with_resource_pools(pools.clone()).with_agent_classes(CLAIMANTS.iter().filter_map(|(id,class)|class.map(|c|(id.to_string(),c.to_string())))).with_station_rows(run_stations()));
            (Fixture{jobs,pools,workflows,log:None},())
        },
        postgres => {
            let db=TestDb::new().await;
            for (id,class) in CLAIMANTS {
                sqlx::query("INSERT INTO agents (id, display_name, default_model, model_class) VALUES ($1, $1, 'opus-5', $2) ON CONFLICT (id) DO UPDATE SET model_class = EXCLUDED.model_class").bind(id).bind(class).execute(&db.pool).await.unwrap();
            }
            for row in run_stations() {
                boss_jobs::stations::StationRegistry::publish_declared(&boss_jobs::stations::PgStations::new(db.pool.clone()), row, &ActorId::automation("flow-contract"), Utc::now()).await.unwrap();
            }
            (Fixture{jobs:Arc::new(PgJobs::for_fixture(db.pool.clone())),pools:Arc::new(PgPools::new(db.pool.clone())),workflows:Arc::new(PgWorkflows::for_fixture(db.pool.clone())),log:Some(db.pool.clone())},db)
        },
    }
    cases {
        acquisition_freezes_its_claim_clock_and_resolved_lease,
        stock_completion_keeps_the_physical_obligation_charged,
        identity_binding_is_once_and_a_swept_lease_still_lets_its_step_exit,
        expired_flow_is_released_by_a_bounded_sweep,
        resource_lease_precision_never_expires_a_future_instant,
        resource_lease_identity_http_is_a_current_holder_claim_door,
        resource_lease_binding_is_fenced_by_expiry_and_a_lapsed_exit_records_it,
        resource_lease_stock_return_requires_an_exact_once_witness,
        the_worker_return_door_uses_the_pinned_pool_and_credentialed_operation,
        resource_lease_stock_sweep_is_bounded_and_keeps_every_obligation,
        resource_lease_uses_the_pinned_wall_clock_duration_before_legacy_duration,
        unknown_capacity_keeps_a_committed_refusal,
        last_unit_has_one_winner_and_publication_cannot_erase_its_capacity,
        completion_releases_exactly_its_generation_and_an_old_copy_cannot_release_the_next,
        metadata_cannot_forge_or_rebind_an_acquisition,
        cancellation_releases_every_current_vector_atomically,
        closing_a_packet_cannot_strand_its_current_flow_vectors,
        cancelled_packets_cannot_reclaim_an_old_generation,
        pinned_bills_and_whole_vector_refusal_do_not_follow_the_newest_protocol,
        generic_activation_cannot_skip_the_pinned_resource_bill,
        an_exit_cannot_record_a_state_that_storage_did_not_write,
        an_http_exit_must_name_the_executors_original_acquisition,
        an_http_exit_whose_lease_lapsed_lands_by_its_original_claim_id,
        representable_held_totals_do_not_depend_on_step_identity_order,
        first_record_cannot_introduce_a_server_acquisition_token,
        the_agent_workspace_example_is_one_atomic_four_dimension_flow_fixture,
        the_lease_sweep_door_expires_a_lapsed_lease_and_spares_a_fresher_generation,
        the_lease_sweep_door_turns_a_lapsed_stock_lease_into_an_obligation,
        the_lease_sweep_door_refuses_an_ungranted_caller_and_writes_nothing,
        the_lease_sweep_door_holds_its_cap,
        the_lease_sweep_door_keeps_each_packets_partition,
        a_run_bill_takes_its_model_slot_from_the_runs_own_model,
        a_run_that_declares_no_model_is_refused_by_name_and_reserves_nothing,
        a_robot_pool_whose_class_no_bill_carries_never_publishes,
        a_nomination_and_a_backlog_build_claim_hold_no_model_slot,
        a_run_bill_takes_the_claimants_class_before_the_runs_model,
        a_run_draws_its_vcpu_from_its_owners_floating_host_share,
        a_run_handed_to_its_gate_frees_its_model_slot_for_the_next_dispatch,
        a_run_whose_profile_binds_no_worker_draws_no_host_vcpu,
        a_claim_the_door_writes_is_read_by_the_reader_before_version_three,
    }
}

// ---------------------------------------------------------------------------
// THE SWEEP DOOR (backlog 3aeaf08c). Car 2b landed the generation-fenced
// sweep with no production caller, so no lapsed lease was ever released
// and no stock lease ever became an obligation. These drive the door the
// dispatcher's clock rule calls, through the real router and the real
// policy check, on both adapters. The door reads the server's own clock,
// so a lapsed lease is one CLAIMED three hours ago (a one-hour step, k=2).
// ---------------------------------------------------------------------------

const SWEEP_DOOR: &str = "/api/jobs/resource-leases/sweep";
const SWEEP_RULE: &str = "rule:resource-leases-are-swept-every-five-minutes";

/// The door's grant as production holds it: platform-admin reads and
/// updates every step at scope `all` (core defaults), which is the role
/// the dispatcher's rule actor presents.
fn sweep_policy() -> FakePolicyClient {
    FakePolicyClient::builder()
        .allow(
            "platform-admin",
            Action::Update,
            Resource::step(),
            Scope::All,
        )
        .allow("platform-admin", Action::Read, Resource::step(), Scope::All)
        // An auditor reads every step and updates none.
        .allow("audit-readonly", Action::Read, Resource::step(), Scope::All)
        // A team lead updates the team's steps and reads every one: the
        // sweep acts on every packet, so a narrower write is refused.
        .allow("team-lead", Action::Read, Resource::step(), Scope::All)
        .allow("team-lead", Action::Update, Resource::step(), Scope::Team)
        .build()
}

fn sweep_app<J: JobsRepository + 'static>(f: &Fixture<J>) -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    router(JobsApiState::minimal(
        f.jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        Arc::new(sweep_policy()),
        Arc::new(boss_clock_client::WallClockClient),
    ))
}

async fn sweep_as(app: &axum::Router, query: &str) -> serde_json::Value {
    let response = TestRequest::post(format!("{SWEEP_DOOR}{query}"))
        .as_user(SWEEP_RULE, "platform-admin")
        .send(app)
        .await;
    response.assert_status(StatusCode::OK);
    response.assert_json()
}

/// A stamp three hours behind the server's clock, so a one-hour step's
/// lease (two hours) has already lapsed when the door reads `now`.
fn three_hours_ago(stamp: &boss_core::publisher::EventStamp) -> boss_core::publisher::EventStamp {
    let mut past = stamp.clone();
    past.timestamp = Utc::now() - chrono::Duration::hours(3);
    past
}

async fn the_lease_sweep_door_expires_a_lapsed_lease_and_spares_a_fresher_generation<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let seeded = seed_fixture(f, json!({"state":"configured","amount":"2"})).await;
    let past = three_hours_ago(&seeded);
    let mut now = seeded.clone();
    now.timestamp = Utc::now();
    // A: claimed three hours ago and never heard from again.
    let lapsed = packet(f, &past).await;
    let lapsed_claim = f
        .jobs
        .claim_step_displacing_at(&lapsed.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    // B: claimed three hours ago too, released, and claimed again NOW —
    // the same step under a fresher generation whose lease runs two more
    // hours. The old generation lapsed; the step is not free.
    let fresh = packet(f, &past).await;
    let mut old = f
        .jobs
        .claim_step_displacing_at(&fresh.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    old.status = StepStatus::Ready;
    old.assignee_id = None;
    f.jobs
        .update_step_at(
            &old,
            past.timestamp,
            &[past.event(boss_jobs::events::STEP_UPDATED, json!(old))],
        )
        .await
        .unwrap();
    let current = f
        .jobs
        .claim_step_displacing_at(&fresh.id, "emp-two", &[], &now, &[])
        .await
        .unwrap();
    assert!(
        current.metadata["resource_claim"]["generation"].as_i64()
            > old.metadata["resource_claim"]["generation"].as_i64(),
        "the re-claim is a fresher generation"
    );
    let app = sweep_app(f);
    let report = sweep_as(&app, "").await;
    assert_eq!(report["expired"], 1, "exactly the lapsed lease: {report}");
    assert_eq!(report["limit"], boss_jobs::resource_leases::MAX_SWEEP);
    assert_eq!(report["saturated"], false);
    assert_eq!(
        sweep_as(&app, "").await["expired"],
        0,
        "a second firing releases nothing twice"
    );
    // A's capacity came back; B's fresher generation still holds its unit.
    let next = packet(f, &now).await;
    f.jobs
        .claim_step_displacing_at(&next.id, "emp-three", &[], &now, &[])
        .await
        .expect("the lapsed lease's FLOW capacity is released");
    let full = packet(f, &now).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&full.id, "emp-four", &[], &now, &[])
            .await
            .is_err(),
        "the fresher generation was never swept: it still holds its unit"
    );
    let mut completed = current.clone();
    completed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &completed,
            now.timestamp,
            &[now.event(boss_jobs::events::STEP_UPDATED, json!(completed))],
        )
        .await
        .expect("the fresher generation's holder can still complete");
    let mut late = lapsed_claim;
    late.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &late,
            now.timestamp,
            &[now.event(boss_jobs::events::STEP_UPDATED, json!(late))],
        )
        .await
        .expect("the swept acquisition's holder can still end its step (backlog ad1c3af2)");
}

async fn the_lease_sweep_door_turns_a_lapsed_stock_lease_into_an_obligation<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let seeded = seed_stock(f, "1").await;
    let past = three_hours_ago(&seeded);
    let step = packet(f, &past).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    assert!(f.jobs.stock_obligations(claim.id).await.unwrap().is_empty());
    let app = sweep_app(f);
    assert_eq!(sweep_as(&app, "").await["expired"], 1);
    let obligations = f.jobs.stock_obligations(claim.id).await.unwrap();
    assert_eq!(obligations.len(), 1, "the stock lease became an obligation");
    assert_eq!(obligations[0].reclaimer, "automation:workspace-reclaimer");
    assert_eq!(obligations[0].allocation.pool, "vcpu");
    assert!(
        obligations[0].returned_by.is_none(),
        "logical expiry is not a physical return"
    );
    let mut now = seeded.clone();
    now.timestamp = Utc::now();
    let next = packet(f, &now).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &now, &[])
            .await
            .is_err(),
        "the obligation keeps the stock charged until its reclaimer returns it"
    );
}

async fn the_lease_sweep_door_refuses_an_ungranted_caller_and_writes_nothing<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let seeded = seed_stock(f, "1").await;
    let past = three_hours_ago(&seeded);
    let step = packet(f, &past).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let app = sweep_app(f);
    // Nobody at all: the anonymous guest.
    TestRequest::post(SWEEP_DOOR)
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // Reads every step, writes none.
    TestRequest::post(SWEEP_DOOR)
        .as_user("emp-auditor", "audit-readonly")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // Writes only the team's steps: the sweep cannot be narrowed to them.
    TestRequest::post(SWEEP_DOOR)
        .as_user("emp-lead", "team-lead")
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    assert!(
        f.jobs.stock_obligations(claim.id).await.unwrap().is_empty(),
        "a refused sweep wrote no obligation"
    );
    assert_eq!(
        sweep_as(&app, "").await["expired"],
        1,
        "the lease was still active after every refusal: nothing was written"
    );
}

/// One firing sweeps every packet, and each packet's partition was fixed
/// at its admission: the expiry fact is the PACKET's, so it carries the
/// packet's partition, not the caller's. The rule fires as a real actor;
/// a simulated or shadow lease expiry recorded under that stamp would
/// read as real to every real consumer (Codex review e7a414ae, R1 P2).
async fn the_lease_sweep_door_keeps_each_packets_partition<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let seeded = seed_fixture(f, json!({"state":"configured","amount":"3"})).await;
    let past = three_hours_ago(&seeded);
    let mut packets = Vec::new();
    for partition in [Partition::Real, Partition::Simulated, Partition::Shadow] {
        let step = packet_in(f, &past, partition).await;
        f.jobs
            .claim_step_displacing_at(
                &step.id,
                "emp-one",
                &[],
                &past.clone().with_partition(partition),
                &[],
            )
            .await
            .unwrap();
        packets.push((step.job_id, partition));
    }
    let app = sweep_app(f);
    assert_eq!(sweep_as(&app, "").await["expired"], 3);
    committed_log(f).await;
    if let Some(pool) = &f.log {
        // The outbox row is what the bus publishes: it carries the same
        // marker the audit row does, never a second one.
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT payload->>'job_id', payload->>'_partition' FROM event_outbox WHERE kind=$1",
        )
        .bind(boss_jobs::resource_leases::LEASE_EXPIRED)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        for (job, partition) in &packets {
            assert!(
                rows.contains(&(job.to_string(), Some(partition.as_str().to_string()))),
                "the outbox row for the {partition} packet: {rows:?}"
            );
        }
    }
    for (job, partition) in packets {
        let expiries: Vec<_> = f
            .jobs
            .events_for_job(&job, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == boss_jobs::resource_leases::LEASE_EXPIRED)
            .collect();
        assert_eq!(expiries.len(), 1, "one expiry for the {partition} packet");
        let payload = &expiries[0].payload;
        assert_eq!(
            payload["_partition"],
            partition.as_str(),
            "the {partition} packet's expiry names its own partition: {payload}"
        );
        assert_eq!(Partition::from_event_payload(payload), partition);
        assert_eq!(
            payload["_actor"],
            format!("automation:{SWEEP_RULE}"),
            "the caller still signs every expiry"
        );
    }
}

async fn the_lease_sweep_door_holds_its_cap<J: JobsRepository + 'static>(f: &Fixture<J>, _: &str) {
    let cap = boss_jobs::resource_leases::MAX_SWEEP;
    let seeded = seed_fixture(
        f,
        json!({"state":"configured","amount":(cap + 1).to_string()}),
    )
    .await;
    let past = three_hours_ago(&seeded);
    for _ in 0..=cap {
        let step = packet(f, &past).await;
        f.jobs
            .claim_step_displacing_at(&step.id, "emp-one", &[], &past, &[])
            .await
            .unwrap();
    }
    let app = sweep_app(f);
    for query in [format!("?limit={}", cap + 1), "?limit=0".to_string()] {
        TestRequest::post(format!("{SWEEP_DOOR}{query}"))
            .as_user(SWEEP_RULE, "platform-admin")
            .send(&app)
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let one = sweep_as(&app, "?limit=1").await;
    assert_eq!(one["expired"], 1);
    assert_eq!(one["saturated"], true, "a full batch says more may remain");
    let batch = sweep_as(&app, "").await;
    assert_eq!(
        batch["expired"], cap,
        "one firing never passes the cap, and the refused bounds expired none"
    );
    assert_eq!(batch["saturated"], true);
    let rest = sweep_as(&app, "").await;
    assert_eq!(
        rest["expired"],
        0,
        "all {} lapsed leases accounted for",
        cap + 1
    );
    assert_eq!(rest["saturated"], false);
}

async fn seed_fixture<J: JobsRepository>(
    f: &Fixture<J>,
    capacity: serde_json::Value,
) -> boss_core::publisher::EventStamp {
    seed_fixture_with_bill(f, capacity, json!({"vcpu":"1"})).await
}
async fn seed_fixture_with_bill<J: JobsRepository>(
    f: &Fixture<J>,
    capacity: serde_json::Value,
    bill: serde_json::Value,
) -> boss_core::publisher::EventStamp {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    let spec:WorkflowSpec=serde_json::from_value(json!({"kind":"flow-contract","label":"Flow","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},{"title":"work","kind":"task","duration_hours":1,"ready_when":"steps.start.done","resources":bill},{"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}]})).unwrap();
    f.workflows
        .publish_authored(spec, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":capacity,"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    stamp
}
async fn packet<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
) -> Step {
    packet_version(f, stamp, 1).await
}
async fn packet_version<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
    version: i32,
) -> Step {
    packet_with_id(f, stamp, version, None).await
}
async fn packet_with_id<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
    version: i32,
    id: Option<boss_core::job::StepId>,
) -> Step {
    packet_job(f, stamp, version, id, Partition::Real).await
}
/// A packet admitted into `partition`: the row carries it and so do its
/// admission facts, the way the create door stamps them from the Job.
async fn packet_in<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
    partition: Partition,
) -> Step {
    let stamp = stamp.clone().with_partition(partition);
    packet_job(f, &stamp, 1, None, partition).await
}
async fn packet_job<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
    version: i32,
    id: Option<boss_core::job::StepId>,
    partition: Partition,
) -> Step {
    let mut job = Job::new(
        "flow-contract",
        Subject::new("custom", "flow-contract"),
        "Resource test",
        "emp-owner",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    job.workflow_version = version;
    job.partition = partition;
    f.jobs
        .create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
        )
        .await
        .unwrap();
    let mut step = Step::new(job.id, "task", "Work", 1);
    step.spec_slug = Some("work".into());
    step.status = StepStatus::Ready;
    if let Some(id) = id {
        step.id = id;
    }
    f.jobs
        .add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
        )
        .await
        .unwrap();
    step
}

async fn unknown_capacity_keeps_a_committed_refusal<J: JobsRepository>(f: &Fixture<J>, _: &str) {
    let stamp = seed_fixture(f, json!({"state":"unknown"})).await;
    let step = packet(f, &stamp).await;
    assert!(matches!(
        f.jobs
            .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
            .await,
        Err(boss_jobs::port::JobsError::ResourceRefused(_))
    ));
    let after = f.jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Ready);
    assert!(after.assignee_id.is_none());
    assert_eq!(after.metadata["waiting_on"]["reason"], "capacity-unknown");
}

async fn acquisition_freezes_its_claim_clock_and_resolved_lease<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let mut stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    stamp.timestamp = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let step = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let token = &claimed.metadata["resource_claim"];
    assert_eq!(
        token["claimed_at"],
        json!(stamp.timestamp),
        "the acquisition freezes the injected claim instant"
    );
    assert_eq!(token["resolved_duration_ms"], json!(3_600_000));
    assert_eq!(
        token["lease_until"],
        json!(stamp.timestamp + chrono::Duration::hours(2))
    );
    let first = token.clone();
    stamp.timestamp += chrono::Duration::minutes(30);
    let retry = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(
        retry.metadata["resource_claim"], first,
        "a retry does not renew the lease"
    );
}

async fn stock_completion_keeps_the_physical_obligation_charged<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let step = packet(f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .expect("a known stock can be reserved as part of the vector");
    let acquisition =
        serde_json::from_value(claimed.metadata["resource_claim"]["id"].clone()).unwrap();
    claimed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    let obligations = f.jobs.stock_obligations(acquisition).await.unwrap();
    assert_eq!(obligations.len(), 1);
    assert!(obligations[0].returned_by.is_none());
    let next = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &stamp, &[])
            .await
            .is_err(),
        "completion cannot free physical stock"
    );
}

async fn identity_binding_is_once_and_a_swept_lease_still_lets_its_step_exit<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let mut stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let binding = boss_jobs::resource_leases::IdentityBinding {
        acquisition: claim.id,
        generation: claim.generation,
        identity: boss_jobs::resource_leases::ResourceIdentity {
            namespace: "boss-dev".into(),
            job_uid: uuid::Uuid::new_v4(),
            pvc_uid: uuid::Uuid::new_v4(),
            run_id: uuid::Uuid::new_v4(),
        },
    };
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
        .await
        .expect("the current holder can bind once");
    assert!(
        f.jobs
            .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
            .await
            .is_err()
    );
    stamp.timestamp += chrono::Duration::hours(2);
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        1
    );
    // THE SWEPT STEP STILL EXITS (backlog ad1c3af2). The sweep freed the
    // slot and recorded the expiry; the step stayed Active under its
    // holder, and refusing its exit left it there for good — runs
    // 056cc3d1 and d659981f stood at `building` 19 h after their cars
    // landed, because no door could end the step. The exit is accepted
    // and releases nothing twice: the expiry was the release.
    let mut late = claimed;
    late.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &late,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(late))],
        )
        .await
        .expect("a swept acquisition's holder can still end its step");
    assert_eq!(
        f.jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Completed
    );
    assert!(
        released(f, &step.job_id).await.is_empty(),
        "the sweep's expiry was the release; the exit records no second one"
    );
    let next = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &stamp, &[])
            .await
            .is_ok(),
        "expiry releases FLOW capacity"
    );
}

async fn expired_flow_is_released_by_a_bounded_sweep<J: JobsRepository>(f: &Fixture<J>, _: &str) {
    let mut stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .expect("a bounded sweep is available")
            .expired,
        0
    );
    stamp.timestamp += chrono::Duration::hours(2);
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        1
    );
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        0,
        "replay does not release twice"
    );
}

async fn resource_lease_precision_never_expires_a_future_instant<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let mut stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    stamp.timestamp = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc);
    let step = packet(f, &stamp).await;
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    stamp.timestamp += chrono::Duration::hours(2) - chrono::Duration::nanoseconds(1);
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        0,
        "SQL timestamp precision must not expire a future canonical instant"
    );
    stamp.timestamp += chrono::Duration::nanoseconds(1);
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        1
    );
}

async fn resource_lease_identity_http_is_a_current_holder_claim_door<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-resource-test", &[], &stamp, &[])
        .await
        .unwrap();
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
            .build(),
    );
    let app = router(JobsApiState::minimal(
        f.jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let body = json!({"acquisition":claimed.metadata["resource_claim"]["id"],"generation":claimed.metadata["resource_claim"]["generation"],"identity":{"namespace":"boss-dev","job_uid":uuid::Uuid::new_v4(),"pvc_uid":uuid::Uuid::new_v4(),"run_id":uuid::Uuid::new_v4()}});
    let path = format!(
        "/api/jobs/{}/steps/{}/resource-identity",
        step.job_id, step.id
    );
    TestRequest::post(&path)
        .as_user("emp-other", "platform-admin")
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CONFLICT);
    TestRequest::post(&path)
        .as_user("emp-resource-test", "platform-admin")
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    TestRequest::post(&path)
        .as_user("emp-resource-test", "platform-admin")
        .json(&body)
        .send(&app)
        .await
        .assert_status(StatusCode::CONFLICT);
}

fn identity_binding(
    claim: &boss_jobs::flow_resources::Claim,
) -> boss_jobs::resource_leases::IdentityBinding {
    boss_jobs::resource_leases::IdentityBinding {
        acquisition: claim.id,
        generation: claim.generation,
        identity: boss_jobs::resource_leases::ResourceIdentity {
            namespace: "boss-dev".into(),
            job_uid: uuid::Uuid::new_v4(),
            pvc_uid: uuid::Uuid::new_v4(),
            run_id: uuid::Uuid::new_v4(),
        },
    }
}
async fn seed_stock<J: JobsRepository>(
    f: &Fixture<J>,
    capacity: &str,
) -> boss_core::publisher::EventStamp {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":capacity})).await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":capacity},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    stamp
}
async fn resource_lease_binding_is_fenced_by_expiry_and_a_lapsed_exit_records_it<
    J: JobsRepository,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let mut stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim = serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let binding = identity_binding(&claim);
    let mut stale = binding.clone();
    stale.generation += 1;
    assert!(
        f.jobs
            .bind_resource_identity_at(&step.id, "emp-one", &stale, &stamp)
            .await
            .is_err()
    );
    assert!(
        f.jobs
            .bind_resource_identity_at(&step.id, "emp-other", &binding, &stamp)
            .await
            .is_err()
    );
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
        .await
        .unwrap();
    stamp.timestamp += chrono::Duration::hours(2);
    assert!(
        f.jobs
            .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
            .await
            .is_err(),
        "an expired lease binds nothing, before the sweep as after it"
    );
    // A LAPSED, UNSWEPT EXIT RELEASES AND SAYS SO (backlog ad1c3af2). The
    // holder's exit after the deadline is accepted — refusing it left the
    // step Active with nothing able to end it — and its release names the
    // lapse, so the record does not read as an exit inside the lease.
    let mut completed = claimed.clone();
    completed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &completed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(completed))],
        )
        .await
        .expect("an exit after the deadline, before the sweep, is accepted");
    let releases = released(f, &step.job_id).await;
    assert_eq!(releases.len(), 1, "one release: {releases:?}");
    assert_eq!(
        releases[0].payload[boss_jobs::flow_resources::LEASE_EXPIRED_KEY],
        true,
        "the release records that the lease had lapsed: {}",
        releases[0].payload
    );
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        0,
        "the exit released it; the sweep has nothing left to expire"
    );
}

/// Every `jobs.step.released` fact the packet's log holds.
async fn released<J: JobsRepository>(f: &Fixture<J>, job: &JobId) -> Vec<boss_core::event::Event> {
    committed_log(f).await;
    f.jobs
        .events_for_job(job, 200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::flow_resources::RELEASED)
        .collect()
}

struct FixtureMachineReturn {
    tamper: bool,
}

async fn the_worker_return_door_uses_the_pinned_pool_and_credentialed_operation<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    f.pools
        .publish_declared(
            serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active",
        "unit":"workspace-slot","kind":"stock","reclaimer":"automation:reap-dead-ci-jobs",
        "program":"infra/forge/reap-dead-ci-jobs.sh","return_verifier":"worker-teardown",
        "capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp}))
            .unwrap(),
            stamp.actor(),
            stamp.timestamp,
        )
        .await
        .unwrap();
    let step = packet(f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &identity_binding(&claim), &stamp)
        .await
        .unwrap();
    claimed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    let obligation = f.jobs.stock_obligations(claim.id).await.unwrap().remove(0);
    // A later publication cannot rewrite the authority of this physical debt.
    let original_pool = f
        .jobs
        .resource_pool_version("vcpu", 2)
        .await
        .unwrap()
        .unwrap();
    let mut later_pool = original_pool.clone();
    later_pool.version = 3;
    later_pool.reclaimer = Some("automation:later-reclaimer".into());
    later_pool.return_verifier = None;
    assert!(
        f.pools
            .publish_declared(later_pool, stamp.actor(), stamp.timestamp)
            .await
            .is_err(),
        "publication cannot replace the authority of outstanding physical stock"
    );
    assert!(
        f.jobs
            .resource_pool_version("vcpu", 3)
            .await
            .unwrap()
            .is_none()
    );
    let pinned_pool = f
        .jobs
        .resource_pool_version("vcpu", 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pinned_pool.reclaimer, original_pool.reclaimer);
    assert_eq!(pinned_pool.return_verifier, original_pool.return_verifier);
    assert_eq!(
        f.jobs.stock_obligations(claim.id).await.unwrap()[0],
        obligation
    );
    let mut protocol = boss_jobs::seed_loader::load_workflows(
        boss_testing::repo_root().join("infra/platform/workflows/ops-request.toml"),
    )
    .unwrap()
    .remove(0);
    protocol.version = 1;
    let protocol = f
        .workflows
        .publish_authored(protocol, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    let spec = protocol
        .steps
        .iter()
        .find(|s| s.title == "execute")
        .unwrap();
    let mut operation = Job::new(
        "ops-request",
        Subject::new("custom", "forge"),
        "fixture physical witness",
        "emp-owner",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    operation.workflow_version = protocol.version;
    let mut execute = Step::new(operation.id, "task", "execute", 0);
    execute.spec_slug = Some("execute".into());
    execute.status = StepStatus::Completed;
    execute.completed_by = Some(ActorId::automation("ops-runner"));
    execute.fields = spec.fields.clone();
    let raw = json!({"schema":1,"claim":"worker-teardown-only","state":"absent","target":boss_jobs::worker_teardown::target(&obligation).unwrap(),
        "request":operation.id,"observed_at":stamp.timestamp,"errors":[],
        "controls":{"namespace":uuid::Uuid::new_v4(),"service_account":uuid::Uuid::new_v4()},
        "inventories":{"jobs":{"complete":true,"count":0,"pages":1,"resource_version":"7"},"pvcs":{"complete":true,"count":0,"pages":1,"resource_version":"8"}},
        "source":boss_jobs::worker_teardown::source_hashes(),"manifest_attachment_receipt":"isolated fixture evidence, not a cluster observation"}).to_string();
    let channel = |bytes: &[u8]| json!({"base64":STANDARD.encode(bytes),"bytes":bytes.len(),"retained_bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(bytes)),"complete":true});
    execute.metadata = json!({"disposition":"answered","exit_code":"0","runner_host":"forge","output":raw,
        "executed":{"verb":"worker-teardown-witness","host":"forge","argv":boss_jobs::worker_teardown::argv(&obligation).unwrap(),
        "verb_file_sha256":hex::encode(Sha256::digest(include_bytes!("../../../../infra/ops/verbs/worker-teardown-witness.json")))},
        "streams":{"stdout":channel(raw.as_bytes()),"stderr":channel(b"")}});
    assert!(matches!(
        f.jobs
            .create_job_with_steps_at(
                &operation,
                &[execute.clone()],
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::JOB_CREATED, json!(operation))],
                &[stamp.event(boss_jobs::events::STEP_CREATED, json!(execute))]
            )
            .await
            .unwrap(),
        boss_jobs::Admission::Admitted
    ));
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
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
    let app = router(JobsApiState {
        kind_registry: Some(f.workflows.clone()),
        ..JobsApiState::minimal(
            f.jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let path = format!("/api/claims/{}/return", claim.id);
    let before = f.jobs.stock_obligations(claim.id).await.unwrap();
    TestRequest::post(&path)
        .as_user("automation:other", "platform-admin")
        .json(&json!({"ops_request":operation.id}))
        .send(&app)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    TestRequest::post(&path)
        .as_user("automation:reap-dead-ci-jobs", "platform-admin")
        .json(&json!({"ops_request":operation.id,"verifier":"caller-chosen"}))
        .send(&app)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.jobs.stock_obligations(claim.id).await.unwrap(), before);
    for _ in 0..2 {
        TestRequest::post(&path)
            .as_user("automation:reap-dead-ci-jobs", "platform-admin")
            .json(&json!({"ops_request":operation.id}))
            .send(&app)
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    assert_eq!(
        f.jobs.stock_obligations(claim.id).await.unwrap()[0].returned_by,
        Some(*operation.id.inner().as_uuid())
    );
}
#[async_trait::async_trait]
impl boss_jobs::resource_leases::ResourceReturnVerifier for FixtureMachineReturn {
    async fn verify(
        &self,
        o: &boss_jobs::resource_leases::StockObligation,
        evidence: uuid::Uuid,
    ) -> Result<boss_jobs::resource_leases::VerifiedStockReturn, boss_jobs::port::JobsError> {
        // Test-only trusted-port implementation. This is not a real operation
        // or a production teardown adapter; 2c owns that qualification.
        let mut observed = o.clone();
        if self.tamper {
            observed.identity.as_mut().unwrap().pvc_uid = uuid::Uuid::new_v4();
        }
        Ok(
            boss_jobs::resource_leases::VerifiedStockReturn::from_qualified_operation(
                observed, evidence,
            ),
        )
    }
}
async fn resource_lease_stock_return_requires_an_exact_once_witness<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_stock(f, "1").await;
    let first = packet(f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&first.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim = serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let binding = identity_binding(&claim);
    f.jobs
        .bind_resource_identity_at(&first.id, "emp-one", &binding, &stamp)
        .await
        .unwrap();
    claimed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    let before = f.jobs.stock_obligations(claim.id).await.unwrap();
    let evidence = uuid::Uuid::new_v4();
    let unqualified = f
        .jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &boss_jobs::resource_leases::NoQualifiedReturnAdapter,
            &stamp,
        )
        .await
        .unwrap_err();
    assert!(
        unqualified
            .to_string()
            .contains("no qualified return adapter")
    );
    assert!(
        f.jobs
            .discharge_stock_at(
                claim.id,
                "vcpu",
                "emp-one",
                evidence,
                &FixtureMachineReturn { tamper: false },
                &stamp
            )
            .await
            .is_err()
    );
    assert!(
        f.jobs
            .discharge_stock_at(
                claim.id,
                "vcpu",
                "automation:workspace-reclaimer",
                evidence,
                &FixtureMachineReturn { tamper: true },
                &stamp
            )
            .await
            .is_err()
    );
    assert_eq!(f.jobs.stock_obligations(claim.id).await.unwrap(), before);
    let verifier = FixtureMachineReturn { tamper: false };
    let (one, two) = tokio::join!(
        f.jobs.discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &verifier,
            &stamp
        ),
        f.jobs.discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &verifier,
            &stamp
        )
    );
    one.unwrap();
    two.unwrap();
    assert_eq!(
        f.jobs.stock_obligations(claim.id).await.unwrap()[0].returned_by,
        Some(evidence)
    );
    let second = packet(f, &stamp).await;
    let mut next = f
        .jobs
        .claim_step_displacing_at(&second.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let new_claim = serde_json::from_value(next.metadata["resource_claim"].clone()).unwrap();
    f.jobs
        .bind_resource_identity_at(&second.id, "emp-one", &identity_binding(&new_claim), &stamp)
        .await
        .unwrap();
    next.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &next,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(next))],
        )
        .await
        .unwrap();
    assert!(
        f.jobs
            .discharge_stock_at(
                new_claim.id,
                "vcpu",
                "automation:workspace-reclaimer",
                evidence,
                &verifier,
                &stamp
            )
            .await
            .is_err(),
        "one witness cannot discharge another obligation"
    );
    assert!(
        f.jobs.stock_obligations(new_claim.id).await.unwrap()[0]
            .returned_by
            .is_none()
    );
}

async fn resource_lease_stock_sweep_is_bounded_and_keeps_every_obligation<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let mut stamp = seed_stock(f, "3").await;
    let mut claims = Vec::new();
    for _ in 0..3 {
        let step = packet(f, &stamp).await;
        let active = f
            .jobs
            .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
            .await
            .unwrap();
        claims.push(
            serde_json::from_value::<boss_jobs::flow_resources::Claim>(
                active.metadata["resource_claim"].clone(),
            )
            .unwrap(),
        );
    }
    stamp.timestamp += chrono::Duration::hours(2);
    assert!(f.jobs.sweep_resource_leases_at(&stamp, 0).await.is_err());
    assert!(f.jobs.sweep_resource_leases_at(&stamp, 65).await.is_err());
    for _ in 0..3 {
        assert_eq!(
            f.jobs
                .sweep_resource_leases_at(&stamp, 1)
                .await
                .unwrap()
                .expired,
            1
        );
    }
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        0
    );
    for claim in claims {
        let obligations = f.jobs.stock_obligations(claim.id).await.unwrap();
        assert_eq!(obligations.len(), 1);
        assert!(obligations[0].returned_by.is_none());
    }
    let next = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &stamp, &[])
            .await
            .is_err(),
        "logical expiry does not free physical stock"
    );
}

async fn resource_lease_uses_the_pinned_wall_clock_duration_before_legacy_duration<
    J: JobsRepository,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let mut protocol = f.workflows.get_active("flow-contract").await.unwrap();
    protocol.version = 2;
    protocol.steps[1].wall_clock_hours = Some(2.0);
    protocol.steps[1].duration_hours = Some(1.0);
    f.workflows
        .publish_authored(protocol, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    let step = packet_version(f, &stamp, 2).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(
        claimed.metadata["resource_claim"]["resolved_duration_ms"],
        json!(7_200_000),
        "lease duration follows the existing wall-clock, duration, kind precedence"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_lease_new_fact_writes_roll_back_all_projections_and_outbox() {
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let mut stamp = seed_stock(&f, "1").await;
    let step = packet(&f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim = serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let binding = identity_binding(&claim);
    sqlx::raw_sql("CREATE FUNCTION fail_lease_fact() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind=TG_ARGV[0] THEN RAISE EXCEPTION 'own lease fact refusal'; END IF; RETURN NEW; END $$;").execute(&db.pool).await.unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object('claims',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY step_id),'[]') FROM step_resource_claims c),'identities',(SELECT coalesce(jsonb_agg(to_jsonb(i) ORDER BY acquisition),'[]') FROM resource_identity_bindings i),'obligations',(SELECT coalesce(jsonb_agg(to_jsonb(o) ORDER BY acquisition,pool),'[]') FROM resource_stock_obligations o),'allocations',(SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY step_id,pool),'[]') FROM step_resource_allocations a),'outbox',(SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY id),'[]') FROM event_outbox e))",
        )
    };
    sqlx::raw_sql("CREATE TRIGGER fail_lease BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_lease_fact('jobs.step.resource_identity_bound');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    assert!(
        f.jobs
            .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
            .await
            .unwrap_err()
            .to_string()
            .contains("own lease fact refusal")
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    sqlx::query("DROP TRIGGER fail_lease ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
        .await
        .unwrap();
    stamp.timestamp += chrono::Duration::hours(2);
    sqlx::raw_sql("CREATE TRIGGER fail_lease BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_lease_fact('jobs.step.resource_lease_expired');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    assert!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("own lease fact refusal")
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    sqlx::query("DROP TRIGGER fail_lease ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    f.jobs.sweep_resource_leases_at(&stamp, 1).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER fail_lease BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_lease_fact('jobs.step.stock_returned');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let evidence = uuid::Uuid::new_v4();
    let verifier = FixtureMachineReturn { tamper: false };
    assert!(
        f.jobs
            .discharge_stock_at(
                claim.id,
                "vcpu",
                "automation:workspace-reclaimer",
                evidence,
                &verifier,
                &stamp
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("own lease fact refusal")
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    sqlx::query("DROP TRIGGER fail_lease ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    f.jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &verifier,
            &stamp,
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_lease_return_replay_keeps_a_newer_acquisition_and_refuses_a_second_fact() {
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let stamp = seed_stock(&f, "2").await;
    let step = packet(&f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim = serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &identity_binding(&claim), &stamp)
        .await
        .unwrap();
    claimed.status = StepStatus::Ready;
    claimed.assignee_id = None;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-two", &[], &stamp, &[])
        .await
        .unwrap();
    let evidence = uuid::Uuid::new_v4();
    f.jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &FixtureMachineReturn { tamper: false },
            &stamp,
        )
        .await
        .unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object('claims',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY step_id),'[]') FROM step_resource_claims c),'identities',(SELECT coalesce(jsonb_agg(to_jsonb(i) ORDER BY acquisition),'[]') FROM resource_identity_bindings i),'obligations',(SELECT coalesce(jsonb_agg(to_jsonb(o) ORDER BY acquisition,pool),'[]') FROM resource_stock_obligations o),'allocations',(SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY step_id,pool),'[]') FROM step_resource_allocations a))",
        )
    };
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    let returned = f.jobs.stock_obligations(claim.id).await.unwrap().remove(0);
    let forged=stamp.event(boss_jobs::resource_leases::STOCK_RETURNED,json!({"job_id":claim.job_id,"step_id":claim.step_id,"obligation":returned,"evidence":evidence}));
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &forged)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    assert!(
        boss_jobs::rebuild_jobs_and_steps(&db.pool).await.is_err(),
        "a second immutable return must refuse before clearing serving projections"
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_lease_rebuild_preserves_binding_obligation_and_expiry() {
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let mut stamp = seed_fixture(&f, json!({"state":"configured","amount":"1"})).await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let step = packet(&f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let binding = boss_jobs::resource_leases::IdentityBinding {
        acquisition: claim.id,
        generation: claim.generation,
        identity: boss_jobs::resource_leases::ResourceIdentity {
            namespace: "boss-dev".into(),
            job_uid: uuid::Uuid::new_v4(),
            pvc_uid: uuid::Uuid::new_v4(),
            run_id: uuid::Uuid::new_v4(),
        },
    };
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
        .await
        .unwrap();
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind='jobs.step.resource_identity_bound'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        payload["job_id"],
        json!(step.job_id),
        "the binding is visible in its originating packet's claim log"
    );
    stamp.timestamp += chrono::Duration::hours(2);
    f.jobs.sweep_resource_leases_at(&stamp, 1).await.unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object('claims',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY step_id),'[]') FROM step_resource_claims c),'identities',(SELECT coalesce(jsonb_agg(to_jsonb(i) ORDER BY acquisition),'[]') FROM resource_identity_bindings i),'obligations',(SELECT coalesce(jsonb_agg(to_jsonb(o) ORDER BY acquisition,pool),'[]') FROM resource_stock_obligations o),'allocations',(SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY step_id,pool),'[]') FROM step_resource_allocations a))",
        )
    };
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    sqlx::raw_sql("DELETE FROM resource_stock_obligations; DELETE FROM resource_identity_bindings; DELETE FROM step_resource_allocations; DELETE FROM step_resource_claims;").execute(&db.pool).await.unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        snapshot().fetch_one(&db.pool).await.unwrap(),
        before,
        "all lease facts reconstruct exactly"
    );
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        snapshot().fetch_one(&db.pool).await.unwrap(),
        before,
        "repeat replay consumes no extra claim or return"
    );
}
// What these families judge is what the projection FOLDS, not how fast a
// shared gate drive answers. Train 260dbd83's gate d7b85890 (253 s of
// io.full in 637 s, two bays busy) spent the adapter's 2 s pre-commit
// statement budget inside `rebuild_native` and failed
// lease_history_completed_stock_remains_charged_until_verified_return on
// `Unavailable("pre-commit statement budget expired")` (backlog 21217d8a;
// the mechanism 97a5a57d seamed for the cuts envelope). So every Pg
// history here — reads, advances, prunes and the rebuild — runs on a
// fixture budget from a clock that has already lost a measured stall:
// whatever this bay's disk does, an answer below is the fold's, never the
// equipment's. Production keeps its 2 s, pinned in the adapter's own tests.
// The stall and the budget are the adapter's own `for_fold_fixture`, spelled
// once for every fold test (backlog ebbf38c6).
fn fixture_history(db: &TestDb) -> boss_jobs::claims_console::PgClaimsHistory {
    boss_jobs::claims_console::PgClaimsHistory::for_fold_fixture(db.pool.clone())
}

// The lease operations PgJobs runs (bind, sweep, discharge) spent a literal
// 2 s of their own under every test here, whatever the history was given
// (backlog ebbf38c6), so every PgJobs in this file is `PgJobs::for_fixture`
// and `the_lease_fixtures_never_take_the_production_budget` keeps it so.
// These two judge the seam: a stall longer than production's whole budget,
// stated by a clock that has already lost it (gate-run a5c6c1ea measured
// 5254 ms of BEGIN), passes on the fixture and is refused, by name, on
// production's 2 s. The clock is boss_events::statement_budget's, where the
// stall is spelled once (backlog 03a5dbdc).

#[tokio::test]
async fn a_lease_sweep_on_the_fixture_budget_outlasts_a_stall_past_production() {
    let db = TestDb::new().await;
    let jobs = PgJobs::for_fixture(db.pool.clone());
    let stamp =
        boss_core::publisher::EventStamp::new("jobs", ActorId::automation("lease-budget-seam"));
    let swept = jobs.sweep_resource_leases_at(&stamp, 1).await.unwrap();
    assert_eq!(swept.expired, 0);
}

#[tokio::test]
async fn a_lease_sweep_whose_budget_the_stall_has_spent_refuses_by_name() {
    let db = TestDb::new().await;
    let jobs = PgJobs::for_fixture(db.pool.clone()).with_lease_budget(
        std::time::Duration::from_secs(2),
        boss_events::statement_budget::after_the_measured_stall,
    );
    let stamp =
        boss_core::publisher::EventStamp::new("jobs", ActorId::automation("lease-budget-seam"));
    match jobs.sweep_resource_leases_at(&stamp, 1).await {
        Err(why) => assert!(
            why.to_string()
                .contains("pre-commit statement budget expired"),
            "{why}"
        ),
        Ok(swept) => panic!(
            "a spent budget must refuse the sweep, not expire {} lease(s)",
            swept.expired
        ),
    }
}

// Exercise actual producer payloads through the native source into BOTH
// adapters. Hand-authored history fixtures would miss a stripped source key.
async fn lease_history_reading(
    db: &TestDb,
    memory: &boss_jobs::claims_console::InMemoryClaimsHistory,
    pg: &boss_jobs::claims_console::PgClaimsHistory,
) -> boss_jobs::claims_console::ClaimsReading {
    use boss_jobs::claims_console::ClaimsHistory;
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    for history in [memory as &dyn ClaimsHistory, pg as &dyn ClaimsHistory] {
        assert!(
            boss_jobs::claims_console::source::advance_native_once(history, &db.pool, 1024)
                .await
                .unwrap()
        );
    }
    let now = Utc::now();
    let one = memory.read(None, 24, now).await.unwrap();
    let two = pg.read(None, 24, now).await.unwrap();
    assert_eq!(one.claims, two.claims);
    assert_eq!(one.pools, two.pools);
    assert_eq!(one.as_of, two.as_of);
    assert_eq!(one.projection.unknown_kinds, two.projection.unknown_kinds);
    two
}

async fn emitted_lease_history(phase: u8) {
    use boss_jobs::claims_console::{ClaimsHistory, InMemoryClaimsHistory};
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let mut stamp = seed_fixture_with_bill(
        &f,
        json!({"state":"configured","amount":"1"}),
        json!({"vcpu":"1","ram_gb":"1"}),
    )
    .await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"ram_gb","version":1,"status":"active","unit":"GB","kind":"flow","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    // Attribution and the command's validated holder are separate facts.
    stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        if phase == 5 {
            ActorId::automation("flow-contract")
        } else {
            ActorId::human("emp-one")
        },
    );
    let step = packet(&f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let memory = InMemoryClaimsHistory::default();
    let pg = fixture_history(&db);
    let before = lease_history_reading(&db, &memory, &pg).await;
    let binding = identity_binding(&claim);
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &binding, &stamp)
        .await
        .unwrap();
    if phase == 5 {
        let bus: Arc<dyn EventBus> = RecordingEventBus::new();
        boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
            .await
            .unwrap();
        boss_jobs::rebuild_jobs_and_steps(&db.pool)
            .await
            .expect("the authoritative replay accepts this real delegated producer fact");
    }
    let bound = lease_history_reading(&db, &memory, &pg).await;
    if phase == 1 {
        let wire = serde_json::to_value(&bound).unwrap();
        assert_eq!(
            wire["claims"][0]["identity_binding"],
            json!(binding),
            "jobs.step.resource_identity_bound must be folded and rendered"
        );
        assert!(bound.claims[0].lease_remaining.is_some_and(|n| n > 0));
        assert!(bound.claims[0].lease_unavailable.is_empty());
        assert!(bound.projection.unknown_kinds.is_empty());
        let old = pg.read(Some(before.as_of), 24, Utc::now()).await.unwrap();
        assert_eq!(
            old.claims, before.claims,
            "binding does not rewrite an earlier cut"
        );
        return;
    }
    if phase == 4 {
        claimed.status = StepStatus::Completed;
        f.jobs
            .update_step_at(
                &claimed,
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
            )
            .await
            .unwrap();
    } else {
        stamp.timestamp += chrono::Duration::hours(2);
        assert_eq!(
            f.jobs
                .sweep_resource_leases_at(&stamp, 1)
                .await
                .unwrap()
                .expired,
            1
        );
    }
    let expired = lease_history_reading(&db, &memory, &pg).await;
    let reserved = |reading: &boss_jobs::claims_console::ClaimsReading, key: &str| {
        reading
            .pools
            .iter()
            .find(|p| p.pool_key == key)
            .unwrap()
            .reserved
            .to_string()
    };
    if phase == 4 {
        assert!(expired.claims.is_empty());
        assert_eq!(
            reserved(&expired, "vcpu"),
            "1",
            "completion does not prove physical STOCK return"
        );
        assert_eq!(reserved(&expired, "ram_gb"), "0");
        let later = Utc::now() + chrono::Duration::hours(169);
        for history in [&memory as &dyn ClaimsHistory, &pg as &dyn ClaimsHistory] {
            let cursor = history.cursor().await.unwrap();
            while history.prune(cursor, later, 1024).await.unwrap() == 1024 {}
            assert_eq!(
                reserved(&history.read(None, 24, later).await.unwrap(), "vcpu"),
                "1",
                "retention never drops a still-charged STOCK obligation"
            );
        }
    }
    if phase == 2 {
        assert_eq!(
            reserved(&expired, "ram_gb"),
            "0",
            "jobs.step.resource_lease_expired releases FLOW"
        );
        assert_eq!(
            reserved(&expired, "vcpu"),
            "1",
            "expiry does not prove physical STOCK return"
        );
        let wire = serde_json::to_value(&expired).unwrap();
        assert_eq!(wire["claims"][0]["lease_expired"], true);
        assert_eq!(
            wire["claims"][0]["stock_obligations"][0]["returned_by"],
            serde_json::Value::Null
        );
        assert!(expired.projection.unknown_kinds.is_empty());
        assert_eq!(
            pg.read(Some(bound.as_of), 24, Utc::now())
                .await
                .unwrap()
                .claims,
            bound.claims
        );
        return;
    }
    let evidence = uuid::Uuid::new_v4();
    let observed = stamp.timestamp;
    stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        ActorId::automation(if phase == 5 {
            "flow-contract"
        } else {
            "workspace-reclaimer"
        }),
    );
    stamp.timestamp = observed;
    assert!(
        f.jobs
            .discharge_stock_at(
                claim.id,
                "vcpu",
                "automation:workspace-reclaimer",
                evidence,
                &boss_jobs::resource_leases::NoQualifiedReturnAdapter,
                &stamp
            )
            .await
            .is_err()
    );
    let unqualified = lease_history_reading(&db, &memory, &pg).await;
    assert_eq!(reserved(&unqualified, "vcpu"), "1");
    f.jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &FixtureMachineReturn { tamper: false },
            &stamp,
        )
        .await
        .unwrap();
    let returned = lease_history_reading(&db, &memory, &pg).await;
    assert_eq!(
        reserved(&returned, "vcpu"),
        "0",
        "jobs.step.stock_returned releases only the verified obligation"
    );
    assert!(returned.projection.unknown_kinds.is_empty());
    let wire = serde_json::to_value(&returned).unwrap();
    if phase == 4 {
        assert!(returned.claims.is_empty());
    } else {
        assert_eq!(
            wire["claims"][0]["stock_obligations"][0]["returned_by"],
            json!(evidence)
        );
    }
    if phase != 4 {
        assert_eq!(
            pg.read(Some(expired.as_of), 24, Utc::now())
                .await
                .unwrap()
                .pools,
            expired.pools
        );
    }
    f.jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            evidence,
            &FixtureMachineReturn { tamper: false },
            &stamp,
        )
        .await
        .unwrap();
    let repeated = lease_history_reading(&db, &memory, &pg).await;
    assert_eq!(repeated.claims, returned.claims);
    assert_eq!(repeated.pools, returned.pools);
    boss_jobs::claims_console::source::rebuild_native_with(&pg, &db.pool)
        .await
        .unwrap();
    let rebuilt = pg.read(None, 24, Utc::now()).await.unwrap();
    assert_eq!(rebuilt.claims, returned.claims);
    assert_eq!(rebuilt.pools, returned.pools);
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM audit_log WHERE kind=ANY($1) ORDER BY kind")
            .bind(vec![
                boss_jobs::resource_leases::IDENTITY_BOUND,
                boss_jobs::resource_leases::LEASE_EXPIRED,
                boss_jobs::resource_leases::STOCK_RETURNED,
            ])
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        kinds.len(),
        if phase == 4 { 2 } else { 3 },
        "each applicable lifecycle fact emitted once, including duplicate-return replay"
    );
}

/// The door records through the adapter's own outbox write, so the claims
/// console folds the expiry the clock rule caused exactly as it folds car
/// 2b's direct call, credited to the rule that fired (backlog 3aeaf08c).
#[tokio::test(flavor = "multi_thread")]
async fn the_lease_sweep_door_fact_is_folded_by_the_claims_console() {
    use boss_jobs::claims_console::InMemoryClaimsHistory;
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let seeded = seed_fixture_with_bill(
        &f,
        json!({"state":"configured","amount":"1"}),
        json!({"vcpu":"1","ram_gb":"1"}),
    )
    .await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":"1"},"created_at":seeded.timestamp})).unwrap(),seeded.actor(),seeded.timestamp).await.unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"ram_gb","version":1,"status":"active","unit":"GB","kind":"flow","capacity":{"state":"configured","amount":"1"},"created_at":seeded.timestamp})).unwrap(),seeded.actor(),seeded.timestamp).await.unwrap();
    let past = three_hours_ago(&seeded);
    let step = packet(&f, &past).await;
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    let memory = InMemoryClaimsHistory::default();
    let pg = fixture_history(&db);
    let before = lease_history_reading(&db, &memory, &pg).await;
    assert_eq!(
        serde_json::to_value(&before).unwrap()["claims"][0]["lease_expired"],
        false
    );
    let app = sweep_app(&f);
    assert_eq!(sweep_as(&app, "").await["expired"], 1);
    let expired = lease_history_reading(&db, &memory, &pg).await;
    let wire = serde_json::to_value(&expired).unwrap();
    assert_eq!(wire["claims"][0]["lease_expired"], true);
    assert_eq!(
        wire["claims"][0]["stock_obligations"][0]["returned_by"],
        serde_json::Value::Null
    );
    let reserved = |key: &str| {
        expired
            .pools
            .iter()
            .find(|p| p.pool_key == key)
            .unwrap()
            .reserved
            .to_string()
    };
    assert_eq!(reserved("ram_gb"), "0", "the door's expiry releases FLOW");
    assert_eq!(reserved("vcpu"), "1", "and keeps STOCK charged");
    assert!(expired.projection.unknown_kinds.is_empty());
    let actor: String =
        sqlx::query_scalar("SELECT payload->>'_actor' FROM audit_log WHERE kind=$1")
            .bind(boss_jobs::resource_leases::LEASE_EXPIRED)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    // A rule's wire id `rule:<name>` is recorded as the automation it
    // names (`User::ambient_actor`), as every rule-fired fact is.
    assert_eq!(
        actor,
        format!("automation:{SWEEP_RULE}"),
        "the fact names the rule that fired"
    );
}

#[tokio::test]
async fn lease_history_identity_bound_is_emitted_folded_and_rendered() {
    emitted_lease_history(1).await;
}
#[tokio::test]
async fn lease_history_expiry_is_emitted_folded_and_rendered() {
    emitted_lease_history(2).await;
}
#[tokio::test]
async fn lease_history_verified_return_is_emitted_folded_and_rendered() {
    emitted_lease_history(3).await;
}
#[tokio::test]
async fn lease_history_completed_stock_remains_charged_until_verified_return() {
    emitted_lease_history(4).await;
}
#[tokio::test]
async fn lease_history_accepts_the_canonical_replays_delegated_producer_attribution() {
    emitted_lease_history(5).await;
}

// Arrange receipt times at the history port, keeping the real native source
// and producer payloads. The immutable audit log itself is never modified.
struct MotionClockHistory<'a> {
    inner: &'a dyn boss_jobs::claims_console::ClaimsHistory,
    recorded: chrono::DateTime<Utc>,
}
#[async_trait::async_trait]
impl boss_jobs::claims_console::ClaimsHistory for MotionClockHistory<'_> {
    // The wrapped history's budget, not the trait's production default.
    fn source_deadline(&self) -> tokio::time::Instant {
        self.inner.source_deadline()
    }
    async fn cursor(
        &self,
    ) -> Result<boss_jobs::claims_console::ProjectionCursor, boss_jobs::claims_console::ClaimsError>
    {
        self.inner.cursor().await
    }
    async fn advance(
        &self,
        expected: boss_jobs::claims_console::ProjectionCursor,
        batch: &boss_jobs::claims_console::HistoryBatch,
        source: boss_jobs::claims_console::SourceObservation,
    ) -> Result<(), boss_jobs::claims_console::ClaimsError> {
        let mut arranged = batch.clone();
        arranged.through_at = self.recorded;
        for fact in &mut arranged.facts {
            fact.recorded_at = self.recorded;
        }
        self.inner
            .advance(
                expected,
                &arranged,
                boss_jobs::claims_console::SourceObservation {
                    observed_at: source.observed_at.max(self.recorded),
                    ..source
                },
            )
            .await
    }
}
async fn motion_clock_reading(
    db: &TestDb,
    memory: &boss_jobs::claims_console::InMemoryClaimsHistory,
    pg: &boss_jobs::claims_console::PgClaimsHistory,
    recorded: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> boss_jobs::claims_console::ClaimsReading {
    use boss_jobs::claims_console::ClaimsHistory;
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    for history in [memory as &dyn ClaimsHistory, pg as &dyn ClaimsHistory] {
        boss_jobs::claims_console::source::advance_native_once(
            &MotionClockHistory {
                inner: history,
                recorded,
            },
            &db.pool,
            1024,
        )
        .await
        .unwrap();
    }
    let one = memory.read(None, 1, now).await.unwrap();
    let two = pg.read(None, 1, now).await.unwrap();
    assert_eq!(one.claims, two.claims);
    assert_eq!(one.pools, two.pools);
    assert_eq!(one.as_of, two.as_of);
    assert_eq!(one.as_of_at, two.as_of_at);
    assert_eq!(one.projection.unknown_kinds, two.projection.unknown_kinds);
    two
}

async fn reservation_motion_clock(phase: u8) {
    use boss_jobs::claims_console::InMemoryClaimsHistory;
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let mut stamp = seed_fixture_with_bill(
        &f,
        json!({"state":"configured","amount":"1"}),
        json!({"vcpu":"1","ram_gb":"1"}),
    )
    .await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"workspace-slot","kind":"stock","reclaimer":"automation:workspace-reclaimer","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"ram_gb","version":1,"status":"active","unit":"GB","kind":"flow","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let t = chrono::DateTime::<Utc>::from_timestamp(Utc::now().timestamp(), 0).unwrap();
    let memory = InMemoryClaimsHistory::default();
    let pg = fixture_history(&db);
    motion_clock_reading(&db, &memory, &pg, t, t).await;
    stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::human("emp-one"));
    let step = packet(&f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let claim: boss_jobs::flow_resources::Claim =
        serde_json::from_value(claimed.metadata["resource_claim"].clone()).unwrap();
    let at = |hours| t + chrono::Duration::hours(hours);
    motion_clock_reading(&db, &memory, &pg, at(2), at(2)).await;
    let band = |r: &boss_jobs::claims_console::ClaimsReading, key: &str| {
        r.pools
            .iter()
            .find(|p| p.pool_key == key)
            .unwrap()
            .band
            .clone()
    };
    f.jobs
        .bind_resource_identity_at(&step.id, "emp-one", &identity_binding(&claim), &stamp)
        .await
        .unwrap();
    let bound = motion_clock_reading(
        &db,
        &memory,
        &pg,
        at(if phase == 1 || phase == 4 { 7 } else { 3 }),
        at(if phase == 1 || phase == 4 { 7 } else { 3 }),
    )
    .await;
    if phase == 1 {
        assert_eq!(
            band(&bound, "vcpu").as_deref(),
            Some(boss_jobs::region_states::STUCK_AT_CAPACITY.id),
            "later identity bookkeeping must not restart a full pool's original clock"
        );
        return;
    }
    if phase == 4 {
        claimed.status = StepStatus::Completed;
        f.jobs
            .update_step_at(
                &claimed,
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
            )
            .await
            .unwrap();
        let completed = motion_clock_reading(&db, &memory, &pg, at(8), at(9)).await;
        assert_eq!(
            band(&completed, "ram_gb"),
            None,
            "released FLOW leaves the fill calculation"
        );
        assert_eq!(
            band(&completed, "vcpu").as_deref(),
            Some(boss_jobs::region_states::STUCK_AT_CAPACITY.id),
            "completed but charged STOCK keeps its original clock"
        );
        return;
    }
    stamp.timestamp += chrono::Duration::hours(2);
    assert_eq!(
        f.jobs
            .sweep_resource_leases_at(&stamp, 1)
            .await
            .unwrap()
            .expired,
        1
    );
    // Keep the receipt sequence monotone; this cut distinguishes the original
    // reservation from both the older declaration and the newer binding.
    let expired = motion_clock_reading(&db, &memory, &pg, at(5), at(5)).await;
    if phase == 2 {
        assert_eq!(
            band(&expired, "vcpu").as_deref(),
            Some(boss_jobs::region_states::FULL.id)
        );
        // The boundary immediately before the original 4h hold must still be
        // FULL, rather than falling back to the older declaration clock.
        use boss_jobs::claims_console::ClaimsHistory;
        for h in [&memory as &dyn ClaimsHistory, &pg as &dyn ClaimsHistory] {
            let r = h.read(None, 1, at(5)).await.unwrap();
            assert_eq!(
                band(&r, "vcpu").as_deref(),
                Some(boss_jobs::region_states::FULL.id),
                "charged expired STOCK uses reserved_at, not capacity_since"
            );
        }
        return;
    }
    let mut reclaimer =
        boss_core::publisher::EventStamp::new("jobs", ActorId::automation("workspace-reclaimer"));
    reclaimer.timestamp = stamp.timestamp;
    f.jobs
        .discharge_stock_at(
            claim.id,
            "vcpu",
            "automation:workspace-reclaimer",
            uuid::Uuid::new_v4(),
            &FixtureMachineReturn { tamper: false },
            &reclaimer,
        )
        .await
        .unwrap();
    motion_clock_reading(&db, &memory, &pg, at(5), at(5)).await;
    let next = packet(&f, &stamp).await;
    f.jobs
        .claim_step_displacing_at(&next.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let fresh = motion_clock_reading(&db, &memory, &pg, at(10), at(12)).await;
    assert_eq!(
        band(&fresh, "vcpu").as_deref(),
        Some(boss_jobs::region_states::FULL.id),
        "verified return then fresh reservation starts a new clock"
    );
}

#[tokio::test]
async fn reservation_motion_binding_preserves_original_clock() {
    reservation_motion_clock(1).await;
}
#[tokio::test]
async fn reservation_motion_expired_stock_keeps_original_clock() {
    reservation_motion_clock(2).await;
}
#[tokio::test]
async fn reservation_motion_verified_return_then_new_claim_starts_clock() {
    reservation_motion_clock(3).await;
}
#[tokio::test]
async fn reservation_motion_completion_excludes_only_released_capacity() {
    reservation_motion_clock(4).await;
}

async fn last_unit_has_one_winner_and_publication_cannot_erase_its_capacity<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let a = packet(f, &stamp).await;
    let b = packet(f, &stamp).await;
    let (one, two) = tokio::join!(
        f.jobs
            .claim_step_displacing_at(&a.id, "emp-one", &[], &stamp, &[]),
        f.jobs
            .claim_step_displacing_at(&b.id, "emp-two", &[], &stamp, &[])
    );
    assert_eq!(
        usize::from(one.is_ok()) + usize::from(two.is_ok()),
        1,
        "last unit is shared across steps and actors"
    );
    let winner = one.ok().or_else(|| two.ok()).unwrap();
    let retry = f
        .jobs
        .claim_step_displacing_at(
            &winner.id,
            winner.assignee_id.as_deref().unwrap(),
            &[],
            &stamp,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        winner.metadata["resource_claim"],
        retry.metadata["resource_claim"]
    );
    let mut replacement = f.pools.list_active().await.unwrap().remove(0);
    replacement.version = 2;
    replacement.capacity = serde_json::from_value(json!({"state":"unknown"})).unwrap();
    assert!(
        f.pools
            .publish_declared(replacement, stamp.actor(), stamp.timestamp)
            .await
            .is_err(),
        "a publisher shares the acquisition fence"
    );
}
async fn completion_releases_exactly_its_generation_and_an_old_copy_cannot_release_the_next<
    J: JobsRepository,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let a = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&a.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let mut release = claimed.clone();
    release.status = StepStatus::Ready;
    release.assignee_id = None;
    f.jobs
        .update_step_at(
            &release,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(release))],
        )
        .await
        .unwrap();
    let next = f
        .jobs
        .claim_step_displacing_at(&a.id, "emp-two", &[], &stamp, &[])
        .await
        .unwrap();
    assert_ne!(
        next.metadata["resource_claim"]["id"],
        claimed.metadata["resource_claim"]["id"]
    );
    assert!(
        f.jobs
            .update_step_at(
                &release,
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(release))]
            )
            .await
            .is_err(),
        "stale holder/generation cannot free a new allocation"
    );
    let mut completed = next.clone();
    completed.status = StepStatus::Completed;
    f.jobs
        .update_step_at(
            &completed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(completed))],
        )
        .await
        .unwrap();
    let b = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&b.id, "emp-three", &[], &stamp, &[])
            .await
            .is_ok(),
        "normal completion releases the actual allocation"
    );
}

async fn metadata_cannot_forge_or_rebind_an_acquisition<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let a = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&a.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let mut forged = claimed.metadata["resource_claim"].clone();
    forged["holder"] = json!("emp-two");
    let patch = json!({"resource_claim":forged})
        .as_object()
        .unwrap()
        .clone();
    assert!(
        f.jobs
            .merge_step_metadata_at(&a.id, &patch, &stamp)
            .await
            .is_err(),
        "the generic merge door must not replace a generation/holder fence"
    );
    assert_eq!(
        f.jobs.get_step(&a.id).await.unwrap().unwrap().metadata["resource_claim"],
        claimed.metadata["resource_claim"]
    );
}

async fn cancellation_releases_every_current_vector_atomically<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"2"})).await;
    let first = packet(f, &stamp).await;
    let mut second = Step::new(first.job_id, "task", "Second work", 2);
    second.spec_slug = Some("work".into());
    second.status = StepStatus::Ready;
    f.jobs
        .add_step_at(
            &second,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_CREATED, json!(second))],
        )
        .await
        .unwrap();
    for (step, actor) in [(&first, "emp-one"), (&second, "emp-two")] {
        f.jobs
            .claim_step_displacing_at(&step.id, actor, &[], &stamp, &[])
            .await
            .unwrap();
    }
    let mut job = f.jobs.get_job(&first.job_id).await.unwrap().unwrap();
    job.status = boss_core::job::JobStatus::Cancelled;
    f.jobs
        .update_job_at(
            &job,
            boss_core::job::JobStatus::Open,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_UPDATED, json!(job))],
        )
        .await
        .unwrap();
    for actor in ["emp-three", "emp-four"] {
        let next = packet(f, &stamp).await;
        assert!(
            f.jobs
                .claim_step_displacing_at(&next.id, actor, &[], &stamp, &[])
                .await
                .is_ok(),
            "cancellation must release ALL its current vectors"
        );
    }
}
async fn cancelled_packets_cannot_reclaim_an_old_generation<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let mut job = f.jobs.get_job(&step.job_id).await.unwrap().unwrap();
    job.status = boss_core::job::JobStatus::Cancelled;
    f.jobs
        .update_job_at(
            &job,
            boss_core::job::JobStatus::Open,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_UPDATED, json!(job))],
        )
        .await
        .unwrap();
    assert!(
        f.jobs
            .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
            .await
            .is_err(),
        "an old holder is not an admission to a cancelled packet"
    );
}
async fn closing_a_packet_cannot_strand_its_current_flow_vectors<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let mut job = f.jobs.get_job(&step.job_id).await.unwrap().unwrap();
    let read = job.status;
    job.status = boss_core::job::JobStatus::Closed;
    let close = f
        .jobs
        .update_job_at(
            &job,
            read,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_UPDATED, json!(job))],
        )
        .await;
    if close.is_ok() {
        let next = packet(f, &stamp).await;
        let acquired = f
            .jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &stamp, &[])
            .await;
        assert!(
            acquired.is_ok(),
            "an accepted terminal close cannot strand FLOW capacity: {acquired:?}"
        );
    } else {
        assert_eq!(
            f.jobs.get_job(&step.job_id).await.unwrap().unwrap().status,
            read,
            "a close refusal must preserve its old state"
        );
    }
}
async fn pinned_bills_and_whole_vector_refusal_do_not_follow_the_newest_protocol<
    J: JobsRepository,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture_with_bill(
        f,
        json!({"state":"unknown"}),
        json!({"model":"fable","vcpu":"1"}),
    )
    .await;
    f.pools.publish_declared(serde_json::from_value(json!({"name":"robots:fable","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"1"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let mut replacement = f.workflows.get_version("flow-contract", 1).await.unwrap();
    replacement.version = 2;
    replacement.steps[1].resources =
        Some(serde_json::from_value(json!({"model":"fable"})).unwrap());
    f.workflows
        .publish_authored(replacement, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    let old = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&old.id, "emp-one", &[], &stamp, &[])
            .await
            .is_err(),
        "v1 retains its unknown vCPU demand after v2 publishes"
    );
    let new = packet_version(f, &stamp, 2).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&new.id, "emp-two", &[], &stamp, &[])
        .await
        .expect("refused v1 cannot have partly consumed the earlier model dimension");
    assert_eq!(claimed.metadata["resource_claim"]["workflow_version"], 2);
    assert_eq!(
        claimed.metadata["resource_claim"]["allocations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn generic_activation_cannot_skip_the_pinned_resource_bill<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"unknown"})).await;
    let step = packet(f, &stamp).await;
    let mut forged = step.clone();
    forged.status = StepStatus::Active;
    forged.assignee_id = Some("emp-one".into());
    assert!(
        f.jobs
            .update_step_at(
                &forged,
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(forged))]
            )
            .await
            .is_err(),
        "the generic write is not a resource admission door"
    );
    assert_eq!(f.jobs.get_step(&step.id).await.unwrap().unwrap(), step);
}
async fn an_exit_cannot_record_a_state_that_storage_did_not_write<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    let mut forged = claimed.clone();
    forged.status = StepStatus::Completed;
    forged.spec_slug = Some("forged-frozen-field".into());
    assert!(
        f.jobs
            .update_step_at(
                &forged,
                stamp.timestamp,
                &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(forged))]
            )
            .await
            .is_err(),
        "the release cause must equal the actual committed step"
    );
    assert_eq!(f.jobs.get_step(&step.id).await.unwrap().unwrap(), claimed);
    let next = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&next.id, "emp-two", &[], &stamp, &[])
            .await
            .is_err(),
        "a rejected exit cannot free capacity"
    );
}
async fn an_http_exit_must_name_the_executors_original_acquisition<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let mut original = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-resource-test", &[], &stamp, &[])
        .await
        .unwrap();
    let original_id = original.metadata["resource_claim"]["id"].clone();
    original.status = StepStatus::Ready;
    original.assignee_id = None;
    f.jobs
        .update_step_at(
            &original,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(original))],
        )
        .await
        .unwrap();
    let current = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-resource-test", &[], &stamp, &[])
        .await
        .unwrap();
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
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState {
        kind_registry: Some(f.workflows.clone()),
        ..JobsApiState::minimal(
            f.jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let path = format!("/api/jobs/{}/steps/{}", step.job_id, step.id);
    for body in [
        json!({"status":"completed","resource_claim_id":original_id}),
        json!({"status":"completed"}),
    ] {
        TestRequest::put(&path)
            .as_user("emp-resource-test", "platform-admin")
            .json(&body)
            .send(&app)
            .await
            .assert_status(StatusCode::CONFLICT);
        assert_eq!(
            f.jobs.get_step(&step.id).await.unwrap().unwrap(),
            current,
            "fresh server reads cannot substitute for the executor's original generation"
        );
    }
    TestRequest::put(&path).as_user("emp-resource-test","platform-admin").json(&json!({"status":"completed","resource_claim_id":current.metadata["resource_claim"]["id"]})).send(&app).await.assert_status(StatusCode::NO_CONTENT);
}

/// THE DOOR A LAPSED RUN WAS LOCKED OUT OF (backlog ad1c3af2, measured
/// 2026-11-09 19:20Z). Runs 056cc3d1 and d659981f went green, their cars
/// landed, and both stood at `building` 19 h later: the claim's lease ran
/// out in the gate queue, the sweep freed the slot, and every exit — the
/// merged-car rule's completion, a by-hand claim-id PUT, `boss step
/// release` — answered 500 `resource-lease-expired`. The holder's exit,
/// naming its original claim, now lands; an exit that names no claim or
/// another one is still refused, so a LIVE claim gains no way around it.
async fn an_http_exit_whose_lease_lapsed_lands_by_its_original_claim_id<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let seeded = seed_fixture(f, json!({"state":"configured","amount":"2"})).await;
    let past = three_hours_ago(&seeded);
    let swept = packet(f, &past).await;
    let swept_claim = f
        .jobs
        .claim_step_displacing_at(&swept.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        kind_registry: Some(f.workflows.clone()),
        ..JobsApiState::minimal(
            f.jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            Arc::new(
                FakePolicyClient::builder()
                    .allow(
                        "platform-admin",
                        Action::Update,
                        Resource::step(),
                        Scope::All,
                    )
                    .allow("platform-admin", Action::Read, Resource::step(), Scope::All)
                    .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                    .build(),
            ),
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    assert_eq!(sweep_as(&app, "").await["expired"], 1);
    // Claimed after the sweep, so its lease lapses unswept.
    let unswept = packet(f, &past).await;
    let unswept_claim = f
        .jobs
        .claim_step_displacing_at(&unswept.id, "emp-one", &[], &past, &[])
        .await
        .unwrap();
    for (step, claim) in [(&swept, &swept_claim), (&unswept, &unswept_claim)] {
        let path = format!("/api/jobs/{}/steps/{}", step.job_id, step.id);
        for body in [
            json!({"status":"completed"}),
            json!({"status":"completed","resource_claim_id":uuid::Uuid::new_v4()}),
        ] {
            TestRequest::put(&path)
                .as_user("emp-one", "platform-admin")
                .json(&body)
                .send(&app)
                .await
                .assert_status(StatusCode::CONFLICT);
        }
        assert_eq!(
            f.jobs.get_step(&step.id).await.unwrap().unwrap(),
            *claim,
            "an exit naming no claim, or another, writes nothing"
        );
        // The body the merged-car rule sends (`complete_read_step`): its
        // status, plus the claim id read off the step it judged.
        let exit = boss_jobs::flow_resources::exiting(&json!(claim), json!({"status":"completed"}));
        assert_eq!(
            exit[boss_jobs::flow_resources::EXIT_CLAIM_KEY],
            claim.metadata["resource_claim"]["id"]
        );
        TestRequest::put(&path)
            .as_user("emp-one", "platform-admin")
            .json(&exit)
            .send(&app)
            .await
            .assert_status(StatusCode::NO_CONTENT);
        assert_eq!(
            f.jobs.get_step(&step.id).await.unwrap().unwrap().status,
            StepStatus::Completed
        );
    }
    assert!(
        released(f, &swept.job_id).await.is_empty(),
        "the swept step's expiry was its release"
    );
    let releases = released(f, &unswept.job_id).await;
    assert_eq!(releases.len(), 1, "{releases:?}");
    assert_eq!(
        releases[0].payload[boss_jobs::flow_resources::LEASE_EXPIRED_KEY],
        true
    );
}
async fn representable_held_totals_do_not_depend_on_step_identity_order<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let cap = "10000000000000000000000000001";
    let stamp = seed_fixture_with_bill(
        f,
        json!({"state":"configured","amount":cap}),
        json!({"vcpu":"0.5"}),
    )
    .await;
    let mut large = f.workflows.get_version("flow-contract", 1).await.unwrap();
    large.version = 2;
    large.steps[1].resources =
        Some(serde_json::from_value(json!({"vcpu":"10000000000000000000000000000"})).unwrap());
    f.workflows
        .publish_authored(large, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    // Acquire fractions first, but make the large row sort first in the
    // current ledger. The exact aggregate is representable in either order.
    for (version, id) in [(1, 2_u128), (1, 3), (2, 1)] {
        let id = serde_json::from_value(json!(uuid::Uuid::from_u128(id))).unwrap();
        let step = packet_with_id(f, &stamp, version, Some(id)).await;
        f.jobs
            .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
            .await
            .unwrap();
    }
    let mut next = f
        .pools
        .list_versions("vcpu")
        .await
        .unwrap()
        .into_iter()
        .find(|pool| pool.version == 1)
        .unwrap();
    next.version = 2;
    let published = f
        .pools
        .publish_declared(next, stamp.actor(), stamp.timestamp)
        .await;
    assert!(
        published.is_ok(),
        "a valid exact held total cannot depend on row identity order: {published:?}"
    );
}
async fn first_record_cannot_introduce_a_server_acquisition_token<J: JobsRepository>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_fixture(f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(f, &stamp).await;
    let attempted = f
        .jobs
        .record_step_metadata_if_unchanged_at(
            &step.id,
            "resource_claim",
            &json!({"generation":1,"holder":"forged"}),
            None,
            &stamp,
        )
        .await;
    assert!(
        attempted.is_err(),
        "an immutable first-record receipt must not manufacture a server-owned acquisition"
    );
    assert_eq!(f.jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    f.jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
}
async fn the_agent_workspace_example_is_one_atomic_four_dimension_flow_fixture<
    J: JobsRepository,
>(
    f: &Fixture<J>,
    _: &str,
) {
    // Existing D1 mapping only. All four rows are FLOW fixtures, NOT an
    // enabled factory bill or a claim that physical STOCK teardown occurred.
    let stamp = seed_fixture_with_bill(
        f,
        json!({"state":"configured","amount":"4"}),
        json!({"model":"agent-workspace","vcpu":"4","ram_gb":"16","disk_gb":"100"}),
    )
    .await;
    for (name, capacity) in [
        (
            "robots:agent-workspace",
            json!({"state":"configured","amount":"1"}),
        ),
        ("ram_gb", json!({"state":"configured","amount":"16"})),
        ("disk:ci-workspaces", json!({"state":"unknown"})),
    ] {
        f.pools.publish_declared(serde_json::from_value(json!({"name":name,"version":1,"status":"active","unit":"fixture-unit","kind":"flow","capacity":capacity,"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    }
    let a = packet(f, &stamp).await;
    let b = packet(f, &stamp).await;
    assert!(
        f.jobs
            .claim_step_displacing_at(&a.id, "emp-one", &[], &stamp, &[])
            .await
            .is_err()
    );
    let waiting = f.jobs.get_step(&a.id).await.unwrap().unwrap();
    // The whole wait, `since` included: a refusal records the instant its
    // wait began, the refusing stamp's own (backlog 587f395a).
    assert_eq!(
        waiting.metadata["waiting_on"],
        json!({"resource":"disk:ci-workspaces","reason":"capacity-unknown","since":stamp.timestamp})
    );
    f.pools.publish_declared(serde_json::from_value(json!({"name":"disk:ci-workspaces","version":2,"status":"active","unit":"fixture-unit","kind":"flow","capacity":{"state":"configured","amount":"100"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let (left, right) = tokio::join!(
        f.jobs
            .claim_step_displacing_at(&a.id, "emp-one", &[], &stamp, &[]),
        f.jobs
            .claim_step_displacing_at(&b.id, "emp-two", &[], &stamp, &[])
    );
    let winners: Vec<_> = [left, right].into_iter().filter_map(Result::ok).collect();
    assert_eq!(winners.len(), 1);
    let claim = &winners[0].metadata["resource_claim"];
    assert_eq!(claim["allocations"].as_array().unwrap().len(), 4);
    assert_eq!(
        claim["bill"],
        json!({"model":"agent-workspace","vcpu":"4","ram_gb":"16","disk_gb":"100"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_pinned_bill_and_current_generations_rebuild_from_their_facts() {
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let exact = "0.1000000000000000000000000001";
    let stamp = seed_fixture_with_bill(
        &f,
        json!({"state":"configured","amount":"2"}),
        json!({"vcpu":exact,"ram_gb":"0"}),
    )
    .await;
    let first = packet(&f, &stamp).await;
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&first.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(claimed.metadata["resource_claim"]["bill"]["vcpu"], exact);
    claimed.status = StepStatus::Ready;
    claimed.assignee_id = None;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    let second = f
        .jobs
        .claim_step_displacing_at(&first.id, "emp-two", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(second.metadata["resource_claim"]["generation"], 2);
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object('headers',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY step_id),'[]') FROM step_resource_claims c),'allocations',(SELECT coalesce(jsonb_agg(jsonb_build_object('step_id',step_id,'pool',pool,'amount',amount::text) ORDER BY step_id,pool),'[]') FROM step_resource_allocations))",
        )
    };
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        before["allocations"][0]["amount"], exact,
        "numeric storage must preserve all 28 fractional digits, not a JSON f64 snapshot"
    );
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    sqlx::query("DELETE FROM step_resource_allocations")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM step_resource_claims")
        .execute(&db.pool)
        .await
        .unwrap();
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    let after = snapshot().fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        before, after,
        "reserved/released facts must reconstruct the ledger, not merely mark that a claim occurred"
    );
    assert_eq!(
        f.jobs.get_step(&first.id).await.unwrap().unwrap().metadata,
        second.metadata
    );
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        snapshot().fetch_one(&db.pool).await.unwrap(),
        after,
        "a second replay cannot reserve another vector"
    );
    // Append a contradictory fixture fact through the real outbox. The audit
    // log is append-only; never disable that guard to construct corruption.
    let mut contradictory = second.metadata["resource_claim"].clone();
    contradictory["generation"] = json!(99);
    let invalid = stamp.event(
        boss_jobs::flow_resources::RESERVED,
        json!({"job_id":first.job_id,"step_id":first.id,"claim":contradictory,"step":second}),
    );
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &invalid)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    assert!(
        boss_jobs::rebuild_jobs_and_steps(&db.pool).await.is_err(),
        "a contradictory immutable acquisition must refuse BEFORE clearing serving projections"
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), after);
}

#[tokio::test(flavor = "multi_thread")]
async fn joined_resource_writes_roll_back_rows_and_outbox_together() {
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let stamp = seed_fixture(&f, json!({"state":"configured","amount":"1"})).await;
    let step = packet(&f, &stamp).await;
    sqlx::raw_sql("CREATE FUNCTION fail_resource_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind=TG_ARGV[0] THEN RAISE EXCEPTION 'own resource outbox refusal'; END IF; RETURN NEW; END $$;").execute(&db.pool).await.unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object('jobs',(SELECT coalesce(jsonb_agg(to_jsonb(j) ORDER BY id),'[]') FROM jobs j),'steps',(SELECT coalesce(jsonb_agg(to_jsonb(s) ORDER BY id),'[]') FROM steps s),'claims',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY step_id),'[]') FROM step_resource_claims c),'allocations',(SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY step_id,pool),'[]') FROM step_resource_allocations a),'outbox',(SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY id),'[]') FROM event_outbox e))",
        )
    };
    sqlx::raw_sql("CREATE TRIGGER fail_resource BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_resource_outbox('jobs.step.reserved');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let error = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("own resource outbox refusal"),
        "original failure must remain named: {error}"
    );
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    sqlx::query("DROP TRIGGER fail_resource ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut claimed = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-one", &[], &stamp, &[])
        .await
        .unwrap();
    assert_eq!(
        claimed.metadata["resource_claim"]["generation"], 1,
        "a rolled-back acquisition did not consume a generation"
    );
    sqlx::raw_sql("CREATE TRIGGER fail_resource BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_resource_outbox('jobs.step.released');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let mut released = claimed.clone();
    released.status = StepStatus::Ready;
    released.assignee_id = None;
    let error = f
        .jobs
        .update_step_at(
            &released,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(released))],
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("own resource outbox refusal"));
    assert_eq!(snapshot().fetch_one(&db.pool).await.unwrap(), before);
    let mut job = f.jobs.get_job(&step.job_id).await.unwrap().unwrap();
    let read = job.status;
    job.status = boss_core::job::JobStatus::Cancelled;
    let error = f
        .jobs
        .update_job_at(
            &job,
            read,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_UPDATED, json!(job))],
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("own resource outbox refusal"));
    assert_eq!(
        snapshot().fetch_one(&db.pool).await.unwrap(),
        before,
        "job state and every vector share the same rollback"
    );
    sqlx::query("DROP TRIGGER fail_resource ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    claimed.status = StepStatus::Ready;
    claimed.assignee_id = None;
    f.jobs
        .update_step_at(
            &claimed,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(claimed))],
        )
        .await
        .unwrap();
    f.pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":2,"status":"active","unit":"vCPU","kind":"flow","capacity":{"state":"unknown"},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER fail_resource BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fail_resource_outbox('jobs.step.resource_refused');").execute(&db.pool).await.unwrap();
    let before = snapshot().fetch_one(&db.pool).await.unwrap();
    let error = f
        .jobs
        .claim_step_displacing_at(&step.id, "emp-two", &[], &stamp, &[])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("own resource outbox refusal"));
    assert_eq!(
        snapshot().fetch_one(&db.pool).await.unwrap(),
        before,
        "a failed refusal receipt cannot leave waiting_on without its fact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_d1_to_2a_window_reads_raw_numbers_without_rewriting_registry_versions_or_facts() {
    // A034858: reserved/released resource facts are introduced by 2a, so
    // pre-2a history contains none of those amount-bearing ledger events.
    // This constructed legacy registry window still pins immutable historical
    // decoding; it is not evidence that any billed version was published live.
    let db = TestDb::new().await;
    let f = Fixture {
        log: Some(db.pool.clone()),
        jobs: Arc::new(PgJobs::for_fixture(db.pool.clone())),
        pools: Arc::new(PgPools::new(db.pool.clone())),
        workflows: Arc::new(PgWorkflows::for_fixture(db.pool.clone())),
    };
    let stamp = seed_fixture_with_bill(
        &f,
        json!({"state":"configured","amount":"2"}),
        json!({"vcpu":"0.1000000000000000000000000001"}),
    )
    .await;
    let pool = f.pools.list_versions("vcpu").await.unwrap().remove(0);
    let mut legacy = serde_json::to_value(pool).unwrap();
    legacy["capacity"]["amount"] = json!(2);
    let mut tx = db.pool.begin().await.unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &stamp.event("jobs.pool.published", legacy))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let bus: Arc<dyn EventBus> = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(&db.pool, &bus, 500)
        .await
        .unwrap();
    let immutable =
        || sqlx::query_scalar::<_, String>("SELECT payload::text FROM audit_log ORDER BY id");
    let before = immutable().fetch_all(&db.pool).await.unwrap();
    // Fixture-only simulation of a value actually retained by an old JSONB
    // row. The new reader must not round it through serde_json::Value.
    sqlx::query("UPDATE workflows SET steps=jsonb_set(steps,'{1,resources,vcpu}','0.1000000000000000000000000001'::jsonb) WHERE kind='flow-contract'").execute(&db.pool).await.unwrap();
    sqlx::query("UPDATE resource_pools SET spec=jsonb_set(spec,'{capacity,amount}','2'::jsonb) WHERE name='vcpu'").execute(&db.pool).await.unwrap();
    let stored_workflows: Vec<String> =
        sqlx::query_scalar("SELECT steps::text FROM workflows ORDER BY kind,version")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let stored_pools: Vec<String> =
        sqlx::query_scalar("SELECT spec::text FROM resource_pools ORDER BY name,version")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert!(
        serde_json::from_value::<boss_jobs::resources::ResourceBill>(json!({"vcpu":0.125}))
            .is_err(),
        "public writes still reject numeric quantities"
    );
    assert_eq!(
        serde_json::to_value(f.workflows.get_version("flow-contract", 1).await.unwrap()).unwrap()["steps"]
            [1]["resources"]["vcpu"],
        "0.1000000000000000000000000001"
    );
    assert_eq!(
        serde_json::to_value(f.pools.list_versions("vcpu").await.unwrap().remove(0)).unwrap()["capacity"]
            ["amount"],
        "2"
    );
    assert_eq!(
        immutable().fetch_all(&db.pool).await.unwrap(),
        before,
        "read compatibility must not rewrite an immutable event"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT steps::text FROM workflows ORDER BY kind,version")
            .fetch_all(&db.pool)
            .await
            .unwrap(),
        stored_workflows
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT spec::text FROM resource_pools ORDER BY name,version"
        )
        .fetch_all(&db.pool)
        .await
        .unwrap(),
        stored_pools
    );
    boss_jobs::rebuild_jobs_and_steps(&db.pool).await.unwrap();
    assert_eq!(
        immutable().fetch_all(&db.pool).await.unwrap(),
        before,
        "historical numeric decoding is confined to replay, without event repair or imagined recovered precision"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_actual_claim_door_refuses_a_pinned_bill_against_unknown_capacity() {
    let db = TestDb::new().await;
    let now = Utc::now();
    let actor = ActorId::automation("resource-claim-test");
    let workflows = Arc::new(PgWorkflows::for_fixture(db.pool.clone()));
    let spec: WorkflowSpec = serde_json::from_value(json!({
        "kind":"flow-reservation-test", "label":"Flow reservation test", "category":"test",
        "version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":now,
        "steps":[
            {"title":"start","kind":"trigger","ready_when":"true"},
            {"title":"work","kind":"task","ready_when":"steps.start.done","resources":{"vcpu":"1"}},
            {"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}
        ]
    })).expect("protocol");
    workflows
        .publish_authored(spec, JobId::new(), &actor, now)
        .await
        .expect("pinned protocol");
    let pool: PoolSpec = serde_json::from_value(json!({
        "name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow",
        "capacity":{"state":"unknown"},"created_at":now
    }))
    .expect("unknown is not zero");
    PgPools::new(db.pool.clone())
        .publish_declared(pool, &actor, now)
        .await
        .expect("pool");
    let jobs = Arc::new(PgJobs::for_fixture(db.pool.clone()));
    let job = Job::new(
        "flow-reservation-test",
        Subject::new("custom", "resource-test"),
        "Billed work",
        "emp-owner",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
    );
    jobs.create_job_at(&job, now, &[]).await.expect("packet");
    let mut step = Step::new(job.id, "task", "Work", 1);
    step.spec_slug = Some("work".into());
    step.status = StepStatus::Ready;
    jobs.add_step_at(&step, now, &[]).await.expect("ready step");
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
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let app = router(JobsApiState {
        kind_registry: Some(workflows),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    let response = TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", job.id, step.id))
        .as_user("emp-resource-test", "platform-admin")
        .send(&app)
        .await;
    response.assert_status(StatusCode::CONFLICT);
    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Ready, "refusal cannot claim");
    assert!(after.assignee_id.is_none());
    assert!(
        after.metadata.to_string().contains("capacity-unknown"),
        "refusal remains on the packet: {}",
        after.metadata
    );
}

// A SLOT MEANS A RUN IS WORKING (backlog af5dd547, allocation order item 2;
// decided on design 012822ea and conductor decisions 20261106_1507Z (3)).
// One `agent-run` row serves every model, so its `building` bill cannot
// name one class: it says `model = "from-run"`, and the claim door reads
// the class off the billed packet's own `model` through the one active
// `robots:<class>` pool whose `models` lists it. Measured on main
// 18366f8f: a bill is reserved only at ready -> active, so a nomination
// holds nothing, and a backlog `build` claim — active until its car closes
// and is proven, days — must never carry the bill. The fixtures below stand
// in for both rows; the live agent-run row is untouched (item 5 publishes
// its bill with David present).

/// The run protocol (a `building` billed from the run) and the item
/// protocol (a `build` that bills nothing, as backlog-item's does), with
/// two robot pools of one slot each.
async fn seed_run_fixture<J: JobsRepository>(f: &Fixture<J>) -> boss_core::publisher::EventStamp {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    for (kind, slug, bill) in [
        (
            "run-fixture",
            "building",
            Some(json!({"model": "from-run"})),
        ),
        ("item-fixture", "build", None),
    ] {
        let mut work =
            json!({"title":slug,"kind":"task","duration_hours":2,"ready_when":"steps.start.done"});
        if let Some(bill) = bill {
            work["resources"] = bill;
        }
        let spec: WorkflowSpec = serde_json::from_value(json!({"kind":kind,"label":kind,"category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
            "steps":[{"title":"start","kind":"trigger","ready_when":"true"},work,{"title":"done","kind":"outcome","ready_when":format!("steps.{slug}.done"),"terminal":{"outcome":"done"}}]})).unwrap();
        f.workflows
            .publish_authored(spec, JobId::new(), stamp.actor(), stamp.timestamp)
            .await
            .unwrap();
    }
    for (class, models) in [("opus", ["opus-5-5"]), ("sol", ["gpt-6.1-sol"])] {
        f.pools.publish_declared(serde_json::from_value(json!({"name":format!("robots:{class}"),"version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"1"},"models":models,"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    }
    stamp
}

/// A packet of `kind` whose job metadata is `metadata`, with its `slug`
/// step ready and nominated to `nominee` (or to nobody).
async fn nominated<J: JobsRepository>(
    f: &Fixture<J>,
    stamp: &boss_core::publisher::EventStamp,
    kind: &str,
    slug: &str,
    metadata: serde_json::Value,
    nominee: Option<&str>,
) -> Step {
    let mut job = Job::new(
        kind,
        Subject::new("custom", kind),
        "Slot test",
        "emp-owner",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    job.metadata = metadata;
    f.jobs
        .create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
        )
        .await
        .unwrap();
    let mut step = Step::new(job.id, "task", slug, 1);
    step.spec_slug = Some(slug.into());
    step.status = StepStatus::Ready;
    step.assignee_id = nominee.map(str::to_string);
    f.jobs
        .add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
        )
        .await
        .unwrap();
    step
}

fn claim_router<J: JobsRepository + 'static>(f: &Fixture<J>) -> axum::Router {
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
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    router(JobsApiState {
        kind_registry: Some(f.workflows.clone()),
        ..JobsApiState::minimal(
            f.jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

async fn claim_as(app: &axum::Router, step: &Step, who: &str) -> boss_testing::TestResponse {
    TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", step.job_id, step.id))
        .as_user(who, "platform-admin")
        .send(app)
        .await
}

async fn a_run_bill_takes_its_model_slot_from_the_runs_own_model<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_run_fixture(f).await;
    let app = claim_router(f);
    // One row, two models: each run takes the slot its OWN model names.
    for (model, pool) in [("opus-5-5", "robots:opus"), ("gpt-6.1-sol", "robots:sol")] {
        let run = nominated(
            f,
            &stamp,
            "run-fixture",
            "building",
            json!({"model": model}),
            None,
        )
        .await;
        claim_as(&app, &run, "emp-runner")
            .await
            .assert_status(StatusCode::OK);
        let active = f.jobs.get_step(&run.id).await.unwrap().unwrap();
        assert_eq!(active.status, StepStatus::Active);
        let token = &active.metadata["resource_claim"];
        assert_eq!(
            token["allocations"],
            json!([{"pool": pool, "version": 1, "amount": "1"}]),
            "a {model} run holds one slot of the class its pool declares: {token}"
        );
        assert_eq!(
            token["bill"]["model"],
            json!(pool.trim_start_matches("robots:")),
            "the claim records the class it resolved, never the unresolved spelling"
        );
    }
    // A model no pool declares names no class, and none is guessed from
    // its spelling.
    let unknown = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "opus-9-9"}),
        None,
    )
    .await;
    claim_as(&app, &unknown, "emp-runner")
        .await
        .assert_status(StatusCode::CONFLICT);
    let waiting = f.jobs.get_step(&unknown.id).await.unwrap().unwrap();
    assert_eq!(waiting.status, StepStatus::Ready);
    assert_eq!(
        waiting.metadata["waiting_on"]["reason"], "run-model-has-no-class",
        "{}",
        waiting.metadata
    );
}

async fn a_run_that_declares_no_model_is_refused_by_name_and_reserves_nothing<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_run_fixture(f).await;
    let app = claim_router(f);
    for metadata in [json!({}), json!({"model": ""}), json!({"model": 5})] {
        let run = nominated(f, &stamp, "run-fixture", "building", metadata, None).await;
        let refused = claim_as(&app, &run, "emp-runner").await;
        refused.assert_status(StatusCode::CONFLICT);
        let after = f.jobs.get_step(&run.id).await.unwrap().unwrap();
        assert_eq!(after.status, StepStatus::Ready, "a refusal claims nothing");
        assert!(after.assignee_id.is_none());
        assert!(after.metadata.get("resource_claim").is_none());
        assert_eq!(after.metadata["waiting_on"]["resource"], "robots:from-run");
        assert_eq!(
            after.metadata["waiting_on"]["reason"], "run-declares-no-model",
            "{}",
            after.metadata
        );
    }
    // Nothing was reserved: a run that declares its model still takes the
    // only opus slot.
    let run = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "opus-5-5"}),
        None,
    )
    .await;
    claim_as(&app, &run, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
}

/// Backlog 3901fc49, the paired suffixes through both adapters. Review run
/// dd37e7af published `model-slot:bad_class` listing
/// `fixture-exact-model`, and a from-run claim on a run of that model
/// answered HTTP 500 on both: the pool's class passed the pool and failed
/// the bill. Now the pool refuses at publish, by the one grammar, so the
/// run's model meters under no class and the claim is a named refusal that
/// reserves nothing, while its normal twin publishes and claims.
async fn a_robot_pool_whose_class_no_bill_carries_never_publishes<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_run_fixture(f).await;
    let app = claim_router(f);
    let pool = |class: &str| -> PoolSpec {
        serde_json::from_value(json!({"name":format!("robots:{class}"),"version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"1"},"models":["fixture-exact-model"],"created_at":stamp.timestamp})).unwrap()
    };
    let refused = f
        .pools
        .publish_declared(pool("bad_class"), stamp.actor(), stamp.timestamp)
        .await
        .expect_err("a class no bill can carry must not publish");
    assert!(
        matches!(refused, boss_jobs::pools::PoolError::Invalid(_)),
        "{refused:?}"
    );
    assert!(
        f.pools
            .list_active()
            .await
            .unwrap()
            .iter()
            .all(|p| p.name != "robots:bad_class"),
        "the refused row recorded nothing"
    );
    let run = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "fixture-exact-model"}),
        None,
    )
    .await;
    claim_as(&app, &run, "emp-runner")
        .await
        .assert_status(StatusCode::CONFLICT);
    let after = f.jobs.get_step(&run.id).await.unwrap().unwrap();
    assert_eq!(after.status, StepStatus::Ready, "a refusal claims nothing");
    assert!(after.metadata.get("resource_claim").is_none());
    assert_eq!(
        after.metadata["waiting_on"]["reason"], "run-model-has-no-class",
        "{}",
        after.metadata
    );
    // The normal twin: the same row with a class a bill carries publishes,
    // and the same run then takes its slot.
    f.pools
        .publish_declared(pool("fixture"), stamp.actor(), stamp.timestamp)
        .await
        .expect("a class a bill carries publishes");
    claim_as(&app, &run, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
}

async fn a_nomination_and_a_backlog_build_claim_hold_no_model_slot<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_run_fixture(f).await;
    let app = claim_router(f);
    // An item's build, CLAIMED and active — the shape that stays active
    // until its car closes and is proven — carries no bill and no token.
    let build = nominated(f, &stamp, "item-fixture", "build", json!({}), None).await;
    claim_as(&app, &build, "emp-builder")
        .await
        .assert_status(StatusCode::OK);
    let held = f.jobs.get_step(&build.id).await.unwrap().unwrap();
    assert_eq!(held.status, StepStatus::Active);
    assert!(
        held.metadata.get("resource_claim").is_none(),
        "a backlog build claim reserves nothing: {}",
        held.metadata
    );
    // Two opus runs NOMINATED, neither claimed: the one slot is still free.
    let first = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "opus-5-5"}),
        Some("emp-first"),
    )
    .await;
    let second = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "opus-5-5"}),
        Some("emp-second"),
    )
    .await;
    claim_as(&app, &first, "emp-first")
        .await
        .assert_status(StatusCode::OK);
    // Only the ACTIVE run holds it: the second nominee is refused on the pool.
    claim_as(&app, &second, "emp-second")
        .await
        .assert_status(StatusCode::CONFLICT);
    let waiting = f.jobs.get_step(&second.id).await.unwrap().unwrap();
    assert_eq!(waiting.status, StepStatus::Ready);
    assert_eq!(waiting.assignee_id.as_deref(), Some("emp-second"));
    assert_eq!(waiting.metadata["waiting_on"]["resource"], "robots:opus");
    assert_eq!(
        waiting.metadata["waiting_on"]["reason"],
        "capacity-exhausted"
    );
}

/// The agents rows both adapters carry (backlog 7750d37d): each robot's
/// recorded `model_class`, and one that records none.
const CLAIMANTS: [(&str, Option<&str>); 3] = [
    ("agent-codex", Some("sol")),
    ("agent-claude", Some("opus")),
    ("agent-unclassed", None),
];

// THE SLOT IS THE EXECUTOR'S CLASS (backlog 7750d37d). Measured 2026-11-08
// 07:20Z on GET /api/claims: agent-codex (agents row model_class sol) held
// two robots:opus slots for reviewer runs whose step declared opus-5-5,
// while robots:sol sat idle — the from-run bill charged the model the run
// DECLARED, not the class of the robot that claimed it. The claimant's
// recorded class decides; the run's declared model is read only for a
// claimant that records none.
async fn a_run_bill_takes_the_claimants_class_before_the_runs_model<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = seed_run_fixture(f).await;
    let app = claim_router(f);
    // Both runs declare opus-5-5; each holds the slot of its claimant's class.
    for (claimant, pool) in [
        ("agent-codex", "robots:sol"),
        ("agent-claude", "robots:opus"),
    ] {
        let run = nominated(
            f,
            &stamp,
            "run-fixture",
            "building",
            json!({"model": "opus-5-5"}),
            None,
        )
        .await;
        claim_as(&app, &run, claimant)
            .await
            .assert_status(StatusCode::OK);
        let active = f.jobs.get_step(&run.id).await.unwrap().unwrap();
        let token = &active.metadata["resource_claim"];
        assert_eq!(
            token["allocations"],
            json!([{"pool": pool, "version": 1, "amount": "1"}]),
            "{claimant} holds a slot of its own class, not the declared model's: {token}"
        );
        assert_eq!(
            token["bill"]["model"],
            json!(pool.trim_start_matches("robots:")),
            "the claim records the claimant's class"
        );
    }
    // A claimant that records no class falls back to the declared model:
    // robots:opus, whose one slot agent-claude now holds.
    let run = nominated(
        f,
        &stamp,
        "run-fixture",
        "building",
        json!({"model": "opus-5-5"}),
        None,
    )
    .await;
    claim_as(&app, &run, "agent-unclassed")
        .await
        .assert_status(StatusCode::CONFLICT);
    let waiting = f.jobs.get_step(&run.id).await.unwrap().unwrap();
    assert_eq!(waiting.metadata["waiting_on"]["resource"], "robots:opus");
    assert_eq!(
        waiting.metadata["waiting_on"]["reason"],
        "capacity-exhausted"
    );
}

// THE RUN'S vCPU IS DRAWN FROM A HOST (backlog 5608c1a2). Measured
// 2026-11-08 10:05-10:42Z: a Codex reviewer's worker waited 300 s on the
// scheduler for w-1 while robots:opus held 4 of 6 — the slot admitted a
// run no box had room for. The fixture tree is Algedonic's numbers:
// engineering holds 28 of host:w-1 and its gates reserve 12, so 16 float
// for the runs of a protocol engineering owns, at 2.5 a worker.
const ENGINEERING: &str = "4c5ddf6c-b8ed-4776-98d9-2f99553317c5";

fn run_stations() -> Vec<boss_jobs::stations::StationSpec> {
    let row = |name: &str, id: &str, parent: Option<&str>, allocation: serde_json::Value| {
        serde_json::from_value(
            json!({"name":name,"version":1,"status":"active","title":name,
            "kind":"group","predicate":{"tags_any":[format!("station:{name}")]},"id":id,
            "parent":parent,"mode":"standing","allocation":allocation,
            "created_at":"2026-11-08T00:00:00Z"}),
        )
        .unwrap()
    };
    vec![
        row(
            "algedonic",
            "00b04483-90c6-46b9-b911-8e19d2a0b410",
            None,
            json!({}),
        ),
        row(
            "engineering",
            ENGINEERING,
            Some("00b04483-90c6-46b9-b911-8e19d2a0b410"),
            json!({"host:w-1":"28"}),
        ),
        row(
            "gates",
            "a426acd5-4c77-4ec9-9686-c96cd59b53d7",
            Some(ENGINEERING),
            json!({"host:w-1":"12"}),
        ),
    ]
}

async fn a_run_draws_its_vcpu_from_its_owners_floating_host_share<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    let spec: WorkflowSpec = serde_json::from_value(json!({"kind":"host-run","label":"host-run","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "metadata":{"owner":ENGINEERING},
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},
            {"title":"building","kind":"task","duration_hours":2,"ready_when":"steps.start.done","resources":{"model":"from-run","vcpu":"2.5"}},
            {"title":"done","kind":"outcome","ready_when":"steps.building.done","terminal":{"outcome":"done"}}]})).unwrap();
    f.workflows
        .publish_authored(spec, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    for pool in [
        json!({"name":"robots:opus","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"10"},"models":["opus-5-5"],"created_at":stamp.timestamp}),
        json!({"name":"host:w-1","version":1,"status":"active","unit":"vCPU","kind":"flow","shape":"capacity","capacity":{"state":"configured","amount":"32"},"created_at":stamp.timestamp}),
    ] {
        f.pools
            .publish_declared(
                serde_json::from_value(pool).unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }
    let app = claim_router(f);
    let run = || {
        nominated(
            f,
            &stamp,
            "host-run",
            "building",
            json!({"model": "opus-5-5"}),
            None,
        )
    };
    // Six workers fit the 16 floating (15 held); the box's 32 is not the bound.
    let mut held = Vec::new();
    for _ in 0..6 {
        let step = run().await;
        claim_as(&app, &step, "emp-runner")
            .await
            .assert_status(StatusCode::OK);
        held.push(f.jobs.get_step(&step.id).await.unwrap().unwrap());
    }
    let token = &held[0].metadata["resource_claim"];
    assert_eq!(
        token["allocations"],
        json!([{"pool":"host:w-1","version":1,"amount":"2.5"},{"pool":"robots:opus","version":1,"amount":"1"}]),
        "the slot and the host vCPU are one claim: {token}"
    );
    assert_eq!(
        token["bill"]["host"],
        json!({"node":"w-1","station":"engineering"}),
        "the record names the box and the station whose share it drew: {token}"
    );
    // The seventh is refused at the claim door, on the HOST pool, by name,
    // though robots:opus has four slots free.
    let seventh = run().await;
    let refused = claim_as(&app, &seventh, "emp-runner").await;
    refused.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = refused.assert_json();
    assert_eq!(body["waiting_on"]["pool"], "host:w-1", "{body}");
    assert_eq!(
        body["why"],
        "host:w-1 exhausted: 28 allocated to engineering, 12 to gates, 16 floating; \
         15 held by running claims, this claim asks 2.5",
        "{body}"
    );
    let waiting = f.jobs.get_step(&seventh.id).await.unwrap().unwrap();
    assert_eq!(waiting.status, StepStatus::Ready);
    assert_eq!(waiting.metadata["waiting_on"]["resource"], "host:w-1");
    assert_eq!(
        waiting.metadata["waiting_on"]["reason"],
        "capacity-exhausted"
    );
    // An exit frees the host vCPU with the slot: the seventh then fits.
    let first = &held[0];
    TestRequest::put(format!("/api/jobs/{}/steps/{}", first.job_id, first.id))
        .as_user("emp-runner", "platform-admin")
        .json(&json!({"status":"completed","resource_claim_id":token["id"]}))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    claim_as(&app, &seventh, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
}

// THE SLOT EXITS AT THE HAND-OFF (backlog 87d1ae40; David 2026-11-08: "A
// station never needs to worry about what happens next ... so that
// resources can be reallocated"). Measured that evening: robots:opus 6 of 6
// held while one robot was building, and a dispatch refused 'exhausted'.
// Through the claim door and the step doors, on the agent-run row the tree
// ships — whose bill is a robots:<class> slot AND 2.5 vCPU of a host
// (backlog 5608c1a2) — six runs fill BOTH pools at once: six opus slots,
// and 15 of engineering's 16 floating on host:w-1. Run B is refused. Then
// run A takes the one write `boss gate` sends at the accepted submission,
// a conditional completion carrying `result = handed`, and B is admitted
// on what A gave up — which it can be only if A's exit freed its host
// vCPU WITH its slot, since 15 held + B's 2.5 is past the 16 floating.
async fn a_run_handed_to_its_gate_frees_its_model_slot_for_the_next_dispatch<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    let mut protocol = boss_jobs::seed_loader::load_workflows(
        boss_testing::repo_root().join("infra/platform/workflows/agent-run.toml"),
    )
    .unwrap()
    .remove(0);
    protocol.version = 1;
    // `building` as the row materializes it: its completion contract
    // (`result`, an enum) rides the step and is judged at done.
    let contract = protocol
        .steps
        .iter()
        .find(|s| s.title == "building")
        .expect("agent-run has a building step")
        .fields
        .clone();
    f.workflows
        .publish_authored(protocol, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    for pool in [
        json!({"name":"robots:opus","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"6"},"models":["opus-5-5"],"created_at":stamp.timestamp}),
        json!({"name":"host:w-1","version":1,"status":"active","unit":"vCPU","kind":"flow","shape":"capacity","capacity":{"state":"configured","amount":"32"},"created_at":stamp.timestamp}),
    ] {
        f.pools
            .publish_declared(
                serde_json::from_value(pool).unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }
    let app = claim_router(f);
    let run = json!({"packet": "5b1d2c3e-0000-4000-8000-0000000000aa", "step": "build",
        "agent": "emp-runner", "model": "opus-5-5", "budget_usd": 5.0, "effort": "high"});
    let mut a = nominated(f, &stamp, "agent-run", "building", run.clone(), None).await;
    a.fields = contract;
    f.jobs
        .update_step_at(
            &a,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_UPDATED, json!(a))],
        )
        .await
        .unwrap();
    let b = nominated(f, &stamp, "agent-run", "building", run.clone(), None).await;
    claim_as(&app, &a, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
    // The id the run read when it claimed: the one its exit must name.
    let claimed = f.jobs.get_step(&a.id).await.unwrap().unwrap();
    let claim_id = claimed.metadata["resource_claim"]["id"].clone();
    assert!(claim_id.is_string(), "{}", claimed.metadata);
    let both = json!([{"pool": "host:w-1", "version": 1, "amount": "2.5"},
        {"pool": "robots:opus", "version": 1, "amount": "1"}]);
    assert_eq!(
        claimed.metadata["resource_claim"]["allocations"], both,
        "A's one claim holds the slot and the host vCPU: {}",
        claimed.metadata
    );
    // Five more fill both pools: 6 of 6 slots, 15 of the 16 floating.
    for _ in 0..5 {
        let other = nominated(f, &stamp, "agent-run", "building", run.clone(), None).await;
        claim_as(&app, &other, "emp-runner")
            .await
            .assert_status(StatusCode::OK);
    }
    claim_as(&app, &b, "emp-runner")
        .await
        .assert_status(StatusCode::CONFLICT);

    // ONE write (review R1 of car d6f24cc7): the evidence, the completion
    // and the claim's exit ride a conditional completion judged on the
    // versioned read, so no verdict can land between a stamped and a
    // completed `building`.
    let path = format!("/api/jobs/{}/steps/{}", a.job_id, a.id);
    let read = TestRequest::get(format!("{path}/version"))
        .as_user("emp-runner", "platform-admin")
        .send(&app)
        .await;
    read.assert_status(StatusCode::OK);
    let read: serde_json::Value = serde_json::from_slice(&read.body_bytes).unwrap();
    assert_eq!(read["step"]["metadata"]["resource_claim"]["id"], claim_id);
    let completed = TestRequest::post(format!("{path}/complete-if"))
        .as_user("emp-runner", "platform-admin")
        .json(&json!({"operation_id": uuid::Uuid::new_v4(),
            "expected": {"status": "active", "assignee_id": read["step"]["assignee_id"],
                "version": read["version"]},
            "evidence": {"result": "handed", "gate_run": {"id": "gate", "branch": "fix/x"}}}))
        .send(&app)
        .await;
    assert!(completed.status.is_success(), "{}", completed.body_text());
    let handed = f.jobs.get_step(&a.id).await.unwrap().unwrap();
    assert_eq!(handed.status, StepStatus::Completed);
    assert_eq!(handed.metadata["result"], "handed");

    // Both pools' in-use dropped at the one write — a slot, and 2.5 vCPU
    // of w-1 — so the next dispatch is admitted holding both.
    claim_as(&app, &b, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
    let admitted = f.jobs.get_step(&b.id).await.unwrap().unwrap();
    assert_eq!(admitted.status, StepStatus::Active);
    assert_eq!(admitted.metadata["resource_claim"]["allocations"], both);
}

// AN ANALYST RUN BINDS NO WORKER, SO IT DRAWS NO HOST vCPU (backlog
// db88f6a9). Measured 2026-11-12 15:08Z on GET /api/claims: host:w-1 was
// the binding pool, six claims holding 2.5 each = 15 of engineering's 16
// floating, and two of the six were triage analyst runs — runs that bind
// no dev-build worker and run no cargo — while the build run of 39d806c0
// was refused 'pool host:w-1 is exhausted'. On the agent-run row the tree
// ships: six builder runs fill the floating share and a seventh builder
// is refused on host:w-1, yet an analyst run is admitted holding its
// model slot ALONE, and its claim names no host. A reviewer still draws:
// a source review binds a worker for its candidate checks
// (reviewer-rules.md), so only a profile that never binds one is waived.
async fn a_run_whose_profile_binds_no_worker_draws_no_host_vcpu<J: JobsRepository + 'static>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    let mut protocol = boss_jobs::seed_loader::load_workflows(
        boss_testing::repo_root().join("infra/platform/workflows/agent-run.toml"),
    )
    .unwrap()
    .remove(0);
    protocol.version = 1;
    f.workflows
        .publish_authored(protocol, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    for pool in [
        json!({"name":"robots:opus","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"10"},"models":["opus-5-5"],"created_at":stamp.timestamp}),
        json!({"name":"host:w-1","version":1,"status":"active","unit":"vCPU","kind":"flow","shape":"capacity","capacity":{"state":"configured","amount":"32"},"created_at":stamp.timestamp}),
    ] {
        f.pools
            .publish_declared(
                serde_json::from_value(pool).unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }
    let app = claim_router(f);
    let run = |profile: &str| {
        nominated(
            f,
            &stamp,
            "agent-run",
            "building",
            json!({"packet": "5b1d2c3e-0000-4000-8000-0000000000bb", "step": "build",
                "agent": "emp-runner", "profile": profile, "model": "opus-5-5",
                "budget_usd": 5.0, "effort": "high"}),
            None,
        )
    };
    // Six builders: 15 of the 16 floating on w-1.
    for _ in 0..6 {
        let builder = run("builder").await;
        claim_as(&app, &builder, "emp-runner")
            .await
            .assert_status(StatusCode::OK);
    }
    let seventh = run("builder").await;
    let refused = claim_as(&app, &seventh, "emp-runner").await;
    refused.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = refused.assert_json();
    assert_eq!(body["waiting_on"]["pool"], "host:w-1", "{body}");
    // A reviewer may bind a worker, so it is refused on the host too.
    let reviewer = run("reviewer").await;
    let refused = claim_as(&app, &reviewer, "emp-runner").await;
    refused.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = refused.assert_json();
    assert_eq!(body["waiting_on"]["pool"], "host:w-1", "{body}");
    // The analyst is admitted on its model slot alone.
    let analyst = run("analyst").await;
    claim_as(&app, &analyst, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
    let held = f.jobs.get_step(&analyst.id).await.unwrap().unwrap();
    let token = &held.metadata["resource_claim"];
    assert_eq!(
        token["allocations"],
        json!([{"pool":"robots:opus","version":1,"amount":"1"}]),
        "an analyst holds its slot and no host vCPU: {token}"
    );
    assert_eq!(
        token["bill"],
        json!({"model":"opus"}),
        "the recorded bill names no vCPU and no host, and carries no waiver list: {token}"
    );
}

// READERS GO FIRST (backlog 30c1efe8; review 6fdc1391 P1). A claim token
// is durable: release, identity binding, expiry and replay each decode it
// from the record, so a token one replica writes must be readable by
// every replica still running during a rolling deploy, and after a
// rollback. The reader before version 3 is frozen here exactly as main
// declared it at e11ff0d9 (crates/core/boss-jobs/src/flow_resources.rs):
// `Claim`, `Demand`, `Share` and `Allocation`, each refusing an unknown
// field, behind the version guard `1 | 2`. The token the claim door
// persists — the richest shape it writes, a robot slot beside a host
// vCPU drawn on its owner's share — must decode under that reader and
// re-serialize to the same JSON, so nothing it carries was dropped.
mod before_version_three {
    use boss_jobs::flow_resources::{Amount, Capacity};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Claim {
        pub version: u32,
        pub id: uuid::Uuid,
        pub step_id: boss_core::job::StepId,
        pub job_id: boss_core::job::JobId,
        pub generation: i64,
        pub holder: String,
        pub workflow_kind: String,
        pub workflow_version: i32,
        pub bill: boss_jobs::resources::ResourceBill,
        pub allocations: Vec<Allocation>,
        pub observed: Vec<Demand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub resolved_duration_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lease_until: Option<chrono::DateTime<chrono::Utc>>,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Demand {
        pub pool: String,
        pub version: i32,
        pub amount: Amount,
        pub capacity: Capacity,
        pub held: Amount,
        pub stock: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub share: Option<Share>,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Share {
        pub station: String,
        pub allocated: Amount,
        pub reserved: BTreeMap<String, Amount>,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Allocation {
        pub pool: String,
        pub version: i32,
        pub amount: Amount,
    }

    /// What the reader before version 3 makes of `token`: refused where it
    /// would refuse, else the token as it reads it back.
    pub fn reads(token: &serde_json::Value) -> Result<serde_json::Value, String> {
        let claim: Claim = serde_json::from_value(token.clone()).map_err(|e| e.to_string())?;
        if !matches!(claim.version, 1 | 2) {
            return Err(format!("version {} is not 1 or 2", claim.version));
        }
        serde_json::to_value(&claim).map_err(|e| e.to_string())
    }
}

async fn a_claim_the_door_writes_is_read_by_the_reader_before_version_three<
    J: JobsRepository + 'static,
>(
    f: &Fixture<J>,
    _: &str,
) {
    let stamp = boss_core::publisher::EventStamp::new("jobs", ActorId::automation("flow-contract"));
    let spec: WorkflowSpec = serde_json::from_value(json!({"kind":"host-run","label":"host-run","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "metadata":{"owner":ENGINEERING},
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},
            {"title":"building","kind":"task","duration_hours":2,"ready_when":"steps.start.done","resources":{"model":"from-run","vcpu":"2.5"}},
            {"title":"done","kind":"outcome","ready_when":"steps.building.done","terminal":{"outcome":"done"}}]})).unwrap();
    f.workflows
        .publish_authored(spec, JobId::new(), stamp.actor(), stamp.timestamp)
        .await
        .unwrap();
    for pool in [
        json!({"name":"robots:opus","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{"state":"configured","amount":"10"},"models":["opus-5-5"],"created_at":stamp.timestamp}),
        json!({"name":"host:w-1","version":1,"status":"active","unit":"vCPU","kind":"flow","shape":"capacity","capacity":{"state":"configured","amount":"32"},"created_at":stamp.timestamp}),
    ] {
        f.pools
            .publish_declared(
                serde_json::from_value(pool).unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }
    let app = claim_router(f);
    let step = nominated(
        f,
        &stamp,
        "host-run",
        "building",
        json!({"model": "opus-5-5"}),
        None,
    )
    .await;
    claim_as(&app, &step, "emp-runner")
        .await
        .assert_status(StatusCode::OK);
    let held = f.jobs.get_step(&step.id).await.unwrap().unwrap();
    let token = &held.metadata["resource_claim"];
    assert_eq!(
        before_version_three::reads(token).as_ref(),
        Ok(token),
        "the reader before version 3 reads the token the door wrote, whole"
    );
    assert!(
        token["observed"]
            .as_array()
            .is_some_and(|d| d.iter().any(|d| d.get("share").is_some())),
        "the token carries the richest shape the door writes, a share: {token}"
    );
    // The reader this car ships reads it too.
    let claim: boss_jobs::flow_resources::Claim = serde_json::from_value(token.clone()).unwrap();
    claim.validate().unwrap();
}
