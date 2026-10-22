//! A reading shared for its lifetime — one read per key at a time, its
//! answer served to every caller inside that lifetime (backlog b852ec37).
//!
//! WHY IT EXISTS. On 2026-10-21 the IT map's two reads, `/api/yard/regions`
//! and `/api/yard/borders`, each took the packet-membership snapshot —
//! a repeatable-read transaction that scans the window's audit history
//! (`membership_history_pg`) — on every 10 s tick of every open map.
//! Measured from the pod at 15:49Z on a quiet API: the 48 h snapshot the
//! map takes answered in 8.1-8.4 s and the 24 h retry behind it in 3.5 s,
//! about twelve of the map read's ~23 s, and both REFUSED on the byte
//! bound, so the map paid that on every read to say "unread". Under the
//! afternoon's load the same reads took 127-187 s, past Cloudflare's 100 s
//! origin limit, and every cancelled poll left its scan running for the
//! next one to queue behind.
//!
//! A flow reading is a count over a window a day wide; one taken a
//! minute ago is the same reading for every map open now. So it is taken
//! once per key and LIFETIME, concurrent callers wait on the read in
//! flight rather than start their own, and the instant it was taken rides
//! with it (the caller states it — `flow_as_of` on the map), so a served
//! reading is never presented as one taken at the serving read's now.
//!
//! Not a store of record: an in-process memo of a read, lost on restart,
//! never written anywhere. A read dropped mid-flight (its request was
//! cancelled) leaves no reading, and the next caller takes it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the IT map's flow reading is served before the next map read
/// takes a new one. Two minutes: twelve map ticks, against a window a day
/// wide — the served counts it carries cannot move by more than the
/// crossings of those two minutes, and the reading names its instant.
pub const MAP_FLOW_LIFETIME: Duration = Duration::from_secs(120);

/// One slot per key: the newest reading and when it was taken. The async
/// mutex is the single flight — a caller holds it while it reads.
type Slot<T> = Arc<tokio::sync::Mutex<Option<(Instant, Arc<T>)>>>;

/// A reading per key, shared for [`ReadingCache::lifetime`].
pub struct ReadingCache<T> {
    lifetime: Duration,
    slots: Mutex<HashMap<String, Slot<T>>>,
}

impl<T> std::fmt::Debug for ReadingCache<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadingCache")
            .field("lifetime", &self.lifetime)
            .finish_non_exhaustive()
    }
}

impl<T> ReadingCache<T> {
    /// A cache whose readings are served for `lifetime`. A zero lifetime
    /// serves nothing twice: every call reads, one at a time per key.
    pub fn new(lifetime: Duration) -> Self {
        Self {
            lifetime,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// How long a reading is served.
    pub fn lifetime(&self) -> Duration {
        self.lifetime
    }

    /// The reading for `key`: the one taken inside its lifetime if there
    /// is one, else `read`'s — and while a read for `key` is in flight,
    /// every other caller for `key` waits on it rather than reading too.
    pub async fn get_or_read<F, Fut>(&self, key: &str, read: F) -> Arc<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let slot = self.slot(key);
        let mut held = slot.lock().await;
        if let Some((at, reading)) = held.as_ref()
            && at.elapsed() < self.lifetime
        {
            return reading.clone();
        }
        let reading = Arc::new(read().await);
        *held = Some((Instant::now(), reading.clone()));
        reading
    }

    /// The slot for `key`, made on first ask. Readings past their
    /// lifetime are dropped here, so the map holds at most the keys read
    /// within one lifetime (a key is a caller, a scope and a window).
    fn slot(&self, key: &str) -> Slot<T> {
        // A poisoned lock held only this map; its contents are still a
        // map of slots, each guarded by its own lock.
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lifetime = self.lifetime;
        slots.retain(|k, slot| {
            k == key
                || match slot.try_lock() {
                    Ok(held) => held.as_ref().is_some_and(|(at, _)| at.elapsed() < lifetime),
                    // In flight: someone is reading it now.
                    Err(_) => true,
                }
        });
        slots.entry(key.to_string()).or_default().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_second_ask_inside_the_lifetime_is_served_the_first_reading() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        let reads = AtomicUsize::new(0);
        let read = || async {
            reads.fetch_add(1, Ordering::SeqCst);
            7
        };
        assert_eq!(*cache.get_or_read("k", read).await, 7);
        assert_eq!(*cache.get_or_read("k", read).await, 7);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn another_key_is_another_reading() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        let reads = AtomicUsize::new(0);
        let read = || async {
            reads.fetch_add(1, Ordering::SeqCst);
        };
        cache.get_or_read("caller-a", read).await;
        cache.get_or_read("caller-b", read).await;
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_asks_wait_on_the_read_in_flight() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        let reads = AtomicUsize::new(0);
        let read = || async {
            reads.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        futures::future::join_all((0..8).map(|_| cache.get_or_read("k", read))).await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_zero_lifetime_reads_every_time() {
        let cache = ReadingCache::new(Duration::ZERO);
        let reads = AtomicUsize::new(0);
        let read = || async {
            reads.fetch_add(1, Ordering::SeqCst);
        };
        cache.get_or_read("k", read).await;
        cache.get_or_read("k", read).await;
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    /// A read whose caller went away leaves no reading behind: the next
    /// caller reads, rather than being served nothing.
    #[tokio::test]
    async fn a_dropped_read_leaves_no_reading() {
        let cache = ReadingCache::new(Duration::from_secs(60));
        let slow = cache.get_or_read("k", || async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            1
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(10), slow)
                .await
                .is_err()
        );
        assert_eq!(*cache.get_or_read("k", || async { 2 }).await, 2);
    }
}
