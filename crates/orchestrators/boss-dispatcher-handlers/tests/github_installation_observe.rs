use boss_dispatcher_handlers::handlers::credential_installation_observe::authorize_observation;
use serde_json::{Value, json};

fn packet() -> Value {
    json!({"id":"00000000-0000-0000-0000-000000000001", "kind":"inspect-a-github-installation",
        "status":"open", "partition":"real", "subject":{"subject_kind":"custom","id":"example-credential"},
        "steps":[{"id":"00000000-0000-0000-0000-000000000002","spec_slug":"scope","kind":"task",
            "status":"completed","completed_by":"emp-reviewer", "metadata":{"human_only":true,
                "authority_role":"platform-admin","credential":"example-credential",
                "expected_account_login":"example-org", "reason":"Read grants before considering a permission repair"}},
            {"id":"00000000-0000-0000-0000-000000000003","spec_slug":"observe","kind":"task",
                "status":"ready","metadata":{"written_by":"rule:example-observer"}}]})
}

#[test]
fn observation_accepts_the_subject_serialized_by_the_actual_job_type() {
    use boss_core::job::{Job, JobStatus, Priority, Subject};
    let mut job = Job::new(
        "inspect-a-github-installation",
        Subject::new("custom", "example-credential"),
        "Inspect the declared installation",
        "emp-reviewer",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
    );
    job.status = JobStatus::Open;
    let mut row = serde_json::to_value(job).unwrap();
    row["steps"] = packet()["steps"].clone();
    assert_eq!(row["subject"]["subject_kind"], "custom");
    assert!(row["subject"].get("kind").is_none());
    authorize_observation(
        &row,
        row["id"].as_str().unwrap(),
        "00000000-0000-0000-0000-000000000002",
        "example-credential",
        "example-observer",
    )
    .unwrap();
    for subject in [
        json!({"subject_kind":"custom","id":"bosspipeline"}),
        json!({"subject_kind":"credential","id":"example-credential"}),
        json!({"kind":"custom","id":"example-credential"}),
    ] {
        let mut wrong = row.clone();
        wrong["subject"] = subject;
        assert!(
            authorize_observation(
                &wrong,
                row["id"].as_str().unwrap(),
                "00000000-0000-0000-0000-000000000002",
                "example-credential",
                "example-observer",
            )
            .is_err()
        );
    }
}

