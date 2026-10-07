//! Preparation is not receiver enrollment (partial 1e50e66b).

use boss_core::job::Assurance;
use boss_jobs::{registry::WorkflowSpec, seed_loader::load_workflows};
use serde_json::json;

fn bundled() -> WorkflowSpec {
    load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the native platform bundle must validate")
        .into_iter()
        .find(|row| row.kind == "prepare-a-deposit-key")
        .expect("preparation protocol ships")
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
        let workflow = bundled();
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
}
