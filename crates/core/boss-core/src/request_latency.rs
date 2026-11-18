//! Backend request timing. The recorder is operational telemetry, not command state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Write,
    SmallRead,
    Bulk,
    /// Internal recorder checkpoint: never a request or latency sample.
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cancelled {
    Cancelled,
}

/// Preserve numeric HTTP statuses; cancellation has no response status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Status {
    Http(u16),
    Cancelled(Cancelled),
}
impl From<u16> for Status {
    fn from(code: u16) -> Self {
        Self::Http(code)
    }
}
impl PartialEq<u16> for Status {
    fn eq(&self, code: &u16) -> bool {
        *self == Self::Http(*code)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub service: String,
    pub class: Class,
    pub build: String,
    pub recorder_epoch: String,
    pub recording_started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub duration_ns: u64,
    pub status: Status,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub route_template: String,
    pub recorder_failures: u64,
    pub dropped_samples: u64,
    pub recording_gap_since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recording_ended_at: Option<DateTime<Utc>>,
    /// What the backend's store was doing while this request was still
    /// running past [`SLOW_REQUEST`], read once by the service's
    /// [`SlowObserver`]. Absent for a fast request, or where no observer is
    /// wired; never a claim that nothing was happening (d9b39685).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

/// The finish line d9b39685 judges: no write or small read over 2 s. A
/// request still running at this age has its moment observed.
pub const SLOW_REQUEST: Duration = Duration::from_secs(2);
/// A slow-moment observation that has not answered by now is recorded as
/// unavailable; it never holds a response or a sample longer than this.
pub const OBSERVATION_BOUND: Duration = Duration::from_secs(5);
/// Concurrent stalls share one observation taken this recently, so a burst
/// of forty slow writes reads the database a handful of times, not forty.
pub const OBSERVATION_REUSE: Duration = Duration::from_secs(5);
/// A recording batch refused before its COMMIT is retried this often, and
/// a retained sample older than [`CARRY_AGE`] is dropped and counted.
const CARRY_RETRY: Duration = Duration::from_secs(5);
const CARRY_AGE: Duration = Duration::from_secs(300);
const CARRY_LIMIT: usize = 256;

/// Reads what the service's store is doing NOW: cheap, bounded, read-only.
/// It runs off the request's path and never on the pool the request waits
/// for, so a stalled pool is observed rather than joined (d9b39685).
#[async_trait]
pub trait SlowObserver: Send + Sync {
    async fn observe(&self) -> serde_json::Value;
}

#[derive(Debug, thiserror::Error)]
#[error("request latency recorder: {0}")]
pub struct RecorderError(pub String);

#[async_trait]
pub trait Recorder: Send + Sync {
    /// A storage adapter may supply its independent writer clock. A refused
    /// producer timestamp must not become the instant of the recording failure.
    fn recording_now(&self) -> Option<DateTime<Utc>> {
        None
    }
    /// Its own operational-telemetry write, after the command transaction ended.
    async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError>;
    /// True only when the last `record` attempt is KNOWN not to have
    /// committed and a later attempt may succeed: a storage refusal before
    /// COMMIT was sent, or a timeout that dropped it there. Such a batch is
    /// retained and retried instead of dropped. A refusal of the data itself,
    /// or one that may have committed, answers false and stays a counted
    /// drop, because a retry could then double-count (d9b39685: 97-670
    /// samples an hour were dropped this way, the stalls among them).
    fn last_attempt_retryable(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct Failures {
    count: u64,
    dropped: u64,
    since: Option<DateTime<Utc>>,
}

pub struct Timing {
    sender: tokio::sync::mpsc::Sender<Sample>,
    service: String,
    classify: fn(&str, &str) -> Option<Class>,
    build: String,
    epoch: String,
    started_at: DateTime<Utc>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    failures: Arc<Mutex<Failures>>,
    queued: Arc<AtomicUsize>,
    watch: Option<Arc<SlowWatch>>,
}

/// One observation in flight or answered, shared by every request that
/// crossed [`SLOW_REQUEST`] while it was fresh.
type Observation = tokio::sync::watch::Receiver<Option<serde_json::Value>>;

struct SlowWatch {
    observer: Arc<dyn SlowObserver>,
    latest: Mutex<Option<(tokio::time::Instant, Observation)>>,
}

impl SlowWatch {
    /// The observation for a request that has just crossed the threshold,
    /// and whether an earlier slow request already started it.
    fn observe(&self) -> (Observation, bool) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, observation)) = latest.as_ref()
            && at.elapsed() < OBSERVATION_REUSE
        {
            return (observation.clone(), true);
        }
        let (sender, observation) = tokio::sync::watch::channel(None);
        let observer = self.observer.clone();
        // Owned by its own task: the request that triggered it may finish
        // or be cancelled first, and the moment must still be read.
        tokio::spawn(async move {
            let value = match tokio::time::timeout(OBSERVATION_BOUND, observer.observe()).await {
                Ok(value) => value,
                Err(_) => serde_json::json!({"state":"unavailable",
                    "reason":"slow-moment observation did not answer within its bound",
                    "bound_ms": OBSERVATION_BOUND.as_millis()}),
            };
            let _ = sender.send(Some(value));
        });
        *latest = Some((tokio::time::Instant::now(), observation.clone()));
        (observation, false)
    }
}

impl Timing {
    pub fn start(
        recorder: Arc<dyn Recorder>,
        service: String,
        build: String,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
        classify: fn(&str, &str) -> Option<Class>,
    ) -> (Arc<Self>, Worker) {
        Self::start_observed(recorder, service, build, now, classify, None)
    }

