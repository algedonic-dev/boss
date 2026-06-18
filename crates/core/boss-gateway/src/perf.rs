//! Per-endpoint latency histograms for the gateway.
//!
//! The `PerfCollector` is a shared, thread-safe table keyed by
//! (HTTP method, normalized path). Each entry holds an HDR histogram
//! recording request durations in microseconds, plus a count of 5xx
//! responses. The timing middleware records into it; the
//! `/api/gateway/perf` endpoint reads snapshots out of it.
//!
//! Histograms are in-memory only — they don't survive a gateway
//! restart. This is intentionally lossy to avoid new infrastructure;
//! persisted historical trends are out of scope.

use std::collections::HashMap;
use std::sync::Mutex;

use hdrhistogram::Histogram;
use serde::Serialize;

/// Maximum recorded latency in microseconds. 60 seconds is a generous
/// ceiling for real request latency; anything longer is clamped.
const HISTOGRAM_MAX_US: u64 = 60 * 1_000_000;
const SIGNIFICANT_FIGURES: u8 = 3;

#[derive(Eq, PartialEq, Hash, Clone)]
struct EndpointKey {
    method: String,
    path: String,
}

struct EndpointStats {
    histogram: Histogram<u64>,
    /// Count of 5xx responses. Unchanged from v1 so downstream
    /// consumers of `errors` keep working.
    error_count: u64,
    /// Count of 4xx responses (excluding 404 misses that genuinely
    /// represent "not found"). This is a new signal: a persistent
    /// 404/422 drumbeat on one endpoint is how the agreements route
    /// hid for weeks after the commerce binary drifted behind the
    /// code. Surfacing it alongside the 5xx count in
    /// `/api/gateway/perf` lets operators catch the same class of
    /// issue at a glance.
    client_error_count: u64,
}

impl EndpointStats {
    fn new() -> Self {
        Self {
            histogram: Histogram::new_with_max(HISTOGRAM_MAX_US, SIGNIFICANT_FIGURES)
                .expect("building hdr histogram"),
            error_count: 0,
            client_error_count: 0,
        }
    }
}

pub struct PerfCollector {
    inner: Mutex<HashMap<EndpointKey, EndpointStats>>,
}

