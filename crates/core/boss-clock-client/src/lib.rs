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
}
