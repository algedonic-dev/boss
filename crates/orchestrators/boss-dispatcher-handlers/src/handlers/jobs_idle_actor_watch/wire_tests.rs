#![cfg(test)]

use super::tests::{actor_b, b, ctx, queue_row};
use super::*;

#[tokio::test]
async fn a_lost_record_ack_is_resolved_through_the_real_immutable_door() {
    use boss_clock_client::ClockClient;
    use boss_core::{
        job::{Job, Priority, Step, StepStatus, Subject},
        publisher::DomainPublisher,
    };
    use boss_jobs::{
        InMemoryJobs, JobsRepository,
        http::{JobsApiState, router},
    };
    use boss_policy_client::{FakePolicyClient, Scope, controls};
    let now = boss_clock_client::WallClockClient.now().await.now;
    let jobs = Arc::new(InMemoryJobs::new());
    let job = Job::new(
        "user-feedback",
        Subject::new("custom", "wait-record"),
        "Wait",
        "emp-owner",
        Priority::Urgent,
        now.date_naive(),
    );
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Wait", 0);
    step.status = StepStatus::Ready;
    jobs.add_step(&step).await.unwrap();
    let bus = boss_testing::RecordingEventBus::new();
    let policy = FakePolicyClient::builder()
        .allow(
            "platform-admin",
            controls::READ_JOB.action(),
            controls::READ_JOB.resource(),
            Scope::All,
        )
        .allow(
            "platform-admin",
            controls::UPDATE_STEP.action(),
            controls::UPDATE_STEP.resource(),
            Scope::All,
        )
        .build();
    let app = router(JobsApiState::minimal(
        jobs.clone(),
        bus.clone(),
        DomainPublisher::new(bus, "jobs"),
        Arc::new(policy),
        Arc::new(boss_clock_client::WallClockClient),
    ))
    .layer(axum::middleware::from_fn(
        |request: axum::extract::Request, next: axum::middleware::Next| async move {
            if request
                .headers()
                .get("x-sim-origin")
                .and_then(|value| value.to_str().ok())
                != Some("false")
            {
                eprintln!("real immutable-door request missing x-sim-origin");
                return axum::response::IntoResponse::into_response((
                    axum::http::StatusCode::BAD_REQUEST,
                    "missing x-sim-origin",
                ));
            }
            let posting = request.method() == reqwest::Method::POST;
            let response = next.run(request).await;
            if posting && response.status() == axum::http::StatusCode::CREATED {
                axum::response::IntoResponse::into_response((
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "ack lost after commit",
                ))
            } else {
                response
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut row = queue_row(&job.id.to_string(), "urgent", "task", "ready", None);
    row["step"]["id"] = json!(step.id);
    row["step"]["metadata"]["waiting_on"] =
        json!({"on":"person","id":"emp-david","since":now,"why":"decision"});
    let mut nomination = nominations(&[row], &actor_b(), &b()).remove(0);
    let rule = ctx("2026-10-19T13:40:00Z").rule_name;
    let create_observer = || {
        JobsIdleActorWatch::new(
            format!("http://{address}"),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
            Arc::new(boss_people_client::FakePeopleClient::new().with_employee("emp-david")),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let wait = nomination.waiting_on.clone().unwrap();
    assert!(
        create_observer()
            .original_wait(&nomination, &wait, &b(), now, &rule)
            .await
    );
    let original = jobs
        .first_step_record(&step.id, WAIT_RECORD)
        .await
        .unwrap()
        .unwrap();
    assert!(
        jobs.recorded_events()
            .iter()
            .any(|event| event.id == original.event_id)
    );
    assert!(
        create_observer()
            .original_wait(&nomination, &wait, &b(), now, &rule)
            .await,
        "restart reads the same original"
    );
    nomination.waiting_on.as_mut().unwrap().why = "rewritten".into();
    assert!(
        !create_observer()
            .original_wait(
                &nomination,
                nomination.waiting_on.as_ref().unwrap(),
                &b(),
                now,
                &rule
            )
            .await
    );
    assert_eq!(
        jobs.first_step_record(&step.id, WAIT_RECORD).await.unwrap(),
        Some(original)
    );
    assert_eq!(
        jobs.recorded_events()
            .iter()
            .filter(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
            .count(),
        1,
        "lost ACK and restart never emit another original"
    );
    server.abort();
    let _ = server.await;
}
