//! Preparation is not receiver enrollment (partial 1e50e66b).

use boss_core::job::Assurance;
use boss_jobs::{registry::WorkflowSpec, seed_loader::load_workflows};
use serde_json::json;

fn bundled() -> WorkflowSpec {
    bundled_kind("prepare-a-deposit-key")
}

fn bundled_kind(kind: &str) -> WorkflowSpec {
    load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the native platform bundle must validate")
        .into_iter()
        .find(|row| row.kind == kind)
        .unwrap_or_else(|| panic!("the platform bundle ships no kind {kind}"))
}

#[test]
fn enrollment_requires_its_complete_public_proposal_after_creation() {
    use boss_core::job::FilledBy;
    use boss_jobs::step_registry::StepRegistry;
    let workflow = bundled();
    use boss_core::job::{JobId, StepId, Subject};
    let empty_steps = boss_jobs::registry::materialize_steps(
        &workflow,
        &Subject::new("custom", "runner-deposit-key"),
        JobId::new(),
        &json!({}),
        StepId::new,
    );
    assert!(boss_jobs::registry::missing_filer_fields(&empty_steps).is_empty());
    let enroll = workflow.steps.iter().find(|s| s.title == "enroll").unwrap();
    let complete = json!({"public_key":"ssh-rsa fixture", "receiver_host":"boss-gcp",
        "purpose":"runner deposit", "decision":"approved", "enrollment_record":"signed proposal"});
    for field in ["public_key", "receiver_host", "purpose"] {
        let declared = enroll.fields.iter().find(|f| f.name == field).unwrap();
        let mut missing = complete.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            StepRegistry::validate_authored_fields(&enroll.fields, &missing).is_err(),
            "{field}"
        );
        missing[field] = json!(null);
        assert!(
            StepRegistry::validate_authored_fields(&enroll.fields, &missing).is_err(),
            "null {field}"
        );
        assert_eq!(
            declared.filled_by,
            FilledBy::Executor,
            "not owed at filing: {field}"
        );
    }
    assert!(StepRegistry::validate_authored_fields(&enroll.fields, &complete).is_ok());
}

#[test]
fn enrollment_completion_refuses_blank_public_proposal_values() {
    use boss_jobs::step_registry::StepRegistry;
    let workflow = bundled();
    let enroll = workflow.steps.iter().find(|s| s.title == "enroll").unwrap();
    let complete = json!({"public_key":"ssh-rsa fixture", "receiver_host":"boss-gcp",
        "purpose":"runner deposit", "decision":"approved", "enrollment_record":"signed proposal"});
    for field in ["public_key", "receiver_host", "purpose"] {
        for blank in ["", " ", "\t\r\n", "\u{2003}"] {
            let mut proposal = complete.clone();
            proposal[field] = json!(blank);
            assert!(
                StepRegistry::validate_authored_fields(&enroll.fields, &proposal).is_err(),
                "native completion accepted blank {field}: {blank:?}"
            );
        }
    }
}

fn permits(predicate: &str, context: &serde_json::Value) -> bool {
    let expression = boss_expr::parse(predicate).unwrap();
    boss_expr::eval(
        &expression,
        &boss_expr::Context {
            payload: context,
            helpers: &boss_expr::NoHelpers,
        },
    )
    .unwrap()
    .as_bool()
    .unwrap()
}

#[test]
fn native_bundle_keeps_public_preparation_distinct_from_human_enrollment() {
    let workflow = bundled();
    for title in ["scope", "enroll", "verify", "revoke"] {
        let step = workflow.steps.iter().find(|s| s.title == title).unwrap();
        assert_eq!(step.metadata_defaults["human_only"], true, "{title}");
    }
    let enroll = workflow.steps.iter().find(|s| s.title == "enroll").unwrap();
    assert_eq!(enroll.assurance_required, Some(Assurance::Presence));
    assert_eq!(enroll.sign_offs_required, vec!["platform-admin"]);
    assert!(
        enroll
            .fields
            .iter()
            .any(|f| f.name == "enrollment_record" && f.required)
    );
    for name in ["public_key", "receiver_host", "purpose"] {
        assert!(enroll.fields.iter().any(|f| f.name == name));
    }
    assert!(
        workflow
            .steps
            .iter()
            .flat_map(|s| &s.fields)
            .all(|f| !f.name.contains("private"))
    );
}

