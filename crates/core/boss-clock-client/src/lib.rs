//! `boss-clock-client` — the read interface to the Clock service.
//!
//! Every BOSS service holds an `Arc<dyn ClockClient>` on its
//! ApiState. Every handler that needs "what time is it" calls
//! `state.clock.now().await?` and uses the returned `ClockNow`.
//! Services include `simulated` from the response in every
//! event payload they emit; the audit log carries the SIM
//! marker forever.
//!
//! Implementations:
//!
//! - [`ReqwestClockClient`] — HTTP client against
//!   `boss-clock-api`. Caches the last response for a short TTL
//!   (default 100ms) so per-handler latency stays sub-millisecond
//!   on warm cache. Production wires this pointing at the deployed
//!   wall-mode `boss-clock-api`. Demo wires it pointing at the
//!   deployed sim-mode `boss-clock-api`.
//! - [`WallClockClient`] — always returns `Utc::now()`. Used by
//!   tests that don't care about sim-time + as the safe default
//!   when no clock-api URL is configured.
//! - [`FixedClockClient`] — returns a frozen instant. Used by
//!   deterministic tests that assert exact dates.
//! - [`InMemoryClockClient`] — mutable in-memory state, for
//!   tests that need to advance time programmatically.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::{Stream, StreamExt};

pub use boss_clock::types::ClockNow;

#[derive(Debug, thiserror::Error)]
pub enum ClockClientError {
    #[error("clock-api transport: {0}")]
    Transport(String),
    #[error("clock-api returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("clock-api response decode: {0}")]
    Decode(String),
}

#[async_trait]
pub trait ClockClient: Send + Sync {
    /// Resolve the canonical "what time is it" for the current
    /// request. Includes the SIM marker so callers can stamp it
    /// onto event payloads. Implementations decide whether to
    /// network-call, read a local cache, or return a frozen
    /// instant — the handler doesn't care.
    ///
    /// Infallible by design: handlers should not have to plumb
    /// clock errors through their happy path. Implementations
    /// that can fail (the reqwest impl) fall back to wallclock
    /// and emit a warn log; that's the right behavior in
    /// production (wallclock matches reality) but produces
    /// wrong-dated data in sim mode — sim deployments should
    /// monitor clock-api health out of band.
    async fn now(&self) -> ClockNow;

    /// Fallible variant for callers that want to surface a
    /// clock-api outage explicitly (e.g. a /health endpoint).
    /// Default just wraps `now()` in `Ok`.
    async fn try_now(&self) -> Result<ClockNow, ClockClientError> {
        Ok(self.now().await)
    }
}

/// Resolve the effective wall-or-sim `now` as a bare
/// `DateTime<Utc>`, dropping the SIM marker.
///
/// Almost every write-side handler in the workspace needs only
/// the timestamp to stamp onto a record or event — not the full
/// [`ClockNow`] — so this is the one owned definition of that
/// `req_now(state) -> DateTime<Utc>` shape. Handlers that need the
/// SIM marker (for event payloads) call `clock.now().await.simulated`
/// directly.
///
/// Takes `&Arc<dyn ClockClient>` — the exact shape every service
/// holds on its ApiState — so `&state.clock` passes straight in:
///
/// ```ignore
/// let now = boss_clock_client::now_from(&state.clock).await;
/// ```
pub async fn now_from(clock: &Arc<dyn ClockClient>) -> chrono::DateTime<Utc> {
    clock.now().await.now
}

/// Subscribe to the clock's streaming tick feed (`GET /api/clock/ticks`,
/// Server-Sent Events) and yield each [`ClockNow`] as it arrives — the
/// low-latency, always-connected clock feed the dispatcher's timing
/// triggers and the sim daemon drive their loops off, instead of polling
/// [`ClockClient::now`] per tick.
///
/// Reconnects on disconnect: the SSE is ephemeral with no replay, so a
/// gap just resumes from the live time (consumers tolerate missed
/// sub-day ticks via their own cursor). The stream never ends; drop it
/// to stop.
///
/// Deliberately a free fn, off the `ClockClient` trait: only the two
/// daemons that run time-based loops need streaming; the ~dozen
/// request/response services keep using `now()`, and the trait stays
/// object-safe.
pub fn subscribe_ticks(base_url: impl Into<String>) -> impl Stream<Item = ClockNow> + Send {
    let url = format!("{}/api/clock/ticks", base_url.into().trim_end_matches('/'));
    async_stream::stream! {
        let client = reqwest::Client::new();
        loop {
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let mut bytes = resp.bytes_stream();
                    let mut buf: Vec<u8> = Vec::new();
                    while let Some(chunk) = bytes.next().await {
                        let Ok(chunk) = chunk else { break };
                        buf.extend_from_slice(&chunk);
                        // SSE events are separated by a blank line ("\n\n").
                        while let Some(pos) = find_event_boundary(&buf) {
                            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
                            if let Ok(s) = std::str::from_utf8(&frame)
                                && let Some(now) = parse_tick_event(s)
                            {
                                yield now;
                            }
                        }
                    }
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "clock /ticks returned non-success; retrying");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "clock /ticks unreachable; retrying");
                }
            }
            // Disconnected / failed — back off, then reconnect.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Index of the first byte of the first SSE event boundary (`\n\n`) in