    /// As [`Timing::start`], and a request still running at
    /// [`SLOW_REQUEST`] has its moment read by `observer` (d9b39685).
    pub fn start_observed(
        recorder: Arc<dyn Recorder>,
        service: String,
        build: String,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
        classify: fn(&str, &str) -> Option<Class>,
        observer: Option<Arc<dyn SlowObserver>>,
    ) -> (Arc<Self>, Worker) {
        let (sender, receiver) = tokio::sync::mpsc::channel(256);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let failures = Arc::new(Mutex::new(Failures::default()));
        let queued = Arc::new(AtomicUsize::new(0));
        let epoch = uuid::Uuid::new_v4().to_string();
        let started_at = now();
        let checkpoint = Sample {
            service: service.clone(),
            class: Class::Recording,
            build: build.clone(),
            recorder_epoch: epoch.clone(),
            recording_started_at: started_at,
            completed_at: started_at,
            duration_ns: 0,
            status: Status::Http(0),
            method: String::new(),
            route_template: String::new(),
            recorder_failures: 0,
            dropped_samples: 0,
            recording_gap_since: None,
            recording_ended_at: None,
            context: None,
        };
        let join = tokio::spawn(record_loop(
            recorder,
            receiver,
            stopped,
            failures.clone(),
            queued.clone(),
            checkpoint,
            now.clone(),
        ));
        (
            Arc::new(Self {
                sender,
                service,
                classify,
                build,
                epoch,
                started_at,
                now,
                failures: failures.clone(),
                queued: queued.clone(),
                watch: observer.map(|observer| {
                    Arc::new(SlowWatch {
                        observer,
                        latest: Mutex::new(None),
                    })
                }),
            }),
            Worker {
                stop,
                join,
                failures,
                queued,
            },
        )
    }
}

/// The service owns this task and joins it after HTTP admission has closed.
pub struct Worker {
    stop: tokio::sync::oneshot::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
    failures: Arc<Mutex<Failures>>,
    queued: Arc<AtomicUsize>,
}

impl Worker {
    pub async fn shutdown(self) {
        self.close(true).await;
    }

    /// Drain known samples but never claim an epoch end after an incomplete HTTP join.
    pub async fn shutdown_incomplete(self) {
        self.close(false).await;
    }

    async fn close(mut self, complete: bool) {
        if !complete {
            tracing::error!(
                "HTTP drain incomplete; request telemetry epoch remains unclosed/unknown"
            );
        }
        let _ = self.stop.send(complete);
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut self.join).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "request telemetry worker did not finish normally; coverage unavailable")
            }
            Err(_) => {
                self.join.abort();
                let _ = self.join.await;
                tracing::error!(
                    queued = self.queued.load(Ordering::Relaxed),
                    "request telemetry drain unavailable; queued/in-flight samples are not a durable coverage claim"
                );
            }
        }
        let failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if failures.count != 0 || failures.dropped != 0 {
            tracing::error!(recorder_failures = failures.count, dropped_samples = failures.dropped, since = ?failures.since,
                "pending telemetry failures could not be persisted before shutdown; retained counts are incomplete");
        }
    }
}

fn failed(failures: &Mutex<Failures>, attempts: u64, dropped: u64, since: DateTime<Utc>) {
    let mut held = failures.lock().unwrap_or_else(|e| e.into_inner());
    held.count = held.count.saturating_add(attempts);
    held.dropped = held.dropped.saturating_add(dropped);
    held.since = Some(held.since.map_or(since, |old| old.min(since)));
}

