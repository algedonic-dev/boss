//! Latest successful readings, refreshed once per key in the background.
//!
//! Expiry permits another read; it never hides the last usable reading.
//! HTTP surfaces use `latest` and `refresh` without waiting on history.
//! Only an explicit cold `get_or_read` waits. This process-local memo is
//! not a store of record, and a reading retains its own timestamp.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// Cool down for two minutes after a successful flow refresh, retaining its own
/// `as_of` rather than re-dating it to the serving request's instant.
pub const MAP_FLOW_LIFETIME: Duration = Duration::from_secs(120);

// Evict least recently accessed idle keys, never active slots (which
// would permit another flight). Active keys may temporarily exceed this
// bound; the next cache access after they finish trims them.
const RETAINED_KEYS: usize = 128;
type Slots<T> = HashMap<String, (Instant, Arc<Slot<T>>)>;

#[derive(Default)]
struct Retry {
    not_before: Option<Instant>,
    failures: u32,
}

struct Slot<T> {
    reading: Mutex<Option<(Instant, Arc<T>)>>,
    failure: Mutex<Option<String>>,
    retry: Mutex<Retry>,
    flight: Arc<tokio::sync::Mutex<()>>,
}

impl<T> Slot<T> {
    fn new() -> Self {
        Self {
            reading: Mutex::new(None),
            failure: Mutex::new(None),
            retry: Mutex::new(Retry::default()),
            flight: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn latest(&self) -> Option<Arc<T>> {
        self.reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|(_, reading)| reading.clone())
    }

    fn cooling(&self) -> bool {
        self.retry_deadline().is_some_and(|at| Instant::now() < at)
    }

    fn retry_deadline(&self) -> Option<Instant> {
        self.retry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .not_before
    }

    fn last_failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn fail(&self, error: String, lifetime: Duration) {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
        let mut retry = self
            .retry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retry.failures = retry.failures.saturating_add(1);
        let factor = 1_u32 << retry.failures.saturating_sub(1).min(4);
        retry.not_before = Instant::now().checked_add(lifetime.saturating_mul(factor));
    }

    fn store(&self, value: T, lifetime: Duration) -> Arc<T> {
        let value = Arc::new(value);
        *self
            .reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Instant::now(), value.clone()));
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .retry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Retry {
            not_before: Instant::now().checked_add(lifetime),
            failures: 0,
        };
        value
    }
}

/// Latest successful readings with one refresh at a time per key.
pub struct ReadingCache<T> {
    lifetime: Duration,
    slots: Mutex<Slots<T>>,
}

impl<T> std::fmt::Debug for ReadingCache<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadingCache")
            .field("lifetime", &self.lifetime)
            .finish_non_exhaustive()
    }
}

impl<T> ReadingCache<T> {
    /// Zero lifetime permits a refresh on every ask, while still serving
    /// the previous reading immediately.
    pub fn new(lifetime: Duration) -> Self {
        Self {
            lifetime,
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub fn lifetime(&self) -> Duration {
        self.lifetime
    }

    /// Latest successful reading, regardless of age. Does not start a
    /// read, create an empty key, or wait on a flight in progress.
    pub fn latest(&self, key: &str) -> Option<Arc<T>> {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let found = slots.get_mut(key).map(|(used, slot)| {
                *used = Instant::now();
                slot.clone()
            });
            Self::trim(&mut slots);
            found
        };
        slot.and_then(|slot| slot.latest())
    }

    /// Last refresh error verbatim, without starting or waiting for a read.
    /// A later successful reading clears it; missing keys remain absent.
    pub fn last_failure(&self, key: &str) -> Option<String> {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let found = slots.get_mut(key).map(|(used, slot)| {
                *used = Instant::now();
                slot.clone()
            });
            Self::trim(&mut slots);
            found
        };
        slot.and_then(|slot| slot.last_failure())
    }

    fn slot(&self, key: &str) -> Arc<Slot<T>> {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (used, slot) = slots
            .entry(key.to_owned())
            .or_insert_with(|| (Instant::now(), Arc::new(Slot::new())));
        *used = Instant::now();
        let slot = slot.clone();
        Self::trim(&mut slots);
        slot
    }

