//! Authorized compact generation reads. History belongs to the owned updater,
//! never to a viewer's request.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

use chrono::{Duration, Utc};
use tokio::sync::Mutex;

use crate::{
    membership_projection::{
        MAX_GENERATIONS, MembershipProjectionRepository, ProjectionCursor, ProjectionIdentity,
        READER_IDLE, StoredProjection,
    },
    membership_replay::{
        FlowWindows, MapFlowReading, ReplayBudget,
        projection::{MembershipProjection, ProjectionBinding},
    },
    port::JobsRepository,
};

pub(crate) const FLOW_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);
pub(crate) const FLOW_COMPARISON_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const FLOW_REPLAY_FUEL: usize = usize::MAX;
pub(crate) const SOURCE_ROW_LIMIT: i64 = 250_000;
const HEALTHY_FOR: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod observation_tests;
mod refresh_trace;
#[cfg(test)]
mod refusal_tests;
pub use refresh_trace::Observation as RefreshObservation;
use refresh_trace::{Receipts, Trace};

#[cfg(test)]
mod source_deadline_tests {
    use super::*;
    use crate::{InMemoryJobs, membership_projection::InMemoryMembershipProjection};

    #[tokio::test(start_paused = true)]
    async fn a_pending_comparison_source_cannot_starve_the_current_window() {
        let jobs = Arc::new(InMemoryJobs::new());
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        *jobs.projection_source_gate.lock().unwrap() = Some(gate.clone());
        let store = Arc::new(InMemoryMembershipProjection::new());
        let identity = ProjectionIdentity::new(Some("alice".into()), crate::port::JobScope::All);
        let until = chrono::Utc::now();
        let since = until - chrono::Duration::hours(48);
        let clock = boss_clock_client::mapping_history::ClockMappingFact {
            schema_version: 1,
            revision: "original-clock".into(),
            previous_revision: None,
            epoch: Some("original-epoch".into()),
            mode: Some(boss_clock_client::ClockMode::Wall),
            params: None,
            captured_at: since - chrono::Duration::seconds(2),
            audit_created_at: since - chrono::Duration::seconds(1),
            seq: 1,
            complete_group: true,
        };
        jobs.answer_membership(crate::membership_replay::ReplayInput {
            groups: vec![],
            since,
            until,
            audit_cutoff: 1,
            actor: identity.actor.clone(),
            scope: identity.scope.clone(),
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
        let service = Arc::new(MembershipProjectionService::new(
            jobs.clone(),
            store.clone(),
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let owner_identity = identity.clone();
        let owner_service = service.clone();
        let owner = tokio::spawn(async move { owner_service.refresh_one(&owner_identity).await });
        gate.0.notified().await;
        // Advance this flight only after actual source admission, then let
        // the adapter's real periodic check observe the shared expiry.
        service.receipts.advance(
            &identity.key().unwrap(),
            FLOW_COMPARISON_DEADLINE + std::time::Duration::from_secs(1),
        );
        tokio::time::advance(std::time::Duration::from_millis(10)).await;
        observation_tests::wait_phase(&service, "source_end").await;
        let current_was_attempted = jobs.membership_reads() > 0;
        // Always release and join the actual source, even on the RED path.
        gate.1.notify_one();
        let result = owner.await.unwrap();
        assert!(
            result.is_ok(),
            "the complete current window must publish: {result:?}"
        );
        let stored = store.load(&identity).await.unwrap().unwrap();
        assert_eq!(
            stored.checkpoint.until(),
            until,
            "retain the original witnessed cut"
        );
        assert!(stored.checkpoint.since() >= since + chrono::Duration::hours(24));
        assert!(
            current_was_attempted,
            "the optional 48h source consumed its 30s allowance without letting the complete 24h source start"
        );
    }

    #[tokio::test]
    async fn cancellation_releases_the_owned_source_without_starting_fallback() {
        let jobs = Arc::new(InMemoryJobs::new());
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        *jobs.projection_source_gate.lock().unwrap() = Some(gate.clone());
        let service = Arc::new(MembershipProjectionService::new(
            jobs.clone(),
            Arc::new(InMemoryMembershipProjection::new()),
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let identity = ProjectionIdentity::new(Some("alice".into()), crate::port::JobScope::All);
        let owner = tokio::spawn(async move { service.refresh_owned(&identity, flag).await });
        gate.0.notified().await;
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        // Completion while the source remains held proves cancellation, not
        // fixture release, ended this owned attempt.
        let result = owner.await.unwrap();
        gate.1.notify_one();
        let why = result.unwrap_err();
        assert!(why.contains("cancelled"), "{why}");
        assert_eq!(
            jobs.membership_reads(),
            0,
            "cancellation cannot reset into another source"
        );
    }
}

struct RefreshFailure {
    why: String,
    retry_at: Instant,
    flight: Option<uuid::Uuid>,
    receipt: Option<refresh_trace::Flight>,
}

struct ActiveRefresh {
    key: String,
    started: Instant,
    wall: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct Viewer {
    identity: ProjectionIdentity,
    seen: tokio::time::Instant,
}

pub struct MembershipProjectionService {
    jobs: Arc<dyn JobsRepository>,
    store: Arc<dyn MembershipProjectionRepository>,
    observation_clock: Arc<dyn boss_clock_client::ClockClient>,
    viewers: Mutex<VecDeque<Viewer>>,
    generations: Mutex<BTreeMap<String, Option<Arc<StoredProjection>>>>,
    failures: Mutex<BTreeMap<String, RefreshFailure>>,
    comparison_failures: Mutex<BTreeMap<String, String>>,
    active: Mutex<Option<ActiveRefresh>>,
    receipts: Receipts,
    #[cfg(test)]
    fold_gate: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

impl MembershipProjectionService {
    pub fn new(
        jobs: Arc<dyn JobsRepository>,
        store: Arc<dyn MembershipProjectionRepository>,
        observation_clock: Arc<dyn boss_clock_client::ClockClient>,
    ) -> Self {
        Self {
            jobs,
            store,
            observation_clock,
            viewers: Mutex::new(VecDeque::new()),
            generations: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeMap::new()),
            comparison_failures: Mutex::new(BTreeMap::new()),
            active: Mutex::new(None),
            receipts: Receipts::default(),
            #[cfg(test)]
            fold_gate: std::sync::Mutex::new(None),
        }
    }

    /// Caller identity is reconstructed by the protected HTTP door. This
    /// reads only receipts; it admits no viewer and touches no repository.
    pub async fn observe_refresh(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<RefreshObservation, String> {
        let key = identity.key().map_err(|error| error.to_string())?;
        let mut observation = self.receipts.read(&key);
        if let Some(failure) = self.failures.lock().await.get(&key) {
            observation.cached_failure_flight = failure.flight;
            observation.cached_failure = failure.receipt.clone();
        }
        Ok(observation)
    }

    /// Observe the actual owned flight, never a spawned-and-forgotten waiter.
    pub async fn progress(&self, identity: &ProjectionIdentity) -> Option<String> {
        let key = identity.key().ok()?;
        if let Some(active) = self
            .active
            .lock()
            .await
            .as_ref()
            .filter(|active| active.key == key)
        {
            let phase = if active.started.elapsed() >= FLOW_READ_DEADLINE {
                "late: response deadline exhausted; actual work remains owned"
            } else {
                "running"
            };
            return Some(format!(
                "membership projection {phase} since {}; elapsed {}s",
                active.wall,
                active.started.elapsed().as_secs()
            ));
        }
        let mut viewers = self.viewers.lock().await;
        viewers.retain(|viewer| viewer.seen.elapsed() < READER_IDLE);
        if viewers.iter().any(|viewer| &viewer.identity == identity) {
            return Some("membership projection queued for the owned updater".into());
        }
        None
    }

    /// Updater-only entry point. The serial owner retains SQL and CPU work
    /// until actual completion. Publication checks the same budget before
    /// COMMIT; an acknowledgement crossing that boundary is retained and
    /// explicitly reported as a late acknowledged publication.
    pub async fn refresh_one(&self, identity: &ProjectionIdentity) -> Result<(), String> {
        self.refresh_owned(identity, Arc::new(AtomicBool::new(false)))
            .await
    }

    async fn refresh_owned(
        &self,
        identity: &ProjectionIdentity,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let trace = Trace::new(Instant::now(), cancelled.clone());
        let key = identity.key().map_err(|error| error.to_string())?;
        self.receipts.begin(&key, trace.clone());
        let result = self.refresh_traced(identity, cancelled, &trace).await;
        trace.finish(if result.is_ok() {
            "completed"
        } else {
            "refused"
        });
        self.receipts.finish(&key, &trace);
        result
    }

    async fn refresh_traced(
        &self,
        identity: &ProjectionIdentity,
        cancelled: Arc<AtomicBool>,
        trace: &Arc<Trace>,
    ) -> Result<(), String> {
        let deadline = trace.deadline;
        let key = identity.key().map_err(|error| error.to_string())?;
        trace.event("generation_lock_begin", "flight", deadline, None);
        let mut generations = self.generations.lock().await;
        trace.event("generation_lock_acquired", "flight", deadline, None);
        if !generations.contains_key(&key) {
            trace.event("generation_load_begin", "flight", deadline, None);
            let loaded = self.store.load(identity).await;
            trace.event(
                if loaded.is_ok() {
                    "generation_load_end"
                } else {
                    "generation_load_refused"
                },
                "flight",
                deadline,
                None,
            );
            let loaded = loaded.map_err(|error| error.to_string())?;
            if generations.len() == MAX_GENERATIONS {
                generations.pop_first();
            }
            generations.insert(key.clone(), loaded.map(Arc::new));
        }
        let previous = generations.get(&key).and_then(Option::as_ref);
        let since = match previous {
            Some(old) => old.checkpoint.since(),
            None => self.observation_clock.now().await.now - Duration::hours(48),
        };
        let cursor = previous.map(|old| ProjectionCursor {
            until: old.checkpoint.until(),
            audit_cutoff: old.checkpoint.audit_cutoff(),
            currently_authorized: old.checkpoint.binding().currently_authorized.clone(),
        });
        let budget = Arc::new(trace.budget(deadline, cancelled.clone()));
        let old = previous.cloned();
        let comparison_budget = if previous.is_none() {
            Arc::new(trace.budget(
                deadline.min(trace.now() + FLOW_COMPARISON_DEADLINE),
                cancelled.clone(),
            ))
        } else {
            budget.clone()
        };
        let mut candidate = self
            .candidate(
                identity,
                since,
                cursor,
                old,
                comparison_budget,
                trace.clone(),
                if previous.is_some() {
                    "delta"
                } else {
                    "initial_48h"
                },
            )
            .await;
        if previous.is_some() && candidate.is_err() {
            // A refused delta never damages the last good generation. A
            // bounded background rebuild is the only recovery path.
            budget.check()?;
            candidate = self
                .candidate(
                    identity,
                    self.observation_clock.now().await.now - Duration::hours(48),
                    None,
                    None,
                    budget.clone(),
                    trace.clone(),
                    "rebuild_48h",
                )
                .await;
        }
        let next = match candidate {
            Ok(next) => next,
            Err(wide_why) => {
                // The comparison is optional; a refused wider fold must not
                // turn a separately provable current window into silence.
                budget.check()?;
                let mut comparisons = self.comparison_failures.lock().await;
                if comparisons.len() == MAX_GENERATIONS && !comparisons.contains_key(&key) {
                    comparisons.pop_first();
                }
                comparisons.insert(key.clone(), wide_why.clone());
                drop(comparisons);
                self.candidate(
                    identity,
                    self.observation_clock.now().await.now - Duration::hours(24),
                    None,
                    None,
                    budget.clone(),
                    trace.clone(),
                    "fallback_24h",
                )
                .await
                .map_err(|why| format!("48-hour history: {wide_why}; 24-hour history: {why}"))?
            }
        };
        budget.check()?;
        trace.event("publication_begin", "flight", deadline, None);
        let revision = self
            .store
            .commit_with_budget(identity, previous.map(Arc::as_ref), &next, &budget)
            .await;
        trace.event(
            match &revision {
                Ok(Some(_)) => "publication_acknowledged",
                Ok(None) => "publication_conflict",
                Err(_) => "publication_refused",
            },
            "flight",
            deadline,
            revision.as_ref().ok().copied().flatten(),
        );
        let revision = revision.map_err(|error| error.to_string())?;
        match revision {
            Some(revision) => {
                generations.insert(
                    key,
                    Some(Arc::new(StoredProjection {
                        revision,
                        checkpoint: next,
                    })),
                );
                trace.event("publication_end", "flight", deadline, Some(revision));
                if let Err(why) = budget.check() {
                    return Err(format!(
                        "membership projection publication acknowledged at revision {revision}, but owned flight completed late: {why}"
                    ));
                }
            }
            None => {
                generations.remove(&key);
                return Err("membership projection concurrent writer won; reload required".into());
            }
        }
        Ok(())
    }

    async fn candidate(
        &self,
        identity: &ProjectionIdentity,
        since: chrono::DateTime<Utc>,
        cursor: Option<ProjectionCursor>,
        old: Option<Arc<StoredProjection>>,
        budget: Arc<ReplayBudget>,
        trace: Arc<Trace>,
        attempt: &'static str,
    ) -> Result<MembershipProjection, String> {
        budget.check()?;
        trace.event("source_begin", attempt, budget.deadline(), None);
        let source = self
            .jobs
            .membership_projection_source_with_budget(
                since,
                identity.actor.clone(),
                &identity.scope,
                cursor,
                SOURCE_ROW_LIMIT,
                budget.clone(),
            )
            .await;
        trace.event(
            if source.is_ok() {
                "source_end"
            } else {
                "source_refused"
            },
            attempt,
            budget.deadline(),
            None,
        );
        let source = source.map_err(|error| error.to_string())?;
        budget.check()?;
        trace.event("fold_queued", attempt, budget.deadline(), None);
        let folding_trace = trace.clone();
        let attempt_deadline = budget.deadline();
        #[cfg(test)]
        let fold_gate = self.fold_gate.lock().unwrap().take();
        let result = tokio::task::spawn_blocking(move || {
            folding_trace.event("fold_start", attempt, attempt_deadline, None);
            #[cfg(test)]
            if let Some((entered, release)) = fold_gate {
                let _ = entered.send(());
                release.recv().unwrap();
            }
            let result = (|| {
                budget.check()?;
                let next = match old {
                    Some(previous) => {
                        let mut checkpoint = previous.checkpoint.clone();
                        checkpoint
                            .reconcile_checked(
                                &source.input,
                                source.replacement_history.as_ref(),
                                &source.replacement_ids,
                                &budget,
                            )
                            .map_err(|error| {
                                folding_trace.refusal(attempt, &error);
                                error.to_string()
                            })?;
                        checkpoint
                    }
                    None => MembershipProjection::rebuild_checked(&source.input, &budget).map_err(
                        |error| {
                            folding_trace.refusal(attempt, &error);
                            error.to_string()
                        },
                    )?,
                };
                budget.check()?;
                Ok(next)
            })();
            folding_trace.event(
                if result.is_ok() {
                    "fold_end"
                } else {
                    "fold_refused"
                },
                attempt,
                attempt_deadline,
                None,
            );
            result
        })
        .await;
        trace.event(
            if result.is_ok() {
                "fold_joined"
            } else {
                "fold_join_failed"
            },
            attempt,
            attempt_deadline,
            None,
        );
        result.map_err(|error| format!("membership projection fold task: {error}"))?
    }

    /// One owned FIFO updater; stopping waits for the current actual work.
    pub async fn run(self: Arc<Self>, mut cancel: tokio::sync::watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        'updates: loop {
            tokio::select! {
                changed = cancel.changed() => { if changed.is_err() || *cancel.borrow() { break; } }
                _ = tick.tick() => {
                    let readers = self.viewers.lock().await.len();
                    for _ in 0..readers {
                    let identity = {
                        let mut viewers = self.viewers.lock().await;
                        viewers.retain(|viewer| viewer.seen.elapsed() < READER_IDLE);
                        let viewer = viewers.pop_front();
                        if let Some(viewer) = &viewer { viewers.push_back(viewer.clone()); }
                        viewer.map(|viewer| viewer.identity)
                    };
                    if let Some(identity) = identity {
                        if let Ok(key) = identity.key()
                            && self.failures.lock().await.get(&key).is_some_and(|failure| Instant::now() < failure.retry_at) { continue; }
                        let cancelled = Arc::new(AtomicBool::new(false));
                        if let Ok(key) = identity.key() {
                            *self.active.lock().await = Some(ActiveRefresh { key, started: Instant::now(), wall: self.observation_clock.now().await.now });
                        }
                        let refresh = self.refresh_owned(&identity, cancelled.clone());
                        tokio::pin!(refresh);
                        let result = tokio::select! {
                            result = &mut refresh => result,
                            _ = cancel.changed() => {
                                if let Ok(key) = identity.key() { self.receipts.cancel_requested(&key); }
                                cancelled.store(true, std::sync::atomic::Ordering::Release);
                                // Keep the actual SQL/fold owned through its exit.
                                let _ = refresh.await;
                                *self.active.lock().await = None;
                                self.viewers.lock().await.clear();
                                break 'updates;
                            }
                        };
                        if let Err(why) = result {
                            tracing::warn!(%why, "membership projection refresh refused");
                            if let Ok(key) = identity.key() {
                                let mut failures = self.failures.lock().await;
                                if failures.len() == MAX_GENERATIONS && !failures.contains_key(&key) { failures.pop_first(); }
                                let receipt = self.receipts.read(&key).latest_completed;
                                let flight = receipt.as_ref().map(|flight| flight.id);
                                failures.insert(key, RefreshFailure { why, retry_at: Instant::now() + crate::reading_cache::MAP_FLOW_LIFETIME, flight, receipt });
                            }
                        } else if let Ok(key) = identity.key() {
                            self.failures.lock().await.remove(&key);
                        }
                        *self.active.lock().await = None;
                    }
                    }
                }
            }
        }
        *self.active.lock().await = None;
        self.viewers.lock().await.clear();
    }

    /// Reauthorize before touching a previously published generation.
    pub async fn binding(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<ProjectionBinding, String> {
        let currently_authorized = self
            .jobs
            .membership_census(&identity.scope, SOURCE_ROW_LIMIT)
            .await
            .map_err(|error| error.to_string())?;
        Ok(ProjectionBinding {
            actor: identity.actor.clone(),
            scope: identity.scope.clone(),
            currently_authorized,
        })
    }

    /// None means a cold identity, never a fabricated empty history. Both
    /// compact windows must come from the same atomic generation.
    pub async fn read(
        &self,
        identity: &ProjectionIdentity,
        hours: i64,
    ) -> Result<Option<MapFlowReading>, String> {
        self.read_compact(identity, hours, true).await
    }

    /// State endpoints authorize and read completed generations without
    /// admitting history work. The independent flow door owns scheduling.
    pub async fn peek(
        &self,
        identity: &ProjectionIdentity,
        hours: i64,
    ) -> Result<Option<MapFlowReading>, String> {
        self.read_compact(identity, hours, false).await
    }

    async fn read_compact(
        &self,
        identity: &ProjectionIdentity,
        hours: i64,
        schedule: bool,
    ) -> Result<Option<MapFlowReading>, String> {
        if !matches!(hours, 24 | 48) {
            return Err("membership projection window must be 24 or 48 hours".into());
        }
        let binding = self.binding(identity).await?;
        if schedule {
            let mut viewers = self.viewers.lock().await;
            viewers.retain(|viewer| viewer.seen.elapsed() < READER_IDLE);
            if let Some(viewer) = viewers
                .iter_mut()
                .find(|viewer| &viewer.identity == identity)
            {
                viewer.seen = tokio::time::Instant::now();
            } else {
                if viewers.len() == MAX_GENERATIONS {
                    viewers.pop_front();
                }
                viewers.push_back(Viewer {
                    identity: identity.clone(),
                    seen: tokio::time::Instant::now(),
                });
            }
        }
        let now = self.observation_clock.now().await.now;
        let Some(pair) = self
            .store
            .read_windows(identity, hours, now, &binding)
            .await
            .map_err(|error| error.to_string())?
        else {
            if let Some(failure) = self
                .failures
                .lock()
                .await
                .get(&identity.key().map_err(|error| error.to_string())?)
            {
                return Err(failure.why.clone());
            }
            return Ok(None);
        };
        let observed = self.observation_clock.now().await.now;
        if pair.until > observed {
            return Err(format!(
                "membership projection has a future native watermark {}",
                pair.until
            ));
        }
        let stale = observed
            .signed_duration_since(pair.until)
            .to_std()
            .is_ok_and(|age| age > HEALTHY_FOR);
        if let Some(failure) = self
            .failures
            .lock()
            .await
            .get(&identity.key().map_err(|error| error.to_string())?)
        {
            return Err(format!(
                "membership projection {}latest refresh refused: {}; last generation {}",
                if stale { "is stale; " } else { "" },
                failure.why,
                pair.until
            ));
        }
        if stale {
            return Err(format!(
                "membership projection is stale; last generation {}",
                pair.until
            ));
        }
        if pair.this.until != pair.until
            || pair.both.as_ref().is_ok_and(|wide| {
                wide.until != pair.until || wide.audit_cutoff != pair.this.audit_cutoff
            })
        {
            return Err(
                "membership projection canonical windows have different original cuts".into(),
            );
        }
        let both = match pair.both {
            Ok(wide) => Ok(wide
                .stations
                .into_iter()
                .map(|station| (station.station.clone(), station))
                .collect()),
            Err(why) if hours == 24 => Err(self
                .comparison_failures
                .lock()
                .await
                .get(&identity.key().map_err(|error| error.to_string())?)
                .cloned()
                .unwrap_or(why)),
            Err(why) => Err(why),
        };
        if let Some(failure) = self
            .failures
            .lock()
            .await
            .get(&identity.key().map_err(|error| error.to_string())?)
        {
            return Err(format!(
                "membership projection latest refresh refused: {}; last generation {}",
                failure.why, pair.until
            ));
        }
        if self
            .observation_clock
            .now()
            .await
            .now
            .signed_duration_since(pair.until)
            .to_std()
            .is_ok_and(|age| age > HEALTHY_FOR)
        {
            return Err(format!(
                "membership projection became stale during compact read; last generation {}",
                pair.until
            ));
        }
        Ok(Some(MapFlowReading {
            as_of: pair.until,
            windows: Ok(FlowWindows {
                this: pair
                    .this
                    .stations
                    .into_iter()
                    .map(|station| (station.station.clone(), station))
                    .collect(),
                both,
            }),
        }))
    }
}
