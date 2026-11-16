//! THE HAND-OFF AND THE VERDICT, IN EVERY ORDER (review R1 of car
//! d6f24cc7, Codex run 15925e15; backlog 87d1ae40).
//!
//! `boss gate` ends a run's `building` as `handed` at the accepted
//! submission; the gate's verdict is delivered by
//! `agent-run-lands-on-gate-green` / `agent-run-is-refused-on-gate-red`,
//! whenever the gate closes. Nothing orders the two. The review found the
//! order that broke: a verdict delivered BETWEEN the hand-off's two
//! writes (merge `result = handed` + `gate_run`, then the exiting PUT)
//! read `building` as already stamped by its own gate and wrote nothing,
//! and the PUT then opened a `verdict` no event would ever complete.
//!
//! Here the jobs router, the login door and the agent-run row are the
//! shipped ones (in memory), the rules are read from
//! `infra/dispatcher/rules/`, the handler is the dispatcher's own, and
//! the hand-off is this crate's `exit_building`. A hook in front of the
//! router delivers the verdict around the hand-off's FIRST write to
//! `building` — before it lands, or after it lands and before its
//! answer returns — so whatever the hand-off writes next comes after
//! the verdict. Every order must leave the run at its verdict's terminal:
//! a green opens `reported` (the landing), a red closes the run refused.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use boss_dispatcher::rules::handler::{Handler, InvocationContext};
use boss_dispatcher_handlers::handlers::jobs_complete_linked_step::JobsCompleteLinkedStep;
use boss_jobs::pools::PoolRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use reqwest::Method;
use serde_json::{Value, json};

use crate::dispatch_report_orders::tests::rule_args;

const LOGIN: &str = "claude@algedonic.dev";
const AGENT: &str = "agent-claude";
const GREEN: &str = "agent-run-lands-on-gate-green";
const RED: &str = "agent-run-is-refused-on-gate-red";

/// When the verdict is delivered, relative to the hand-off's first write
/// to `building`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    /// Before the write reaches the router.
    Before,
    /// After the write landed, before its answer returns.
    After,
}

struct Stack {
    base: String,
    http: crate::machine_http::MachineClient,
    jobs: Arc<InMemoryJobs>,
}

impl Stack {
    async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Option<Value> {
        crate::sor_client::api_at_signed(
            &self.http,
            &self.base,
            method.clone(),
            path,
            body,
            crate::identity::Signature::As(LOGIN.to_string()),
        )
        .await
        .unwrap_or_else(|e| panic!("{method} {path}: {e:#}"))
    }

    async fn run(&self, id: &str) -> Value {
        self.api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await
            .expect("the run reads")
    }

    fn step<'a>(run: &'a Value, slug: &str) -> &'a Value {
        run["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .find(|s| s["spec_slug"] == json!(slug))
            .unwrap_or_else(|| panic!("the run has a `{slug}` step"))
    }
}

/// The verdict the hook delivers: the rule, and the closed gate-run it
/// fires on.
struct Verdict {
    rule: &'static str,
    gate: String,
}

/// Engineering's share of host:w-1 as Algedonic's tree holds it: 28 of
/// the box, 12 reserved by its gates, so 16 float for the runs of the
/// protocol it owns (agent-run's `metadata.owner`) — the vCPU half of a
/// run's bill (backlog 5608c1a2), which the hand-off frees with the slot.
fn engineering_on_w1() -> Vec<boss_jobs::stations::StationSpec> {
    const ROOT: &str = "00b04483-90c6-46b9-b911-8e19d2a0b410";
    const ENGINEERING: &str = "4c5ddf6c-b8ed-4776-98d9-2f99553317c5";
    let row = |name: &str, id: &str, parent: Option<&str>, allocation: Value| {
        serde_json::from_value(json!({"name": name, "version": 1, "status": "active",
            "title": name, "kind": "group",
            "predicate": {"tags_any": [format!("station:{name}")]}, "id": id,
            "parent": parent, "mode": "standing", "allocation": allocation,
            "created_at": "2026-11-08T00:00:00Z"}))
        .unwrap()
    };
    vec![
        row("algedonic", ROOT, None, json!({})),
        row(
            "engineering",
            ENGINEERING,
            Some(ROOT),
            json!({"host:w-1": "28"}),
        ),
        row(
            "gates",
            "a426acd5-4c77-4ec9-9686-c96cd59b53d7",
            Some(ENGINEERING),
            json!({"host:w-1": "12"}),
        ),
    ]
}