#[test]
fn rejected_or_missing_enrollment_never_opens_verification() {
    let workflow = bundled();
    let verify = workflow.steps.iter().find(|s| s.title == "verify").unwrap();
    for decision in [json!("rejected"), json!(null), json!("pending")] {
        assert!(!permits(
            &verify.ready_when,
            &json!({"steps":{"enroll":{"done":true,"metadata":{"decision":decision}}}})
        ));
    }
    assert!(!permits(
        &verify.ready_when,
        &json!({"steps":{"enroll":{"done":false,"metadata":{"decision":"approved"}}}})
    ));
    assert!(permits(
        &verify.ready_when,
        &json!({"steps":{"enroll":{"done":true,"metadata":{"decision":"approved"}}}})
    ));
}

#[test]
fn abandonment_requires_a_scoped_packet_and_explicit_recorded_marker() {
    let workflow = bundled();
    let abort = workflow
        .steps
        .iter()
        .find(|s| s.title == "abandoned")
        .unwrap();
    assert_eq!(abort.metadata_defaults["outcome_kind"], "aborted");
    assert!(!permits(
        &abort.ready_when,
        &json!({"steps":{"scope":{"done":true}},"job":{"metadata":{}}})
    ));
    assert!(!permits(
        &abort.ready_when,
        &json!({"steps":{"scope":{"done":false}},"job":{"metadata":{"abandoned":"true"}}})
    ));
    assert!(permits(
        &abort.ready_when,
        &json!({"steps":{"scope":{"done":true}},"job":{"metadata":{"abandoned":"true"}}})
    ));
}

#[test]
fn preparation_steps_route_to_their_actual_machine_or_human_executor() {
    use boss_jobs::{
        audience::Audience, orphan_steps::orphan_steps, station_projection::derived_stations,
        step_registry::StepRegistry,
    };
    let workflow = bundled();
    let rule_path = std::path::Path::new(boss_jobs::registry::platform_bundle_path())
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("dispatcher/rules/broker-prepares-the-runner-deposit-key.toml");
    let rule: toml::Value = toml::from_str(&std::fs::read_to_string(rule_path).unwrap()).unwrap();
    let executor = format!("rule:{}", rule["rule"][0]["name"].as_str().unwrap());
    for slug in ["issue", "install"] {
        let step = workflow.steps.iter().find(|s| s.title == slug).unwrap();
        assert_eq!(
            step.audience,
            Some(Audience::Role("platform-admin".into())),
            "{slug}"
        );
        assert_eq!(step.selectors().assignee_id, None);
        assert_eq!(step.metadata_defaults["written_by"], executor);
        assert_eq!(
            boss_jobs::active_holder::declared_executor(&workflow, Some(slug)),
            None,
            "routing must not grant claim-for authority"
        );
    }
    for slug in ["verify", "revoke"] {
        let step = workflow.steps.iter().find(|s| s.title == slug).unwrap();
        assert_eq!(
            step.audience,
            Some(Audience::Role("platform-admin".into())),
            "{slug}"
        );
        assert_eq!(step.metadata_defaults["human_only"], true);
    }
    let workflows = std::slice::from_ref(&workflow);
    let stations = derived_stations(workflows, &[], chrono::Utc::now());
    assert!(orphan_steps(workflows, &stations, &StepRegistry::v1()).is_empty());
    let mut stripped = workflow.clone();
    for step in &mut stripped.steps {
        if ["issue", "install", "verify", "revoke"].contains(&step.title.as_str()) {
            step.audience = None;
            step.authority_role = None;
        }
    }
    let workflows = std::slice::from_ref(&stripped);
    let stations = derived_stations(workflows, &[], chrono::Utc::now());
    let orphans = orphan_steps(workflows, &stations, &StepRegistry::v1());
    let names: Vec<_> = orphans.iter().map(|o| o.step.as_str()).collect();
    assert_eq!(names, ["install", "issue", "revoke", "verify"]);
}

