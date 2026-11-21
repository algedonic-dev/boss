//! A released pool re-offers the step that has waited longest in its
//! line (backlog 587f395a, allocation item 1 of design 012822ea) — end
//! to end, from the claim the pool refuses to the release that frees it,
//! the rule FILE matched against the release the jobs API recorded, and
//! the handler's own code writing through the real router.

//!
//! Before this car nothing read `jobs.step.released`: a claim refused
//! capacity-exhausted left `waiting_on` on its step and waited for
//! nobody. The store half (the position answer, the line, claim-next
//! passing over a step holding its place) is pinned on both adapters in
//! boss-jobs (`an_exhausted_pool_makes_a_claim_wait_in_its_queue`).

use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::handler::{Handler, InvocationContext};
use boss_dispatcher::rules::registry::{Registry, match_event};
use boss_dispatcher_handlers::handlers::jobs_reoffer_waiting::JobsReofferWaiting;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::pools::PoolRegistry;
use boss_jobs::registry::{WorkflowRegistry, WorkflowSpec};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, dispatcher_rules_dir};
use serde_json::json;
use std::sync::Arc;

const RULE: &str = "jobs-reoffer-waiting-on-release";

struct Floor {
    jobs: Arc<InMemoryJobs>,
    base: String,
    stamp: boss_core::publisher::EventStamp,
}

/// One pool of capacity 1 and a protocol whose `work` step bills it, the
/// in-memory jobs API served on a socket the handler can reach.
async fn floor() -> Floor {
    floor_with("1", None).await
}

async fn floor_with(capacity: &str, barrier: Option<Arc<tokio::sync::Barrier>>) -> Floor {
    floor_fault(capacity, barrier, Fault::None).await
}

/// Review R3 (Codex, run c143d322 at aaf8a8b6): one write of the
/// handler's interrupted, once. The account is the releasing packet's
/// metadata PATCH; the mark is a waiter's step metadata PATCH.
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// The account write answers 503 before the router runs: nothing lands.
    AccountBefore,
    /// The mark answers 503 before the router runs: nothing lands.
    MarkBefore,
    /// The mark lands through the router and its answer is lost (503).
    MarkAfter,
}

async fn floor_fault(
    capacity: &str,
    barrier: Option<Arc<tokio::sync::Barrier>>,
    fault: Fault,
) -> Floor {
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("reoffer-test"),
    );
    let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
    let workflows = Arc::new(boss_jobs::registry::InMemoryWorkflows::for_fixture());
    let spec: WorkflowSpec = serde_json::from_value(json!({"kind":"flow-contract","label":"Flow","category":"test","version":1,"status":"active","subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
        "steps":[{"title":"start","kind":"trigger","ready_when":"true"},{"title":"work","kind":"task","duration_hours":1,"ready_when":"steps.start.done","resources":{"vcpu":"1"}},{"title":"done","kind":"outcome","ready_when":"steps.work.done","terminal":{"outcome":"done"}}]})).unwrap();
    workflows
        .publish_authored(
            spec,
            boss_core::job::JobId::new(),
            stamp.actor(),
            stamp.timestamp,
        )
        .await
        .unwrap();
    pools.publish_declared(serde_json::from_value(json!({"name":"vcpu","version":1,"status":"active","unit":"vCPU","kind":"flow","capacity":{"state":"configured","amount":capacity},"created_at":stamp.timestamp})).unwrap(),stamp.actor(),stamp.timestamp).await.unwrap();
    let jobs = Arc::new(
        InMemoryJobs::new()
            .with_workflows(workflows.clone())
            .with_resource_pools(pools),
    );
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            // Review 4ed9395a R2: the release keeps its spent account on
            // its packet, written as the dispatcher writes packet metadata
            // in every other handler.
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        kind_registry: Some(workflows as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    // Review 4ed9395a R1: hold the FIRST two line reads until both have
    // read the same line, so two releases race for one waiter. Later
    // reads pass straight through: the release that loses reads again.
    let raced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(fault != Fault::None));
    let app = app.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let barrier = barrier.clone();
            let raced = raced.clone();
            let armed = armed.clone();
            async move {
                use axum::response::IntoResponse;
                let path = request.uri().path().to_string();
                let patch =
                    request.method() == axum::http::Method::PATCH && path.ends_with("/metadata");
                let hit = match fault {
                    Fault::None => false,
                    Fault::AccountBefore => patch && !path.contains("/steps/"),
                    Fault::MarkBefore | Fault::MarkAfter => patch && path.contains("/steps/"),
                };
                if hit && armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    if fault == Fault::MarkAfter {
                        let landed = next.run(request).await;
                        assert!(landed.status().is_success(), "the faulted mark lands");
                    }
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "review R3 fixture: this write's answer is refused",
                    )
                        .into_response();
                }
                let waiting = path == "/api/claims/waiting";
                let response = next.run(request).await;
                if waiting
                    && raced.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
                    && let Some(barrier) = barrier
                {
                    barrier.wait().await;
                }
                response
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Floor {
        jobs,
        base: format!("http://{addr}"),
        stamp,
    }
}