impl PerfCollector {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record a single request observation. `ms` is wall-clock
    /// duration in milliseconds; `status` is the HTTP status code.
    pub fn record(&self, method: &str, path: &str, ms: f64, status: u16) {
        let key = EndpointKey {
            method: method.to_string(),
            path: normalize_path(path),
        };
        let us = (ms.max(0.0) * 1000.0) as u64;
        let mut map = self.inner.lock().expect("poisoned perf mutex");
        let stats = map.entry(key).or_insert_with(EndpointStats::new);
        let _ = stats.histogram.record(us.min(HISTOGRAM_MAX_US));
        if status >= 500 {
            stats.error_count += 1;
        } else if (400..500).contains(&status) {
            stats.client_error_count += 1;
        }
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        let map = self.inner.lock().expect("poisoned perf mutex");
        let mut endpoints: Vec<EndpointSnapshot> = map
            .iter()
            .map(|(k, v)| EndpointSnapshot {
                method: k.method.clone(),
                path: k.path.clone(),
                count: v.histogram.len(),
                p50_ms: quantile_ms(&v.histogram, 0.5),
                p95_ms: quantile_ms(&v.histogram, 0.95),
                p99_ms: quantile_ms(&v.histogram, 0.99),
                min_ms: v.histogram.min() as f64 / 1000.0,
                max_ms: v.histogram.max() as f64 / 1000.0,
                errors: v.error_count,
                client_errors: v.client_error_count,
            })
            .collect();
        // Sort by p95 descending so the slowest endpoints surface first.
        endpoints.sort_by(|a, b| {
            b.p95_ms
                .partial_cmp(&a.p95_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        PerfSnapshot {
            taken_at: chrono::Utc::now().to_rfc3339(),
            endpoints,
        }
    }

    pub fn reset(&self) {
        self.inner.lock().expect("poisoned perf mutex").clear();
    }
}

impl Default for PerfCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn quantile_ms(h: &Histogram<u64>, q: f64) -> f64 {
    h.value_at_quantile(q) as f64 / 1000.0
}

#[derive(Serialize)]
pub struct PerfSnapshot {
    pub taken_at: String,
    pub endpoints: Vec<EndpointSnapshot>,
}

#[derive(Serialize)]
pub struct EndpointSnapshot {
    pub method: String,
    pub path: String,
    pub count: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    /// 5xx responses on this endpoint.
    pub errors: u64,
    /// 4xx responses on this endpoint. A sustained value here is
    /// usually a contract drift (client sending the wrong shape,
    /// deployed upstream missing the route, etc.) — as opposed to
    /// a transient service outage.
    pub client_errors: u64,
}

/// Collapse dynamic path segments to placeholders so requests with
/// different IDs bucket together. Heuristics for the Boss ID
/// conventions: segments that are all digits, UUIDs, or look like
/// `prefix-001` / `SN-ABC123` collapse to `{id}`.
fn normalize_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if seg.is_empty() || !looks_like_id(seg) {
                seg.to_string()
            } else {
                "{id}".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn looks_like_id(seg: &str) -> bool {
    if seg.len() < 3 {
        return false;
    }
    // Pure digits: IDs like 12345.
    if seg.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // UUID-ish: 8+ hex characters with dashes.
    if seg.len() >= 8
        && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && seg.chars().filter(|c| c.is_ascii_digit()).count() >= 2
    {
        return true;
    }
    // Prefix-number style: `emp-005`, `SN-ABC123`, `account-00001`,
    // `inv-20260401-0001`, `tkt-done`, `VND-TEST01`. We want any
    // segment that contains at least one digit AND at least one
    // non-digit AND a hyphen.
    let has_digit = seg.chars().any(|c| c.is_ascii_digit());
    let has_alpha = seg.chars().any(|c| c.is_ascii_alphabetic());
    let has_dash = seg.contains('-');
    if has_digit && has_alpha && has_dash {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_prefix_number_ids() {
        assert_eq!(normalize_path("/api/people/emp-005"), "/api/people/{id}");
        assert_eq!(
            normalize_path("/api/people/accounts/account-00001"),
            "/api/people/accounts/{id}"
        );
        assert_eq!(normalize_path("/api/assets/SN-ABC-001"), "/api/assets/{id}");
    }

    #[test]
    fn leaves_static_routes_alone() {
        assert_eq!(normalize_path("/api/assets/tickets"), "/api/assets/tickets");
        assert_eq!(
            normalize_path("/api/commerce/invoices/create"),
            "/api/commerce/invoices/create"
        );
    }

    #[test]
    fn records_into_buckets() {
        let c = PerfCollector::new();
        c.record("GET", "/api/assets/tickets", 12.0, 200);
        c.record("GET", "/api/assets/tickets", 20.0, 200);
        c.record("GET", "/api/assets/tickets", 500.0, 500);
        let snap = c.snapshot();
        assert_eq!(snap.endpoints.len(), 1);
        let ep = &snap.endpoints[0];
        assert_eq!(ep.count, 3);
        assert_eq!(ep.errors, 1);
        assert_eq!(ep.client_errors, 0);
        assert!(ep.max_ms >= 499.0);
    }

    #[test]
    fn tracks_4xx_separately_from_5xx() {
        // The "agreements 404 drumbeat" hid for weeks because the old
        // collector only counted 5xx. Make sure 4xx lands in its own
        // bucket so the operator dashboard can surface it.
        let c = PerfCollector::new();
        c.record("POST", "/api/commerce/agreements", 3.0, 404);
        c.record("POST", "/api/commerce/agreements", 2.5, 404);
        c.record("POST", "/api/commerce/agreements", 4.0, 422);
        c.record("POST", "/api/commerce/agreements", 2.0, 201);
        c.record("POST", "/api/commerce/agreements", 800.0, 503);
        let snap = c.snapshot();
        let ep = snap
            .endpoints
            .iter()
            .find(|e| e.path == "/api/commerce/agreements")
            .expect("endpoint present");
        assert_eq!(ep.count, 5);
        assert_eq!(ep.errors, 1, "one 5xx");
        assert_eq!(ep.client_errors, 3, "two 404s + one 422");
    }

    #[test]
    fn buckets_dynamic_ids_together() {
        let c = PerfCollector::new();
        c.record("GET", "/api/assets/SN-001", 5.0, 200);
        c.record("GET", "/api/assets/SN-002", 6.0, 200);
        c.record("GET", "/api/assets/SN-003", 7.0, 200);
        let snap = c.snapshot();
        assert_eq!(snap.endpoints.len(), 1);
        assert_eq!(snap.endpoints[0].count, 3);
        assert_eq!(snap.endpoints[0].path, "/api/assets/{id}");
    }
}
