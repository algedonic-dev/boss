//! Concurrency belongs to the actual work, including late blocking replay.
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

pub(super) type FlowLease = Arc<OwnedSemaphorePermit>;

pub(super) struct FlowGate {
    permits: Arc<Semaphore>,
    progress: Arc<Mutex<HashMap<String, (Instant, bool)>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl FlowGate {
    pub(super) fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(1)),
            progress: Arc::new(Mutex::new(HashMap::new())),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn progress(&self, key: &str) -> Option<String> {
        let progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        progress.get(key).map(|(since, running)| {
            let others = 1_usize.saturating_sub(self.permits.available_permits());
            if *running {
                format!("flow refresh running for {}s", since.elapsed().as_secs())
            } else {
                format!(
                    "flow refresh queued for {}s behind {others} running refresh",
                    since.elapsed().as_secs()
                )
            }
        })
    }

    pub(super) async fn run<T, F, Fut>(
        &self,
        key: &str,
        deadline: Duration,
        read: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(FlowLease, Arc<AtomicBool>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        {
            let mut progress = self
                .progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if progress.contains_key(key) {
                return Err("previous owned flow refresh for this key is still running or queued; no duplicate started".into());
            }
            progress.insert(key.to_owned(), (Instant::now(), false));
        }
        let progress = Progress {
            progress: self.progress.clone(),
            key: key.to_owned(),
        };
        let permit = tokio::time::timeout(deadline, self.permits.clone().acquire_owned()).await
            .map_err(|_| format!("flow refresh queued deadline exceeded after {}s; another refresh still owns concurrency", deadline.as_secs()))?
            .map_err(|e| e.to_string())?;
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_owned(), (Instant::now(), true));
        let lease = Arc::new(permit);
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = Cancel(cancelled.clone());
        let (send, receive) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _progress = progress;
            // A response deadline never abandons native SQL or releases its
            // capacity merely because the caller stopped awaiting it.
            let _owner = lease.clone();
            let result = read(lease, cancelled).await;
            let _ = send.send(result);
        });
        {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
        }
        match tokio::time::timeout(deadline, receive).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(format!(
                "flow refresh task ended without a reading: {error}"
            )),
            Err(_) => Err(format!(
                "flow refresh running deadline exceeded after {}s; {}",
                deadline.as_secs(),
                "late owned work retains its concurrency permit until it exits; cancellation requested, no late reading will publish"
            )),
        }
    }
}

struct Progress {
    progress: Arc<Mutex<HashMap<String, (Instant, bool)>>>,
    key: String,
}
impl Drop for Progress {
    fn drop(&mut self) {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
