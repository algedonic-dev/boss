use super::*;
use crate::{
    InMemoryJobs,
    membership_projection::{InMemoryMembershipProjection, ProjectionReading},
    port::{JobScope, JobsError},
};
use async_trait::async_trait;

fn identity(actor: &str) -> ProjectionIdentity {
    ProjectionIdentity::new(Some(actor.into()), JobScope::All)
}

pub(super) fn jobs() -> Arc<InMemoryJobs> {
    let jobs = Arc::new(InMemoryJobs::new());
    let until = Utc::now();
    let since = until - Duration::hours(48);
    let clock = boss_clock_client::mapping_history::ClockMappingFact {
        schema_version: 1,
        revision: "original-clock".into(),
        previous_revision: None,
        epoch: Some("original-epoch".into()),
        mode: Some(boss_clock_client::ClockMode::Wall),
        params: None,
        captured_at: since - Duration::seconds(2),
        audit_created_at: since - Duration::seconds(1),
        seq: 1,
        complete_group: true,
    };
    jobs.answer_membership(crate::membership_replay::ReplayInput {
        groups: vec![],
        since,
        until,
        audit_cutoff: 1,
        actor: Some("alice".into()),
        scope: JobScope::All,
        currently_authorized: None,
        clock: boss_clock_client::mapping_history::ClockTimeline::build(
            &[clock],
            Some("original-clock"),
            since,
            until,
        ),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: crate::membership_replay::InitialState::default(),
    });
    jobs
}

