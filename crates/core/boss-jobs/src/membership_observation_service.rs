//! Own one native diagnostic task through HTTP cancellation and process shutdown.
use crate::membership_observation::Observation;
use futures::future::{BoxFuture, FutureExt, Shared};
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, oneshot};

type Completion = Shared<BoxFuture<'static, Result<(), String>>>;

struct Flight {
    id: u64,
    cancelled: Arc<AtomicBool>,
    completion: Completion,
}

#[derive(Default)]
struct State {
    closed: bool,
    sequence: u64,
    active: Option<Flight>,
}

/// Capacity belongs to the actual task, never to the HTTP response future.
#[derive(Default)]
pub struct MembershipObservationOwner {
    state: Mutex<State>,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl MembershipObservationOwner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A dropped reader requests cancellation but retains the task and its slot.
    pub async fn run<F, Fut>(&self, operation: F) -> Result<Observation, String>
    where
        F: FnOnce(Arc<AtomicBool>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Observation, String>> + Send + 'static,
    {
        let (id, completion, receiver, cancelled) = {
            let mut state = self.state.lock().await;
            if state.closed {
                return Err("membership observation owner is closed".into());
            }
            let finished = state
                .active
                .as_ref()
                .is_some_and(|flight| flight.completion.clone().now_or_never().is_some());
            if finished {
                state.active = None;
            }
            if state.active.is_some() {
                return Err("membership observation is busy with an owned query".into());
            }
            let id = state
                .sequence
                .checked_add(1)
                .ok_or_else(|| "membership observation sequence exhausted".to_owned())?;
            state.sequence = id;
            let cancelled = Arc::new(AtomicBool::new(false));
            let task_cancelled = cancelled.clone();
            let (sender, receiver) = oneshot::channel();
            let task = tokio::spawn(async move {
                let result = operation(task_cancelled).await;
                let _ = sender.send(result);
            });
            let completion = async move {
                task.await
                    .map_err(|error| format!("membership observation task failed: {error}"))
            }
            .boxed()
            .shared();
            state.active = Some(Flight {
                id,
                cancelled: cancelled.clone(),
                completion: completion.clone(),
            });
            (id, completion, receiver, cancelled)
        };
        let _cancel_on_drop = CancelOnDrop(cancelled);
        let result = receiver.await;
        let joined = completion.await;
        {
            let mut state = self.state.lock().await;
            if state.active.as_ref().is_some_and(|flight| flight.id == id) {
                state.active = None;
            }
        }
        joined?;
        result.map_err(|error| format!("membership observation result unavailable: {error}"))?
    }

    /// Stop admission, request cooperative cancellation, and join actual work.
    pub async fn shutdown(&self) {
        let completion = {
            let mut state = self.state.lock().await;
            state.closed = true;
            state.active.as_ref().map(|flight| {
                flight.cancelled.store(true, Ordering::Release);
                flight.completion.clone()
            })
        };
        if let Some(completion) = completion
            && let Err(error) = completion.await
        {
            tracing::warn!(%error, "owned membership observation failed during shutdown");
        }
        self.state.lock().await.active = None;
    }
}
