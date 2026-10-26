//! Viewer reads consume a durable, freshly authorized generation, never
//! original packet history or another caller's cached counts (c7870939).
use std::{collections::BTreeSet, sync::Arc};

use boss_clock_client::{
    ClockMode,
    mapping_history::{ClockMappingFact, ClockTimeline},
};
use boss_jobs::{
    InMemoryJobs,
    membership_projection::{
        InMemoryMembershipProjection, MembershipProjectionRepository, ProjectionIdentity,
    },
    membership_projection_service::MembershipProjectionService,
    membership_replay::{InitialState, ReplayInput, projection::MembershipProjection},
    port::JobScope,
};
use chrono::{Duration, Utc};

#[derive(Default)]
struct HeldStore {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[derive(Default)]
struct NewlyCommittedStore {
    until: std::sync::OnceLock<chrono::DateTime<Utc>>,
}

#[async_trait::async_trait]
impl MembershipProjectionRepository for NewlyCommittedStore {
    async fn read(
        &self,
        _: &ProjectionIdentity,
        since: chrono::DateTime<Utc>,
        _: &boss_jobs::membership_replay::projection::ProjectionBinding,
    ) -> Result<
        Option<boss_jobs::membership_projection::ProjectionReading>,
        boss_jobs::port::JobsError,
    > {
        Ok(Some(boss_jobs::membership_projection::ProjectionReading {
            revision: 1,
            reading: boss_jobs::membership_replay::MembershipReading {
                stations: vec![],
                since,
                until: *self.until.get_or_init(Utc::now),
                audit_cutoff: 1,
                denied: false,
            },
        }))
    }
    async fn load(
        &self,
        _: &ProjectionIdentity,
    ) -> Result<
        Option<boss_jobs::membership_projection::StoredProjection>,
        boss_jobs::port::JobsError,
    > {
        panic!("viewer cannot load original generation state")
    }
    async fn commit(
        &self,
        _: &ProjectionIdentity,
        _: Option<&boss_jobs::membership_projection::StoredProjection>,
        _: &MembershipProjection,
    ) -> Result<Option<i64>, boss_jobs::port::JobsError> {
        panic!("viewer cannot publish")
    }
}

#[tokio::test]
async fn a_generation_committed_during_the_compact_read_is_not_future() {
    let service = MembershipProjectionService::new(
        Arc::new(InMemoryJobs::new()),
        Arc::new(NewlyCommittedStore::default()),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    assert!(service.peek(&identity, 48).await.unwrap().is_some());
}

struct WindowStore {
    until: chrono::DateTime<Utc>,
    starts: std::sync::Mutex<Vec<chrono::DateTime<Utc>>>,
}

#[async_trait::async_trait]
impl MembershipProjectionRepository for WindowStore {
    async fn read(
        &self,
        _: &ProjectionIdentity,
        since: chrono::DateTime<Utc>,
        _: &boss_jobs::membership_replay::projection::ProjectionBinding,
    ) -> Result<
        Option<boss_jobs::membership_projection::ProjectionReading>,
        boss_jobs::port::JobsError,
    > {
        self.starts.lock().unwrap().push(since);
        Ok(Some(boss_jobs::membership_projection::ProjectionReading {
            revision: 1,
            reading: boss_jobs::membership_replay::MembershipReading {
                stations: vec![],
                since,
                until: self.until,
                audit_cutoff: 1,
                denied: false,
            },
        }))
    }
    async fn load(
        &self,
        _: &ProjectionIdentity,
    ) -> Result<
        Option<boss_jobs::membership_projection::StoredProjection>,
        boss_jobs::port::JobsError,
    > {
        panic!("compact viewer cannot recover history")
    }
    async fn commit(
        &self,
        _: &ProjectionIdentity,
        _: Option<&boss_jobs::membership_projection::StoredProjection>,
        _: &MembershipProjection,
    ) -> Result<Option<i64>, boss_jobs::port::JobsError> {
        panic!("compact viewer cannot publish")
    }
}

#[tokio::test]
async fn both_windows_end_at_the_original_generation_cut_not_the_request_clock() {
    let store = Arc::new(WindowStore {
        until: Utc::now() - Duration::seconds(1),
        starts: std::sync::Mutex::new(vec![]),
    });
    let service = MembershipProjectionService::new(
        Arc::new(InMemoryJobs::new()),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let reading = service.peek(&identity, 24).await.unwrap().unwrap();
    assert_eq!(reading.as_of, store.until);
    let starts = store.starts.lock().unwrap();
    assert!(
        starts
            .iter()
            .any(|start| store.until - *start == Duration::hours(24)),
        "current window must cover exactly24h of its actual generation: {starts:?}"
    );
    assert!(
        starts
            .iter()
            .any(|start| store.until - *start == Duration::hours(48)),
        "comparison window must cover exactly48h of the SAME generation: {starts:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_idle_viewer_expires_instead_of_querying_forever() {
    let jobs = Arc::new(InMemoryJobs::new());
    let service = MembershipProjectionService::new(
        jobs.clone(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    assert!(service.read(&identity, 24).await.unwrap().is_none());
    assert!(
        service
            .progress(&identity)
            .await
            .unwrap()
            .contains("queued")
    );
    tokio::time::advance(
        boss_jobs::membership_projection::READER_IDLE + std::time::Duration::from_secs(1),
    )
    .await;
    assert!(
        service.progress(&identity).await.is_none(),
        "inactive viewers must leave the updater queue"
    );
    assert_eq!(jobs.membership_reads(), 0);
}

#[async_trait::async_trait]
impl MembershipProjectionRepository for HeldStore {
    async fn read(
        &self,
        _: &ProjectionIdentity,
        _: chrono::DateTime<Utc>,
        _: &boss_jobs::membership_replay::projection::ProjectionBinding,
    ) -> Result<
        Option<boss_jobs::membership_projection::ProjectionReading>,
        boss_jobs::port::JobsError,
    > {
        Ok(None)
    }
    async fn load(
        &self,
        _: &ProjectionIdentity,
    ) -> Result<
        Option<boss_jobs::membership_projection::StoredProjection>,
        boss_jobs::port::JobsError,
    > {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(None)
    }
    async fn commit(
        &self,
        _: &ProjectionIdentity,
        _: Option<&boss_jobs::membership_projection::StoredProjection>,
        _: &MembershipProjection,
    ) -> Result<Option<i64>, boss_jobs::port::JobsError> {
        panic!("cancelled recovery must never publish a generation")
    }
}

#[tokio::test]
async fn cancellation_retains_the_actual_recovery_future_until_it_exits() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(HeldStore::default());
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    assert!(service.progress(&identity).await.is_none());
    assert!(service.read(&identity, 24).await.unwrap().is_none());
    assert!(
        service
            .progress(&identity)
            .await
            .unwrap()
            .contains("queued")
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(service.clone().run(receiver));
    store.entered.notified().await;
    assert!(
        service
            .progress(&identity)
            .await
            .unwrap()
            .contains("running")
    );
    cancel.send(true).unwrap();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        !task.is_finished(),
        "cancelling a viewer must not orphan actual store recovery"
    );
    assert!(
        service
            .progress(&identity)
            .await
            .unwrap()
            .contains("running")
    );
    store.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(service.progress(&identity).await.is_none());
    assert_eq!(
        jobs.membership_reads(),
        0,
        "cancelled recovery cannot advance into source work"
    );
}

fn known_input(identity: &ProjectionIdentity, census: Option<BTreeSet<String>>) -> ReplayInput {
    known_input_at(identity, census, Utc::now())
}

fn known_input_at(
    identity: &ProjectionIdentity,
    census: Option<BTreeSet<String>>,
    until: chrono::DateTime<Utc>,
) -> ReplayInput {
    let since = until - Duration::hours(48);
    let clock = ClockMappingFact {
        schema_version: 1,
        revision: "original-clock".into(),
        previous_revision: None,
        epoch: Some("original-epoch".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: since - Duration::seconds(2),
        audit_created_at: since - Duration::seconds(1),
        seq: 1,
        complete_group: true,
    };
    ReplayInput {
        groups: vec![],
        since,
        until,
        audit_cutoff: 1,
        actor: identity.actor.clone(),
        scope: identity.scope.clone(),
        currently_authorized: census,
        clock: ClockTimeline::build(&[clock], Some("original-clock"), since, until),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: InitialState::default(),
    }
}

#[tokio::test]
async fn a_future_generation_is_never_a_ready_reading() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let input = known_input_at(&identity, None, Utc::now() + Duration::minutes(1));
    let checkpoint = MembershipProjection::rebuild(&input).unwrap();
    store.commit(&identity, None, &checkpoint).await.unwrap();
    let service =
        MembershipProjectionService::new(jobs, store, Arc::new(boss_clock_client::WallClockClient));
    let result = service.read(&identity, 24).await;
    assert!(
        matches!(result, Err(ref why) if why.contains("future")),
        "future native watermark must be refused"
    );
}

#[tokio::test]
async fn a_warm_viewer_reads_compact_generation_and_never_original_history() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let alice = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let input = known_input(&alice, None);
    let checkpoint = MembershipProjection::rebuild(&input).unwrap();
    store.commit(&alice, None, &checkpoint).await.unwrap();
    let service = MembershipProjectionService::new(
        jobs.clone(),
        store,
        Arc::new(boss_clock_client::WallClockClient),
    );
    let reading = service.read(&alice, 24).await.unwrap().unwrap();
    assert_eq!(reading.as_of, input.until);
    assert!(reading.windows.unwrap().both.is_ok());
    assert_eq!(
        jobs.membership_reads(),
        0,
        "viewer must not ask the full native snapshot port"
    );
    let bob = ProjectionIdentity::new(Some("bob".into()), JobScope::All);
    assert!(service.read(&bob, 24).await.unwrap().is_none());
    assert_eq!(
        jobs.membership_reads(),
        0,
        "cold caller schedules work, never replays in the request"
    );
}

#[tokio::test]
async fn revoked_current_census_refuses_an_old_generation_for_the_same_identity() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let alice = ProjectionIdentity::new(
        Some("alice".into()),
        JobScope::OwnerIn(vec!["alice".into()]),
    );
    let input = known_input(
        &alice,
        Some(BTreeSet::from([
            "aaaaaaaa-0000-4000-8000-0000000029f7".into()
        ])),
    );
    let checkpoint = MembershipProjection::rebuild(&input).unwrap();
    store.commit(&alice, None, &checkpoint).await.unwrap();
    let service = MembershipProjectionService::new(
        jobs.clone(),
        store,
        Arc::new(boss_clock_client::WallClockClient),
    );
    let why = match service.read(&alice, 24).await {
        Err(why) => why,
        Ok(_) => panic!("current empty census must not reuse the former generation"),
    };
    assert!(why.contains("census") || why.contains("binding"), "{why}");
    assert_eq!(jobs.membership_reads(), 0);
}

#[tokio::test]
async fn three_cold_callers_are_served_by_the_owned_fifo_and_stop_cleanly() {
    let jobs = Arc::new(InMemoryJobs::new());
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    let identities: Vec<_> = ["alice", "bob", "carol"]
        .into_iter()
        .map(|actor| ProjectionIdentity::new(Some(actor.into()), JobScope::All))
        .collect();
    for identity in &identities {
        assert!(service.read(identity, 24).await.unwrap().is_none());
    }
    assert_eq!(
        jobs.membership_reads(),
        0,
        "registration never scans history"
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(service.clone().run(receiver));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut refused = 0;
            for identity in &identities {
                if let Err(why) = service.read(identity, 24).await {
                    assert!(
                        why.contains("native membership history unavailable"),
                        "{why}"
                    );
                    refused += 1;
                }
            }
            if refused == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send(true).unwrap();
    task.await.unwrap();
    assert!(
        jobs.membership_reads() >= 6,
        "each caller's wide and current attempt actually ran"
    );
    let stopped = jobs.membership_reads();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        jobs.membership_reads(),
        stopped,
        "stopped owner leaves no continuing source task"
    );
}

#[tokio::test]
async fn a_stale_generation_keeps_the_original_refresh_refusal_visible() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let input = known_input_at(&identity, None, Utc::now() - Duration::minutes(5));
    store
        .commit(
            &identity,
            None,
            &MembershipProjection::rebuild(&input).unwrap(),
        )
        .await
        .unwrap();
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        store,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    assert!(
        service
            .read(&identity, 24)
            .await
            .unwrap_err()
            .contains("stale")
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let owner = tokio::spawn(service.clone().run(receiver));
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let why = service.peek(&identity, 24).await.unwrap_err();
            if why.contains("native membership history unavailable") {
                break why;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    cancel.send(true).unwrap();
    owner.await.unwrap();
    let why = result.expect("stale generation must retain its actual failed refresh cause");
    assert!(why.contains(&input.until.to_string()), "{why}");
    assert!(
        jobs.membership_reads() >= 3,
        "the actual delta and bounded bootstrap attempts ran"
    );
}

#[tokio::test]
async fn a_fresh_generation_does_not_hide_a_later_actual_refresh_failure() {
    let jobs = Arc::new(InMemoryJobs::new());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    let input = known_input(&identity, None);
    store
        .commit(
            &identity,
            None,
            &MembershipProjection::rebuild(&input).unwrap(),
        )
        .await
        .unwrap();
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        store,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    assert!(service.read(&identity, 24).await.unwrap().is_some());
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let owner = tokio::spawn(service.clone().run(receiver));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while jobs.membership_reads() < 3
            || service
                .progress(&identity)
                .await
                .is_some_and(|state| state.contains("running"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let result = service.peek(&identity, 24).await;
    cancel.send(true).unwrap();
    owner.await.unwrap();
    let why =
        result.expect_err("latest actual source failure cannot be a fresh READY with why:null");
    assert!(
        why.contains("native membership history unavailable")
            && why.contains(&input.until.to_string()),
        "{why}"
    );
}

#[tokio::test]
async fn generation_freshness_uses_the_explicit_observation_clock() {
    let cut = chrono::DateTime::parse_from_rfc3339("2020-01-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let identity = ProjectionIdentity::new(Some("alice".into()), JobScope::All);
    for (seconds, expected) in [(1, "ready"), (6, "stale"), (-1, "future")] {
        let clock = boss_clock_client::FixedClockClient::new(
            serde_json::from_value(serde_json::json!({
                "now": cut + Duration::seconds(seconds), "simulated": false
            }))
            .unwrap(),
        );
        let service = MembershipProjectionService::new(
            Arc::new(InMemoryJobs::new()),
            Arc::new(WindowStore {
                until: cut,
                starts: std::sync::Mutex::new(vec![]),
            }),
            Arc::new(clock),
        );
        let result = service.peek(&identity, 24).await;
        if expected == "ready" {
            assert!(result.unwrap().is_some());
        } else {
            assert!(
                result.unwrap_err().contains(expected),
                "observation clock must determine {expected}"
            );
        }
    }
}