async fn record_loop(
    recorder: Arc<dyn Recorder>,
    mut rx: tokio::sync::mpsc::Receiver<Sample>,
    mut stopped: tokio::sync::oneshot::Receiver<bool>,
    failures: Arc<Mutex<Failures>>,
    queued: Arc<AtomicUsize>,
    checkpoint: Sample,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
) {
    enum Next {
        Fresh(Option<Box<Sample>>),
        Carried,
    }
    let mut draining = false;
    let mut complete = false;
    // Request samples the store refused before COMMIT, oldest first. They are
    // retried on their own timer and never batched with fresh samples, so a
    // sample the store rejects every time cannot hold fresh recording behind
    // it. Until 2026-10-29 such a batch was dropped on its first refusal, and
    // the refusals were the stalls this series exists to see: 79-143 failed
    // attempts and 97-670 dropped samples an hour, 37 of 51 in one pod hour a
    // statement timeout before COMMIT (d9b39685).
    let mut carried: Vec<Sample> = Vec::new();
    let mut retry_at = tokio::time::Instant::now();
    let cadence = std::time::Duration::from_secs(60);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let next = if draining {
            match rx.recv().await {
                Some(sample) => Next::Fresh(Some(Box::new(sample))),
                // One last attempt for retained samples before the end.
                None if !carried.is_empty() => Next::Carried,
                None => Next::Fresh(None),
            }
        } else {
            tokio::select! {
                result = &mut stopped => { rx.close(); draining = true; complete = result == Ok(true); continue; }
                sample = rx.recv() => Next::Fresh(sample.map(Box::new)),
                _ = tick.tick() => { let mut c = checkpoint.clone(); c.completed_at = now(); Next::Fresh(Some(Box::new(c))) }
                () = tokio::time::sleep_until(retry_at), if !carried.is_empty() => Next::Carried,
            }
        };
        let mut ending = false;
        let mut batch = match next {
            Next::Carried => {
                let at = now();
                let (fresh, aged): (Vec<_>, Vec<_>) = std::mem::take(&mut carried)
                    .into_iter()
                    // A sample stamped ahead of this clock is not yet aged.
                    .partition(|s| match (at - s.completed_at).to_std() {
                        Ok(age) => age < CARRY_AGE,
                        Err(_) => true,
                    });
                carried = fresh;
                if let Some(oldest) = aged.iter().map(|s| s.completed_at).min() {
                    failed(&failures, 0, aged.len() as u64, oldest);
                    tracing::error!(
                        dropped = aged.len(),
                        "retained request telemetry aged out unrecorded; counted as dropped"
                    );
                }
                if carried.is_empty() {
                    continue;
                }
                let take = carried.len().min(64);
                carried.drain(..take).collect::<Vec<_>>()
            }
            Next::Fresh(Some(sample)) => {
                let mut batch = vec![*sample];
                while batch.len() < 64 {
                    match rx.try_recv() {
                        Ok(sample) => batch.push(sample),
                        Err(_) => break,
                    }
                }
                let fresh = batch.iter().filter(|s| s.class != Class::Recording).count();
                queued.fetch_sub(fresh, Ordering::Relaxed);
                batch
            }
            Next::Fresh(None) if !draining => {
                // HTTP may drop its last sender before the owner can publish
                // the successful join. Channel closure alone is not that decision.
                complete = (&mut stopped).await == Ok(true);
                rx.close();
                draining = true;
                continue;
            }
            Next::Fresh(None) if complete => {
                let mut end = checkpoint.clone();
                end.completed_at = now();
                end.recording_ended_at = Some(end.completed_at);
                ending = true;
                vec![end]
            }
            Next::Fresh(None) => break,
        };
        let requests = batch.iter().filter(|s| s.class != Class::Recording).count();
        let pending = {
            let mut held = failures.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *held)
        };
        batch[0].recorder_failures = pending.count;
        batch[0].dropped_samples = pending.dropped;
        batch[0].recording_gap_since = pending.since;
        // One bounded persistence attempt per ready batch, not per request.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), recorder.record(&batch)).await;
        if !matches!(result, Ok(Ok(()))) {
            let recording_now = recorder.recording_now().unwrap_or_else(|| now());
            let since = pending
                .since
                .unwrap_or(batch[0].completed_at.min(recording_now));
            // Retain only what the store says it did not commit; a refusal
            // that may have committed stays a counted drop, never a retry
            // that could count a request twice.
            let mut dropped = requests;
            if !draining && recorder.last_attempt_retryable() {
                let keep = batch
                    .into_iter()
                    .filter(|s| s.class != Class::Recording)
                    .map(|mut s| {
                        s.recorder_failures = 0;
                        s.dropped_samples = 0;
                        s.recording_gap_since = None;
                        s
                    })
                    .collect::<Vec<_>>();
                let room = CARRY_LIMIT.saturating_sub(carried.len());
                dropped = keep.len().saturating_sub(room);
                carried.extend(keep.into_iter().take(room));
                retry_at = tokio::time::Instant::now() + CARRY_RETRY;
            }
            failed(
                &failures,
                pending.count.saturating_add(1),
                pending.dropped.saturating_add(dropped as u64),
                since,
            );
            tracing::error!(result = ?result, retained = carried.len(),
                "request telemetry unavailable; failure count awaits successful recording; restart may lose pending counts");
        }
        if ending {
            break;
        }
    }
}

pub fn mount(app: Router, timing: Arc<Timing>) -> Router {
    app.layer(axum::middleware::from_fn_with_state(timing, measure))
}

fn measured_duration(duration: std::time::Duration) -> u128 {
    duration.as_nanos()
}

struct RequestTiming {
    timing: Arc<Timing>,
    sample: Option<Sample>,
    start: Instant,
    /// Set when the request crossed [`SLOW_REQUEST`]; the bool says an
    /// earlier slow request had already started this observation.
    observation: Option<(Observation, bool)>,
}

fn slow_context(observation: serde_json::Value, shared: bool) -> serde_json::Value {
    serde_json::json!({
        "trigger": "request still running at the 2 s line",
        "trigger_after_ms": SLOW_REQUEST.as_millis(),
        "shared": shared,
        "observation": observation,
    })
}

fn deliver(t: &Timing, sample: Sample) {
    let completed_at = sample.completed_at;
    if t.sender.try_send(sample).is_err() {
        t.queued.fetch_sub(1, Ordering::Relaxed);
        failed(&t.failures, 0, 1, completed_at);
        tracing::error!(
            "request telemetry queue full/closed; sample unavailable, response unchanged"
        );
    }
}