/// The shipped stack with a robots:opus pool of one slot (and the host
/// share its bill draws vCPU from), the hook armed
/// to deliver `verdict` around the first non-GET write to a path starting
/// with `armed_on` (set once the run exists).
#[allow(clippy::type_complexity)]
async fn serve(
    when: When,
) -> anyhow::Result<(
    Stack,
    Arc<std::sync::Mutex<Option<(String, Verdict)>>>,
    Arc<AtomicBool>,
)> {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for spec in boss_jobs::registry::seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::automation("hand-off-race"),
    );
    let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
    for pool in [
        json!({"name": "robots:opus", "version": 1,
            "status": "active", "unit": "slot", "kind": "flow",
            "capacity": {"state": "configured", "amount": "1"},
            "models": ["opus-5-5"], "created_at": stamp.timestamp}),
        json!({"name": "host:w-1", "version": 1, "status": "active", "unit": "vCPU",
            "kind": "flow", "shape": "capacity",
            "capacity": {"state": "configured", "amount": "32"},
            "created_at": stamp.timestamp}),
    ] {
        pools
            .publish_declared(
                serde_json::from_value(pool).unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
    }
    let jobs = Arc::new(
        InMemoryJobs::new()
            .with_workflows(kinds.clone() as Arc<dyn WorkflowRegistry>)
            .with_resource_pools(pools)
            .with_station_rows(engineering_on_w1()),
    );
    let policy: Arc<dyn PolicyClient> =
        Arc::new(FakePolicyClient::builder().with_default_rules().build());
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
    let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
    let state = boss_jobs::http::JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher.clone(),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new().with_agent(AGENT, [LOGIN]));
    let door = Arc::new(boss_jobs::agents::LoginDoor::new(agents.clone(), publisher));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    // The hook. Armed once: the first non-GET request to a path under
    // the armed prefix delivers the verdict, through the real handler,
    // around that request — and only that one.
    let target: Arc<std::sync::Mutex<Option<(String, Verdict)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let armed = Arc::new(AtomicBool::new(false));
    let (hook_target, hook_armed, hook_base) = (target.clone(), armed.clone(), base.clone());
    let hook = move |request: axum::extract::Request, next: axum::middleware::Next| {
        let (target, armed, base) = (hook_target.clone(), hook_armed.clone(), hook_base.clone());
        async move {
            let fire = {
                let guard = target.lock().unwrap();
                request.method() != axum::http::Method::GET
                    && guard
                        .as_ref()
                        .is_some_and(|(prefix, _)| request.uri().path().starts_with(prefix))
                    && armed.swap(false, Ordering::SeqCst)
            };
            let deliver = || async {
                let (rule, gate) = {
                    let guard = target.lock().unwrap();
                    let (_, v) = guard.as_ref().unwrap();
                    (v.rule, v.gate.clone())
                };
                deliver(&base, rule, &gate).await;
            };
            if fire && when == When::Before {
                deliver().await;
            }
            let response = next.run(request).await;
            if fire && when == When::After {
                deliver().await;
            }
            response
        }
    };
    let app = boss_jobs::http::router(state)
        .merge(boss_jobs::agents::http::router(
            boss_jobs::agents::http::AgentsApiState {
                registry: agents,
                classes: None,
                departments: None,
                pools: None,
                stations: None,
            },
        ))
        .layer(axum::middleware::from_fn_with_state(
            door,
            boss_jobs::agents::resolve_login,
        ))
        .layer(axum::middleware::from_fn(hook));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = crate::sor_client::machine_client()?;
    Ok((Stack { base, http, jobs }, target, armed))
}