/// `buf`, or `None` if no complete event is buffered yet. Caller drains
/// `pos + 2` to consume the event including its terminating blank line.
fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

/// Parse one SSE event's `data:` payload into a [`ClockNow`].
/// Concatenates multiple `data:` lines (SSE permits them), ignores
/// comment/keep-alive lines (`:`), and returns `None` for an empty or
/// unparseable event.
fn parse_tick_event(event: &str) -> Option<ClockNow> {
    let mut data = String::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(&data).ok()
}

/// Adapter that lets any `ClockClient` act as a
/// `boss_core::publisher::SimulatedProbe` for DomainPublisher.
/// The publisher needs only the `simulated: bool` bit, not the
/// full ClockNow struct; this thin wrapper keeps publisher.rs
/// free of a boss-clock-client dep.
///
/// Wire at service binary startup:
///
/// ```ignore
/// let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(url));
/// let publisher = DomainPublisher::new(bus, "people")
///     .with_audit(audit)
///     .with_sim_probe(Arc::new(ClockSimProbe::new(clock.clone())));
/// ```
pub struct ClockSimProbe {
    clock: std::sync::Arc<dyn ClockClient>,
}

impl ClockSimProbe {
    pub fn new(clock: std::sync::Arc<dyn ClockClient>) -> Self {
        Self { clock }
    }
}

#[async_trait]
impl boss_core::publisher::SimulatedProbe for ClockSimProbe {
    async fn simulated(&self) -> bool {
        self.clock.now().await.simulated
    }
}

/// HTTP client against `boss-clock-api` with a short TTL cache.
/// The cache means a service running at 10k handler-calls/sec
/// only hits the clock-api ~10/sec (default 100ms TTL), so the
/// network hop is amortized.
pub struct ReqwestClockClient {
    url: String,
    client: reqwest::Client,
    cache_ttl: Duration,
    cache: RwLock<Option<(Instant, ClockNow)>>,
}

impl ReqwestClockClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("default reqwest client always builds"),
            cache_ttl: Duration::from_millis(100),
            cache: RwLock::new(None),
        }
    }

    /// Override the cache TTL. Default 100ms. Set to
    /// `Duration::ZERO` to disable caching (every `now()` hits
    /// the wire) — useful for tests that need to see sim
    /// advances immediately.
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    fn cached(&self) -> Option<ClockNow> {
        let guard = self.cache.read().ok()?;
        let (at, snapshot) = guard.as_ref()?;
        if at.elapsed() < self.cache_ttl {
            Some(*snapshot)
        } else {
            None
        }
    }

    fn store(&self, now: ClockNow) {
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some((Instant::now(), now));
        }
    }
}

impl ReqwestClockClient {
    async fn fetch(&self) -> Result<ClockNow, ClockClientError> {
        let url = format!("{}/api/clock/now", self.url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ClockClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClockClientError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let snapshot: ClockNow = resp
            .json()
            .await
            .map_err(|e| ClockClientError::Decode(e.to_string()))?;
        Ok(snapshot)
    }
}

#[async_trait]
impl ClockClient for ReqwestClockClient {
    async fn now(&self) -> ClockNow {
        if let Some(cached) = self.cached() {
            return cached;
        }
        match self.fetch().await {
            Ok(snapshot) => {
                self.store(snapshot);
                snapshot
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    url = %self.url,
                    "clock-api unreachable; falling back to wallclock — \
                     dates may be wrong in sim mode"
                );
                let fallback = ClockNow {
                    now: Utc::now(),
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                };
                self.store(fallback);
                fallback
            }
        }
    }

