//! Bounded active map readers and one best-effort warmer per replica.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use boss_core::port::EventBus;
use boss_policy_client::User;
use tokio::time::Instant;

use super::JobsApiState;
use crate::{JobsRepository, reading_cache::ReadingCache};

const READERS: usize = 16;
use crate::membership_projection::READER_IDLE as IDLE;
const TICK: Duration = Duration::from_secs(10);

struct Reader {
    key: String,
    user: User,
    window: i64,
    seen: Instant,
}

/// Completed paired map readings and the callers this replica keeps warm.
/// Remembering a caller grants nothing: every warm tick reauthorizes it.
pub struct MapReadings {
    pub(super) cache: ReadingCache<super::regions::MapReading>,
    readers: Mutex<Vec<Reader>>,
}

impl Default for MapReadings {
    fn default() -> Self {
        Self::new()
    }
}

impl MapReadings {
    pub fn new() -> Self {
        Self::with_lifetime(Duration::from_secs(30))
    }

    /// Configure the paired reading's age independently of the warm tick.
    pub fn with_lifetime(lifetime: Duration) -> Self {
        Self {
            cache: ReadingCache::new(lifetime),
            readers: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn remember(&self, user: &User, window: i64) {
        let key = match serde_json::to_string(&(user, window)) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(%error, "map warmer could not retain reader identity");
                return;
            }
        };
        let now = Instant::now();
        let mut readers = self
            .readers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        readers.retain(|entry| now.duration_since(entry.seen) < IDLE && entry.key != key);
        readers.push(Reader {
            key,
            user: user.clone(),
            window,
            seen: now,
        });
        if readers.len() > READERS {
            readers.remove(0);
        }
    }

    pub(super) fn readers(&self) -> Vec<(User, i64)> {
        let now = Instant::now();
        let mut readers = self
            .readers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        readers.retain(|entry| now.duration_since(entry.seen) < IDLE);
        readers
            .iter()
            .map(|entry| (entry.user.clone(), entry.window))
            .collect()
    }
}

async fn run_loop<F, Fut>(
    readings: &MapReadings,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    mut warm: F,
) where
    F: FnMut(User, i64) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let default: User = match serde_json::from_str(&boss_core::roles::reader_header(
        boss_core::roles::PROBE_READER_ACTOR,
    )) {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(%error, "map warmer could not read canonical probe identity");
            return;
        }
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if *cancel.borrow() {
            return;
        }
        tokio::select! {
            biased;
            _ = cancel.changed() => return,
            _ = tick.tick() => {}
        }
        // Seed only the exact native probe-reader keys. Other callers enter
        // through authorized HTTP reads, never through identity substitution.
        readings.remember(&default, 24);
        for (user, window) in readings.readers() {
            let actor = user.id.clone();
            tokio::select! {
                biased;
                _ = cancel.changed() => return,
                result = warm(user, window) => if let Err(error) = result {
                    tracing::warn!(%actor, window, %error, "map warmer read refused");
                }
            }
        }
    }
}

