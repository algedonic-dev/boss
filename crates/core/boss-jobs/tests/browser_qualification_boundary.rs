use axum::{Router, body::Body, http::Request};
use boss_core::{
    job::{Job, Priority, StepId, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::{
    InMemoryJobs, JobsRepository,
    http::{JobsApiState, router},
    registry::{InMemoryWorkflows, materialize_steps},
};
use boss_policy_client::{AccessTier, FakePolicyClient, User};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

#[path = "browser_qualification_boundary/protocol.rs"]
mod protocol;
#[path = "browser_qualification_boundary/refusals.rs"]
mod refusals;

async fn call(app: &Router, method: &str, path: String, value: Value) -> (u16, Value) {
    call_as(app, "emp-observer", method, path, value).await
}
/// The fixed gateway-session key the fixture's jobs API verifies with.
const SESSION_KEY: &[u8; 32] = b"test-key-0123456789abcdef0123456";

/// The gateway's session cookie for `id` as platform-admin, signed with
/// [`SESSION_KEY`] — the wire shape the deployed gateway writes (pinned in
/// only_the_signer_writes_an_ops_request_decision.rs).
fn session_cookie(id: &str) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, KeyInit, Mac};
    let payload = URL_SAFE_NO_PAD.encode(
        json!({"u": id, "e": boss_core::presence::now_epoch() + 3600,
            "r": "platform-admin", "i": id, "t": "user"})
        .to_string(),
    );
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(SESSION_KEY).unwrap();
    mac.update(payload.as_bytes());
    format!(
        "boss_session={payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

/// A person's signed-in browser: the gateway-verified session cookie beside
/// the identity the gateway injects (rework of car b16e373d, 2026-10-19).
async fn call_signed(
    app: &Router,
    actor: &str,
    method: &str,
    path: String,
    value: Value,
) -> (u16, Value) {
    call_with(
        app,
        actor,
        Some(&session_cookie(actor)),
        method,
        path,
        value,
    )
    .await
}

async fn call_as(
    app: &Router,
    actor: &str,
    method: &str,
    path: String,
    value: Value,
) -> (u16, Value) {
    call_with(app, actor, None, method, path, value).await
}

async fn call_with(
    app: &Router,
    actor: &str,
    cookie: Option<&str>,
    method: &str,
    path: String,
    value: Value,
) -> (u16, Value) {
    let (code, text) = call_text(app, actor, cookie, method, path, value).await;
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    println!("HTTP {method}: {code} {body}");
    (code, body)
}

/// The answer's body as sent, for a refusal written as plain text — a JSON
/// read of one is `null`, which can name nothing.
async fn call_text(
    app: &Router,
    actor: &str,
    cookie: Option<&str>,
    method: &str,
    path: String,
    value: Value,
) -> (u16, String) {
    let user = User {
        id: actor.into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("platform".into()),
    };
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(&user).unwrap());
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let code = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (code, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn an_observer_with_ordinary_update_authority_cannot_qualify_without_any_observation() {
    let spec = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap()
    .remove(0);
    let registry = Arc::new(InMemoryWorkflows::for_bootstrap());
    registry.seed(spec.clone()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
    let mut job = Job::new(
        "ship-a-change",
        Subject::new("custom", "synthetic-review-control"),
        "Landed visual claim",
        "emp-observer",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    job.workflow_version = spec.version;
    job.metadata = json!({"merged":"true"});
    jobs.create_job(&job).await.unwrap();
    let mut steps = materialize_steps(&spec, &job.subject, job.id, &job.metadata, StepId::new);
    for step in &mut steps {
        if ["opened", "scope", "build", "gate", "review"]
            .contains(&step.spec_slug.as_deref().unwrap_or(""))
        {
            step.status = StepStatus::Completed;
        }
        if step.spec_slug.as_deref() == Some("review") {
            step.metadata["release"] = json!({"review":"existing-independent-source-review"});
        }
    }
    let terminal = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("attended-qualified"))
        .unwrap()
        .id;
    let proven = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("proven"))
        .unwrap()
        .id;
    for step in steps {
        jobs.add_step(&step).await.unwrap();
    }
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy = Arc::new(FakePolicyClient::builder().with_default_rules().build());
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    state.kind_registry = Some(registry);
    let app = router(state);
    // No browser-observation job, observation original, reviewer, file listing or attachment exists.
    // These are the unchanged public update/first-record doors, not privileged correction or direct repo writes.
    let record = call(
        &app,
        "POST",
        format!("/api/jobs/{}/steps/{terminal}/metadata/records", job.id),
        json!({"key":"attended_qualification","value":{"passed":true},"expected_absence":true}),
    )
    .await;
    assert_eq!(record.0, 201);
    let without_marker = call(
        &app,
        "PUT",
        format!("/api/jobs/{}/steps/{terminal}", job.id),
        json!({"status":"completed"}),
    )
    .await;
    assert_eq!(without_marker.0, 409);
    let marker = call(
        &app,
        "PATCH",
        format!("/api/jobs/{}/metadata", job.id),
        json!({"attended_qualified":"true"}),
    )
    .await;
    assert_eq!(marker.0, 204);
    let complete = call(
        &app,
        "PUT",
        format!("/api/jobs/{}/steps/{terminal}", job.id),
        json!({"status":"completed"}),
    )
    .await;
    assert_eq!(complete.0, 409);
    assert_eq!(
        call(
            &app,
            "PUT",
            format!("/api/jobs/{}/steps/{terminal}", job.id),
            json!({"status":"skipped"})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            format!("/api/jobs/{}/steps/{terminal}/metadata", job.id),
            json!({"completion_contract":null})
        )
        .await
        .0,
        204
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            format!("/api/jobs/{}/steps/{terminal}", job.id),
            json!({"status":"completed"})
        )
        .await
        .0,
        409
    );
    let mut whole = serde_json::to_value(jobs.get_job(&job.id).await.unwrap().unwrap()).unwrap();
    whole["status"] = json!("closed");
    whole["metadata"]["outcome"] = json!("attended-qualified");
    assert_eq!(
        call(&app, "PUT", format!("/api/jobs/{}", job.id), whole)
            .await
            .0,
        409
    );
    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    let proof = jobs.get_step(&proven).await.unwrap().unwrap();
    println!(
        "FINAL status={:?} outcome={} proof={:?} events={}",
        after.status,
        after.metadata["outcome"],
        proof.status,
        jobs.recorded_events().len()
    );
    println!(
        "COMPLETE_RECORD {}",
        json!({"job":after,"proven":proof,"events":jobs.recorded_events()})
    );
    assert_ne!(
        after.metadata["outcome"], "attended-qualified",
        "no observation or independent qualification exists, so a raw client object must not qualify this car"
    );
}

#[test]
fn conversion_cannot_retroactively_attach_a_typed_evidence_contract_to_completed_work() {
    let to = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap()
    .remove(0);
    let mut from = to.clone();
    from.steps
        .iter_mut()
        .find(|s| s.title == "attended-qualified")
        .unwrap()
        .metadata_defaults
        .as_object_mut()
        .unwrap()
        .remove("completion_contract");
    let mut at =
        std::collections::BTreeMap::from([("attended-qualified".into(), StepStatus::Completed)]);
    assert!(
        !boss_jobs::protocol_conversion::convertibility_for_packet(&from, &to, &at).is_automatic()
    );
    at.insert("attended-qualified".into(), StepStatus::Pending);
    assert!(
        boss_jobs::protocol_conversion::convertibility_for_packet(&from, &to, &at).is_automatic()
    );
}
