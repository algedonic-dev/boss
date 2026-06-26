//! Per-actor API-call tallies — "how the sim engages the public API,
//! by who's acting."
//!
//! The simulator already models its actors: the workforce (employees,
//! by role) and the named `CounterpartySpec` chains (which decode to
//! real-world Account / Vendor / Bank actors). This module is the thin
//! telemetry layer that attributes each outbound HTTP call to the actor
//! making it. We count **our own calls, on the ack** — when a request
//! returns, we bump `calls` and (on a non-2xx) `errors`, keyed by
//! `(actor kind, actor label, "METHOD /templated/path")`. No
//! reconciliation against what actually landed in the system — that's a
//! separate concern.
//!
//! The handle is shared (`Arc<Mutex<…>>`) between the workforce (its own
//! HTTP client) and `LiveApiOutput`; the daemon snapshots it into its
//! telemetry each tick, mirroring how `WorkforceStats` / `LiveApiStats`
//! already flow.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Real-world actor type. Lines up with BOSS subject kinds — the sim's
/// counterparty chains decode into these (ar-aging/bad-debt → Account,
/// the suppliers + courier → Vendor, bank-ach → Bank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    Employee,
    Account,
    Vendor,
    Bank,
    Environment,
}

impl ActorKind {
    /// Parse the `actor_kind` data tag on a `[counterparty.*]` spec.
    /// Unknown / absent → `Environment` (the catch-all), so a new chain
    /// without a tag is never silently mis-grouped under a real party.
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "employee" => Self::Employee,
            "account" => Self::Account,
            "vendor" => Self::Vendor,
            "bank" => Self::Bank,
            _ => Self::Environment,
        }
    }
}

/// One (actor, endpoint) cell: total calls + how many failed (non-2xx).
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Tally {
    pub calls: u64,
    pub errors: u64,
}

/// Shared per-`(kind, label, endpoint)` tally. Cumulative since daemon
/// start (like the existing stats; resets on restart).
pub type ApiActivity = Arc<Mutex<BTreeMap<(ActorKind, String, String), Tally>>>;

