//! Approved f623 signer authority is the authenticated gateway session,
//! never the machine caller's asserted role or a runner credential.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use boss_core::job::{Job, Priority, Step, StepField, Subject};
use boss_jobs::http::{JobsApiState, PresenceKey, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

const KEY: &[u8; 32] = b"test-key-0123456789abcdef0123456";

fn cookie_until(expiry: u64) -> String {
    // The exact deployed gateway wire, independent of the new verifier.
    cookie_payload(json!({"u":"david","e":expiry,"r":"platform-admin","i":"emp-david","t":"user"}))
}
fn cookie_payload(value: serde_json::Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(value.to_string());
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(KEY).unwrap();
    mac.update(payload.as_bytes());
    format!(
        "boss_session={payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}
fn cookie() -> String {
    cookie_until(boss_core::presence::now_epoch() + 3600)
}

async fn memory_fixture(signoff: Option<Scope>) -> (axum::Router, Arc<InMemoryJobs>, Step) {
    let mut builder = FakePolicyClient::builder().allow(
        "platform-admin",
        Action::Update,
        Resource::step(),
        Scope::All,
    );
    if let Some(scope) = signoff {
        builder = builder.allow(
            "platform-admin",
            Action::SignOff,
            Resource::new("step-signoff:approver"),
            scope,
        );
    }
    memory_with_policy(Arc::new(builder.build())).await
}
async fn memory_with_policy(
    policy: Arc<dyn boss_policy_client::PolicyClient>,
) -> (axum::Router, Arc<InMemoryJobs>, Step) {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "signer-fixture",
        Subject::new("custom", "approval"),
        "Approve",
        "emp-david",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 6).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Approve", 0);
    step.sign_offs_required = vec!["not-granted".into(), "approver".into()];
    step.fields = vec![
        StepField {
            writer: Some("signer".into()),
            ..StepField::new("decision", "string")
        },
        StepField {
            writer: Some("signer".into()),
            ..StepField::new("comment", "string")
        },
        StepField {
            writer: Some("runner:ops".into()),
            ..StepField::new("plan", "string")
        },
    ];
    jobs.add_step(&step).await.unwrap();
    let bus = RecordingEventBus::new();
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        boss_core::publisher::DomainPublisher::new(bus, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    state.presence_key = Some(Arc::new(PresenceKey::fixed(KEY.to_vec())));
    (router(state), jobs, step)
}

struct PausedPolicy {
    inner: FakePolicyClient,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl boss_policy_client::PolicyClient for PausedPolicy {
    async fn check(
        &self,
        user: &boss_policy_client::User,
        action: Action,
        resource: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        let paused = action == Action::SignOff && resource.as_str() == "step-signoff:approver";
        let decision = self.inner.check(user, action, resource).await?;
        if paused {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(decision)
    }
    async fn scope_predicate(
        &self,
        user: &boss_policy_client::User,
        resource: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        self.inner.scope_predicate(user, resource).await
    }
}
fn paused_policy(scope: Scope) -> Arc<PausedPolicy> {
    Arc::new(PausedPolicy {
        inner: FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:approver"),
                scope,
            )
            .build(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn a_newer_step_row_cannot_be_overwritten_by_earlier_signer_authority() {
    for record in [false, true] {
        let policy = paused_policy(Scope::All);
        let (app, jobs, step) = memory_with_policy(policy.clone()).await;
        let target = step.clone();
        let pending = tokio::spawn(async move {
            write(
                &app,
                &target,
                record,
                &[cookie()],
                forged(),
                if record {
                    json!({"key":"comment","value":"old","expected_absence":true})
                } else {
                    json!({"decision":"approved"})
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), policy.entered.notified())
            .await
            .unwrap();
        let mut newer = step.clone();
        newer.metadata["comment"] = json!("newer record");
        jobs.update_step(&newer).await.unwrap();
        policy.release.notify_one();
        let (status, output) = pending.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{output}");
        assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), newer);
        assert!(jobs.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn a_packet_leaving_the_signers_scope_cannot_receive_the_old_judgment() {
    let policy = paused_policy(Scope::Self_);
    let (app, jobs, step) = memory_with_policy(policy.clone()).await;
    let target = step.clone();
    let pending = tokio::spawn(async move {
        write(
            &app,
            &target,
            false,
            &[cookie()],
            forged(),
            json!({"decision":"approved"}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), policy.entered.notified())
        .await
        .unwrap();
    let mut job = jobs.get_job(&step.job_id).await.unwrap().unwrap();
    job.owner_id = "emp-another".into();
    jobs.update_job(&job).await.unwrap();
    policy.release.notify_one();
    let (status, output) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{output}");
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    assert!(jobs.recorded_events().is_empty());
}

async fn write(
    app: &axum::Router,
    step: &Step,
    record: bool,
    cookies: &[String],
    user: serde_json::Value,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let route = format!(
        "/api/jobs/{}/steps/{}/metadata{}",
        step.job_id,
        step.id,
        if record { "/records" } else { "" }
    );
    let mut request = Request::builder()
        .method(if record { "POST" } else { "PATCH" })
        .uri(route)
        .header("content-type", "application/json")
        .header("x-boss-user", user.to_string());
    for cookie in cookies {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap(),
    )
}
fn forged() -> serde_json::Value {
    json!({"id":"attacker","role":"platform-admin","access_tier":"operator"})
}

#[tokio::test]
async fn every_unverified_or_ambiguous_signer_leaves_both_metadata_doors_unchanged() {
    let valid = cookie();
    let invalid = vec![
        vec![],
        vec!["boss_session=malformed".into()],
        vec![cookie_until(1)],
        vec![valid.clone(), valid.clone()],
        vec![format!("boss_session; {valid}")],
        vec![format!("{valid}; boss_session")],
        vec!["boss_session".into(), valid.clone()],
        vec![valid.clone(), "boss_session".into()],
        vec![format!("{valid}x")],
    ];
    for record in [false, true] {
        for cookies in &invalid {
            let (app, jobs, step) = memory_fixture(Some(Scope::All)).await;
            let body = if record {
                json!({"key":"comment","value":"forged","expected_absence":true})
            } else {
                json!({"decision":"approved"})
            };
            let (status, output) = write(&app, &step, record, cookies, forged(), body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{output}");
            let refusal: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(
                refusal["refused_keys"][0]["key"],
                if record { "comment" } else { "decision" }
            );
            assert_eq!(refusal["refused_keys"][0]["writer"], "signer");
            assert_eq!(refusal["asked_by"], forged()["id"]);
            assert!(
                refusal["door"]
                    .as_str()
                    .unwrap()
                    .contains(&step.id.to_string())
            );
            assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
            assert!(jobs.recorded_events().is_empty());
        }
    }
}

#[tokio::test]
async fn a_signed_session_supplies_its_actual_identity_role_scope_and_event_actor() {
    for record in [false, true] {
        let (app, jobs, step) = memory_fixture(Some(Scope::All)).await;
        let body = if record {
            json!({"key":"comment","value":"reviewed","expected_absence":true})
        } else {
            json!({"decision":"approved"})
        };
        let (status, output) = write(
            &app,
            &step,
            record,
            &[cookie()],
            json!({"id":"anonymous","role":"visitor","access_tier":"user"}),
            body,
        )
        .await;
        assert_eq!(
            status,
            if record {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            },
            "{output}"
        );
        let events = jobs.recorded_events();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| event.payload["_actor"] == "emp-david"),
            "actual cookie identity must sign every effect: {events:?}"
        );
    }
}

#[tokio::test]
async fn signer_authority_needs_its_own_required_role_and_packet_scope() {
    for scope in [None, Some(Scope::Department("another-department".into()))] {
        let (app, jobs, step) = memory_fixture(scope).await;
        let (status, output) = write(
            &app,
            &step,
            false,
            &[cookie()],
            forged(),
            json!({"decision":"approved"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{output}");
        assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
        assert!(jobs.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn unchanged_signer_resend_without_a_cookie_is_stripped_and_records_nothing() {
    let (app, jobs, mut step) = memory_fixture(Some(Scope::All)).await;
    step.metadata["decision"] = json!("approved");
    jobs.update_step(&step).await.unwrap();
    let (status, output) = write(
        &app,
        &step,
        false,
        &[],
        forged(),
        json!({"decision":"approved"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{output}");
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    assert!(jobs.recorded_events().is_empty());
}

#[tokio::test]
async fn a_signer_cannot_write_the_runners_plan_in_a_mixed_patch() {
    let (app, jobs, step) = memory_fixture(Some(Scope::All)).await;
    let (status, output) = write(
        &app,
        &step,
        false,
        &[cookie()],
        forged(),
        json!({"decision":"approved","plan":"PLAN wipe"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{output}");
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    assert!(jobs.recorded_events().is_empty());
}

#[tokio::test]
async fn an_undeclared_metadata_write_retains_its_original_machine_path() {
    let (app, jobs, mut step) = memory_fixture(Some(Scope::All)).await;
    step.fields.iter_mut().for_each(|field| field.writer = None);
    jobs.update_step(&step).await.unwrap();
    let (status, output) = write(
        &app,
        &step,
        false,
        &[],
        forged(),
        json!({"decision":"approved"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{output}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata["decision"],
        "approved"
    );
}

#[tokio::test]
async fn a_signed_automation_session_is_not_a_person_signer() {
    let (app, jobs, step) = memory_fixture(Some(Scope::All)).await;
    let cookie = cookie_payload(
        json!({"u":"automation:ops-runner","e":boss_core::presence::now_epoch()+3600,
        "r":"platform-admin","t":"user"}),
    );
    let (status, output) = write(
        &app,
        &step,
        false,
        &[cookie],
        forged(),
        json!({"decision":"approved"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{output}");
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
}

#[tokio::test]
async fn policy_refusal_precedes_any_step_existence_answer_and_plain_paths_do_not_gain_session_authority()
 {
    let (app, jobs, mut step) = memory_fixture(Some(Scope::All)).await;
    let guest = json!({"id":"anonymous","role":"visitor","access_tier":"user"});
    let (known, known_body) = write(
        &app,
        &step,
        false,
        &[],
        guest.clone(),
        json!({"decision":"approved"}),
    )
    .await;
    let mut missing = step.clone();
    missing.id = boss_core::job::StepId::new();
    let (absent, absent_body) = write(
        &app,
        &missing,
        false,
        &[],
        guest.clone(),
        json!({"decision":"approved"}),
    )
    .await;
    assert_eq!(known, StatusCode::FORBIDDEN);
    assert_eq!(absent, known);
    assert_eq!(known_body, absent_body);
    step.fields.iter_mut().for_each(|field| field.writer = None);
    jobs.update_step(&step).await.unwrap();
    let (plain, output) = write(
        &app,
        &step,
        false,
        &[cookie()],
        guest,
        json!({"decision":"approved"}),
    )
    .await;
    assert_eq!(
        plain,
        StatusCode::FORBIDDEN,
        "a new signer resolver must not grant unrelated ordinary routes: {output}"
    );
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
}

#[tokio::test]
async fn an_actual_runner_credential_cannot_be_replaced_by_a_hostless_signer() {
    for host in ["forge", "boss-gcp"] {
        let (app, jobs, step) = memory_fixture(Some(Scope::All)).await;
        let dir = boss_testing::scratch_dir("signer-runner-conflict");
        boss_testing::write_file(
            &dir.join(format!("{host}.current")),
            "signer-runner-fixture-token",
        );
        let app = boss_jobs::runner_credential::mount(app, dir);
        let response = app
            .oneshot(
                Request::patch(format!(
                    "/api/jobs/{}/steps/{}/metadata",
                    step.job_id, step.id
                ))
                .header("content-type", "application/json")
                .header("cookie", cookie())
                .header("x-boss-user", forged().to_string())
                .header(
                    boss_jobs::runner_credential::HEADER,
                    "signer-runner-fixture-token",
                )
                .body(Body::from(
                    json!({"decision":"approved","plan":"PLAN wipe"}).to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
        assert!(jobs.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn the_real_merge_door_accepts_a_signed_session_for_a_required_signer() {
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "signer-fixture",
        Subject::new("custom", "approval"),
        "Approve",
        "emp-david",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 6).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Approve", 0);
    step.sign_offs_required = vec!["platform-admin".into()];
    step.fields = vec![StepField {
        writer: Some("signer".into()),
        ..StepField::new("decision", "string")
    }];
    jobs.add_step(&step).await.unwrap();
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:platform-admin"),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        boss_core::publisher::DomainPublisher::new(bus, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    state.presence_key = Some(Arc::new(PresenceKey::fixed(KEY.to_vec())));
    let app = router(state);
    let response = app
        .oneshot(
            Request::patch(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
                .header("content-type", "application/json")
                .header("cookie", cookie())
                .header(
                    "x-boss-user",
                    json!({"id":"emp-david","role":"platform-admin","access_tier":"user"})
                        .to_string(),
                )
                .body(Body::from(json!({"decision":"approved"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "the approved authenticated signer must retain its write path"
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().metadata["decision"],
        "approved"
    );
}

#[tokio::test]
async fn signer_expiry_during_the_actual_outbox_write_rolls_back_every_effect() {
    let db = boss_testing::TestDb::new().await;
    let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
    let job = Job::new(
        "signer-fixture",
        Subject::new("custom", "approval"),
        "Approve",
        "emp-david",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 6).unwrap(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Approve", 0);
    step.sign_offs_required = vec!["platform-admin".into()];
    step.fields = vec![StepField {
        writer: Some("signer".into()),
        ..StepField::new("decision", "string")
    }];
    jobs.add_step(&step).await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:platform-admin"),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let mut state = JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        boss_core::publisher::DomainPublisher::new(bus, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    state.presence_key = Some(Arc::new(PresenceKey::fixed(KEY.to_vec())));
    let app = router(state);
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE event_outbox IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let expiry = boss_core::presence::now_epoch() + 2;
    let request = Request::patch(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
        .header("content-type", "application/json")
        .header("cookie", cookie_until(expiry))
        .header(
            "x-boss-user",
            json!({"id":"emp-david","role":"platform-admin","access_tier":"user"}).to_string(),
        )
        .body(Body::from(json!({"decision":"approved"}).to_string()))
        .unwrap();
    let pending = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(1),async {
        loop {
            let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%INSERT INTO event_outbox%'")
                .fetch_one(&db.pool).await.unwrap();
            if waiting>0 {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("actual outbox write must be reached while signer session is valid");
    assert!(boss_core::presence::now_epoch() < expiry);
    while boss_core::presence::now_epoch() <= expiry {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    blocker.rollback().await.unwrap();
    assert!(
        !pending.await.unwrap().status().is_success(),
        "expired signer cannot commit after its actual SQL wait"
    );
    assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), step);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        before
    );
}