    async fn try_now(&self) -> Result<ClockNow, ClockClientError> {
        if let Some(cached) = self.cached() {
            return Ok(cached);
        }
        let snapshot = self.fetch().await?;
        self.store(snapshot);
        Ok(snapshot)
    }
}

/// Always-wall-clock client. Used as the safe production default
/// when no clock-api URL is configured + by tests that don't
/// care about sim-vs-real distinction. `simulated` is always
/// `false`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClockClient;

#[async_trait]
impl ClockClient for WallClockClient {
    async fn now(&self) -> ClockNow {
        ClockNow {
            now: Utc::now(),
            simulated: false,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
        }
    }
}

/// Frozen-instant client. Tests instantiate this so asserted
/// dates don't drift with the wall clock at run time.
#[derive(Debug, Clone, Copy)]
pub struct FixedClockClient {
    snapshot: ClockNow,
}

impl FixedClockClient {
    pub fn new(snapshot: ClockNow) -> Self {
        Self { snapshot }
    }
}

#[async_trait]
impl ClockClient for FixedClockClient {
    async fn now(&self) -> ClockNow {
        self.snapshot
    }
}

/// Mutable in-memory client. Tests use this to advance the clock
/// programmatically without standing up an HTTP server.
#[derive(Debug, Clone)]
pub struct InMemoryClockClient {
    current: Arc<RwLock<ClockNow>>,
}

impl InMemoryClockClient {
    pub fn new(initial: ClockNow) -> Self {
        Self {
            current: Arc::new(RwLock::new(initial)),
        }
    }

    pub fn set(&self, snapshot: ClockNow) {
        if let Ok(mut guard) = self.current.write() {
            *guard = snapshot;
        }
    }
}

#[async_trait]
impl ClockClient for InMemoryClockClient {
    async fn now(&self) -> ClockNow {
        match self.current.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wall_client_is_not_simulated() {
        let c = WallClockClient;
        let ctx = c.now().await;
        assert!(!ctx.simulated);
    }

    #[tokio::test]
    async fn fixed_client_returns_frozen_instant() {
        let target = chrono::DateTime::parse_from_rfc3339("2025-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot = ClockNow {
            now: target,
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
        };
        let c = FixedClockClient::new(snapshot);
        tokio::time::sleep(Duration::from_millis(10)).await;
        let ctx = c.now().await;
        assert_eq!(ctx.now, target);
        assert!(ctx.simulated);
    }

    #[tokio::test]
    async fn in_memory_client_reflects_set() {
        let early = chrono::DateTime::parse_from_rfc3339("2025-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let later = chrono::DateTime::parse_from_rfc3339("2025-05-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = InMemoryClockClient::new(ClockNow {
            now: early,
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
        });
        assert_eq!(c.now().await.now, early);
        c.set(ClockNow {
            now: later,
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
        });
        assert_eq!(c.now().await.now, later);
    }

    // --- streaming tick subscriber (SSE parsing) ---

    #[test]
    fn find_event_boundary_locates_blank_line() {
        assert_eq!(find_event_boundary(b"data: x\n\nrest"), Some(7));
        assert_eq!(find_event_boundary(b"data: x\n"), None);
        assert_eq!(find_event_boundary(b""), None);
    }

    #[test]
    fn parse_tick_event_round_trips_a_clocknow() {
        let original = ClockNow::wall();
        let frame = format!("data: {}\n\n", serde_json::to_string(&original).unwrap());
        let parsed = parse_tick_event(&frame).expect("a data frame parses");
        assert_eq!(parsed.now, original.now);
        assert_eq!(parsed.simulated, original.simulated);
    }

    #[test]
    fn parse_tick_event_handles_minimal_and_junk_frames() {
        // Minimal wire frame — epoch/paused fields default.
        let now =
            parse_tick_event("data: {\"now\":\"2025-04-01T13:00:00Z\",\"simulated\":true}\n\n")
                .expect("minimal frame parses");
        assert!(now.simulated);
        assert!(!now.paused);
        // Keep-alive comment, empty event, and non-JSON → None.
        assert!(parse_tick_event(": keep-alive\n\n").is_none());
        assert!(parse_tick_event("\n").is_none());
        assert!(parse_tick_event("data: not-json\n\n").is_none());
    }
}