#[test]
fn observation_requires_its_current_completed_human_scope_and_exact_writer() {
    let p = packet();
    let a = authorize_observation(
        &p,
        p["id"].as_str().unwrap(),
        "00000000-0000-0000-0000-000000000002",
        "example-credential",
        "example-observer",
    )
    .unwrap();
    assert_eq!(a.expected_account_login, "example-org");
    assert_eq!(a.expected_account_id, None);
    for (key, value) in [
        ("status", json!("ready")),
        ("completed_by", json!("rule:other")),
    ] {
        let mut wrong = packet();
        wrong["steps"][0][key] = value;
        assert!(
            authorize_observation(
                &wrong,
                p["id"].as_str().unwrap(),
                "00000000-0000-0000-0000-000000000002",
                "example-credential",
                "example-observer"
            )
            .is_err()
        );
    }
    for (key, value) in [
        ("human_only", json!(false)),
        ("authority_role", json!("other")),
        ("credential", json!("other")),
        ("expected_account_login", json!(" ")),
    ] {
        let mut wrong = packet();
        wrong["steps"][0]["metadata"][key] = value;
        assert!(
            authorize_observation(
                &wrong,
                p["id"].as_str().unwrap(),
                "00000000-0000-0000-0000-000000000002",
                "example-credential",
                "example-observer"
            )
            .is_err()
        );
    }
    let mut wrong = packet();
    wrong["steps"][1]["metadata"]["written_by"] = json!("rule:other");
    assert!(
        authorize_observation(
            &wrong,
            p["id"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000002",
            "example-credential",
            "example-observer"
        )
        .is_err()
    );
}

#[test]
fn optional_numeric_identity_is_a_real_optional_assertion_not_an_invented_default() {
    let mut p = packet();
    p["steps"][0]["metadata"]["expected_account_id"] = json!(123);
    assert_eq!(
        authorize_observation(
            &p,
            p["id"].as_str().unwrap(),
            "00000000-0000-0000-0000-000000000002",
            "example-credential",
            "example-observer"
        )
        .unwrap()
        .expected_account_id,
        Some(123)
    );
    for v in [json!(0), json!("123"), json!(-1)] {
        p["steps"][0]["metadata"]["expected_account_id"] = v;
        assert!(
            authorize_observation(
                &p,
                p["id"].as_str().unwrap(),
                "00000000-0000-0000-0000-000000000002",
                "example-credential",
                "example-observer"
            )
            .is_err()
        );
    }
}

#[test]
fn the_diagnostic_compares_the_actual_publication_declaration_and_writer() {
    let root = boss_testing::repo_root();
    let read = |path: &str| -> toml::Value {
        toml::from_str(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
    };
    let diagnostic =
        read("infra/dispatcher/rules/broker-observes-the-github-installation-grants.toml");
    let publication = read(
        "infra/dispatcher/rules/broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed.toml",
    );
    for key in ["credential_id", "permissions", "installation_id"] {
        assert_eq!(
            diagnostic["rule"][0]["do"][0]["args"].get(key),
            publication["rule"][0]["do"][0]["args"].get(key),
            "{key} declaration drifted"
        );
    }
    let workflow = boss_jobs::seed_loader::load_workflows(
        root.join("infra/platform/workflows/inspect-a-github-installation.toml"),
    )
    .unwrap()
    .remove(0);
    let scope = workflow.steps.iter().find(|s| s.title == "scope").unwrap();
    assert_eq!(scope.metadata_defaults["human_only"], true);
    assert_eq!(scope.metadata_defaults["installation_diagnostic"], "scope");
    let observe = workflow
        .steps
        .iter()
        .find(|s| s.title == "observe")
        .unwrap();
    assert_eq!(
        observe.metadata_defaults["written_by"],
        format!("rule:{}", diagnostic["rule"][0]["name"].as_str().unwrap())
    );
    let handler=boss_dispatcher_handlers::handlers::credential_installation_observe::CredentialObserveInstallation::new("http://localhost",std::sync::Arc::new(Reader(std::sync::atomic::AtomicUsize::new(0))));
    use boss_dispatcher::rules::handler::Handler;
    assert_eq!(
        handler.name(),
        diagnostic["rule"][0]["do"][0]["handler"].as_str().unwrap()
    );
    // A completed approval for another subject never selects this broker rule.
    // Do not widen routing to compensate for a wrongly admitted request.
    use boss_dispatcher::rules::expr::{Context, NoHelpers, eval, parse};
    let predicate = parse(diagnostic["rule"][0]["when"].as_str().unwrap()).unwrap();
    for (subject, diagnostic_phase, expected) in [
        ("github-app-algedonic-dev", "scope", true),
        ("bosspipeline", "scope", false),
        ("github-app-algedonic-dev", "observe", false),
    ] {
        let payload = json!({"subject_id":subject,
            "metadata":{"installation_diagnostic":diagnostic_phase}});
        assert_eq!(
            eval(
                &predicate,
                &Context {
                    payload: &payload,
                    helpers: &NoHelpers
                }
            )
            .unwrap()
            .as_bool(),
            Some(expected)
        );
    }
}

struct Reader(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl boss_dispatcher_handlers::handlers::credential_issuer::installation_read::InstallationReader
    for Reader
{
    fn app_id(&self) -> Result<u64, String> {
        Ok(42)
    }
    fn default_installation_id(&self) -> Result<u64, String> {
        Ok(77)
    }
    async fn read_installation(&self, e: &boss_dispatcher_handlers::handlers::credential_issuer::installation_read::ExpectedInstallation)
    -> Result<boss_dispatcher_handlers::handlers::credential_issuer::installation_read::InstallationSnapshot,String>{
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        boss_dispatcher_handlers::handlers::credential_issuer::installation_read::parse_snapshot(
            json!({"id":77,"app_id":42,"account":{"id":123,"login":"example-org"},
                "permissions":{"contents":"read","unknown_permission":"write"},
                "repository_selection":"selected","suspended_at":null}),
            e,
            chrono::Utc::now(),
        )
    }
}

#[tokio::test]
async fn denied_native_scope_never_calls_the_issuer_or_any_write() {
    use boss_dispatcher::rules::handler::{Handler, InvocationContext};
    use boss_dispatcher_handlers::handlers::credential_installation_observe::CredentialObserveInstallation;
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    let writes = Arc::new(Mutex::new(Vec::new()));
    let seen = writes.clone();
    let mut row = packet();
    row["steps"][0]["status"] = json!("ready");
    let app = axum::Router::new().fallback(move |r: axum::http::Request<axum::body::Body>| {
        let row = row.clone();
        let seen = seen.clone();
        async move {
            if r.method() != axum::http::Method::GET {
                seen.lock().unwrap().push(r.uri().to_string());
            }
            axum::Json(row)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reader = Arc::new(Reader(AtomicUsize::new(0)));
    let handler = CredentialObserveInstallation::new(&url, reader.clone());
    let ctx = InvocationContext {
        rule_name: "example-observer".into(),
        triggering_event_id: uuid::Uuid::new_v4().to_string(),
        triggering_topic: "step.done.task".into(),
        event_timestamp: Some(chrono::Utc::now()),
        event_payload: json!({"job_id":"00000000-0000-0000-0000-000000000001",
            "step_id":"00000000-0000-0000-0000-000000000002","kind":"task",
            "subject_kind":"custom","subject_id":"example-credential","metadata":{}}),
    };
    let args = vec![
        (
            "credential_id".into(),
            boss_dispatcher::rules::expr::Value::String("example-credential".into()),
        ),
        (
            "permissions".into(),
            boss_dispatcher::rules::expr::Value::String("contents:write".into()),
        ),
    ];
    assert!(handler.invoke(&args, &ctx).await.is_err());
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(writes.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn a_completion_failure_replays_full_first_evidence_without_another_issuer_read() {
    use boss_dispatcher::rules::handler::{Handler, InvocationContext};
    use boss_dispatcher_handlers::handlers::credential_installation_observe::CredentialObserveInstallation;
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    #[derive(Default)]
    struct Native {
        row: Value,
        record: Option<boss_jobs::first_record::FirstRecord>,
        completions: usize,
        inserts: usize,
        receipt_checks: usize,
    }
    let native = Arc::new(Mutex::new(Native {
        row: packet(),
        ..Default::default()
    }));
    let state = native.clone();
    let app=axum::Router::new().fallback(move |r:axum::http::Request<axum::body::Body>| {
        let state=state.clone(); async move {
            use axum::response::IntoResponse;
            let method=r.method().clone();let path=r.uri().path().to_owned();
            let actor=r.headers()["x-boss-user"].to_str().unwrap().to_owned();
            let bytes=axum::body::to_bytes(r.into_body(),usize::MAX).await.unwrap();
            let mut s=state.lock().unwrap();
            match (method.as_str(),path.as_str()) {
                ("GET","/api/jobs/00000000-0000-0000-0000-000000000001")=>axum::Json(s.row.clone()).into_response(),
                // The kind is spelled out, as GET /api/credentials/github-app-algedonic-dev
                // answers it — never the handler's constant, which would agree with
                // itself whatever it said (backlog 8dbef2a7).
                ("GET","/api/credentials/example-credential")=>axum::Json(json!({"id":"example-credential","kind":"github-app-installation-token"})).into_response(),
                ("POST","/api/jobs/00000000-0000-0000-0000-000000000001/steps/00000000-0000-0000-0000-000000000003/metadata/records")=> {
                    s.receipt_checks+=1;
                    assert!(actor.contains("rule:example-observer"));
                    let body:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(body["expected_absence"],true);assert_eq!(body["key"],"observation");assert_eq!(body.as_object().unwrap().len(),3);
                    let result=if let Some(record)=&s.record {
                        assert!(record.matches(&body["value"]));boss_jobs::first_record::FirstRecordResult::Replayed(record.clone())
                    } else {
                        let stamp=boss_core::publisher::EventStamp::new("jobs",boss_core::actor::ActorId::Automation("rule:example-observer".into()));
                        let record=boss_jobs::first_record::FirstRecord::new(
                            serde_json::from_value(json!("00000000-0000-0000-0000-000000000001")).unwrap(),
                            serde_json::from_value(json!("00000000-0000-0000-0000-000000000003")).unwrap(),
                            "observation",&body["value"],&stamp,uuid::Uuid::new_v4());
                        s.row["steps"][1]["metadata"]["observation"]=record.value.clone();s.record=Some(record.clone());s.inserts+=1;
                        boss_jobs::first_record::FirstRecordResult::Recorded(record)
                    };axum::Json(result).into_response()
                },
                ("PUT","/api/jobs/00000000-0000-0000-0000-000000000001/steps/00000000-0000-0000-0000-000000000003")=> {
                    assert!(actor.contains("rule:example-observer"));assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(),json!({"status":"completed"}));
                    s.completions+=1;if s.completions==1 { return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(); }
                    s.row["steps"][1]["status"]=json!("completed");s.row["status"]=json!("closed");axum::http::StatusCode::NO_CONTENT.into_response()
                },
                _=>panic!("unexpected issuer/Secret/rotation/write path {method} {path}"),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reader = Arc::new(Reader(AtomicUsize::new(0)));
    let handler = CredentialObserveInstallation::new(&url, reader.clone());
    let ctx = InvocationContext {
        rule_name: "example-observer".into(),
        triggering_event_id: uuid::Uuid::new_v4().to_string(),
        triggering_topic: "step.done.task".into(),
        event_timestamp: Some(chrono::Utc::now()),
        event_payload: json!({"job_id":"00000000-0000-0000-0000-000000000001","step_id":"00000000-0000-0000-0000-000000000002","kind":"task","subject_kind":"custom","subject_id":"example-credential","metadata":{}}),
    };
    let args = vec![
        (
            "credential_id".into(),
            boss_dispatcher::rules::expr::Value::String("example-credential".into()),
        ),
        (
            "permissions".into(),
            boss_dispatcher::rules::expr::Value::String(
                "contents:write,missing_permission:read".into(),
            ),
        ),
    ];
    assert!(handler.invoke(&args, &ctx).await.is_err());
    let original = native.lock().unwrap().record.clone().unwrap();
    handler.invoke(&args, &ctx).await.unwrap();
    handler.invoke(&args, &ctx).await.unwrap();
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    {
        let s = native.lock().unwrap();
        assert_eq!(s.inserts, 1);
        assert_eq!(s.completions, 2);
        assert_eq!(
            s.receipt_checks, 3,
            "a completed row alone is not an immutable native receipt"
        );
        assert_eq!(s.record.as_ref().unwrap(), &original);
        assert_eq!(
            original.value["snapshot"]["permissions"]["unknown_permission"],
            "write"
        );
        assert_eq!(
            original.value["comparison"]["insufficient"],
            json!(["contents"])
        );
        assert_eq!(
            original.value["comparison"]["missing"],
            json!(["missing_permission"])
        );
        assert_eq!(
            original.value["snapshot"]["expected_account_id"],
            Value::Null
        );
    }
    for (field, malformed) in [
        ("account_id", json!(0)),
        ("repository_selection", json!("")),
        ("permissions", json!({" ":"write"})),
    ] {
        native.lock().unwrap().row["steps"][1]["metadata"]["observation"]["snapshot"][field] =
            malformed;
        assert!(
            handler.invoke(&args, &ctx).await.is_err(),
            "malformed recorded {field} must refuse without rereading"
        );
        let mut s = native.lock().unwrap();
        assert_eq!(s.receipt_checks, 3);
        assert_eq!(s.completions, 2);
        s.row["steps"][1]["metadata"]["observation"] = original.value.clone();
    }
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn the_materialized_observer_uses_native_policy_and_declared_writer_without_new_grants() {
    use boss_core::{
        job::{Job, Priority, StepId, StepStatus, Subject},
        port::EventBus,
        publisher::DomainPublisher,
    };
    use boss_jobs::{
        InMemoryJobs, JobsRepository,
        http::{JobsApiState, router},
    };
    use std::sync::Arc;
    use tower::ServiceExt;
    let source = boss_testing::repo_root()
        .join("infra/platform/workflows/inspect-a-github-installation.toml");
    let workflow = boss_jobs::seed_loader::load_workflows(source)
        .unwrap()
        .remove(0);
    let spec = workflow
        .steps
        .iter()
        .find(|s| s.title == "observe")
        .unwrap();
    assert_eq!(
        spec.metadata_defaults["written_by"],
        "rule:broker-observes-the-github-installation-grants"
    );
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "inspect-a-github-installation",
        Subject::new("custom", "example-credential"),
        "Inspect",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    jobs.create_job(&job).await.unwrap();
    let materialized = boss_jobs::registry::materialize_steps(
        &workflow,
        &job.subject,
        job.id,
        &job.metadata,
        StepId::new,
    );
    let mut step = materialized
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some("observe"))
        .unwrap();
    step.status = StepStatus::Active;
    jobs.add_step(&step).await.unwrap();
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy = Arc::new(
        boss_policy_client::FakePolicyClient::builder()
            .with_default_rules()
            .build(),
    );
    let app = router(JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let value = json!({"complete_public_map":{"future_permission":"write"},"numeric_identity_unasserted":true});
    let body = json!({"key":"observation","value":value,"expected_absence":true});
    for (id, role, status) in [
        (
            "rule:other",
            "platform-admin",
            axum::http::StatusCode::FORBIDDEN,
        ),
        (
            "rule:broker-observes-the-github-installation-grants",
            "viewer",
            axum::http::StatusCode::FORBIDDEN,
        ),
        (
            "rule:broker-observes-the-github-installation-grants",
            "platform-admin",
            axum::http::StatusCode::CREATED,
        ),
    ] {
        let user = json!({"id":id,"role":role,"access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"});
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/jobs/{}/steps/{}/metadata/records",
                        job.id, step.id
                    ))
                    .header("content-type", "application/json")
                    .header("x-boss-user", user.to_string())
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{id}/{role}");
        if status != axum::http::StatusCode::CREATED {
            assert!(jobs.recorded_events().is_empty());
        } else {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let receipt: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(receipt["record"]["value"], value);
            assert_eq!(
                receipt["record"]["actor"],
                "automation:rule:broker-observes-the-github-installation-grants"
            );
        }
    }
    assert_eq!(
        jobs.recorded_events()
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        [
            boss_jobs::events::STEP_FIRST_RECORDED,
            boss_jobs::events::STEP_UPDATED
        ]
    );
}

/// Backlog 8dbef2a7 (2026-10-06): the handler compared the registry row's
/// kind against a literal of its own, `github-app-installation`, while the
/// live rows are `github-app-installation-token` — and this suite's fixture
/// invented a row that agreed with the handler, so every live packet
/// dead-lettered under a green test. The registry rows are instance data
/// (no seed in the tree declares one), so the pins are: the one constant
/// reads as the live registry spells it, the broker manifest's declaration
/// of both App rows spells the same, and a row of ANY other kind — the old
/// literal among them — is still refused before the issuer is asked or
/// anything is written.
#[tokio::test]
async fn the_declared_kind_is_the_registry_spelling_and_any_other_kind_is_refused() {
    use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
    use boss_dispatcher_handlers::handlers::{
        credential_installation_observe::CredentialObserveInstallation,
        credential_rotate_github_app::REGISTRY_KIND,
    };
    use std::sync::{Arc, Mutex, atomic::AtomicUsize};
    assert_eq!(
        REGISTRY_KIND, "github-app-installation-token",
        "GET /api/credentials/github-app-algedonic-dev answers this kind (read 2026-10-06)"
    );
    let manifest = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/cluster/manifests/boss-credential-broker.yaml"),
    )
    .unwrap();
    let declared: Vec<&str> = manifest
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("#") && words.next() == Some("kind"))
                .then(|| words.next())
                .flatten()
        })
        .filter(|kind| kind.starts_with("github-app"))
        .collect();
    assert_eq!(
        declared,
        [format!("{REGISTRY_KIND},"), format!("{REGISTRY_KIND},")],
        "the broker manifest declares github-dr-push-token and github-app-algedonic-dev \
         under another kind than the handlers judge"
    );

    let kind = Arc::new(Mutex::new(String::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (served, seen) = (kind.clone(), writes.clone());
    let app = axum::Router::new().fallback(move |r: axum::http::Request<axum::body::Body>| {
        let (served, seen) = (served.clone(), seen.clone());
        async move {
            if r.method() != axum::http::Method::GET {
                seen.lock().unwrap().push(r.uri().to_string());
            }
            if r.uri().path().starts_with("/api/credentials/") {
                let kind = served.lock().unwrap().clone();
                axum::Json(json!({"id":"example-credential","kind":kind}))
            } else {
                axum::Json(packet())
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reader = Arc::new(Reader(AtomicUsize::new(0)));
    let handler = CredentialObserveInstallation::new(&url, reader.clone());
    let ctx = InvocationContext {
        rule_name: "example-observer".into(),
        triggering_event_id: uuid::Uuid::new_v4().to_string(),
        triggering_topic: "step.done.task".into(),
        event_timestamp: Some(chrono::Utc::now()),
        event_payload: json!({"job_id":"00000000-0000-0000-0000-000000000001",
            "step_id":"00000000-0000-0000-0000-000000000002","kind":"task",
            "subject_kind":"custom","subject_id":"example-credential","metadata":{}}),
    };
    let args = vec![
        (
            "credential_id".into(),
            boss_dispatcher::rules::expr::Value::String("example-credential".into()),
        ),
        (
            "permissions".into(),
            boss_dispatcher::rules::expr::Value::String("contents:write".into()),
        ),
    ];
    for other in [
        "github-app-installation",
        "forgejo-access-token",
        "github-app-installation-token ",
        "",
    ] {
        *kind.lock().unwrap() = other.into();
        match handler.invoke(&args, &ctx).await {
            Err(HandlerError::Permanent(why)) => assert_eq!(
                why, "installation credential declaration does not match the selected kind",
                "{other:?}"
            ),
            other_result => panic!("kind {other:?} was not refused: {other_result:?}"),
        }
        assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(writes.lock().unwrap().is_empty());
    }
    // The control's own control: the same server, the registry's kind, and the
    // handler passes the declaration check — it reaches the issuer and the
    // first-record door (which this bare server cannot answer, so it errs later).
    *kind.lock().unwrap() = REGISTRY_KIND.into();
    assert!(handler.invoke(&args, &ctx).await.is_err());
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(writes.lock().unwrap().len(), 1);
    server.abort();
}

/// The live packet's NEXT doors, rehearsed on the real jobs router (backlog
/// 8dbef2a7: the fix must not dead-letter one check further down). The
/// observe step of inspect-a-github-installation 87b68bf3 stands READY and
/// assigned to an agent, not ACTIVE as the test above has it: the rule's
/// actor must still be admitted at the first-record door and then complete
/// the step with the status-only PUT the handler sends.
#[tokio::test]
async fn the_rule_records_and_completes_a_ready_observe_step_assigned_to_an_agent() {
    use boss_core::{
        job::{Job, Priority, StepId, StepStatus, Subject},
        port::EventBus,
        publisher::DomainPublisher,
    };
    use boss_jobs::{
        InMemoryJobs, JobsRepository,
        http::{JobsApiState, router},
    };
    use std::sync::Arc;
    use tower::ServiceExt;
    let workflow = boss_jobs::seed_loader::load_workflows(
        boss_testing::repo_root()
            .join("infra/platform/workflows/inspect-a-github-installation.toml"),
    )
    .unwrap()
    .remove(0);
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "inspect-a-github-installation",
        Subject::new("custom", "example-credential"),
        "Inspect",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = boss_jobs::registry::materialize_steps(
        &workflow,
        &job.subject,
        job.id,
        &job.metadata,
        StepId::new,
    )
    .into_iter()
    .find(|s| s.spec_slug.as_deref() == Some("observe"))
    .unwrap();
    step.status = StepStatus::Ready;
    step.assignee_id = Some("agent-claude".into());
    jobs.add_step(&step).await.unwrap();
    let bus = boss_testing::RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .with_default_rules()
                .build(),
        ),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let actor = boss_dispatcher::rules::actor::dispatcher_actor_header(
        "broker-observes-the-github-installation-grants",
    );
    let send = |method: &str, path: String, body: Value| {
        let (app, actor) = (app.clone(), actor.clone());
        let method = method.to_owned();
        async move {
            app.oneshot(
                axum::http::Request::builder()
                    .method(method.as_str())
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("x-boss-user", actor)
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    let recorded = send(
        "POST",
        format!("/api/jobs/{}/steps/{}/metadata/records", job.id, step.id),
        json!({"key":"observation","value":{"snapshot":{"permissions":{"contents":"read"}}},
            "expected_absence":true}),
    )
    .await;
    assert_eq!(recorded.status(), axum::http::StatusCode::CREATED);
    let completed = send(
        "PUT",
        format!("/api/jobs/{}/steps/{}", job.id, step.id),
        json!({"status":"completed"}),
    )
    .await;
    let status = completed.status();
    let bytes = axum::body::to_bytes(completed.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        status.is_success(),
        "the status-only completion was refused {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Completed
    );
}