/// Fresh, empty handle.
pub fn new_handle() -> ApiActivity {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// Record one call **on its ack**. `ok = status.is_success()`; a false
/// bumps `errors` too. A poisoned lock drops the sample rather than
/// panicking the tick loop — telemetry must never wedge the sim.
pub fn record(act: &ApiActivity, kind: ActorKind, label: &str, endpoint: &str, ok: bool) {
    if let Ok(mut m) = act.lock() {
        let t = m
            .entry((kind, label.to_string(), endpoint.to_string()))
            .or_default();
        t.calls += 1;
        if !ok {
            t.errors += 1;
        }
    }
}

/// Collapse a concrete path into a stable endpoint label: any segment
/// that looks like an id — has a digit, or ≥2 hyphens (BOSS ids are
/// `inv-step-…`, `acc-direct-shop`, …) — becomes `{}`. Static route
/// segments (`sign-offs`, `tax-filings`, `business-calendars`) keep
/// their single hyphen and no digit, so they survive. `?query` is
/// dropped. Returns e.g. `"PUT /api/jobs/{}/steps/{}"`.
pub fn endpoint_label(method: &str, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let templated = path
        .split('/')
        .map(|seg| {
            let has_digit = seg.bytes().any(|b| b.is_ascii_digit());
            let hyphens = seg.bytes().filter(|&b| b == b'-').count();
            if has_digit || hyphens >= 2 { "{}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/");
    format!("{method} {templated}")
}

/// One endpoint's tally inside an actor's activity (telemetry shape;
/// mirrored in `apps/simulator/src/types.ts`).
#[derive(Debug, Clone, Serialize)]
pub struct EndpointCount {
    pub endpoint: String,
    pub calls: u64,
    pub errors: u64,
}

/// One actor's API activity: its endpoints + rolled-up totals.
#[derive(Debug, Clone, Serialize)]
pub struct ActorActivity {
    pub kind: ActorKind,
    pub label: String,
    pub calls: u64,
    pub errors: u64,
    pub endpoints: Vec<EndpointCount>,
}

/// Snapshot the flat tally map into one `ActorActivity` per
/// `(kind, label)`, endpoints sorted busiest-first. Deterministic order
/// (BTreeMap keys), so the cockpit doesn't jitter between polls.
pub fn snapshot(act: &ApiActivity) -> Vec<ActorActivity> {
    let m = match act.lock() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut grouped: BTreeMap<(ActorKind, String), Vec<EndpointCount>> = BTreeMap::new();
    for ((kind, label, endpoint), t) in m.iter() {
        grouped
            .entry((*kind, label.clone()))
            .or_default()
            .push(EndpointCount {
                endpoint: endpoint.clone(),
                calls: t.calls,
                errors: t.errors,
            });
    }
    grouped
        .into_iter()
        .map(|((kind, label), mut endpoints)| {
            endpoints.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.endpoint.cmp(&b.endpoint)));
            let calls = endpoints.iter().map(|e| e.calls).sum();
            let errors = endpoints.iter().map(|e| e.errors).sum();
            ActorActivity {
                kind,
                label,
                calls,
                errors,
                endpoints,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_label_templates_ids_keeps_static_segments() {
        assert_eq!(
            endpoint_label("PUT", "/api/jobs/job-bigseed-0001/steps/stp-7f3a/"),
            "PUT /api/jobs/{}/steps/{}/"
        );
        // Static hyphenated segments survive (one hyphen, no digit).
        assert_eq!(
            endpoint_label("POST", "/api/jobs/job-1/steps/s-2/sign-offs"),
            "POST /api/jobs/{}/steps/{}/sign-offs"
        );
        // ≥2 hyphens reads as an id even without a digit.
        assert_eq!(
            endpoint_label("GET", "/api/people/accounts/acc-direct-shop/notes"),
            "GET /api/people/accounts/{}/notes"
        );
        // Query string dropped.
        assert_eq!(
            endpoint_label("GET", "/api/assets?account_id=acc-1&limit=5"),
            "GET /api/assets"
        );
    }

    #[test]
    fn from_tag_defaults_unknown_to_environment() {
        assert_eq!(ActorKind::from_tag("Account"), ActorKind::Account);
        assert_eq!(ActorKind::from_tag("vendor"), ActorKind::Vendor);
        assert_eq!(ActorKind::from_tag("bank"), ActorKind::Bank);
        assert_eq!(ActorKind::from_tag("whatever"), ActorKind::Environment);
        assert_eq!(ActorKind::from_tag(""), ActorKind::Environment);
    }

    #[test]
    fn record_and_snapshot_group_by_actor_busiest_first() {
        let act = new_handle();
        record(
            &act,
            ActorKind::Employee,
            "head-brewer",
            "PUT /api/jobs/{}/steps",
            true,
        );
        record(
            &act,
            ActorKind::Employee,
            "head-brewer",
            "PUT /api/jobs/{}/steps",
            false,
        );
        record(
            &act,
            ActorKind::Employee,
            "head-brewer",
            "POST /api/jobs/{}/steps/{}/sign-offs",
            true,
        );
        record(
            &act,
            ActorKind::Account,
            "ar-aging",
            "PUT /api/commerce/invoices/{}/paid",
            true,
        );

        let snap = snapshot(&act);
        assert_eq!(snap.len(), 2, "two distinct actors");
        let brewer = snap.iter().find(|a| a.label == "head-brewer").unwrap();
        assert_eq!(brewer.kind, ActorKind::Employee);
        assert_eq!(brewer.calls, 3);
        assert_eq!(brewer.errors, 1);
        // Busiest endpoint first.
        assert_eq!(brewer.endpoints[0].endpoint, "PUT /api/jobs/{}/steps");
        assert_eq!(brewer.endpoints[0].calls, 2);
        assert_eq!(brewer.endpoints[0].errors, 1);
    }
}