/// The gate-run's close, as the dispatcher delivers it to `rule`. A
/// retryable refusal is redelivered, as the bus would.
async fn deliver(base: &str, rule: &str, gate: &str) {
    let handler = JobsCompleteLinkedStep::with_client(
        boss_dispatcher_handlers::handlers::common::api_client(),
        base,
        Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
    );
    let ctx = InvocationContext {
        event_timestamp: None,
        rule_name: rule.into(),
        triggering_event_id: format!("evt-close-{}", &gate[..8]),
        triggering_topic: "jobs.job.closed".into(),
        event_payload: json!({"id": gate, "kind": "gate-run"}),
    };
    for attempt in 1..=3 {
        match handler.invoke(&rule_args(rule), &ctx).await {
            Ok(()) => return,
            Err(e) if attempt < 3 => eprintln!("redelivering {rule} after: {e}"),
            Err(e) => panic!("{rule} never applied: {e}"),
        }
    }
}

/// A run as `boss dispatch` leaves it: filed, briefed, and its
/// `building` claimed by its agent — its id, `building`'s id, and the
/// claim's id, or `None` when the pool refused the claim.
async fn dispatched(stack: &Stack) -> (String, String, Option<String>) {
    let made = stack
        .api(
            Method::POST,
            "/api/jobs",
            Some(json!({
                "kind": "agent-run",
                "subject": { "subject_kind": "custom", "id": "bosspipeline" },
                "title": "builder run: the hand-off races its verdict",
                "owner_id": "emp-david",
                "priority": "standard",
                "status": "open",
                "tags": [],
                "metadata": {
                    "packet": "87d1ae40-ebe4-4217-b64d-ebb3a1f18d3c", "step": "build",
                    "agent": LOGIN, "model": "opus-5-5", "budget_usd": 5.0, "effort": "medium",
                },
            })),
        )
        .await
        .expect("the run is filed");
    let id = made["id"].as_str().expect("an id").to_string();
    let run = stack.run(&id).await;
    let briefed = Stack::step(&run, "briefed")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/jobs/{id}/steps/{briefed}");
    stack
        .api(Method::POST, &format!("{path}/claim"), Some(json!({})))
        .await;
    stack
        .api(
            Method::PATCH,
            &format!("{path}/metadata"),
            Some(json!({"prompt_bytes": "40763"})),
        )
        .await;
    stack
        .api(Method::PUT, &path, Some(json!({"status": "completed"})))
        .await;
    let run = stack.run(&id).await;
    let building = Stack::step(&run, "building")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let claimed = crate::sor_client::api_at_signed(
        &stack.http,
        &stack.base,
        Method::POST,
        &format!("/api/jobs/{id}/steps/{building}/claim"),
        Some(json!({})),
        crate::identity::Signature::As(LOGIN.to_string()),
    )
    .await;
    let run = stack.run(&id).await;
    let held = Stack::step(&run, "building");
    let claim = claimed.ok().and_then(|_| {
        held["metadata"]["resource_claim"]["id"]
            .as_str()
            .map(str::to_string)
    });
    (id, building, claim)
}