/// Start independent map/flow refreshes without waiting on their history.
pub async fn run_map_warmer<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: Arc<JobsApiState<R, B>>,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    run_loop(&state.map_readings, cancel, |user, window| {
        let state = state.clone();
        async move { super::regions::warm_map_for(&state, &user, window).await }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hung_flow_releases_io_but_a_late_cpu_keeps_its_permit() {
        let gate = std::sync::Arc::new(super::super::flow_gate::FlowGate::new());
        let first = gate.clone();
        let refused = first
            .run(
                "hung",
                Duration::from_millis(10),
                |_lease, cancelled| async move {
                    while !cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err::<(), _>("cancelled I/O".to_string())
                },
            )
            .await;
        assert!(refused.unwrap_err().contains("deadline"));
        assert!(
            gate.run(
                "next",
                Duration::from_millis(100),
                |_lease, _cancelled| async { Ok::<_, String>(()) }
            )
            .await
            .is_ok()
        );
        let cancelling_gate = gate.clone();
        let cancelled = tokio::spawn(async move {
            cancelling_gate
                .run(
                    "cancelled",
                    Duration::from_secs(60),
                    |_lease, cancelled| async move {
                        while !cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err::<(), _>("cancelled I/O".to_string())
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(gate.progress("cancelled").unwrap().contains("running"));
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        assert!(
            gate.run(
                "after-cancel",
                Duration::from_millis(100),
                |_lease, _cancelled| async { Ok::<_, String>(()) }
            )
            .await
            .is_ok()
        );
        assert!(gate.progress("cancelled").is_none());
        let (release, held) = std::sync::mpsc::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let late = gate
            .run(
                "late",
                Duration::from_millis(10),
                move |lease, _cancelled| async move {
                    tokio::task::spawn_blocking(move || {
                        let _lease = lease;
                        held.recv().unwrap();
                        let _ = done.send(());
                        Ok::<_, String>(())
                    })
                    .await
                    .map_err(|e| e.to_string())?
                },
            )
            .await;
        assert!(late.unwrap_err().contains("deadline"));
        assert!(
            gate.run(
                "late",
                Duration::from_millis(10),
                |_lease, _cancelled| async { Ok::<_, String>(()) }
            )
            .await
            .unwrap_err()
            .contains("no duplicate")
        );
        let waiting_gate = gate.clone();
        let waiting = tokio::spawn(async move {
            waiting_gate
                .run(
                    "queued",
                    Duration::from_millis(20),
                    |_lease, _cancelled| async { Ok::<_, String>(()) },
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            gate.progress("queued")
                .unwrap()
                .contains("behind 1 running")
        );
        assert!(waiting.await.unwrap().unwrap_err().contains("queued"));
        release.send(()).unwrap();
        finished.await.unwrap();
        assert!(
            gate.run(
                "released",
                Duration::from_millis(100),
                |_lease, _cancelled| async { Ok::<_, String>(()) }
            )
            .await
            .is_ok()
        );
    }

    fn reader(id: &str) -> boss_policy_client::User {
        serde_json::from_str(&boss_core::roles::reader_header(id)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn remembered_readers_are_bounded_idle_expired_and_scope_distinct() {
        let readings = MapReadings::new();
        for n in 0..20 {
            readings.remember(&reader(&format!("reader-{n}")), 24);
        }
        assert_eq!(readings.readers().len(), 16);
        assert!(!readings.readers().iter().any(|(u, _)| u.id == "reader-0"));
        let mut changed = reader("reader-19");
        changed.department = Some("another-department".into());
        readings.remember(&changed, 24);
        assert_eq!(
            readings
                .readers()
                .iter()
                .filter(|(u, _)| u.id == changed.id)
                .count(),
            2
        );
        tokio::time::advance(std::time::Duration::from_secs(601)).await;
        assert!(readings.readers().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn defaults_are_the_native_reader_and_cancellation_stops_ticks() {
        let readings = std::sync::Arc::new(MapReadings::new());
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            run_loop(&readings, receiver, move |user, window| {
                observed.lock().unwrap().push((user, window));
                std::future::ready(Ok(()))
            })
            .await;
        });
        tokio::task::yield_now().await;
        let first = calls.lock().unwrap().clone();
        assert_eq!(first.len(), 1);
        for (user, _) in &first {
            assert_eq!(
                serde_json::to_value(user).unwrap(),
                serde_json::to_value(reader(boss_core::roles::PROBE_READER_ACTOR)).unwrap()
            );
        }
        assert_eq!(first.iter().map(|(_, w)| *w).collect::<Vec<_>>(), vec![24]);
        cancel.send(true).unwrap();
        task.await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_also_interrupts_a_policy_read_that_never_answers() {
        let readings = std::sync::Arc::new(MapReadings::new());
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = entered.clone();
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            run_loop(&readings, receiver, move |_, _| {
                signal.notify_one();
                std::future::pending::<Result<(), String>>()
            })
            .await;
        });
        entered.notified().await;
        cancel.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