// Exercise the published protocol through its real HTTP boundaries. A
// role routes work; it must not implicitly delegate claim-for authority.
mod native_boundary {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use boss_core::{
        job::{Job, JobStatus, Priority, Step, StepId, StepStatus, Subject},
        port::EventBus,
        publisher::DomainPublisher,
    };
    use boss_jobs::{
        InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry,
        http::{JobsApiState, router},
        registry::materialize_steps,
    };
    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
    use boss_testing::RecordingEventBus;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    const RULE: &str = "rule:broker-prepares-the-runner-deposit-key";

    struct Roster;
    #[async_trait::async_trait]
    impl boss_jobs::owner_resolution::RosterLookup for Roster {
        async fn active_holders(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec!["emp-david".into()])
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(["emp-david", "emp-other"].contains(&id))
        }
    }

    async fn fixture(slug: &str) -> (Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>, Job, Step) {
        fixture_of(bundled(), slug).await
    }

    async fn fixture_of(
        workflow: WorkflowSpec,
        slug: &str,
    ) -> (Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>, Job, Step) {
        let kinds = Arc::new(InMemoryWorkflows::for_fixture());
        kinds.seed(workflow.clone()).unwrap();
        let jobs = Arc::new(InMemoryJobs::new());
        let job = Job {
            status: JobStatus::Open,
            metadata: json!({}),
            ..Job::new(
                &workflow.kind,
                Subject::new("custom", "boss-gcp"),
                "Public preparation",
                "emp-david",
                Priority::Standard,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
            )
        };
        jobs.create_job(&job).await.unwrap();
        let mut selected = None;
        for mut step in
            materialize_steps(&workflow, &job.subject, job.id, &job.metadata, StepId::new)
        {
            match step.spec_slug.as_deref() {
                Some("scope") => step.status = StepStatus::Completed,
                Some("issue") if slug == "install" => {
                    step.status = StepStatus::Completed;
                    step.metadata["issued"] = json!("earlier public issue receipt");
                }
                Some(s) if s == slug => {
                    step.status = StepStatus::Ready;
                    selected = Some(step.clone());
                }
                _ => {}
            }
            jobs.add_step(&step).await.unwrap();
        }
        let step = selected.unwrap();
        let bus = RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        let agents: Arc<dyn boss_jobs::agents::AgentsRegistry> = Arc::new(
            boss_jobs::agents::InMemoryAgents::new()
                .with_agent("agent-codex", ["codex@algedonic.dev"]),
        );
        let door = Arc::new(boss_jobs::agents::LoginDoor::new(
            agents.clone(),
            DomainPublisher::new(bus_dyn.clone(), "jobs"),
        ));
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
        let app = router(JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            roster: Some(Arc::new(Roster)),
            agent_budget: Some(Arc::new(boss_jobs::agent_budget::BudgetDoor {
                agents,
                runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
            })),
            ..JobsApiState::minimal(
                jobs.clone(),
                bus.clone(),
                DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        })
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            door,
            boss_jobs::agents::resolve_login,
        ));
        (app, jobs, bus, job, step)
    }

    async fn send(
        app: &Router,
        job: &Job,
        step: &Step,
        method: &str,
        suffix: &str,
        id: &str,
        role: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(Request::builder()
            .method(method)
            .uri(format!("/api/jobs/{}/steps/{}{}", job.id, step.id, suffix))
            .header("content-type", "application/json")
            .header("x-boss-user", json!({"id":id,"role":role,"access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}).to_string())
            .body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            json!(null)
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
        };
        (status, body)
    }

    #[tokio::test]
    async fn role_routing_does_not_delegate_claim_for_or_bypass_policy() {
        for slug in ["issue", "install"] {
            let (app, jobs, bus, job, step) = fixture(slug).await;
            let before = bus.event_count();
            let (status, body) = send(
                &app,
                &job,
                &step,
                "POST",
                "/claim?claimed_for=emp-other",
                RULE,
                "platform-admin",
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(body.to_string().contains("step-assign"), "{body}");
            assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
            assert_eq!(bus.event_count(), before);
            let (status, _) = send(
                &app,
                &job,
                &step,
                "POST",
                "/claim",
                RULE,
                "unrelated",
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(bus.event_count(), before);
            let (status, body) = send(
                &app,
                &job,
                &step,
                "POST",
                "/claim",
                RULE,
                "platform-admin",
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
            assert_eq!(stored.status, StepStatus::Active);
            assert_eq!(stored.assignee_id.as_deref(), Some(RULE));
        }
    }

    #[tokio::test]
    async fn receipts_are_required_and_the_exact_machine_writer_is_preserved() {
        for (slug, field) in [("issue", "issued"), ("install", "installed")] {
            let (app, jobs, bus, job, step) = fixture(slug).await;
            let before = bus.event_count();
            let (status, body) = send(
                &app,
                &job,
                &step,
                "PUT",
                "",
                RULE,
                "platform-admin",
                json!({"status":"completed"}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
            assert_eq!(bus.event_count(), before);
            for actor in ["automation:other", "agent-codex", "codex@algedonic.dev"] {
                let (status, body) = send(
                    &app,
                    &job,
                    &step,
                    "PATCH",
                    "/metadata",
                    actor,
                    "platform-admin",
                    json!({field:"public receipt"}),
                )
                .await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {body}");
                assert_eq!(bus.event_count(), before);
            }
            let (status, body) = send(
                &app,
                &job,
                &step,
                "PATCH",
                "/metadata",
                RULE,
                "platform-admin",
                json!({"written_by":null}),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(bus.event_count(), before);
            let (status, body) = send(
                &app,
                &job,
                &step,
                "PATCH",
                "/metadata",
                RULE,
                "platform-admin",
                json!({field:"public receipt"}),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            assert_eq!(
                jobs.get_step(&step.id).await.unwrap().unwrap().metadata[field],
                "public receipt"
            );
            // written_by guards field changes, not status. A normally
            // authorised machine can complete already-recorded evidence.
            let (status, body) = send(
                &app,
                &job,
                &step,
                "PUT",
                "",
                "automation:other",
                "platform-admin",
                json!({"status":"completed"}),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
            assert_eq!(stored.status, StepStatus::Completed);
            assert_eq!(stored.metadata[field], "public receipt");
            assert_eq!(
                stored.completed_by.map(|a| a.to_string()).as_deref(),
                Some("automation:other")
            );
        }
    }

    #[tokio::test]
    async fn human_override_remains_a_named_human_act() {
        let (app, jobs, _, job, step) = fixture("issue").await;
        let (status, body) = send(
            &app,
            &job,
            &step,
            "PATCH",
            "/metadata",
            "emp-david",
            "platform-admin",
            json!({"issued":"explicit human recovery receipt"}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (status, body) = send(
            &app,
            &job,
            &step,
            "PUT",
            "",
            "emp-david",
            "platform-admin",
            json!({"status":"completed"}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(
            stored.completed_by.map(|a| a.to_string()).as_deref(),
            Some("emp-david")
        );
        assert_eq!(stored.metadata["issued"], "explicit human recovery receipt");
    }

    #[tokio::test]
    async fn absent_or_malformed_receipts_cannot_complete_ready_or_active_steps() {
        for (slug, field) in [("issue", "issued"), ("install", "installed")] {
            for active in [false, true] {
                let (app, jobs, bus, job, step) = fixture(slug).await;
                if active {
                    let (status, body) = send(
                        &app,
                        &job,
                        &step,
                        "POST",
                        "/claim",
                        RULE,
                        "platform-admin",
                        json!({}),
                    )
                    .await;
                    assert_eq!(status, StatusCode::OK, "{body}");
                }
                for value in [json!(null), json!(37)] {
                    let (status, body) = send(
                        &app,
                        &job,
                        &step,
                        "PATCH",
                        "/metadata",
                        RULE,
                        "platform-admin",
                        json!({field:value}),
                    )
                    .await;
                    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
                    let before = jobs.get_step(&step.id).await.unwrap().unwrap();
                    let count = bus.event_count();
                    let (status, body) = send(
                        &app,
                        &job,
                        &step,
                        "PUT",
                        "",
                        RULE,
                        "platform-admin",
                        json!({"status":"completed"}),
                    )
                    .await;
                    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
                    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), before);
                    assert_eq!(bus.event_count(), count);
                }
            }
        }
    }

    /// The rule row of the machine token's deposit key, read from the
    /// tree: the actor its handler signs as, and the protocol kind it
    /// declares its packets are.
    fn machine_token_rule() -> (String, String) {
        let path = std::path::Path::new(boss_jobs::registry::platform_bundle_path())
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("dispatcher/rules/broker-prepares-the-machine-token-deposit-key.toml");
        let rule: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let actor = format!("rule:{}", rule["rule"][0]["name"].as_str().unwrap());
        let kind = rule["rule"][0]["do"][0]["args"]["protocol_kind"]
            .as_str()
            .unwrap()
            .trim_matches('"')
            .to_string();
        (actor, kind)
    }

    /// THE MACHINE TOKEN KEY'S RULE CAN WRITE THE RECEIPTS OF THE
    /// PROTOCOL IT NAMES (adversarial review ecd9263a, F1; backlog
    /// 88379df3). The handler signs every write as `rule:<the rule that
    /// fired>`, and a step's `written_by` believes ONE machine actor. The
    /// first draft pointed the new rule at prepare-a-deposit-key, whose
    /// `issue` and `install` name the RUNNER's rule: sent through the
    /// real router, both of the handler's receipts answered 403, so after
    /// the human scope the pair was minted, the line landed on `enroll`,
    /// and `enroll` could never become ready. The handler's own tests ran
    /// against a stub jobs server that enforces nothing. This sends the
    /// handler's own bodies, as the rule's own actor, through the real
    /// router against the bundled protocol the rule names — and holds
    /// the property the declaration exists for: the other broker rule,
    /// and every other automation, is still refused.
    #[tokio::test]
    async fn the_machine_token_keys_rule_writes_the_receipts_of_the_protocol_it_names() {
        let (actor, kind) = machine_token_rule();
        let workflow = bundled_kind(&kind);
        for (slug, field, evidence) in [
            (
                "issue",
                "issued",
                "Broker prepared an SSH pair; public key awaits receiver enrollment",
            ),
            (
                "install",
                "installed",
                "Pair and packet provenance read back from declared Secret; receiver installation unproved",
            ),
        ] {
            let (app, jobs, _, job, step) = fixture_of(workflow.clone(), slug).await;
            // The handler's own receipt body (broker_transport_key.rs).
            let body = json!({
                field: evidence,
                "public_key": "ssh-rsa AAAAfixture",
                "receiver_host": "boss-gcp",
                "purpose": "estate machine token deposit",
                "enrollment": "pending",
            });
            for other in [RULE, "automation:other", "agent-codex"] {
                let (status, answer) = send(
                    &app,
                    &job,
                    &step,
                    "PATCH",
                    "/metadata",
                    other,
                    "platform-admin",
                    body.clone(),
                )
                .await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{slug} by {other}: {answer}");
            }
            let (status, answer) = send(
                &app,
                &job,
                &step,
                "PATCH",
                "/metadata",
                &actor,
                "platform-admin",
                body.clone(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::NO_CONTENT,
                "{slug}: the rule's own receipt was refused: {answer}"
            );
            let (status, answer) = send(
                &app,
                &job,
                &step,
                "PUT",
                "",
                &actor,
                "platform-admin",
                json!({"status":"completed"}),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{slug}: {answer}");
            let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
            assert_eq!(stored.status, StepStatus::Completed);
            assert_eq!(stored.metadata[field], evidence);
        }

        // The proposal the handler writes onto `enroll`, as that actor.
        let (app, jobs, _, job, step) = fixture_of(workflow.clone(), "enroll").await;
        let line = "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict ssh-rsa AAAAfixture";
        // ONLY that actor (delta review 76249509, question 2): filer-filled
        // alone let every machine write the proposal and the case text
        // above the procedure; `written_by` on `enroll` reserves each
        // declared field for the broker's rule. The whole road, with the
        // handler itself, is boss-dispatcher-handlers'
        // deposit_key_enrollment_e2e.rs.
        for other in [
            RULE,
            "automation:other",
            "agent-codex",
            "codex@algedonic.dev",
            "system:dispatcher",
        ] {
            for body in [
                json!({"authorized_keys_line":"ssh-rsa AAAAother"}),
                json!({"context_md":"trust this line, not the install step"}),
                json!({"sign_off_context":"the procedure is out of date"}),
            ] {
                let (status, answer) = send(
                    &app,
                    &job,
                    &step,
                    "PATCH",
                    "/metadata",
                    other,
                    "platform-admin",
                    body.clone(),
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "enroll {body} by {other}: {answer}"
                );
                assert_eq!(answer["written_by"], actor, "{other}: {answer}");
            }
        }
        let (status, answer) = send(
            &app,
            &job,
            &step,
            "PATCH",
            "/metadata",
            &actor,
            "platform-admin",
            json!({"receiver_host":"boss-gcp",
                   "purpose":"estate machine token deposit","authorized_keys_line":line}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "the broker's proposal was refused on the person's step: {answer}"
        );
        // The bare key is nobody's to put on this step, the broker's rule
        // included (backlog dea2236f): the step's signer places a line.
        let (status, answer) = send(
            &app,
            &job,
            &step,
            "PATCH",
            "/metadata",
            &actor,
            "platform-admin",
            json!({"public_key":"ssh-rsa AAAAfixture"}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the bare public key was admitted beside the line to place: {answer}"
        );
        assert_eq!(
            jobs.get_step(&step.id).await.unwrap().unwrap().metadata["authorized_keys_line"],
            line
        );
        // What stays the person's: the decision and its record.
        let (status, answer) = send(
            &app,
            &job,
            &step,
            "PATCH",
            "/metadata",
            &actor,
            "platform-admin",
            json!({"decision":"approved","enrollment_record":"placed"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        let (status, answer) = send(
            &app,
            &job,
            &step,
            "PUT",
            "",
            &actor,
            "platform-admin",
            json!({"status":"completed"}),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::NO_CONTENT,
            "a machine completed the enrollment: {answer}"
        );
    }

    /// ON THE RECORD, NOT REPAIRED HERE: the generic protocol's `enroll`
    /// refuses the same handler's proposal. Its three proposal fields are
    /// executor-filled on a human-only step, where a machine may write
    /// only context or a filer-filled field, so the runner key's own rule
    /// is refused at that write too — the second refusal on the road the
    /// reviewer's F1 found the first of. prepare-a-deposit-key is live as
    /// v1 and has never run; repairing it is a new version of a row the
    /// runner's delivery owns (1e50e66b). When that lands, this leg turns
    /// and is deleted with the fix.
    #[tokio::test]
    async fn the_generic_protocols_enroll_still_refuses_the_brokers_proposal() {
        let (app, _, _, job, step) = fixture("enroll").await;
        let (status, answer) = send(
            &app,
            &job,
            &step,
            "PATCH",
            "/metadata",
            RULE,
            "platform-admin",
            json!({"public_key":"ssh-rsa AAAAfixture","receiver_host":"boss-gcp",
                   "purpose":"ops-runner credential deposit"}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "prepare-a-deposit-key's enroll now takes the broker's proposal — delete this \
             leg and say so on 1e50e66b: {answer}"
        );
        assert!(answer.to_string().contains("human_only"), "{answer}");
    }

    /// THE STEP NAMES THE LINE ITS SIGNER PLACES (review ecd9263a, F4).
    /// The line is placed on the host BEFORE the sign-off, so the step
    /// must declare it — required at done — and its procedure must say
    /// what to check in it. And the per-key protocol is the generic one
    /// in every contract that matters: the same steps, kinds, human
    /// gates and presence sign-off, differing in the two writers and in
    /// what `enroll` declares.
    #[test]
    fn the_per_key_protocol_declares_the_line_and_keeps_every_human_gate() {
        use boss_jobs::step_registry::StepRegistry;
        let (actor, kind) = machine_token_rule();
        assert_ne!(
            kind, "prepare-a-deposit-key",
            "the generic protocol's receipts are the runner rule's to write"
        );
        let own = bundled_kind(&kind);
        let generic = bundled();
        let shape =
            |w: &WorkflowSpec| -> Vec<(String, String, Option<Assurance>, Vec<String>, bool)> {
                w.steps
                    .iter()
                    .map(|s| {
                        (
                            s.title.clone(),
                            s.kind.clone(),
                            s.assurance_required,
                            s.sign_offs_required.clone(),
                            s.metadata_defaults["human_only"] == true,
                        )
                    })
                    .collect()
            };
        assert_eq!(shape(&own), shape(&generic));
        for slug in ["issue", "install"] {
            let step = own.steps.iter().find(|s| s.title == slug).unwrap();
            assert_eq!(step.metadata_defaults["written_by"], actor, "{slug}");
        }
        let enroll = own.steps.iter().find(|s| s.title == "enroll").unwrap();
        // The three proposal fields are SUPPLIED to the person: filer-filled
        // (so the broker may write them onto a human-only step) and not
        // owed at filing (nobody holds a key then). The decision and its
        // record are the person's, required at done.
        //
        // AND THE STEP DECLARES NO BARE KEY (backlog dea2236f; packet
        // 96a0f7bb, 2026-10-07). `public_key` stood here beside the line,
        // was the one copied into authorized_keys, and opened a shell. A
        // step whose signer places a line shows the line; the key to
        // compare it with is the install step's.
        use boss_core::job::FilledBy;
        assert!(
            enroll.fields.iter().all(|f| f.name != "public_key"),
            "enroll declares a bare public_key beside the line its signer places"
        );
        for name in ["receiver_host", "purpose", "authorized_keys_line"] {
            let field = enroll
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("enroll must declare {name}"));
            assert_eq!(field.filled_by, FilledBy::Filer, "{name}");
            assert!(
                !field.required,
                "{name} would be owed when the packet is filed"
            );
        }
        for name in ["decision", "enrollment_record"] {
            let field = enroll.fields.iter().find(|f| f.name == name).unwrap();
            assert_eq!(field.filled_by, FilledBy::Executor, "{name}");
            assert!(field.required, "{name}");
        }
        // The proposal and the case text above the procedure are the
        // broker's rule's ALONE (delta review 76249509, question 2):
        // `written_by` judges a step's DECLARED fields, so the step names
        // the writer and declares the two context keys beside the three
        // proposal fields.
        assert_eq!(enroll.metadata_defaults["written_by"], actor, "enroll");
        for name in ["context_md", "sign_off_context"] {
            let field = enroll
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("enroll must declare {name} for written_by to cover it"));
            assert_eq!(field.filled_by, FilledBy::Filer, "{name}");
            assert!(!field.required, "{name}");
        }
        use boss_core::job::{JobId, StepId, Subject};
        let empty = boss_jobs::registry::materialize_steps(
            &own,
            &Subject::new("custom", "machine-token-deposit-key"),
            JobId::new(),
            &json!({}),
            StepId::new,
        );
        assert!(
            boss_jobs::registry::missing_filer_fields(&empty).is_empty(),
            "the packet must be fileable before any key exists"
        );
        // What holds the proposal present: the step is ready only after
        // `install`, and the handler writes the proposal before it
        // completes `issue` and `install`.
        assert!(format!("{:?}", enroll.ready_when).contains("steps.install.done"));
        // What the signer checks the line against is a field `written_by`
        // reserves for the broker's rule.
        for slug in ["issue", "install"] {
            let step = own.steps.iter().find(|s| s.title == slug).unwrap();
            let key = step
                .fields
                .iter()
                .find(|f| f.name == "public_key")
                .unwrap_or_else(|| panic!("{slug} must declare public_key"));
            assert!(key.required, "{slug}.public_key");
            assert!(
                StepRegistry::validate_authored_fields(
                    &step.fields,
                    &json!({"issued":"r","installed":"r","public_key":"ssh-rsa fixture"})
                )
                .is_ok()
            );
        }
        let procedure = enroll.metadata_defaults["procedure"].as_str().unwrap();
        for said in [
            "authorized_keys_line",
            "command=",
            "restrict",
            "machine-token",
            "install step",
        ] {
            assert!(
                procedure.contains(said),
                "the procedure must name `{said}`: {procedure}"
            );
        }
        // THE SENTENCES, NOT THE KEYWORDS (delta review 76249509, mutants
        // N8 and N14). The procedure is the last control a person holds
        // before pasting a line into authorized_keys, and the keyword
        // list above let two rewrites through: the required prefix
        // without `,restrict` (the word survives later in the text), and
        // the check sent to this step instead of the install step. So:
        // the prefix the line MUST BEGIN WITH is the rule row's own
        // forced command wrapped as the handler wraps it, in one
        // sentence with where the line must end; and the check is sent
        // to the install step by name.
        let forced = machine_token_rule_arg("forced_command");
        let must_begin = format!(
            "it must begin command=\"{forced}\",restrict and end with the public_key on this \
             packet's install step, with nothing between and nothing after."
        );
        assert!(
            procedure.contains(&must_begin),
            "the procedure must state the whole required prefix `{must_begin}`: {procedure}"
        );
        assert!(
            procedure.contains("Check it against the INSTALL step, not this one:"),
            "the procedure must send the check to the install step: {procedure}"
        );
        assert!(
            procedure
                .contains("a missing restrict, or a second key is a refusal: abandon the packet."),
            "{procedure}"
        );
        // WHAT THE FIRST LIVE ENROLMENT TAUGHT, AS SENTENCES (backlog
        // dea2236f): what is placed is the line and never the key, the
        // install step's key is for comparing, and the push reads the
        // placement before the passkey does — with each of its three
        // closes named, the shell among them.
        for sentence in [
            "PLACE THE authorized_keys_line, WHOLE, AND NOTHING ELSE:",
            "the public key alone is never what you place.",
            "A bare key in authorized_keys is a login, not a deposit key:",
            "The install step's public_key is there to COMPARE with, never to place.",
            "THEN LET THE PUSH READ YOUR PLACEMENT BEFORE YOU SIGN.",
            "Wait for the first maintenance-machine-token-push packet opened after you placed the line,",
            "put that packet's id in enrollment_record and sign.",
            "Failed, saying this key reaches a shell: the line went in without its command= prefix, so remove it at once,",
            "so do not sign.",
        ] {
            assert!(
                procedure.contains(sentence),
                "the procedure must say `{sentence}`: {procedure}"
            );
        }
        assert!(
            !procedure.contains("signs this step's public_key"),
            "the passkey signs no bare key on this step: {procedure}"
        );
    }

    fn machine_token_rule_arg(name: &str) -> String {
        let path = std::path::Path::new(boss_jobs::registry::platform_bundle_path())
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("dispatcher/rules/broker-prepares-the-machine-token-deposit-key.toml");
        let rule: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rule["rule"][0]["do"][0]["args"][name]
            .as_str()
            .unwrap_or_else(|| panic!("the rule row declares no {name}"))
            .trim_matches('"')
            .to_string()
    }
}