/// One order: a run dispatched onto the pool's one slot, its gate
/// submission accepted and its gate closed `rule`'s way; then the
/// hand-off, with the verdict delivered `when` around its first write to
/// `building`. Returns the run as it ends.
async fn race(rule: &'static str, when: When) -> (Stack, Value) {
    let (stack, target, armed) = serve(when).await.expect("serve the in-memory stack");
    let (id, building, claim) = dispatched(&stack).await;
    let claim = claim.expect("`building` holds the pool's one robots:opus slot");

    // The gate the submission was accepted for: filed, then its
    // submission bound through the adapter's own conditional door, so the
    // handler reads the immutable submission it would read live and the
    // hand-off holds the server's own receipt of it. (`rule` is what
    // fires on its close; the handler reads the packet, not its status.)
    let shape = super::hand_to_gate_tests::accepted_for(&id, &building, &claim);
    let mut gate = shape.job.clone();
    let mut filed = gate.metadata.as_object().cloned().unwrap_or_default();
    filed.remove("gate_submission");
    filed.remove("_immutable_metadata");
    gate.metadata = Value::Object(filed);
    stack
        .jobs
        .create_job_at(&gate, chrono::Utc::now(), &[])
        .await
        .expect("the gate-run is filed");
    let mut command = serde_json::to_value(&shape.command).unwrap();
    command["expected"] = json!({"gate_submission": {"present": false}});
    let command: boss_jobs::conditional_metadata::Command =
        serde_json::from_value(command).unwrap();
    let receipt = stack
        .jobs
        .conditional_job_metadata_at(
            &gate.id,
            &command,
            None,
            &boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::human("agent-claude-b"),
            ),
        )
        .await
        .expect("the server accepts the submission");
    *target.lock().unwrap() = Some((
        format!("/api/jobs/{id}/steps/{building}"),
        Verdict {
            rule,
            gate: gate.id.to_string(),
        },
    ));
    armed.store(true, Ordering::SeqCst);

    let stamped = super::with_worktree(
        super::agent_run_patch(Some(id.clone())).unwrap(),
        Some("/scratch/wt".into()),
    );
    super::exit_building(
        &stack.http,
        &stack.base,
        &id,
        LOGIN,
        &receipt,
        stamped.as_ref(),
    )
    .await;
    assert!(
        !armed.load(Ordering::SeqCst),
        "the hand-off wrote nothing to `building`, so the verdict was never delivered"
    );
    let run = stack.run(&id).await;
    (stack, run)
}

/// A step's result, or `-` when it has none.
fn result(run: &Value, slug: &str) -> String {
    let step = Stack::step(run, slug);
    format!(
        "{}:{}",
        step["status"].as_str().unwrap_or("?"),
        step["metadata"]["result"].as_str().unwrap_or("-")
    )
}

/// A GREEN between the hand-off's writes still lands the run: whichever
/// of `building` and `verdict` ends with `gated`, `reported` opens.
#[tokio::test(flavor = "multi_thread")]
async fn a_green_racing_the_hand_off_still_lands_the_run() {
    for when in [When::Before, When::After] {
        let (_stack, run) = race(GREEN, when).await;
        let reported = Stack::step(&run, "reported");
        assert_eq!(
            reported["status"],
            "ready",
            "{when:?}: the green opens `reported` — building {}, verdict {}",
            result(&run, "building"),
            result(&run, "verdict"),
        );
        assert_eq!(run["status"], "open", "{when:?}: it waits for its report");
    }
}

/// A RED between the hand-off's writes still ends the run refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_red_racing_the_hand_off_still_refuses_the_run() {
    for when in [When::Before, When::After] {
        let (_stack, run) = race(RED, when).await;
        // The `refused` terminal is reached: its outcome step is open or
        // done (the in-memory stack closes the packet on it only where
        // the outcome step completes), and nothing waits on `verdict`.
        let refused = Stack::step(&run, "refused");
        assert!(
            matches!(refused["status"].as_str(), Some("ready" | "completed")),
            "{when:?}: the red reaches `refused` ({}) — building {}, verdict {}",
            refused["status"],
            result(&run, "building"),
            result(&run, "verdict"),
        );
        assert!(
            !matches!(
                Stack::step(&run, "verdict")["status"].as_str(),
                Some("ready" | "active")
            ),
            "{when:?}: a `verdict` still waits after its red"
        );
        if run["status"] == "closed" {
            assert_eq!(run["outcome"], "refused", "{when:?}: {run}");
        }
    }
}

/// Whatever the order, the robot slot is given back: the pool has ONE
/// slot, and the next run's `building` is admitted on it once the
/// hand-off and the verdict have both been through.
#[tokio::test(flavor = "multi_thread")]
async fn either_order_frees_the_robot_slot() {
    for when in [When::Before, When::After] {
        let (stack, run) = race(GREEN, when).await;
        let building = Stack::step(&run, "building");
        assert_eq!(building["status"], "completed", "{when:?}: {building}");
        let (_, _, next) = dispatched(&stack).await;
        assert!(
            next.is_some(),
            "{when:?}: the next dispatch is refused — the slot is still held"
        );
    }
}