async fn ready_work(f: &Floor) -> boss_core::job::Step {
    use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
    let job = Job::new(
        "flow-contract",
        Subject::new("custom", "flow-contract"),
        "Reoffer test",
        "emp-owner",
        Priority::Standard,
        f.stamp.timestamp.date_naive(),
    );
    f.jobs
        .create_job_at(
            &job,
            f.stamp.timestamp,
            &[f.stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
        )
        .await
        .unwrap();
    let mut step = Step::new(job.id, "task", "Work", 1);
    step.spec_slug = Some("work".into());
    step.status = StepStatus::Ready;
    f.jobs
        .add_step_at(
            &step,
            f.stamp.timestamp,
            &[f.stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
        )
        .await
        .unwrap();
    step
}

/// A claim through the door every actor uses, signed as `who`.
async fn claim(f: &Floor, step: &boss_core::job::Step, who: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{}/api/jobs/{}/steps/{}/claim",
            f.base, step.job_id, step.id
        ))
        .header(
            "x-boss-user",
            json!({"id":who,"role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}).to_string(),
        )
        .send()
        .await
        .unwrap()
        .status()
}

async fn wait_of(f: &Floor, step: &boss_core::job::Step) -> serde_json::Value {
    f.jobs.get_step(&step.id).await.unwrap().unwrap().metadata["waiting_on"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_released_pool_reoffers_its_oldest_waiting_step() {
    let f = floor().await;
    let held = ready_work(&f).await;
    let first = ready_work(&f).await;
    let second = ready_work(&f).await;
    assert_eq!(claim(&f, &held, "emp-one").await, reqwest::StatusCode::OK);
    assert_eq!(
        claim(&f, &first, "emp-two").await,
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        claim(&f, &second, "emp-three").await,
        reqwest::StatusCode::CONFLICT
    );

    // The held step completes, and its allocation is returned.
    let (mut done, read) = f.jobs.get_step_versioned(&held.id).await.unwrap().unwrap();
    done.status = boss_core::job::StepStatus::Completed;
    f.jobs
        .update_step_if_unchanged_at(
            &done,
            read,
            f.stamp.timestamp,
            &[f.stamp.event(boss_jobs::events::STEP_UPDATED, json!(done))],
        )
        .await
        .unwrap();
    let release = f
        .jobs
        .recorded_events()
        .into_iter()
        .find(|e| e.kind == boss_jobs::flow_resources::RELEASED)
        .expect("completing a claimed step records the release of its allocation");

    // The rule as the dispatcher boots it: its file, not a copy.
    let path = dispatcher_rules_dir().join(format!("{RULE}.toml"));
    let toml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read rule file {}: {e}", path.display()));
    let registry = Registry::from_toml(&toml).expect("the rule file parses");
    let outcome = match_event(
        &registry,
        boss_jobs::flow_resources::RELEASED,
        &release.payload,
        &NoHelpers,
    );
    assert_eq!(outcome.matched.len(), 1, "the rule hears every release");
    let invocation = &outcome.matched[0].invocations[0];
    assert_eq!(invocation.handler, "jobs.reoffer_waiting");

    let handler = JobsReofferWaiting::with_client(
        boss_dispatcher_handlers::handlers::common::api_client(),
        f.base.clone(),
    );
    let ctx = InvocationContext {
        event_timestamp: Some(release.timestamp),
        rule_name: RULE.into(),
        triggering_event_id: release.id.to_string(),
        triggering_topic: boss_jobs::flow_resources::RELEASED.into(),
        event_payload: release.payload.clone(),
    };
    handler
        .invoke(&invocation.args, &ctx)
        .await
        .expect("the re-offer");

    let offered = wait_of(&f, &first).await;
    assert_eq!(
        offered["reoffered_by"],
        json!(release.id.to_string()),
        "the step that waited longest is re-offered, naming the release that freed it: {offered}"
    );
    assert!(offered["reoffered_at"].is_string(), "{offered}");
    assert_eq!(
        offered["reason"], "capacity-exhausted",
        "its wait is kept: {offered}"
    );
    assert!(
        wait_of(&f, &second).await.get("reoffered_at").is_none(),
        "one release re-offers one waiter"
    );
    assert_eq!(
        f.jobs.get_step(&first.id).await.unwrap().unwrap().status,
        boss_core::job::StepStatus::Ready,
        "a re-offer claims nothing"
    );

    // A redelivered release offers nobody else.
    handler
        .invoke(&invocation.args, &ctx)
        .await
        .expect("the redelivery");
    assert!(
        wait_of(&f, &second).await.get("reoffered_at").is_none(),
        "a redelivery finds its own mark and writes nothing"
    );

    // The re-offered step's claim takes the freed unit, and its wait ends.
    assert_eq!(claim(&f, &first, "emp-two").await, reqwest::StatusCode::OK);
    assert!(
        wait_of(&f, &first).await.is_null(),
        "a claim that reserves ends the wait"
    );

    // The same durable release can be redelivered after its offered step claims.
    handler
        .invoke(&invocation.args, &ctx)
        .await
        .expect("redelivery after consumption");
    let second_wait = wait_of(&f, &second).await;
    eprintln!("REVIEW_AFTER_CONSUMED_REPLAY second={second_wait}");
    assert!(
        second_wait.get("reoffered_at").is_none(),
        "the same release must not offer a second waiter after its first offer is consumed: {second_wait}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_distinct_releases_racing_for_one_waiter_reoffer_two_waiters() {
    let f = floor_with("2", Some(Arc::new(tokio::sync::Barrier::new(2)))).await;
    let held_a = ready_work(&f).await;
    let held_b = ready_work(&f).await;
    let first = ready_work(&f).await;
    let second = ready_work(&f).await;
    assert_eq!(claim(&f, &held_a, "emp-one").await, reqwest::StatusCode::OK);
    assert_eq!(claim(&f, &held_b, "emp-two").await, reqwest::StatusCode::OK);
    assert_eq!(
        claim(&f, &first, "emp-three").await,
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        claim(&f, &second, "emp-four").await,
        reqwest::StatusCode::CONFLICT
    );
    for held in [&held_a, &held_b] {
        let (mut done, read) = f.jobs.get_step_versioned(&held.id).await.unwrap().unwrap();
        done.status = boss_core::job::StepStatus::Completed;
        f.jobs
            .update_step_if_unchanged_at(
                &done,
                read,
                f.stamp.timestamp,
                &[f.stamp.event(boss_jobs::events::STEP_UPDATED, json!(done))],
            )
            .await
            .unwrap();
    }
    let releases: Vec<_> = f
        .jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::flow_resources::RELEASED)
        .collect();
    assert_eq!(releases.len(), 2);
    let context = |e: &boss_core::event::Event| InvocationContext {
        event_timestamp: Some(e.timestamp),
        rule_name: RULE.into(),
        triggering_event_id: e.id.to_string(),
        triggering_topic: boss_jobs::flow_resources::RELEASED.into(),
        event_payload: e.payload.clone(),
    };
    let a = context(&releases[0]);
    let b = context(&releases[1]);
    let handler = JobsReofferWaiting::with_client(
        boss_dispatcher_handlers::handlers::common::api_client(),
        f.base.clone(),
    );
    let args = [];
    // Hold each real-router GET response until both have read the same line.
    let outcomes = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(handler.invoke(&args, &a), handler.invoke(&args, &b))
    })
    .await
    .expect("bounded concurrent invocation");
    assert!(outcomes.0.is_ok() && outcomes.1.is_ok(), "{outcomes:?}");
    let wa = wait_of(&f, &first).await;
    let wb = wait_of(&f, &second).await;
    eprintln!("REVIEW_TWO_RELEASES first={wa} second={wb} outcomes={outcomes:?}");
    assert!(
        wa.get("reoffered_at").is_some() && wb.get("reoffered_at").is_some(),
        "two distinct recorded releases must not acknowledge only one offer: first={wa} second={wb}"
    );
}

/// A capacity-1 floor with one holder and two refused waiters, the
/// holder completed: the one recorded release, and the handler and the
/// context that hear it.
async fn one_release(
    fault: Fault,
) -> (
    Floor,
    boss_core::job::Step,
    boss_core::job::Step,
    Arc<JobsReofferWaiting>,
    InvocationContext,
) {
    let f = floor_fault("1", None, fault).await;
    let held = ready_work(&f).await;
    let first = ready_work(&f).await;
    let second = ready_work(&f).await;
    assert_eq!(claim(&f, &held, "emp-one").await, reqwest::StatusCode::OK);
    assert_eq!(
        claim(&f, &first, "emp-two").await,
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        claim(&f, &second, "emp-three").await,
        reqwest::StatusCode::CONFLICT
    );
    let (mut done, read) = f.jobs.get_step_versioned(&held.id).await.unwrap().unwrap();
    done.status = boss_core::job::StepStatus::Completed;
    f.jobs
        .update_step_if_unchanged_at(
            &done,
            read,
            f.stamp.timestamp,
            &[f.stamp.event(boss_jobs::events::STEP_UPDATED, json!(done))],
        )
        .await
        .unwrap();
    let release = f
        .jobs
        .recorded_events()
        .into_iter()
        .find(|e| e.kind == boss_jobs::flow_resources::RELEASED)
        .expect("completing a claimed step records the release");
    let handler = JobsReofferWaiting::with_client(
        boss_dispatcher_handlers::handlers::common::api_client(),
        f.base.clone(),
    );
    let ctx = InvocationContext {
        event_timestamp: Some(release.timestamp),
        rule_name: RULE.into(),
        triggering_event_id: release.id.to_string(),
        triggering_topic: boss_jobs::flow_resources::RELEASED.into(),
        event_payload: release.payload.clone(),
    };
    (f, first, second, handler, ctx)
}

fn offered_by(wait: &serde_json::Value, release: &str) -> bool {
    wait.get("reoffered_by") == Some(&json!(release))
}

/// Review R3 (Codex, run c143d322): the mark lands and its answer is
/// lost, the offered waiter claims, and the SAME release is redelivered.
/// The offer was spent; the replay must not offer the second waiter.
#[tokio::test(flavor = "multi_thread")]
async fn a_mark_whose_answer_is_lost_spends_its_release_once() {
    let (f, first, second, handler, ctx) = one_release(Fault::MarkAfter).await;
    let release = ctx.triggering_event_id.clone();
    handler
        .invoke(&[], &ctx)
        .await
        .expect_err("the mark's answer was lost, so the release is redelivered");
    assert!(
        offered_by(&wait_of(&f, &first).await, &release),
        "the mark landed"
    );
    assert_eq!(claim(&f, &first, "emp-two").await, reqwest::StatusCode::OK);
    assert!(
        wait_of(&f, &first).await.is_null(),
        "its claim ended the wait"
    );
    handler.invoke(&[], &ctx).await.expect("the replay");
    let second_wait = wait_of(&f, &second).await;
    assert!(
        second_wait.get("reoffered_at").is_none(),
        "one release offers one waiter, though its first offer's answer was lost: {second_wait}"
    );
}

/// Review R3, the other half Codex named: an account written before its
/// mark must not lose the wake when the mark never lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_mark_that_never_lands_is_offered_on_the_replay() {
    let (f, first, second, handler, ctx) = one_release(Fault::MarkBefore).await;
    let release = ctx.triggering_event_id.clone();
    handler
        .invoke(&[], &ctx)
        .await
        .expect_err("the mark was refused before it landed");
    assert!(
        wait_of(&f, &first).await.get("reoffered_at").is_none(),
        "nothing landed"
    );
    handler.invoke(&[], &ctx).await.expect("the replay");
    assert!(
        offered_by(&wait_of(&f, &first).await, &release),
        "the replay spends the wake the refused mark did not"
    );
    assert!(wait_of(&f, &second).await.get("reoffered_at").is_none());
}

/// Codex's case at aaf8a8b6, in this handler's order: the account write
/// refused leaves nothing offered, and the replay offers the oldest waiter
/// once; after that waiter claims, a second replay offers nobody.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_account_write_does_not_spend_the_same_release_twice() {
    let (f, first, second, handler, ctx) = one_release(Fault::AccountBefore).await;
    let release = ctx.triggering_event_id.clone();
    handler
        .invoke(&[], &ctx)
        .await
        .expect_err("the account write was refused before persistence");
    assert!(
        wait_of(&f, &first).await.get("reoffered_at").is_none(),
        "no offer is visible before its account exists"
    );
    handler.invoke(&[], &ctx).await.expect("the first replay");
    assert!(offered_by(&wait_of(&f, &first).await, &release));
    assert_eq!(claim(&f, &first, "emp-two").await, reqwest::StatusCode::OK);
    handler.invoke(&[], &ctx).await.expect("the second replay");
    let second_wait = wait_of(&f, &second).await;
    assert!(
        second_wait.get("reoffered_at").is_none(),
        "the same release must not offer a second waiter: {second_wait}"
    );
}