impl RequestTiming {
    fn finish(&mut self, status: Status) {
        let Some(mut sample) = self.sample.take() else {
            return;
        };
        let t = self.timing.clone();
        // Stop BEFORE the independent recording write: this measure is named
        // handler-to-response-headers, not end-to-end client or telemetry-commit time.
        let elapsed = measured_duration(self.start.elapsed());
        let completed_at = (t.now)();
        let Ok(duration_ns) = u64::try_from(elapsed) else {
            failed(&t.failures, 0, 1, completed_at);
            tracing::error!(
                "request duration does not fit nanoseconds; sample unavailable, response unchanged"
            );
            return;
        };
        t.queued.fetch_add(1, Ordering::Relaxed);
        sample.completed_at = completed_at;
        sample.duration_ns = duration_ns;
        sample.status = status;
        let Some((mut observation, shared)) = self.observation.take() else {
            deliver(&t, sample);
            return;
        };
        let answered = observation.borrow().clone();
        match (answered, tokio::runtime::Handle::try_current()) {
            (Some(value), _) => {
                sample.context = Some(slow_context(value, shared));
                deliver(&t, sample);
            }
            // The response has already gone; the sample waits for its
            // moment, which the watch bounds by OBSERVATION_BOUND.
            (None, Ok(runtime)) => {
                runtime.spawn(async move {
                    let value = match observation.wait_for(Option::is_some).await {
                        Ok(answer) => answer.clone().unwrap_or_default(),
                        Err(_) => serde_json::json!({"state":"unavailable",
                            "reason":"slow-moment observation ended without an answer"}),
                    };
                    sample.context = Some(slow_context(value, shared));
                    deliver(&t, sample);
                });
            }
            (None, Err(_)) => {
                sample.context = Some(slow_context(
                    serde_json::json!({"state":"unavailable",
                        "reason":"slow-moment observation pending when the runtime ended"}),
                    shared,
                ));
                deliver(&t, sample);
            }
        }
    }
}

impl Drop for RequestTiming {
    fn drop(&mut self) {
        self.finish(Status::Cancelled(Cancelled::Cancelled));
    }
}

