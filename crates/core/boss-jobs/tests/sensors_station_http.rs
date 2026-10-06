use std::sync::Arc;

use axum::{body::Body, http::Request};
use boss_core::{port::EventBus, publisher::DomainPublisher};
use boss_jobs::JobsRepository;
use boss_jobs::sensors::{
    BatchOutcome, InMemorySensors, NewReading, PollStamp, Reading, ReadingsWindow, SensorInput,
    SensorRow, Sensors, SensorsError,
};
use boss_jobs::{
    InMemoryJobs,
    http::{JobsApiState, router},
};
use boss_policy_client::{AccessTier, Action, FakePolicyClient, Resource, Scope, User};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

struct ObservedSensors {
    inner: Arc<InMemorySensors>,
    reads: AtomicUsize,
    fail: bool,
}
#[async_trait::async_trait]
impl Sensors for ObservedSensors {
    async fn list(&self) -> Result<Vec<SensorRow>, SensorsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(SensorsError::Storage("fixture unavailable".into()))
        } else {
            self.inner.list().await
        }
    }
    async fn unstamped(&self, id: &str) -> Result<Vec<Reading>, SensorsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.unstamped(id).await
    }
    async fn window(
        &self,
        id: &str,
        since: chrono::DateTime<chrono::Utc>,
        until: chrono::DateTime<chrono::Utc>,
    ) -> Result<ReadingsWindow, SensorsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.window(id, since, until).await
    }
    async fn publish(&self, t: &str, s: &[SensorInput]) -> Result<BatchOutcome, SensorsError> {
        self.inner.publish(t, s).await
    }
    async fn record(&self, id: &str, r: &[NewReading]) -> Result<BatchOutcome, SensorsError> {
        self.inner.record(id, r).await
    }
    async fn stamp(&self, id: &str, e: &str, p: &str) -> Result<(), SensorsError> {
        self.inner.stamp(id, e, p).await
    }
    async fn mark_polled(&self, id: &str, s: &PollStamp) -> Result<(), SensorsError> {
        self.inner.mark_polled(id, s).await
    }
    async fn sweep(&self, before: chrono::DateTime<chrono::Utc>) -> Result<u64, SensorsError> {
        self.inner.sweep(before).await
    }
}

fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
}
fn state() -> JobsApiState<InMemoryJobs, RecordingEventBus> {
    let bus = RecordingEventBus::new();
    JobsApiState::minimal(
        Arc::new(InMemoryJobs::new()),
        bus.clone(),
        DomainPublisher::new(bus as Arc<dyn EventBus>, "jobs"),
        Arc::new(
            FakePolicyClient::builder()
                .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                .build(),
        ),
        Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: at("2026-10-02T12:00:00Z"),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
    )
}
fn user(tier: AccessTier) -> User {
    User {
        id: "reader".into(),
        role: "platform-admin".into(),
        access_tier: tier,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    }
}
async fn map(state: JobsApiState<InMemoryJobs, RecordingEventBus>, tier: AccessTier) -> Value {
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/yard/regions?window=24h")
                .header("x-boss-user", serde_json::to_string(&user(tier)).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
fn sensors(body: &Value) -> &Value {
    body["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "sensors")
        .unwrap()
}
async fn repo(source: &str, observed: Option<&str>) -> Arc<InMemorySensors> {
    let repo = Arc::new(InMemorySensors::new());
    repo.publish(
        "test",
        &[SensorInput {
            id: "source-one".into(),
            source: source.into(),
            credential: if source == "site" { "" } else { "cred" }.into(),
            every_minutes: if source == "site" { 0 } else { 5 },
            opens: "receive-message".into(),
            subject_kind: "message".into(),
            enabled: true,
            selector: None,
        }],
    )
    .await
    .unwrap();
    if let Some(observed) = observed {
        repo.record(
            "source-one",
            &[NewReading {
                external_id: "reading-one".into(),
                observed_at: at(observed),
                payload: serde_json::json!({}),
            }],
        )
        .await
        .unwrap();
    }
    repo
}

#[tokio::test]
async fn operator_and_auditor_read_real_registry_while_user_tier_keeps_map_without_sensor_authority()
 {
    for tier in [AccessTier::Operator, AccessTier::Auditor] {
        let body = map(
            JobsApiState {
                sensors: Some(repo("stripe", None).await),
                ..state()
            },
            tier,
        )
        .await;
        assert_eq!(sensors(&body)["count"], 1);
        assert_eq!(sensors(&body)["state"], "clear");
    }
    let baseline = map(state(), AccessTier::User).await;
    let denied = map(
        JobsApiState {
            sensors: Some(repo("stripe", None).await),
            ..state()
        },
        AccessTier::User,
    )
    .await;
    assert_eq!(sensors(&denied)["count"], Value::Null);
    assert!(
        sensors(&denied)["why"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    assert_eq!(
        baseline["regions"][0], denied["regions"][0],
        "ordinary map boundary unchanged"
    );
}

#[tokio::test]
async fn only_unstamped_polled_readings_strictly_older_than_the_declared_interval_need_attention() {
    for (source, observed, expected) in [
        ("stripe", "2026-10-02T11:54:59Z", "attention"),
        ("stripe", "2026-10-02T11:55:00Z", "clear"),
        ("site", "2026-10-01T11:00:00Z", "clear"),
    ] {
        let body = map(
            JobsApiState {
                sensors: Some(repo(source, Some(observed)).await),
                ..state()
            },
            AccessTier::Operator,
        )
        .await;
        assert_eq!(sensors(&body)["state"], expected, "{body}");
        assert_eq!(sensors(&body)["count"], 1);
    }
    let repo = repo("stripe", Some("2026-10-02T11:50:00Z")).await;
    repo.stamp("source-one", "reading-one", "packet-one")
        .await
        .unwrap();
    let body = map(
        JobsApiState {
            sensors: Some(repo),
            ..state()
        },
        AccessTier::Operator,
    )
    .await;
    assert_eq!(sensors(&body)["state"], "clear");
}

#[tokio::test]
async fn forbidden_or_narrowed_readers_never_touch_the_sensor_port_and_failures_stay_unread() {
    let observed = Arc::new(ObservedSensors {
        inner: repo("stripe", None).await,
        reads: AtomicUsize::new(0),
        fail: true,
    });
    let denied = map(
        JobsApiState {
            sensors: Some(observed.clone()),
            ..state()
        },
        AccessTier::User,
    )
    .await;
    assert_eq!(sensors(&denied)["count"], Value::Null);
    assert_eq!(observed.reads.load(Ordering::SeqCst), 0);
    let failed = map(
        JobsApiState {
            sensors: Some(observed.clone()),
            ..state()
        },
        AccessTier::Operator,
    )
    .await;
    assert_eq!(sensors(&failed)["count"], Value::Null);
    assert!(
        sensors(&failed)["why"]
            .as_str()
            .unwrap()
            .contains("fixture unavailable")
    );
    assert_eq!(observed.reads.load(Ordering::SeqCst), 1);
    let narrowed = JobsApiState {
        policy: Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    Action::Read,
                    Resource::job(),
                    Scope::Self_,
                )
                .build(),
        ),
        sensors: Some(observed.clone()),
        ..state()
    };
    let partial = map(narrowed, AccessTier::Auditor).await;
    assert_eq!(sensors(&partial)["count"], Value::Null);
    assert_eq!(observed.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn only_urgent_real_open_alarms_count_as_sensor_failures() {
    use boss_core::job::{Job, JobStatus, Priority, Subject};
    use boss_core::partition::Partition;
    let cases = [
        (
            Priority::Standard,
            Partition::Real,
            JobStatus::Open,
            "clear",
        ),
        (
            Priority::Urgent,
            Partition::Simulated,
            JobStatus::Open,
            "clear",
        ),
        (
            Priority::Urgent,
            Partition::Shadow,
            JobStatus::Open,
            "clear",
        ),
        (
            Priority::Urgent,
            Partition::Real,
            JobStatus::Closed,
            "clear",
        ),
        (
            Priority::Urgent,
            Partition::Real,
            JobStatus::Open,
            "troubled",
        ),
    ];
    let mut failures = Vec::new();
    for (priority, partition, status, expected) in cases {
        let wired = JobsApiState {
            sensors: Some(repo("stripe", None).await),
            ..state()
        };
        let mut alarm = Job::new(
            "backlog-item",
            Subject::new("sensor", "source-one"),
            "Source unreadable",
            "reader",
            priority,
            at("2026-10-02T12:00:00Z").date_naive(),
        );
        alarm.partition = partition;
        alarm.status = status;
        alarm.metadata = serde_json::json!({"estate_finding":"sensor_unreadable:source-one"});
        wired.jobs.create_job(&alarm).await.unwrap();
        let body = map(wired, AccessTier::Operator).await;
        let actual = sensors(&body)["state"].as_str().unwrap().to_owned();
        if actual != expected {
            failures.push(format!(
                "{priority:?}/{partition:?}/{status:?}: expected {expected}, got {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

#[tokio::test]
async fn only_the_exact_open_real_sensor_unreadable_finding_troubles_the_station() {
    let wired = JobsApiState {
        sensors: Some(repo("stripe", None).await),
        ..state()
    };
    let mut alarm = boss_core::job::Job::new(
        "backlog-item",
        boss_core::job::Subject::new("sensor", "source-one"),
        "Source unreadable",
        "reader",
        boss_core::job::Priority::Urgent,
        at("2026-10-02T12:00:00Z").date_naive(),
    );
    alarm.status = boss_core::job::JobStatus::Open;
    alarm.metadata = serde_json::json!({"estate_finding":"sensor_unreadable:other"});
    wired.jobs.create_job(&alarm).await.unwrap();
    let body = map(
        JobsApiState {
            sensors: wired.sensors.clone(),
            jobs: wired.jobs.clone(),
            ..state()
        },
        AccessTier::Operator,
    )
    .await;
    assert_eq!(sensors(&body)["state"], "clear");
    alarm.id = boss_core::job::JobId::new();
    alarm.metadata = serde_json::json!({"estate_finding":"sensor_unreadable:source-one"});
    wired.jobs.create_job(&alarm).await.unwrap();
    let id = alarm.id.to_string();
    let body = map(wired, AccessTier::Operator).await;
    assert_eq!(sensors(&body)["state"], "troubled");
    assert!(sensors(&body)["why"].as_str().unwrap().contains(&id));
}

#[tokio::test]
async fn sensors_station_without_a_read_port_is_unread_never_healthy_empty() {
    let bus = RecordingEventBus::new();
    let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
    let state = JobsApiState::minimal(
        Arc::new(InMemoryJobs::new()),
        bus,
        publisher,
        Arc::new(
            FakePolicyClient::builder()
                .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                .build(),
        ),
        Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: chrono::DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z")
                    .unwrap()
                    .into(),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
    );
    let user = User {
        id: "operator-test".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("it".into()),
    };
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/yard/regions?window=24h")
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let sensor = body["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "sensors")
        .expect("approved SENSORS station must stand on the map");
    assert_eq!(sensor["count"], Value::Null);
    assert_eq!(sensor["state"], "troubled");
}