async fn wait_phase(service: &MembershipProjectionService, phase: &str) -> RefreshObservation {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let observation = service.observe_refresh(&identity("alice")).await.unwrap();
            if observation
                .active
                .as_ref()
                .is_some_and(|flight| flight.events.iter().any(|event| event.phase == phase))
            {
                break observation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_owned_source_receipt_is_scoped_and_retains_a_previous_flight_separately() {
    let jobs = jobs();
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    // A real refused source/fold becomes the previous completed flight.
    let cancelled = Arc::new(AtomicBool::new(true));
    assert!(
        service
            .refresh_owned(&identity("alice"), cancelled)
            .await
            .is_err()
    );
    let previous = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    assert_eq!(previous.outcome, Some("refused"));
    service.failures.lock().await.insert(
        identity("alice").key().unwrap(),
        RefreshFailure {
            why: "old failure".into(),
            retry_at: Instant::now(),
            flight: Some(previous.id),
            receipt: Some(previous.clone()),
        },
    );
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    *jobs.projection_source_gate.lock().unwrap() = Some(gate.clone());
    let owner_service = service.clone();
    let owner = tokio::spawn(async move { owner_service.refresh_one(&identity("alice")).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    let observation = service.observe_refresh(&identity("alice")).await.unwrap();
    let active = observation.active.unwrap();
    assert_ne!(active.id, previous.id);
    assert_eq!(observation.latest_completed.unwrap().id, previous.id);
    assert_eq!(observation.cached_failure_flight, Some(previous.id));
    assert_eq!(active.deadline_ms, 90_000);
    let source = active.events.last().unwrap();
    assert_eq!(source.phase, "source_begin");
    assert_eq!(source.attempt, "initial_48h");
    assert!(source.attempt_remaining_ms <= 30_000);
    assert!(source.remaining_ms > source.attempt_remaining_ms);
    for foreign in [
        identity("bob"),
        ProjectionIdentity::new(Some("alice".into()), JobScope::None),
    ] {
        assert_eq!(
            service.observe_refresh(&foreign).await.unwrap().state,
            "not_observed"
        );
    }
    assert!(
        service.generations.try_lock().is_err(),
        "the observer must not wait on the generation lock held by actual work"
    );
    gate.1.notify_one();
    owner.await.unwrap().unwrap();
    let done = service.observe_refresh(&identity("alice")).await.unwrap();
    assert!(done.active.is_none());
    let completed = done.latest_completed.unwrap();
    assert_eq!(completed.id, active.id);
    assert_eq!(completed.outcome, Some("completed"));
    assert!(completed.record_complete);
    let phases: Vec<_> = completed.events.iter().map(|event| event.phase).collect();
    assert_eq!(
        phases,
        [
            "generation_lock_begin",
            "generation_lock_acquired",
            "source_begin",
            "source_end",
            "fold_queued",
            "fold_start",
            "fold_end",
            "fold_joined",
            "publication_begin",
            "publication_acknowledged",
            "publication_end",
            "owner_end"
        ]
    );
    assert_eq!(
        jobs.membership_reads(),
        1,
        "observing never adds source work"
    );
}

#[tokio::test]
async fn a_refused_wide_source_and_the_current_fallback_have_distinct_attempt_budgets() {
    let service = MembershipProjectionService::new(
        Arc::new(InMemoryJobs::new()),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    );
    assert!(service.refresh_one(&identity("alice")).await.is_err());
    let done = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    let sources: Vec<_> = done
        .events
        .iter()
        .filter(|event| event.phase == "source_begin")
        .collect();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].attempt, "initial_48h");
    assert_eq!(sources[1].attempt, "fallback_24h");
    assert!(sources[0].attempt_remaining_ms <= 30_000);
    assert_eq!(sources[1].attempt_remaining_ms, sources[1].remaining_ms);
    assert!(sources[1].remaining_ms <= sources[0].remaining_ms);
    assert!(
        done.events
            .iter()
            .filter(|event| event.phase == "source_refused")
            .count()
            == 2
    );
    assert!(!done.events.iter().any(|event| event.phase == "fold_start"));
    assert_eq!(done.outcome, Some("refused"));
}

struct DelayedStore {
    inner: InMemoryMembershipProjection,
    load_gate: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
    commit_gate: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
}

#[async_trait]
impl MembershipProjectionRepository for DelayedStore {
    async fn read(
        &self,
        identity: &ProjectionIdentity,
        since: chrono::DateTime<Utc>,
        binding: &ProjectionBinding,
    ) -> Result<Option<ProjectionReading>, JobsError> {
        self.inner.read(identity, since, binding).await
    }
    async fn load(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<Option<StoredProjection>, JobsError> {
        if let Some(gate) = &self.load_gate {
            gate.0.notify_one();
            gate.1.notified().await;
        }
        self.inner.load(identity).await
    }
    async fn commit(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
    ) -> Result<Option<i64>, JobsError> {
        self.inner.commit(identity, previous, next).await
    }
    async fn commit_with_budget(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
        budget: &ReplayBudget,
    ) -> Result<Option<i64>, JobsError> {
        budget.check().map_err(JobsError::Storage)?;
        if let Some(gate) = &self.commit_gate {
            gate.0.notify_one();
            gate.1.notified().await;
        }
        // The actual adapter can acknowledge a COMMIT after its final pre-COMMIT
        // budget check. This fixture preserves that contract, without retrying.
        self.inner.commit(identity, previous, next).await
    }
}

#[tokio::test]
async fn generation_load_and_lock_wait_spend_the_original_flight_budget() {
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let service = Arc::new(MembershipProjectionService::new(
        jobs(),
        Arc::new(DelayedStore {
            inner: InMemoryMembershipProjection::new(),
            load_gate: Some(gate.clone()),
            commit_gate: None,
        }),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let lock = service.generations.lock().await;
    let owner_service = service.clone();
    let owner = tokio::spawn(async move { owner_service.refresh_one(&identity("alice")).await });
    let waiting = wait_phase(&service, "generation_lock_begin").await;
    assert_eq!(waiting.active.unwrap().events.len(), 1);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    drop(lock);
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    let loaded = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .active
        .unwrap();
    assert_eq!(loaded.events.last().unwrap().phase, "generation_load_begin");
    assert!(loaded.remaining_ms < 90_000);
    gate.1.notify_one();
    owner.await.unwrap().unwrap();
    let completed = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    assert_eq!(completed.events[4].phase, "source_begin");
    assert!(completed.events[4].remaining_ms <= loaded.remaining_ms);
}

#[tokio::test]
async fn late_publication_acknowledgement_is_recorded_as_an_effect_before_the_refusal() {
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let store = Arc::new(DelayedStore {
        inner: InMemoryMembershipProjection::new(),
        load_gate: None,
        commit_gate: Some(gate.clone()),
    });
    let service = Arc::new(MembershipProjectionService::new(
        jobs(),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let owner_service = service.clone();
    let owner =
        tokio::spawn(async move { owner_service.refresh_owned(&identity("alice"), flag).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    let active = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .active
        .unwrap();
    assert_eq!(active.events.last().unwrap().phase, "publication_begin");
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    gate.1.notify_one();
    assert!(
        owner
            .await
            .unwrap()
            .unwrap_err()
            .contains("publication acknowledged")
    );
    let done = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    assert_eq!(done.outcome, Some("refused"));
    let acknowledged = done
        .events
        .iter()
        .find(|event| event.phase == "publication_acknowledged")
        .unwrap();
    assert_eq!(acknowledged.revision, Some(1));
    assert_eq!(
        store
            .load(&identity("alice"))
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn updater_cancellation_is_retained_until_actual_source_join_and_owner_exit() {
    let jobs = jobs();
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    *jobs.projection_source_gate.lock().unwrap() = Some(gate.clone());
    let service = Arc::new(MembershipProjectionService::new(
        jobs,
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    service.viewers.lock().await.push_back(Viewer {
        identity: identity("alice"),
        seen: tokio::time::Instant::now(),
    });
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let updater = tokio::spawn(service.clone().run(receiver));
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    cancel.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), updater)
        .await
        .unwrap()
        .unwrap();
    let done = service.observe_refresh(&identity("alice")).await.unwrap();
    assert!(done.active.is_none());
    let done = done.latest_completed.unwrap();
    assert_eq!(done.outcome, Some("refused"));
    assert_eq!(
        done.events
            .iter()
            .filter(|event| event.phase == "source_begin")
            .count(),
        1
    );
    let phases: Vec<_> = done.events.iter().map(|event| event.phase).collect();
    assert!(
        phases.ends_with(&["cancel_requested", "source_refused", "owner_end"]),
        "{phases:?}"
    );
    gate.1.notify_one();
}

#[test]
fn blocking_pool_queue_time_is_distinct_from_actual_fold_time() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            released
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        entry
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let service = Arc::new(MembershipProjectionService::new(
            jobs(),
            Arc::new(InMemoryMembershipProjection::new()),
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let owner_service = service.clone();
        let owner =
            tokio::spawn(async move { owner_service.refresh_one(&identity("alice")).await });
        let queued = wait_phase(&service, "fold_queued").await.active.unwrap();
        assert_eq!(queued.events.last().unwrap().phase, "fold_queued");
        assert!(
            !queued
                .events
                .iter()
                .any(|event| event.phase == "fold_start")
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        release.send(()).unwrap();
        occupied.await.unwrap();
        owner.await.unwrap().unwrap();
        let done = service
            .observe_refresh(&identity("alice"))
            .await
            .unwrap()
            .latest_completed
            .unwrap();
        let queue = done
            .events
            .iter()
            .find(|event| event.phase == "fold_queued")
            .unwrap();
        let start = done
            .events
            .iter()
            .find(|event| event.phase == "fold_start")
            .unwrap();
        let end = done
            .events
            .iter()
            .find(|event| event.phase == "fold_end")
            .unwrap();
        let join = done
            .events
            .iter()
            .find(|event| event.phase == "fold_joined")
            .unwrap();
        assert!(start.elapsed_ms >= queue.elapsed_ms + 20);
        assert!(end.elapsed_ms >= start.elapsed_ms);
        assert!(join.elapsed_ms >= end.elapsed_ms);
        assert!(start.remaining_ms < queue.remaining_ms);
    });
}

#[tokio::test]
async fn a_slow_actual_fold_remains_owned_after_cancellation_until_its_real_join() {
    let service = Arc::new(MembershipProjectionService::new(
        jobs(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    *service.fold_gate.lock().unwrap() = Some((entered, released));
    service.viewers.lock().await.push_back(Viewer {
        identity: identity("alice"),
        seen: tokio::time::Instant::now(),
    });
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let updater = tokio::spawn(service.clone().run(receiver));
    tokio::time::timeout(std::time::Duration::from_secs(5), entry)
        .await
        .unwrap()
        .unwrap();
    let before = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .active
        .unwrap();
    assert_eq!(before.events.last().unwrap().phase, "fold_start");
    cancel.send(true).unwrap();
    let cancelling = wait_phase(&service, "cancel_requested")
        .await
        .active
        .unwrap();
    assert!(
        !cancelling
            .events
            .iter()
            .any(|event| event.phase == "fold_joined")
    );
    assert!(!updater.is_finished());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), updater)
        .await
        .unwrap()
        .unwrap();
    let done = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    assert!(done.cancelled);
    assert_eq!(done.outcome, Some("refused"));
    let start = done
        .events
        .iter()
        .find(|event| event.phase == "fold_start")
        .unwrap();
    let end = done
        .events
        .iter()
        .find(|event| event.phase == "fold_refused")
        .unwrap();
    assert!(end.elapsed_ms >= start.elapsed_ms + 20);
    assert!(end.cancelled);
    assert_eq!(done.events[done.events.len() - 2].phase, "fold_joined");
    assert_eq!(done.events.last().unwrap().phase, "owner_end");
    assert!(
        !done
            .events
            .iter()
            .any(|event| event.phase == "publication_begin")
    );
}

#[tokio::test]
async fn an_acknowledgement_after_the_original_deadline_keeps_its_revision() {
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let store = Arc::new(DelayedStore {
        inner: InMemoryMembershipProjection::new(),
        load_gate: None,
        commit_gate: Some(gate.clone()),
    });
    let service = Arc::new(MembershipProjectionService::new(
        jobs(),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let flag = Arc::new(AtomicBool::new(false));
    // Already-spent time is carried into the actual refresh body, without
    // adding a production timeout knob or enlarging the original allowance.
    let trace = Trace::new(
        Instant::now() - FLOW_READ_DEADLINE + std::time::Duration::from_millis(250),
        flag.clone(),
    );
    service
        .receipts
        .begin(&identity("alice").key().unwrap(), trace.clone());
    let owner_service = service.clone();
    let owner_trace = trace.clone();
    let owner = tokio::spawn(async move {
        owner_service
            .refresh_traced(&identity("alice"), flag, &owner_trace)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    tokio::time::sleep_until(tokio::time::Instant::from_std(
        trace.deadline + std::time::Duration::from_millis(1),
    ))
    .await;
    gate.1.notify_one();
    let why = owner.await.unwrap().unwrap_err();
    assert!(why.contains("publication acknowledged"));
    assert!(why.contains("deadline exhausted"));
    let active = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .active
        .unwrap();
    let ack = active
        .events
        .iter()
        .find(|event| event.phase == "publication_acknowledged")
        .unwrap();
    assert_eq!(ack.remaining_ms, 0);
    assert_eq!(ack.revision, Some(1));
    assert!(!ack.cancelled);
    assert_eq!(
        store
            .load(&identity("alice"))
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    trace.finish("refused");
    service
        .receipts
        .finish(&identity("alice").key().unwrap(), &trace);
}

#[tokio::test]
async fn a_failed_blocking_join_is_named_before_the_current_window_fallback() {
    let service = MembershipProjectionService::new(
        jobs(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let (entered, _entry) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    *service.fold_gate.lock().unwrap() = Some((entered, released));
    drop(release); // Cause a real owned blocking task panic, not a fabricated Result.
    service.refresh_one(&identity("alice")).await.unwrap();
    let done = service
        .observe_refresh(&identity("alice"))
        .await
        .unwrap()
        .latest_completed
        .unwrap();
    assert_eq!(done.outcome, Some("completed"));
    assert!(
        done.events
            .iter()
            .any(|event| event.phase == "fold_join_failed" && event.attempt == "initial_48h")
    );
    assert!(
        done.events
            .iter()
            .any(|event| event.phase == "source_begin" && event.attempt == "fallback_24h")
    );
}

#[tokio::test]
async fn the_real_updater_binds_its_cached_failure_to_the_completed_flight() {
    let service = Arc::new(MembershipProjectionService::new(
        Arc::new(InMemoryJobs::new()),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    service.viewers.lock().await.push_back(Viewer {
        identity: identity("alice"),
        seen: tokio::time::Instant::now(),
    });
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let updater = tokio::spawn(service.clone().run(receiver));
    let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let observation = service.observe_refresh(&identity("alice")).await.unwrap();
            if observation.cached_failure_flight.is_some() {
                break observation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        observed.cached_failure_flight,
        Some(observed.latest_completed.unwrap().id)
    );
    cancel.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), updater)
        .await
        .unwrap()
        .unwrap();
}