async fn measure(State(t): State<Arc<Timing>>, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>();
    let class = route.and_then(|path| (t.classify)(req.method().as_str(), path.as_str()));
    let mut guard = class.map(|class| RequestTiming {
        timing: t.clone(),
        start: Instant::now(),
        sample: Some(Sample {
            service: t.service.clone(),
            class,
            build: t.build.clone(),
            recorder_epoch: t.epoch.clone(),
            recording_started_at: t.started_at,
            completed_at: (t.now)(),
            duration_ns: 0,
            status: Status::Cancelled(Cancelled::Cancelled),
            method: req.method().to_string(),
            route_template: route.map(|p| p.as_str()).unwrap_or_default().into(),
            recorder_failures: 0,
            dropped_samples: 0,
            recording_gap_since: None,
            recording_ended_at: None,
            context: None,
        }),
        observation: None,
    });
    let run = next.run(req);
    let response = match (guard.as_mut(), t.watch.as_ref()) {
        (Some(timing), Some(watch)) => {
            tokio::pin!(run);
            tokio::select! {
                biased;
                response = &mut run => response,
                // Read the moment WHILE the request is stalled: by the time
                // its response is decided the stall may have cleared.
                () = tokio::time::sleep(SLOW_REQUEST) => {
                    timing.observation = Some(watch.observe());
                    run.await
                }
            }
        }
        _ => run.await,
    };
    if let Some(guard) = guard.as_mut() {
        guard.finish(Status::Http(response.status().as_u16()));
    }
    response
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_real_tcp_disconnect_records_exactly_one_cancelled_elapsed_sample() {
        use axum::{extract::Path, routing::post};
        use tokio::io::AsyncWriteExt;
        let sink = Arc::new(Sink::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        let began = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        struct OnDrop(Arc<tokio::sync::Notify>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let app = mount(
            Router::new().route(
                "/fixture/{id}",
                post({
                    let began = began.clone();
                    let dropped = dropped.clone();
                    move |Path(id): Path<String>| {
                        let began = began.clone();
                        let dropped = dropped.clone();
                        async move {
                            if id == "complete" {
                                return axum::http::StatusCode::CREATED;
                            }
                            let _dropped = OnDrop(dropped);
                            began.notify_one();
                            std::future::pending::<()>().await;
                            axum::http::StatusCode::CREATED
                        }
                    }
                }),
            ),
            timing,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        assert_eq!(
            reqwest::Client::new()
                .post(format!("http://{addr}/fixture/complete"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::CREATED
        );
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"POST /fixture/disconnect HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), began.notified())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(2), dropped.notified())
            .await
            .expect("real TCP EOF must drop the handler");
        stop.send(()).unwrap();
        server.await.unwrap();
        worker.shutdown().await;
        let rows = sink.0.lock().unwrap();
        let requests = rows
            .iter()
            .filter(|s| s.class != Class::Recording)
            .collect::<Vec<_>>();
        assert_eq!(
            requests.len(),
            2,
            "completed control and disconnected request each count exactly once"
        );
        let cancelled = requests
            .iter()
            .filter(|s| serde_json::to_value(s).unwrap()["status"] == "cancelled")
            .collect::<Vec<_>>();
        assert_eq!(cancelled.len(), 1);
        assert!(cancelled[0].duration_ns >= 100_000_000);
        let value = serde_json::to_value(cancelled[0]).unwrap();
        assert_eq!(value["method"], "POST");
        assert_eq!(value["route_template"], "/fixture/{id}");
    }
    #[tokio::test]
    async fn a_closed_router_channel_waits_for_the_owned_normal_shutdown_decision() {
        use axum::{body::Body, http::Request, routing::post};
        use tower::ServiceExt;
        #[derive(Default)]
        struct Observed {
            ready: tokio::sync::Notify,
            rows: Mutex<Vec<Sample>>,
        }
        #[async_trait]
        impl Recorder for Observed {
            async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
                self.rows.lock().unwrap().extend_from_slice(samples);
                self.ready.notify_one();
                Ok(())
            }
        }
        let sink = Arc::new(Observed::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        mount(
            Router::new().route(
                "/fixture",
                post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        )
        .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
        .await
        .unwrap();
        sink.ready.notified().await;
        tokio::task::yield_now().await;
        assert!(
            !worker.join.is_finished(),
            "dropping the last router cannot preempt the owner's normal-end decision"
        );
        worker.shutdown().await;
        assert!(
            sink.rows
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .recording_ended_at
                .is_some()
        );
    }
    #[tokio::test]
    async fn incomplete_http_drain_never_claims_a_normal_epoch_end() {
        use axum::{body::Body, http::Request, routing::post};
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        mount(
            Router::new().route(
                "/fixture",
                post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        )
        .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
        .await
        .unwrap();
        worker.shutdown_incomplete().await;
        let rows = sink.0.lock().unwrap();
        assert_eq!(rows.iter().filter(|s| s.class == Class::Write).count(), 1);
        assert!(
            rows.iter().all(|s| s.recording_ended_at.is_none()),
            "an incomplete HTTP lifetime must remain unclosed/unknown"
        );
    }

    #[tokio::test]
    async fn failed_end_persistence_is_not_a_gracefully_closed_epoch() {
        use axum::{body::Body, http::Request, routing::post};
        use tower::ServiceExt;
        #[derive(Default)]
        struct CannotEnd {
            rows: Mutex<Vec<Sample>>,
            ends: AtomicUsize,
        }
        #[async_trait]
        impl Recorder for CannotEnd {
            async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
                if samples.iter().any(|s| s.recording_ended_at.is_some()) {
                    self.ends.fetch_add(1, Ordering::Relaxed);
                    return Err(RecorderError("fixture end unavailable".into()));
                }
                self.rows.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
        }
        let sink = Arc::new(CannotEnd::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        mount(
            Router::new().route(
                "/fixture",
                post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        )
        .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
        .await
        .unwrap();
        worker.shutdown().await;
        assert_eq!(
            sink.ends.load(Ordering::Relaxed),
            1,
            "the end write must actually be attempted and refused"
        );
        let rows = sink.rows.lock().unwrap();
        assert_eq!(rows.iter().filter(|s| s.class == Class::Write).count(), 1);
        assert!(
            rows.iter().all(|s| s.recording_ended_at.is_none()),
            "a failed end is unavailable, never a closure belief"
        );
    }
    #[tokio::test]
    async fn a_normal_owned_drain_records_an_explicit_epoch_end_after_its_samples() {
        use axum::{body::Body, http::Request, routing::post};
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route(
                "/fixture",
                post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        );
        app.oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
            .await
            .unwrap();
        worker.shutdown().await;
        let rows = sink.0.lock().unwrap();
        assert_eq!(
            rows.iter()
                .filter(|sample| sample.class == Class::Write)
                .count(),
            1
        );
        let ended = serde_json::to_value(rows.last().unwrap()).unwrap();
        assert!(
            ended["recording_ended_at"].is_string(),
            "normal drain needs a durable explicit end, not an open/unclean epoch forever"
        );
        assert_eq!(
            rows.last().unwrap().class,
            Class::Recording,
            "epoch end is not a request"
        );
    }
    #[test]
    fn a_threshold_plus_one_nanosecond_cannot_be_rounded_into_a_pass() {
        let threshold = measured_duration(std::time::Duration::from_secs(2));
        let over = measured_duration(std::time::Duration::new(2, 1));
        assert!(
            over > threshold,
            "2s+1ns must remain strictly above the2s limit"
        );
        assert_eq!(over, 2_000_000_001);
        assert_eq!(
            measured_duration(std::time::Duration::from_secs(61)),
            61_000_000_000
        );
    }
    #[tokio::test]
    async fn machine_refusal_and_authorized_response_are_both_timed_without_changing_auth() {
        use crate::machine_gate::{MachineGate, Mode, Reading, Slots};
        use axum::{
            body::Body,
            extract::ConnectInfo,
            http::{Request, StatusCode},
            routing::post,
        };
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        let gate = Arc::new(MachineGate::new(
            "fixture",
            &[],
            Reading::new(
                Mode::Enforce,
                Slots::new(Some("fixture-token".into()), None, None),
            ),
        ));
        let app = crate::machine_gate::gated_with_request_latency(
            Router::new().route("/fixture", post(|| async { StatusCode::CREATED })),
            gate,
            timing,
        );
        for (token, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some("fixture-token"), StatusCode::CREATED),
        ] {
            let mut request = Request::post("/fixture").body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(std::net::SocketAddr::from((
                    [10, 20, 0, 7],
                    40000,
                ))));
            if let Some(token) = token {
                request
                    .headers_mut()
                    .insert(crate::machine_token::HEADER, token.parse().unwrap());
            }
            assert_eq!(app.clone().oneshot(request).await.unwrap().status(), status);
        }
        worker.shutdown().await;
        let rows = sink.0.lock().unwrap();
        let rows = rows
            .iter()
            .filter(|s| s.class != Class::Recording)
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            2,
            "machine refusal must have its own sample too"
        );
        assert_eq!(
            rows.iter().map(|s| s.status).collect::<Vec<_>>(),
            [401, 201]
        );
    }
    use super::*;

    fn fixture_classifier(method: &str, path: &str) -> Option<Class> {
        if method == "POST" && path.starts_with("/fixture") {
            Some(Class::Write)
        } else {
            None
        }
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<Sample>>);
    #[async_trait]
    impl Recorder for Sink {
        async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
            self.0.lock().unwrap().extend_from_slice(samples);
            Ok(())
        }
    }

    #[tokio::test]
    async fn refused_response_is_unchanged_and_has_its_own_timing_sample() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
            routing::post,
        };
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let at = DateTime::parse_from_rfc3339("2026-10-27T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture-build".into(),
            Arc::new(move || at),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route(
                "/fixture/{id}/record",
                post(|| async { (StatusCode::CONFLICT, "unchanged refusal") }),
            ),
            timing,
        );
        let response = app
            .oneshot(
                Request::post("/fixture/item/record")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "unchanged refusal"
        );
        let before_shutdown = tokio::time::Instant::now();
        worker.shutdown().await;
        assert!(before_shutdown.elapsed() <= std::time::Duration::from_secs(2));
        let rows = sink.0.lock().unwrap();
        let rows = rows
            .iter()
            .filter(|s| s.class != Class::Recording)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].class, Class::Write);
        assert_eq!(rows[0].status, 409);
    }

    struct Hung;
    #[async_trait]
    impl Recorder for Hung {
        async fn record(&self, _: &[Sample]) -> Result<(), RecorderError> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_recorder_cannot_hold_the_response_headers() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
            routing::post,
        };
        use tower::ServiceExt;
        let at = DateTime::parse_from_rfc3339("2026-10-27T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (timing, worker) = Timing::start(
            Arc::new(Hung),
            "fixture".into(),
            "fixture-build".into(),
            Arc::new(move || at),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route("/fixture", post(|| async { StatusCode::CREATED })),
            timing,
        );
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            app.oneshot(Request::post("/fixture").body(Body::empty()).unwrap()),
        )
        .await;
        assert!(
            response.is_ok(),
            "operational telemetry must not delay the command response"
        );
        assert_eq!(response.unwrap().unwrap().status(), StatusCode::CREATED);
        worker.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_refuses_samples_not_responses_and_a_healthy_burst_is_conserved() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
            routing::post,
        };
        use tower::ServiceExt;
        let at = DateTime::parse_from_rfc3339("2026-10-27T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (timing, worker) = Timing::start(
            Arc::new(Hung),
            "fixture".into(),
            "fixture".into(),
            Arc::new(move || at),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route("/fixture", post(|| async { StatusCode::CREATED })),
            timing.clone(),
        );
        for _ in 0..300 {
            let response = app
                .clone()
                .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }
        assert!(
            timing.failures.lock().unwrap().dropped > 0,
            "bounded overflow is a recorded coverage gap"
        );
        assert!(timing.queued.load(Ordering::Relaxed) <= 256);
        let before_drain = tokio::time::Instant::now();
        worker.shutdown().await;
        assert!(
            before_drain.elapsed() <= std::time::Duration::from_secs(2),
            "a saturated hung recorder has a bounded shutdown"
        );

        let sink = Arc::new(Sink::default());
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(move || at),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route("/fixture", post(|| async { StatusCode::CREATED })),
            timing,
        );
        for _ in 0..128 {
            app.clone()
                .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
                .await
                .unwrap();
        }
        worker.shutdown().await;
        assert_eq!(
            sink.0
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.class != Class::Recording)
                .count(),
            128,
            "batching conserves every accepted timing"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_recovery_flushes_failure_debt_without_inventing_a_request() {
        use axum::{body::Body, http::Request, routing::post};
        use tower::ServiceExt;
        struct Recovers {
            calls: AtomicUsize,
            kept: Mutex<Vec<Sample>>,
            recovered: tokio::sync::Notify,
        }
        #[async_trait]
        impl Recorder for Recovers {
            async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
                if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(RecorderError("fixture unavailable".into()));
                }
                self.kept.lock().unwrap().extend_from_slice(samples);
                self.recovered.notify_one();
                Ok(())
            }
        }
        let sink = Arc::new(Recovers {
            calls: AtomicUsize::new(0),
            kept: Mutex::new(Vec::new()),
            recovered: tokio::sync::Notify::new(),
        });
        let at = DateTime::parse_from_rfc3339("2026-10-27T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (timing, worker) = Timing::start(
            sink.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(move || at),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route(
                "/fixture",
                post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        );
        // Keep the server/router alive while it is idle. Consuming the only
        // router would close its sender and correctly end the recording loop.
        app.clone()
            .oneshot(Request::post("/fixture").body(Body::empty()).unwrap())
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(sink.calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), sink.recovered.notified())
            .await
            .expect("idle checkpoint actually reaches the recovered recorder");
        worker.shutdown().await;
        let rows = sink.kept.lock().unwrap();
        let rows = rows
            .iter()
            .filter(|s| s.recording_ended_at.is_none())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].class, Class::Recording);
        assert_eq!(rows[0].recorder_failures, 1);
        assert_eq!(rows[0].dropped_samples, 1);
        assert_eq!(rows[0].recording_gap_since, Some(at));
    }

    // d9b39685, 2026-10-29: every hour failed the 2 s line with write maxima
    // of 15.8-77.1 s, and nothing recorded what the database was doing at
    // the time. These pin that a request still running at 2 s carries the
    // moment it stalled in, read once, bounded, off the request's path.
    struct Moment {
        calls: AtomicUsize,
        handler_done: Arc<std::sync::atomic::AtomicBool>,
        seen_running: std::sync::atomic::AtomicBool,
        hang: bool,
    }
    impl Moment {
        fn new(handler_done: Arc<std::sync::atomic::AtomicBool>, hang: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                handler_done,
                seen_running: std::sync::atomic::AtomicBool::new(false),
                hang,
            })
        }
    }
    #[async_trait]
    impl SlowObserver for Moment {
        async fn observe(&self) -> serde_json::Value {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.seen_running.store(
                !self.handler_done.load(Ordering::Acquire),
                Ordering::Relaxed,
            );
            if self.hang {
                std::future::pending::<()>().await;
            }
            serde_json::json!({"state": "observed", "marker": "fixture-moment"})
        }
    }

    fn observed_app(
        recorder: Arc<dyn Recorder>,
        observer: Arc<dyn SlowObserver>,
        handler: Duration,
        done: Arc<std::sync::atomic::AtomicBool>,
    ) -> (Router, Worker) {
        use axum::routing::post;
        let (timing, worker) = Timing::start_observed(
            recorder,
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
            Some(observer),
        );
        let app = mount(
            Router::new().route(
                "/fixture/{id}",
                post(move || {
                    let done = done.clone();
                    async move {
                        tokio::time::sleep(handler).await;
                        done.store(true, Ordering::Release);
                        axum::http::StatusCode::CREATED
                    }
                }),
            ),
            timing,
        );
        (app, worker)
    }

    fn post_fixture(id: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::post(format!("/fixture/{id}"))
            .body(axum::body::Body::empty())
            .unwrap()
    }

    fn requests(rows: &[Sample]) -> Vec<Sample> {
        rows.iter()
            .filter(|s| s.class != Class::Recording)
            .cloned()
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_still_running_at_two_seconds_carries_the_moment_it_stalled_in() {
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let moment = Moment::new(done.clone(), false);
        let (app, worker) =
            observed_app(sink.clone(), moment.clone(), Duration::from_secs(3), done);
        let response = app.oneshot(post_fixture("slow")).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CREATED);
        worker.shutdown().await;
        let rows = requests(&sink.0.lock().unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!(moment.calls.load(Ordering::Relaxed), 1);
        assert!(
            moment.seen_running.load(Ordering::Relaxed),
            "the moment is read while the request is still stalled, not after it"
        );
        let context = rows[0]
            .context
            .as_ref()
            .expect("a request past the 2 s line carries its moment");
        assert_eq!(context["observation"]["marker"], "fixture-moment");
        assert_eq!(context["shared"], false);
        assert_eq!(
            serde_json::to_value(&rows[0]).unwrap()["context"]["observation"]["state"],
            "observed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_under_two_seconds_never_reads_the_database() {
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let moment = Moment::new(done.clone(), false);
        let (app, worker) = observed_app(
            sink.clone(),
            moment.clone(),
            Duration::from_millis(1_900),
            done,
        );
        app.oneshot(post_fixture("fast")).await.unwrap();
        worker.shutdown().await;
        let rows = requests(&sink.0.lock().unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!(moment.calls.load(Ordering::Relaxed), 0);
        assert!(rows[0].context.is_none());
        assert!(
            serde_json::to_value(&rows[0])
                .unwrap()
                .get("context")
                .is_none(),
            "a fast sample's stored shape is unchanged"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_stalls_share_one_observation_of_their_moment() {
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let moment = Moment::new(done.clone(), false);
        let (app, worker) =
            observed_app(sink.clone(), moment.clone(), Duration::from_secs(3), done);
        let handles = (0..8)
            .map(|n| {
                let app = app.clone();
                tokio::spawn(async move { app.oneshot(post_fixture(&n.to_string())).await })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        drop(app);
        worker.shutdown().await;
        let rows = requests(&sink.0.lock().unwrap());
        assert_eq!(rows.len(), 8);
        assert_eq!(
            moment.calls.load(Ordering::Relaxed),
            1,
            "a burst of stalls reads the database once, not once per request"
        );
        assert!(rows.iter().all(|s| s.context.is_some()));
        assert_eq!(
            rows.iter()
                .filter(|s| s.context.as_ref().unwrap()["shared"] == false)
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_observer_holds_neither_the_response_nor_loses_the_sample() {
        use tower::ServiceExt;
        let sink = Arc::new(Sink::default());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let moment = Moment::new(done.clone(), true);
        let (app, worker) =
            observed_app(sink.clone(), moment.clone(), Duration::from_secs(3), done);
        let started = tokio::time::Instant::now();
        let response = app.oneshot(post_fixture("hung")).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CREATED);
        assert!(
            started.elapsed() < Duration::from_millis(3_100),
            "a hung observer never delays the response"
        );
        tokio::time::sleep(OBSERVATION_BOUND).await;
        worker.shutdown().await;
        let rows = requests(&sink.0.lock().unwrap());
        assert_eq!(
            rows.len(),
            1,
            "the sample lands once its observation is bounded"
        );
        assert_eq!(moment.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            rows[0].context.as_ref().unwrap()["observation"]["state"],
            "unavailable"
        );
    }

    /// Refuses before COMMIT for the first `refusals` attempts, or for every
    /// batch carrying the poison route, and says so.
    struct Uncommitted {
        refusals: AtomicUsize,
        poison: Option<&'static str>,
        calls: AtomicUsize,
        kept: Mutex<Vec<Sample>>,
        last_retryable: std::sync::atomic::AtomicBool,
    }
    impl Uncommitted {
        fn new(refusals: usize, poison: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                refusals: AtomicUsize::new(refusals),
                poison,
                calls: AtomicUsize::new(0),
                kept: Mutex::new(Vec::new()),
                last_retryable: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }
    #[async_trait]
    impl Recorder for Uncommitted {
        async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let poisoned = self.poison.is_some_and(|poison| {
                samples
                    .iter()
                    .any(|s| s.route_template == poison && s.status == 201)
            });
            let refuse = poisoned
                || self
                    .refusals
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                    .is_ok();
            self.last_retryable.store(refuse, Ordering::Relaxed);
            if refuse {
                return Err(RecorderError(
                    "fixture statement timeout before COMMIT".into(),
                ));
            }
            self.kept.lock().unwrap().extend_from_slice(samples);
            Ok(())
        }
        fn last_attempt_retryable(&self) -> bool {
            self.last_retryable.load(Ordering::Relaxed)
        }
    }

    fn counted(rows: &[Sample]) -> (u64, u64) {
        rows.iter().fold((0, 0), |(f, d), s| {
            (f + s.recorder_failures, d + s.dropped_samples)
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_refused_before_its_commit_is_retried_not_dropped() {
        use tower::ServiceExt;
        let recorder = Uncommitted::new(1, None);
        let (timing, worker) = Timing::start(
            recorder.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        let app = mount(
            Router::new().route(
                "/fixture/{id}",
                axum::routing::post(|| async { axum::http::StatusCode::CREATED }),
            ),
            timing,
        );
        for id in ["stalled", "second", "third"] {
            app.clone().oneshot(post_fixture(id)).await.unwrap();
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(CARRY_RETRY * 2).await;
        drop(app);
        worker.shutdown().await;
        let kept = recorder.kept.lock().unwrap();
        assert_eq!(
            requests(&kept).len(),
            3,
            "the sample refused during the stall is recorded once the store answers"
        );
        assert_eq!(
            counted(&kept),
            (1, 0),
            "the refused attempt is a counted failure; no sample is dropped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_poisoned_retained_batch_cannot_block_fresh_recording_and_ages_out_counted() {
        use tower::ServiceExt;
        let recorder = Uncommitted::new(0, Some("/fixture/{id}"));
        let (timing, worker) = Timing::start(
            recorder.clone(),
            "fixture".into(),
            "fixture".into(),
            Arc::new(Utc::now),
            fixture_classifier,
        );
        let app = mount(
            Router::new()
                .route(
                    "/fixture/{id}",
                    axum::routing::post(|| async { axum::http::StatusCode::CREATED }),
                )
                .route(
                    "/fixture/fresh/{id}",
                    axum::routing::post(|| async { axum::http::StatusCode::ACCEPTED }),
                ),
            timing,
        );
        app.clone().oneshot(post_fixture("poison")).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(CARRY_RETRY).await;
        for id in ["a", "b", "c"] {
            app.clone()
                .oneshot(post_fixture(&format!("fresh/{id}")))
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(CARRY_AGE + CARRY_RETRY * 2).await;
        drop(app);
        worker.shutdown().await;
        let kept = recorder.kept.lock().unwrap();
        let fresh = requests(&kept);
        assert_eq!(
            fresh.len(),
            3,
            "fresh samples never wait behind a retained batch"
        );
        assert!(fresh.iter().all(|s| s.status == 202));
        assert_eq!(
            counted(&kept).1,
            1,
            "the poison sample ages out as one counted drop, never silently"
        );
    }
}
