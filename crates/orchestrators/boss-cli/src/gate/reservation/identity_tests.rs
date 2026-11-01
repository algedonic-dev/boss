//! Real admission and alias middleware, without a native client or controller.
use super::*;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use boss_jobs::{
    JobsRepository,
    agents::{AgentsRegistry, InMemoryAgents},
};
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

struct StateData {
    agents: Arc<InMemoryAgents>,
    fault: AtomicU8,
    posts: AtomicUsize,
    commands: std::sync::Mutex<Vec<Uuid>>,
}
async fn adversary(State(state): State<Arc<StateData>>, request: Request, next: Next) -> Response {
    let mut request = request;
    let roster = request.uri().path() == "/api/agents";
    if request.method() == reqwest::Method::POST {
        state.posts.fetch_add(1, Ordering::SeqCst);
        let (parts, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
        let command: Command = serde_json::from_slice(&bytes).unwrap();
        state.commands.lock().unwrap().push(command.idempotency_key);
        request = Request::from_parts(parts, axum::body::Body::from(bytes));
        if state.fault.load(Ordering::SeqCst) == 5 {
            let mut row = state
                .agents
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|row| row.id == "agent-other")
                .unwrap();
            row.aliases = vec!["builder@example.invalid".into()];
            let input: boss_jobs::agents::AgentInput =
                serde_json::from_value(serde_json::to_value(row).unwrap()).unwrap();
            state
                .agents
                .publish(
                    &[input],
                    boss_core::publish::PublishMode::Take,
                    &boss_core::publisher::EventStamp::new(
                        "fixture",
                        boss_core::actor::ActorId::Automation("fixture".into()),
                    ),
                )
                .await
                .unwrap();
        }
    }
    if roster {
        match state.fault.load(Ordering::SeqCst) {
            1 => return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "registry unavailable").into_response(),
            2 => return axum::Json(json!({"total":3,"data":[]})).into_response(),
            3 => return axum::Json(json!({"total":2,"data":[{"id":"agent-one","aliases":["builder@example.invalid"]},{"id":"agent-two","aliases":["builder@example.invalid"]}]})).into_response(),
            4 => return axum::Json(json!({"total":0,"data":[]})).into_response(),
            _ => {}
        }
    }
    next.run(request).await
}
struct Fixture {
    base: String,
    job: boss_core::job::Job,
    jobs: Arc<boss_jobs::InMemoryJobs>,
    state: Arc<StateData>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(fault: u8) -> Self {
        let workflows = Arc::new(boss_jobs::registry::InMemoryWorkflows::for_fixture());
        let protocol =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .unwrap()
                .into_iter()
                .find(|row| row.kind == "gate-run")
                .unwrap();
        workflows.seed(protocol).unwrap();
        let mut job = boss_core::job::Job::new(
            "gate-run",
            boss_core::job::Subject::new("custom", "fixture"),
            "Identity admission",
            "agent-fixture",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        );
        job.status = boss_core::job::JobStatus::Open;
        job.metadata = json!({"branch":"fixture","sha":"fixture-head"});
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new().with_workflows(workflows.clone()));
        jobs.create_job(&job).await.unwrap();
        let bus = boss_testing::RecordingEventBus::new();
        let publisher = boss_core::publisher::DomainPublisher::new(bus.clone(), "jobs");
        let mut api = boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher.clone(),
            Arc::new(boss_policy_client::PermissivePolicyClient),
            Arc::new(boss_clock_client::WallClockClient),
        );
        api.kind_registry = Some(workflows);
        let agents = Arc::new(
            InMemoryAgents::new()
                .with_agent("agent-fixture", ["builder@example.invalid"])
                .with_agent("agent-other", []),
        );
        let state = Arc::new(StateData {
            agents: agents.clone(),
            fault: AtomicU8::new(fault),
            posts: AtomicUsize::new(0),
            commands: std::sync::Mutex::new(Vec::new()),
        });
        let login = Arc::new(boss_jobs::agents::LoginDoor::new(agents.clone(), publisher));
        let app = boss_jobs::http::router(api)
            .merge(boss_jobs::agents::http::router(
                boss_jobs::agents::http::AgentsApiState {
                    registry: agents,
                    classes: None,
                    departments: None,
                },
            ))
            .layer(axum::middleware::from_fn_with_state(
                login,
                boss_jobs::agents::resolve_login,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                adversary,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            job,
            jobs,
            state,
            server,
        }
    }
    fn doors<'a>(&self, http: &'a crate::machine_http::MachineClient, actor: &str) -> Doors<'a> {
        Doors {
            http,
            jobs_base: self.base.clone(),
            content_base: self.base.clone(),
            caller: Some(identity::Caller {
                id: actor.into(),
                source: identity::Source::Env,
            }),
            native_args: vec![],
            create_program: None,
        }
    }
    fn intent(&self) -> LaunchIntent {
        LaunchIntent {
            contract_version: 1,
            packet: self.job.id.to_string(),
            namespace: "fixture".into(),
            name: "fixture".into(),
            head: "fixture-head".into(),
            mode: "full".into(),
            transition_generation: None,
            file_id: Uuid::new_v4().to_string(),
            sha256: "a".repeat(64),
            size_bytes: 1,
        }
    }
    fn overrides(&self) -> Value {
        let row = boss_jobs::seed_loader::load_delivery_policies(
            boss_jobs::delivery_policy_seed::platform_delivery_policy_path(),
        )
        .unwrap()
        .remove(0)
        .row;
        json!({"gate_queue_policy":row})
    }
}
#[tokio::test]
async fn alias_submission_and_detached_original_share_the_authenticated_actor() {
    for actor in [
        "builder@example.invalid",
        "agent-fixture",
        "automation:fixture",
    ] {
        let fixture = Fixture::new(0).await;
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = fixture.doors(&http, actor);
        let receipt = submission::submit_on(&doors, &fixture.intent(), &fixture.overrides())
            .await
            .unwrap();
        let expected = if actor.contains('@') {
            "agent-fixture"
        } else {
            actor
        };
        assert_eq!(receipt.actor, expected);
        assert_eq!(
            receipt.job.metadata["gate_submission"]["submitted_by"],
            expected
        );
        let (_, original) = submission::original_on(&doors, &fixture.job.id.to_string())
            .await
            .unwrap();
        assert_eq!(original, receipt);
        let mut malformed = receipt.clone();
        for value in [
            malformed.command.patch.get_mut(model::SUBMISSION).unwrap(),
            malformed.job.metadata.get_mut(model::SUBMISSION).unwrap(),
        ] {
            value["submitted_by"] = json!("builder@example.invalid");
        }
        malformed.job.metadata["_immutable_metadata"][model::SUBMISSION]["submitted_by"] =
            json!("builder@example.invalid");
        assert!(
            model::acceptance(&malformed.job, &malformed).is_err(),
            "matching payload/index copies cannot launder an actor mismatch"
        );
        assert_eq!(fixture.state.posts.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn unreadable_or_nonunique_rosters_refuse_before_a_submission_post() {
    for fault in [1, 2, 3, 4] {
        let fixture = Fixture::new(fault).await;
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let result = submission::submit_on(
            &fixture.doors(&http, "builder@example.invalid"),
            &fixture.intent(),
            &fixture.overrides(),
        )
        .await;
        assert!(result.is_err(), "fault {fault} admitted");
        assert_eq!(
            fixture.state.posts.load(Ordering::SeqCst),
            0,
            "fault {fault}"
        );
        assert!(
            fixture
                .jobs
                .get_job(&fixture.job.id)
                .await
                .unwrap()
                .unwrap()
                .metadata
                .get("gate_submission")
                .is_none()
        );
    }
}
#[tokio::test]
async fn alias_movement_before_post_leaves_no_immutable_submission_or_receipt() {
    let fixture = Fixture::new(5).await;
    let http = crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
        .unwrap();
    let result = submission::submit_on(
        &fixture.doors(&http, "builder@example.invalid"),
        &fixture.intent(),
        &fixture.overrides(),
    )
    .await;
    assert!(result.is_err());
    let job = fixture
        .jobs
        .get_job(&fixture.job.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.metadata.get("gate_submission").is_none(),
        "a failed admission immortalized a malformed submission"
    );
    assert!(job.metadata.get("_immutable_metadata").is_none());
    let command = fixture.state.commands.lock().unwrap()[0];
    for actor in ["agent-fixture", "agent-other", "builder@example.invalid"] {
        assert!(
            fixture
                .jobs
                .conditional_job_metadata_receipt(&job.id, actor, command)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn direct_first_record_actor_assertions_are_checked_by_the_real_http_boundary() {
    for (key, field) in [
        (model::SUBMISSION, "submitted_by"),
        ("gate_orphan_claim", "actor"),
        (model::ATTEMPT, "actor"),
    ] {
        let fixture = Fixture::new(0).await;
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let command = Command {
            contract_version: 2,
            idempotency_key: Uuid::new_v4(),
            expected: std::collections::BTreeMap::from([(
                key.into(),
                boss_jobs::conditional_metadata::Expected {
                    present: false,
                    value: Value::Null,
                },
            )]),
            patch: serde_json::Map::from_iter([(
                key.into(),
                json!({(field):"asserted-other","id":Uuid::new_v4()}),
            )]),
            immutable_keys: if key == model::ATTEMPT {
                vec![]
            } else {
                vec![key.into()]
            },
            guarded_keys: if key == model::ATTEMPT {
                vec![key.into()]
            } else {
                vec![]
            },
        };
        assert_eq!(command.patch[key][field], "asserted-other");
        let response = http
            .post(format!(
                "{}/api/jobs/{}/metadata/conditional",
                fixture.base, fixture.job.id
            ))
            .header("x-boss-user", identity::header("builder@example.invalid"))
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT, "{key}");
        assert_eq!(
            fixture
                .jobs
                .get_job(&fixture.job.id)
                .await
                .unwrap()
                .unwrap()
                .metadata,
            fixture.job.metadata
        );
        assert!(
            fixture
                .jobs
                .conditional_job_metadata_receipt(
                    &fixture.job.id,
                    "agent-fixture",
                    command.idempotency_key
                )
                .await
                .unwrap()
                .is_none()
        );
    }
}

/// WHY (backlog 59ce3050, review 421d6960 F1). A same-id attempt update
/// was judged against the STORED actor only, so another principal with
/// update scope on the packet could rewrite an attempt it never made —
/// here, demote it to `not_started`, which reopens the send — and the
/// record would still name the original actor. Through the real HTTP
/// boundary: the owner (signing as its alias) records the attempt,
/// another principal's same-id rewrite is refused with nothing written,
/// and the owner's own same-id evidence still lands.
#[tokio::test]
async fn a_same_id_attempt_rewrite_by_another_principal_is_refused_at_the_http_boundary() {
    let fixture = Fixture::new(0).await;
    let http = crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
        .unwrap();
    let url = format!(
        "{}/api/jobs/{}/metadata/conditional",
        fixture.base, fixture.job.id
    );
    let id = Uuid::new_v4();
    let attempt = |state: &str| json!({"id": id, "actor": "agent-fixture", "state": state});
    let command = |expected: Option<Value>, state: &str, guarded: bool| Command {
        contract_version: 2,
        idempotency_key: Uuid::new_v4(),
        expected: std::collections::BTreeMap::from([(
            model::ATTEMPT.into(),
            boss_jobs::conditional_metadata::Expected {
                present: expected.is_some(),
                value: expected.unwrap_or(Value::Null),
            },
        )]),
        patch: serde_json::Map::from_iter([(model::ATTEMPT.into(), attempt(state))]),
        immutable_keys: vec![],
        guarded_keys: if guarded {
            vec![model::ATTEMPT.into()]
        } else {
            vec![]
        },
    };
    let stored = || async {
        fixture
            .jobs
            .get_job(&fixture.job.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
    };
    let first = http
        .post(&url)
        .header("x-boss-user", identity::header("builder@example.invalid"))
        .json(&command(None, "sending", true))
        .send()
        .await
        .unwrap();
    assert!(
        first.status().is_success(),
        "the owner, signing as its alias, records its attempt: {}",
        first.text().await.unwrap()
    );
    let before = stored().await;
    assert_eq!(before[model::ATTEMPT], attempt("sending"));

    let rewrite = command(Some(attempt("sending")), "not_started", false);
    let refused = http
        .post(&url)
        .header("x-boss-user", identity::header("agent-other"))
        .json(&rewrite)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let why = refused.text().await.unwrap();
    assert!(why.contains("gate_launch_attempt.actor"), "{why}");
    assert_eq!(stored().await, before, "a refused rewrite wrote nothing");

    let own = http
        .post(&url)
        .header("x-boss-user", identity::header("agent-fixture"))
        .json(&command(Some(attempt("sending")), "not_started", false))
        .send()
        .await
        .unwrap();
    assert!(
        own.status().is_success(),
        "the owner's own same-id evidence still lands: {}",
        own.text().await.unwrap()
    );
    assert_eq!(stored().await[model::ATTEMPT], attempt("not_started"));
}

/// WHY (backlog 0471152b) — the probe of reviewer run 5426da05 of car
/// b34f114c, through the real HTTP boundary. agent-fixture records
/// attempt X; agent-other replaces it with Y naming itself; then
/// agent-other posts X back, naming itself, pinned on Y. On main that
/// answered 200, and attempt id X named two actors. It is refused, with
/// nothing written and no receipt kept, whether or not the reposted
/// record names Y as its `previous`.
#[tokio::test]
async fn a_replaced_attempt_id_reposted_under_a_new_actor_is_refused_and_writes_nothing() {
    let fixture = Fixture::new(0).await;
    let http = crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
        .unwrap();
    let url = format!(
        "{}/api/jobs/{}/metadata/conditional",
        fixture.base, fixture.job.id
    );
    let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
    let command = |expected: Option<Value>, next: Value, guarded: bool| Command {
        contract_version: 2,
        idempotency_key: Uuid::new_v4(),
        expected: std::collections::BTreeMap::from([(
            model::ATTEMPT.into(),
            boss_jobs::conditional_metadata::Expected {
                present: expected.is_some(),
                value: expected.unwrap_or(Value::Null),
            },
        )]),
        patch: serde_json::Map::from_iter([(model::ATTEMPT.into(), next)]),
        immutable_keys: vec![],
        guarded_keys: if guarded {
            vec![model::ATTEMPT.into()]
        } else {
            vec![]
        },
    };
    let stored = || async {
        fixture
            .jobs
            .get_job(&fixture.job.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
    };
    let original = json!({"id": x, "actor": "agent-fixture", "state": "not_started"});
    let first = http
        .post(&url)
        .header("x-boss-user", identity::header("agent-fixture"))
        .json(&command(None, original.clone(), true))
        .send()
        .await
        .unwrap();
    assert!(
        first.status().is_success(),
        "{}",
        first.text().await.unwrap()
    );
    let replacement = json!({"id": y, "actor": "agent-other", "previous": x, "state": "sending"});
    let replaced = http
        .post(&url)
        .header("x-boss-user", identity::header("agent-other"))
        .json(&command(Some(original), replacement.clone(), false))
        .send()
        .await
        .unwrap();
    assert!(
        replaced.status().is_success(),
        "a fresh replacement naming the stored attempt still lands: {}",
        replaced.text().await.unwrap()
    );
    let before = stored().await;
    assert_eq!(before[model::ATTEMPT], replacement);

    // Naming no `previous`, the repost is refused before the repository
    // reads any history; naming Y, it reaches the freshness check the
    // repository makes under its row lock (backlog 301b9060).
    for (reposted, refusal) in [
        (
            json!({"id": x, "actor": "agent-other", "state": "sending"}),
            "gate_launch_attempt.previous",
        ),
        (
            json!({"id": x, "actor": "agent-other", "previous": y, "state": "sending"}),
            "gate_launch_attempt.id",
        ),
    ] {
        let repost = command(Some(replacement.clone()), reposted, false);
        let refused = http
            .post(&url)
            .header("x-boss-user", identity::header("agent-other"))
            .json(&repost)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
        let why = refused.text().await.unwrap();
        assert!(why.contains(refusal), "{why}");
        assert_eq!(stored().await, before, "a refused repost wrote nothing");
        assert!(
            fixture
                .jobs
                .conditional_job_metadata_receipt(
                    &fixture.job.id,
                    "agent-other",
                    repost.idempotency_key
                )
                .await
                .unwrap()
                .is_none(),
            "a refused repost kept no receipt"
        );
    }
}
