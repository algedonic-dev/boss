//! Count actual port reads around the real yard HTTP handler; data and failures
//! are supplied by the existing InMemoryJobs adapter, not a copied yard model.
use super::*;
use boss_jobs::port::*;
use std::sync::Mutex;

struct CountedJobs {
    inner: Arc<InMemoryJobs>,
    calls: Mutex<Vec<&'static str>>,
    batches: Mutex<Vec<Vec<JobId>>>,
    close_on_recent: Option<JobId>,
}
#[async_trait::async_trait]
impl JobsRepository for CountedJobs {
    async fn create_job_with_steps_at(
        &self,
        job: &Job,
        steps: &[Step],
        now: DateTime<Utc>,
        job_events: &[boss_core::event::Event],
        step_events: &[boss_core::event::Event],
    ) -> Result<Admission, JobsError> {
        self.inner
            .create_job_with_steps_at(job, steps, now, job_events, step_events)
            .await
    }
    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
        self.inner.get_job(id).await
    }
    async fn resolve_job_id_prefix(&self, prefix: &str) -> Result<Vec<JobId>, JobsError> {
        self.inner.resolve_job_id_prefix(prefix).await
    }
    async fn update_job_at(
        &self,
        job: &Job,
        read: JobStatus,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner.update_job_at(job, read, now, events).await
    }
    async fn update_job_expecting_at(
        &self,
        job: &Job,
        read: JobStatus,
        expect: Option<&serde_json::Map<String, serde_json::Value>>,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner
            .update_job_expecting_at(job, read, expect, now, events)
            .await
    }
    async fn merge_job_metadata_expecting_at(
        &self,
        id: &JobId,
        patch: &serde_json::Map<String, serde_json::Value>,
        judged_on: Option<&Job>,
        expect: Option<&serde_json::Map<String, serde_json::Value>>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        self.inner
            .merge_job_metadata_expecting_at(id, patch, judged_on, expect, stamp)
            .await
    }
    async fn merge_job_metadata_at(
        &self,
        id: &JobId,
        patch: &serde_json::Map<String, serde_json::Value>,
        judged_on: Option<&Job>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        self.inner
            .merge_job_metadata_at(id, patch, judged_on, stamp)
            .await
    }
    async fn close_job_at(
        &self,
        id: &JobId,
        closed_on: chrono::NaiveDate,
        owned: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
        markers: &(dyn for<'j> Fn(&'j Job) -> Vec<boss_core::event::Event> + Send + Sync),
    ) -> Result<Option<Job>, JobsError> {
        self.inner
            .close_job_at(id, closed_on, owned, stamp, markers)
            .await
    }
    async fn append_step_correction_at(
        &self,
        id: &JobId,
        entry: &serde_json::Value,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(Job, usize), JobsError> {
        self.inner.append_step_correction_at(id, entry, stamp).await
    }
    async fn append_input_classification_at(
        &self,
        id: &JobId,
        entry: &serde_json::Value,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(Job, usize), JobsError> {
        self.inner
            .append_input_classification_at(id, entry, stamp)
            .await
    }
    async fn list_estate_nodes(&self) -> Result<Vec<EstateNode>, JobsError> {
        self.inner.list_estate_nodes().await
    }
    async fn declare_estate_nodes(
        &self,
        declared: &[EstateNodeInput],
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<EstateBatchOutcome, JobsError> {
        self.inner.declare_estate_nodes(declared, stamp).await
    }
    async fn recent_events_by_kind(
        &self,
        kind: &str,
        window: &EventWindow,
        limit: i64,
    ) -> Result<EventPage, JobsError> {
        self.inner.recent_events_by_kind(kind, window, limit).await
    }
    async fn step_flow_cube(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<boss_jobs::station_flow::FlowCell>, JobsError> {
        self.inner.step_flow_cube(since).await
    }
    async fn events_for_job(
        &self,
        job_id: &JobId,
        limit: i64,
    ) -> Result<Vec<boss_core::event::Event>, JobsError> {
        self.inner.events_for_job(job_id, limit).await
    }
    async fn repin_workflow_version_at(
        &self,
        id: &JobId,
        to_version: i32,
        plan: &boss_jobs::repin::RepinPlan,
        record: &serde_json::Value,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        self.inner
            .repin_workflow_version_at(id, to_version, plan, record, stamp)
            .await
    }
    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError> {
        if filter.kind.as_deref() == Some("pr-train")
            && filter.closed_since.is_some()
            && let Some(id) = self.close_on_recent
            && let Some(mut train) = self.inner.get_job(&id).await?
            && train.status == JobStatus::Open
        {
            train.status = JobStatus::Closed;
            train.closed_on = Some(t(NOW).date_naive());
            train.metadata = json!({"outcome":"arrived", "closed_at":"2026-09-03T05:30:00Z"});
            self.inner
                .update_job_at(&train, JobStatus::Open, t(NOW), &[])
                .await?;
        }
        self.inner.list_jobs(filter, limit, offset).await
    }
    async fn add_step_at(
        &self,
        step: &Step,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner.add_step_at(step, now, events).await
    }
    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
        self.inner.get_step(id).await
    }
    async fn get_step_versioned(
        &self,
        id: &StepId,
    ) -> Result<Option<(Step, StepVersion)>, JobsError> {
        self.inner.get_step_versioned(id).await
    }
    async fn list_steps_versioned(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<(Step, StepVersion)>, JobsError> {
        self.inner.list_steps_versioned(job_id).await
    }
    #[cfg(feature = "test-support")]
    async fn update_step_at(
        &self,
        step: &Step,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner.update_step_at(step, now, events).await
    }
    async fn update_step_if_unchanged_at(
        &self,
        step: &Step,
        read: StepVersion,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner
            .update_step_if_unchanged_at(step, read, now, events)
            .await
    }
    async fn merge_step_metadata_at(
        &self,
        id: &StepId,
        patch: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Step, JobsError> {
        self.inner.merge_step_metadata_at(id, patch, stamp).await
    }
    async fn record_step_metadata_if_unchanged_at(
        &self,
        id: &StepId,
        key: &str,
        value: &serde_json::Value,
        read: Option<StepVersion>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<boss_jobs::first_record::FirstRecordResult, JobsError> {
        self.inner
            .record_step_metadata_if_unchanged_at(id, key, value, read, stamp)
            .await
    }
    async fn claim_step_displacing_at(
        &self,
        step_id: &StepId,
        actor: &str,
        displaceable: &[String],
        stamp: &boss_core::publisher::EventStamp,
        events: &[boss_core::event::Event],
    ) -> Result<Step, JobsError> {
        self.inner
            .claim_step_displacing_at(step_id, actor, displaceable, stamp, events)
            .await
    }
    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        event_stamp: &boss_core::publisher::EventStamp,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.inner
            .append_sign_off(step_id, stamp, event_stamp, events)
            .await
    }
    async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError> {
        self.inner.record_events(events).await
    }
    async fn active_step_plugin_version(&self, kind: &str) -> Result<i32, JobsError> {
        self.inner.active_step_plugin_version(kind).await
    }
    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
        self.calls.lock().unwrap().push("single");

        self.inner.list_steps(job_id).await
    }
    async fn queue_age(&self, scope: &JobScope) -> Result<Vec<QueueAgeRow>, JobsError> {
        self.inner.queue_age(scope).await
    }
    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
        self.inner.count_in_flight_steps_by_kind(step_kind).await
    }
    async fn count_open_jobs_for_workflow(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<i64, JobsError> {
        self.inner.count_open_jobs_for_workflow(kind, version).await
    }
    async fn jobs_pinned_to_workflow(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<PinnedJobs, JobsError> {
        self.inner.jobs_pinned_to_workflow(kind, version).await
    }
    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
        scope: &JobScope,
    ) -> Result<Vec<(String, i64)>, JobsError> {
        self.inner.count_jobs_by_kind(status, scope).await
    }
    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
        self.inner.resolve_blockers(ids).await
    }
    async fn record_step_write_refusal_at(
        &self,
        refusal: &boss_jobs::refusals::StepWriteRefusal,
        now: DateTime<Utc>,
    ) -> Result<(), JobsError> {
        self.inner.record_step_write_refusal_at(refusal, now).await
    }
    async fn step_write_refusals(
        &self,
        limit: i64,
    ) -> Result<Vec<boss_jobs::refusals::RecordedRefusal>, JobsError> {
        self.inner.step_write_refusals(limit).await
    }
    async fn list_steps_for_jobs(&self, ids: &[JobId]) -> Result<Vec<Step>, JobsError> {
        self.calls.lock().unwrap().push("batch");
        self.batches.lock().unwrap().push(ids.to_vec());
        self.inner.list_steps_for_jobs(ids).await
    }
}

fn counted_app(jobs: Arc<CountedJobs>) -> axum::Router {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations.seed(dock_station_row()).unwrap();
    let mut state = JobsApiState::minimal(
        jobs,
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        Arc::new(
            FakePolicyClient::builder()
                .allow("operator", Action::Read, Resource::job(), Scope::All)
                .allow("builder", Action::Read, Resource::job(), Scope::Self_)
                .build(),
        ),
        Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: t(NOW),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
    );
    state.stations = Some(stations);
    router(state)
}

async fn population() -> (Arc<CountedJobs>, Vec<Job>) {
    let inner = Arc::new(InMemoryJobs::new());
    let mut rows = Vec::new();
    for n in 0..5 {
        let status = if n < 2 {
            JobStatus::Open
        } else {
            JobStatus::Closed
        };
        let mut train = job(
            "pr-train",
            &Uuid::from_u128(0x7700 + n).to_string(),
            &format!("train #{n}"),
            status,
            json!({}),
        );
        train.owner_id = "emp-another".into();
        inner.create_job_at(&train, t(NOW), &[]).await.unwrap();
        for order in [1, 0] {
            let mut s = step(
                &train.id,
                &format!("phase-{order}"),
                "Actual train phase",
                StepStatus::Ready,
                json!({}),
            );
            s.sort_order = order;
            inner.add_step_at(&s, t(NOW), &[]).await.unwrap();
        }
        rows.push(train);
    }
    (
        Arc::new(CountedJobs {
            inner,
            calls: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            close_on_recent: None,
        }),
        rows,
    )
}

#[tokio::test]
async fn train_steps_use_one_storage_batch_for_the_complete_population() {
    let (jobs, rows) = population().await;
    let (status, body) = get(&counted_app(jobs.clone()), "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["trains"].as_array().unwrap().len(), 2);
    assert_eq!(body["recent"].as_array().unwrap().len(), 3);
    {
        let calls = jobs.calls.lock().unwrap();
        eprintln!("actual yard step port calls: {calls:?}");
        assert_eq!(
            calls.iter().filter(|c| **c == "single").count(),
            0,
            "train count must not multiply storage round trips"
        );
        assert_eq!(calls.iter().filter(|c| **c == "batch").count(), 1);
        let batches = jobs.batches.lock().unwrap();
        let requested: std::collections::BTreeSet<_> =
            batches[0].iter().map(ToString::to_string).collect();
        assert_eq!(requested, rows.iter().map(|r| r.id.to_string()).collect());
    }
    for row in &rows {
        let ordered = jobs.inner.list_steps(&row.id).await.unwrap();
        assert_eq!(
            ordered.iter().map(|s| s.sort_order).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}

#[tokio::test]
async fn failed_open_or_recent_train_batch_never_clears_the_track() {
    for failed_index in [0, 4] {
        let (jobs, rows) = population().await;
        jobs.inner.fail_steps_read(&rows[failed_index].id);
        let (status, _) = get(&counted_app(jobs), "operator").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[tokio::test]
async fn an_empty_or_withheld_population_does_not_issue_a_step_batch() {
    let (jobs, _) = population().await;
    let (status, body) = get(&counted_app(jobs.clone()), "builder").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["trains"].as_array().unwrap().len(), 0);
    assert!(jobs.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_train_closing_between_the_two_lists_keeps_its_steps_in_both() {
    let inner = Arc::new(InMemoryJobs::new());
    let train = job(
        "pr-train",
        &Uuid::from_u128(0x9900).to_string(),
        "A closing train",
        JobStatus::Open,
        json!({}),
    );
    inner.create_job_at(&train, t(NOW), &[]).await.unwrap();
    let boarded = step(
        &train.id,
        "collect",
        "Boarded",
        StepStatus::Completed,
        json!({"completed_at":"2026-09-03T05:00:00Z"}),
    );
    inner.add_step_at(&boarded, t(NOW), &[]).await.unwrap();
    let jobs = Arc::new(CountedJobs {
        inner,
        calls: Mutex::new(Vec::new()),
        batches: Mutex::new(Vec::new()),
        close_on_recent: Some(train.id),
    });
    let (status, body) = get(&counted_app(jobs.clone()), "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["trains"][0]["id"], train.id.to_string());
    assert_eq!(body["trains"][0]["boarded_at"], "2026-09-03T05:00:00Z");
    assert_eq!(body["recent"][0]["id"], train.id.to_string());
    assert_eq!(
        body["recent"][0]["journey_seconds"], 1800,
        "the same train's later terminal row must retain its actual boarding step: {body}"
    );
    assert_eq!(jobs.batches.lock().unwrap()[0], vec![train.id, train.id]);
}

async fn remaining_population(jobs: &CountedJobs) -> Vec<Vec<JobId>> {
    let mut populations = Vec::new();
    for (group, kind) in [
        "gate-run",
        "maintenance-cluster-converge",
        "maintenance-forge-converge",
        "maintenance-boss-gcp-converge",
        "ship-a-change",
    ]
    .into_iter()
    .enumerate()
    {
        let mut ids = Vec::new();
        for n in 0..3 {
            let mut row = job(
                kind,
                &Uuid::from_u128(0xd000 + group as u128 * 16 + n).to_string(),
                "A retained yard packet",
                JobStatus::Open,
                json!({"branch":format!("fix/retained-{group}-{n}")}),
            );
            row.owner_id = "emp-another".into();
            jobs.inner.create_job_at(&row, t(NOW), &[]).await.unwrap();
            for order in [1, 0] {
                let mut s = step(
                    &row.id,
                    if order == 0 { "review" } else { "run" },
                    "Original ordered step",
                    StepStatus::Ready,
                    json!({}),
                );
                s.sort_order = order;
                jobs.inner.add_step_at(&s, t(NOW), &[]).await.unwrap();
            }
            ids.push(row.id);
        }
        populations.push(ids);
    }
    populations
}

#[tokio::test]
async fn gates_converges_and_dock_each_use_one_complete_storage_batch() {
    let (jobs, _) = population().await;
    let selected = remaining_population(&jobs).await;
    let (status, body) = get(&counted_app(jobs.clone()), "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = jobs.calls.lock().unwrap();
    eprintln!("actual mixed yard step port calls: {calls:?}");
    assert_eq!(
        calls.iter().filter(|c| **c == "single").count(),
        0,
        "gate/converge/dock packet counts must not multiply round trips"
    );
    let batches = jobs.batches.lock().unwrap();
    assert_eq!(batches.len(), 4, "train, gates, all converge kinds, dock");
    assert_eq!(
        batches.iter().map(Vec::len).sum::<usize>(),
        20,
        "recent/open gate overlap is read once, without losing any selected packet"
    );
    let sets: Vec<std::collections::BTreeSet<_>> = batches
        .iter()
        .map(|ids| ids.iter().map(ToString::to_string).collect())
        .collect();
    assert!(sets.contains(&selected[0].iter().map(ToString::to_string).collect()));
    assert!(
        sets.contains(
            &selected[1..4]
                .iter()
                .flatten()
                .map(ToString::to_string)
                .collect()
        )
    );
    assert!(sets.contains(&selected[4].iter().map(ToString::to_string).collect()));
    assert_eq!(body["dock_source"], "station", "{body}");
    assert_eq!(
        body["dock"].as_array().unwrap().len(),
        3,
        "all original review steps remain attached: {body}"
    );
}

#[tokio::test]
async fn all_remaining_batches_keep_narrow_and_unreadable_scopes_empty() {
    for user in ["builder", "no-grant"] {
        let (jobs, _) = population().await;
        remaining_population(&jobs).await;
        let (status, body) = get(&counted_app(jobs.clone()), user).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            jobs.calls.lock().unwrap().is_empty(),
            "an empty or withheld selection must not read other callers' steps"
        );
    }
}

#[tokio::test]
async fn gate_and_dock_batch_failures_keep_the_original_refusal_posture() {
    for group in [0, 4] {
        let (jobs, _) = population().await;
        let selected = remaining_population(&jobs).await;
        jobs.inner.fail_steps_read(&selected[group][1]);
        let (status, body) = get(&counted_app(jobs), "operator").await;
        if group == 0 {
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        } else {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["dock_source"], "unavailable", "{body}");
            assert!(body["dock"].as_array().unwrap().is_empty(), "{body}");
        }
    }
}

#[tokio::test]
async fn dock_batch_keeps_candidates_without_steps_until_the_registry_predicate() {
    let (jobs, _) = population().await;
    let mut selected = remaining_population(&jobs).await;
    let empty = job(
        "ship-a-change",
        &Uuid::from_u128(0xef00).to_string(),
        "A candidate without a review",
        JobStatus::Open,
        json!({"branch":"fix/no-review"}),
    );
    jobs.inner.create_job_at(&empty, t(NOW), &[]).await.unwrap();
    selected[4].push(empty.id);
    let (status, body) = get(&counted_app(jobs.clone()), "operator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["dock"].as_array().unwrap().len(),
        3,
        "missing review excludes only that candidate: {body}"
    );
    let expected: std::collections::BTreeSet<_> =
        selected[4].iter().map(ToString::to_string).collect();
    assert!(
        jobs.batches.lock().unwrap().iter().any(|ids| ids
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::BTreeSet<_>>()
            == expected),
        "membership is judged after all scoped candidates' steps are read"
    );
}