    fn trim(slots: &mut Slots<T>) {
        while slots.len() > RETAINED_KEYS {
            let oldest = slots
                .iter()
                .filter(|(_, (_, slot))| Arc::strong_count(slot) == 1)
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            slots.remove(&oldest);
        }
    }
}

impl<T: Send + Sync + 'static> ReadingCache<T> {
    /// Schedule a cold or expired read and return immediately. Fresh
    /// readings, cooling keys and existing flights start no additional work. Errors
    /// retain the previous reading and expose their exact cause via `last_failure`.
    /// RAII releases the flight on cancellation or panic too.
    pub fn refresh<F, Fut, E>(&self, key: &str, read: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        E: Send + ToString + 'static,
    {
        let slot = self.slot(key);
        if slot.cooling() {
            return;
        }
        let Ok(flight) = slot.flight.clone().try_lock_owned() else {
            return;
        };
        if slot.cooling() {
            return;
        }
        let lifetime = self.lifetime;
        tokio::spawn(async move {
            let _flight = flight;
            match read().await {
                Ok(value) => {
                    slot.store(value, lifetime);
                }
                Err(error) => slot.fail(error.to_string(), lifetime),
            }
        });
    }

    /// Serve any previous reading immediately and refresh if expired.
    /// Only a cold miss may wait; cancellation releases its flight.
    pub async fn get_or_read<F, Fut, E>(&self, key: &str, read: F) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        E: Send + ToString + 'static,
    {
        let slot = self.slot(key);
        if let Some(reading) = slot.latest() {
            self.refresh(key, read);
            return Ok(reading);
        }
        let _flight = slot.flight.clone().lock_owned().await;
        if let Some(reading) = slot.latest() {
            return Ok(reading);
        }
        if let Some(deadline) = slot.retry_deadline() {
            tokio::time::sleep_until(deadline).await;
        }
        match read().await {
            Ok(value) => Ok(slot.store(value, self.lifetime)),
            Err(error) => {
                slot.fail(error.to_string(), self.lifetime);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, oneshot};

    async fn received<T>(rx: oneshot::Receiver<T>) -> T {
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap()
    }

    async fn seed(cache: &ReadingCache<u8>, key: &str, value: u8) -> Arc<u8> {
        cache
            .get_or_read(key, move || async move { Ok::<_, &str>(value) })
            .await
            .unwrap()
    }

    async fn settled(cache: &ReadingCache<u8>, key: &str) {
        tokio::task::yield_now().await;
        let slot = cache.slot(key);
        let _done = slot.flight.lock().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_cold_getter_observes_the_failed_refresh_cooldown() {
        let cache = Arc::new(ReadingCache::new(Duration::from_secs(10)));
        cache.refresh("k", || async { Err::<u8, _>("cold refusal") });
        settled(&cache, "k").await;
        let reads = Arc::new(AtomicUsize::new(0));
        let other = cache.clone();
        let calls = reads.clone();
        let task = tokio::spawn(async move {
            other
                .get_or_read("k", move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, &str>(7)
                })
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(9)).await;
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(*task.await.unwrap(), 7);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(cache.last_failure("k"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn success_cooldown_starts_when_a_slow_refresh_finishes() {
        let cache = ReadingCache::new(Duration::from_secs(120));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, wait) = oneshot::channel();
        cache.refresh("k", || async move {
            wait.await.unwrap();
            Ok::<_, &str>(7)
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(300)).await;
        release.send(()).unwrap();
        settled(&cache, "k").await;
        let first = cache.latest("k").unwrap();
        tokio::time::advance(Duration::from_secs(119)).await;
        let reads = calls.clone();
        cache.refresh("k", move || async move {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &str>(8)
        });
        settled(&cache, "k").await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(Arc::ptr_eq(&first, &cache.latest("k").unwrap()));
        tokio::time::advance(Duration::from_secs(1)).await;
        let reads = calls.clone();
        cache.refresh("k", move || async move {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &str>(8)
        });
        settled(&cache, "k").await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failures_back_off_from_completion_with_a_cap_and_success_resets_them() {
        let cache = ReadingCache::new(Duration::from_secs(10));
        let first = seed(&cache, "k", 7).await;
        tokio::time::advance(Duration::from_secs(10)).await;
        for delay in [10, 20, 40, 80, 160, 160] {
            let (release, wait) = oneshot::channel();
            cache.refresh("k", || async move {
                wait.await.unwrap();
                Err::<u8, _>("exact refusal")
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(300)).await;
            release.send(()).unwrap();
            settled(&cache, "k").await;
            assert_eq!(cache.last_failure("k").as_deref(), Some("exact refusal"));
            assert!(Arc::ptr_eq(&first, &cache.latest("k").unwrap()));
            tokio::time::advance(Duration::from_secs(delay - 1)).await;
            let forbidden = Arc::new(AtomicUsize::new(0));
            let reads = forbidden.clone();
            cache.refresh("k", move || async move {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>(0)
            });
            settled(&cache, "k").await;
            assert_eq!(
                forbidden.load(Ordering::SeqCst),
                0,
                "refresh started during backoff"
            );
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        cache.refresh("k", || async { Ok::<_, &str>(8) });
        settled(&cache, "k").await;
        assert_eq!(*cache.latest("k").unwrap(), 8);
        assert_eq!(cache.last_failure("k"), None);
        tokio::time::advance(Duration::from_secs(10)).await;
        cache.refresh("k", || async { Err::<u8, _>("after recovery") });
        settled(&cache, "k").await;
        tokio::time::advance(Duration::from_secs(10)).await;
        cache.refresh("k", || async { Ok::<_, &str>(9) });
        settled(&cache, "k").await;
        assert_eq!(*cache.latest("k").unwrap(), 9);
    }

    #[tokio::test]
    async fn cold_background_failure_is_visible_without_inventing_a_reading() {
        let cache = ReadingCache::<u8>::new(Duration::ZERO);
        assert_eq!(cache.last_failure("missing"), None);
        assert!(cache.slots.lock().unwrap().is_empty());
        let (done, completed) = oneshot::channel();
        cache.refresh("k", move || async move {
            done.send(()).unwrap();
            Err::<u8, _>("history refused: exact upstream cause")
        });
        received(completed).await;
        let slot = cache.slot("k");
        let _finished = slot.flight.lock().await;
        assert!(cache.latest("k").is_none());
        assert_eq!(
            cache.last_failure("k").as_deref(),
            Some("history refused: exact upstream cause")
        );
    }

    #[tokio::test]
    async fn an_expired_reading_returns_before_a_slow_refresh_finishes() {
        let cache = ReadingCache::new(Duration::ZERO);
        let first = seed(&cache, "k", 7).await;
        let (_release, wait) = oneshot::channel::<()>();
        let answer = tokio::time::timeout(
            Duration::from_secs(1),
            cache.get_or_read("k", move || async move {
                let _ = wait.await;
                Ok::<_, &str>(8)
            }),
        )
        .await
        .expect("a stale reading must not wait for the refresh")
        .unwrap();
        assert!(Arc::ptr_eq(&first, &answer));
    }

    #[tokio::test]
    async fn interleaved_24_and_48_hour_keys_keep_their_stale_readings() {
        let cache = ReadingCache::new(Duration::ZERO);
        let first = seed(&cache, "48h", 48).await;
        seed(&cache, "24h", 24).await;
        assert!(Arc::ptr_eq(&first, &cache.latest("48h").unwrap()));
    }

    #[tokio::test]
    async fn latest_serves_a_stale_reading_while_its_background_refresh_is_blocked() {
        let cache = ReadingCache::new(Duration::ZERO);
        let first = seed(&cache, "k", 7).await;
        let (started, began) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let (done, completed) = oneshot::channel();
        cache.refresh("k", move || async move {
            started.send(()).unwrap();
            wait.await.unwrap();
            done.send(()).unwrap();
            Ok::<_, &str>(8)
        });
        received(began).await;
        assert!(Arc::ptr_eq(&first, &cache.latest("k").unwrap()));
        release.send(()).unwrap();
        received(completed).await;
        assert_eq!(*cache.latest("k").unwrap(), 8);
    }

    #[tokio::test]
    async fn cold_refresh_returns_immediately_and_concurrent_refreshes_share_one_flight() {
        let cache = ReadingCache::new(Duration::ZERO);
        let reads = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let (release, wait) = oneshot::channel();
        let (done, completed) = oneshot::channel();
        let calls = reads.clone();
        let signal = started.clone();
        cache.refresh("k", move || async move {
            calls.fetch_add(1, Ordering::SeqCst);
            signal.notify_one();
            wait.await.unwrap();
            done.send(()).unwrap();
            Ok::<_, &str>(7)
        });
        assert!(cache.latest("k").is_none());
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        // Idle-key pressure cannot evict the active flight and allow
        // another refresh of the same cold key.
        for i in 0..RETAINED_KEYS {
            seed(&cache, &format!("other-{i}"), 1).await;
        }
        for _ in 0..8 {
            let calls = reads.clone();
            cache.refresh("k", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>(8)
            });
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        received(completed).await;
        assert_eq!(*cache.latest("k").unwrap(), 7);
    }

    #[tokio::test]
    async fn concurrent_cold_getters_share_the_same_reading() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        let reads = Arc::new(AtomicUsize::new(0));
        let readers = (0..8).map(|_| {
            let calls = reads.clone();
            cache.get_or_read("k", move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok::<_, &str>(7)
            })
        });
        let answers = futures::future::join_all(readers).await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let first = answers[0].as_ref().unwrap();
        assert!(
            answers
                .iter()
                .all(|answer| Arc::ptr_eq(first, answer.as_ref().unwrap()))
        );
    }

    #[tokio::test]
    async fn refresh_failure_keeps_the_previous_arc_and_allows_a_later_success() {
        let cache = ReadingCache::new(Duration::ZERO);
        let first = seed(&cache, "k", 7).await;
        let (done, completed) = oneshot::channel();
        cache.refresh("k", move || async move {
            done.send(()).unwrap();
            Err::<u8, _>("failed")
        });
        received(completed).await;
        assert!(Arc::ptr_eq(&first, &cache.latest("k").unwrap()));
        assert_eq!(cache.last_failure("k").as_deref(), Some("failed"));
        let (done, completed) = oneshot::channel();
        cache.refresh("k", move || async move {
            done.send(()).unwrap();
            Ok::<_, &str>(8)
        });
        received(completed).await;
        assert_eq!(*cache.latest("k").unwrap(), 8);
        assert_eq!(cache.last_failure("k"), None);
    }

    #[tokio::test]
    async fn a_cancelled_cold_read_releases_the_flight() {
        let cache = ReadingCache::new(Duration::ZERO);
        let pending = cache.get_or_read("k", || async {
            std::future::pending::<()>().await;
            Ok::<_, &str>(1)
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(10), pending)
                .await
                .is_err()
        );
        assert_eq!(*seed(&cache, "k", 2).await, 2);
    }

    #[tokio::test]
    async fn a_cold_error_leaves_the_key_available_for_a_retry() {
        let cache = ReadingCache::new(Duration::ZERO);
        assert_eq!(
            cache
                .get_or_read("k", || async { Err::<u8, _>("failed") })
                .await,
            Err("failed")
        );
        assert!(cache.latest("k").is_none());
        assert_eq!(cache.last_failure("k").as_deref(), Some("failed"));
        assert_eq!(*seed(&cache, "k", 2).await, 2);
        assert_eq!(cache.last_failure("k"), None);
    }

    #[tokio::test]
    async fn fresh_readings_start_no_refresh_and_latest_does_not_allocate_a_key() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        assert!(cache.latest("missing").is_none());
        assert!(cache.slots.lock().unwrap().is_empty());
        seed(&cache, "k", 7).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let calls = reads.clone();
        cache.refresh("k", move || async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &str>(8)
        });
        tokio::task::yield_now().await;
        assert_eq!(*seed(&cache, "k", 8).await, 7);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn idle_capacity_evicts_the_least_recently_used_key() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        for i in 0..RETAINED_KEYS {
            seed(&cache, &i.to_string(), 7).await;
        }
        assert!(cache.latest("0").is_some());
        seed(&cache, "new", 8).await;
        assert!(cache.latest("0").is_some());
        assert!(cache.latest("1").is_none());
        assert_eq!(cache.slots.lock().unwrap().len(), RETAINED_KEYS);
    }
}
