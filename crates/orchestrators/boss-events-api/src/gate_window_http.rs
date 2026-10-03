//! Protected, report-only joined gate evidence in Apps (design 21946380).
//! No caller supplies a service subset or an upstream URL. The port
//! registry defines every required machine gate; an unavailable or
//! non-recording service stays a named gap, never a silent exemption.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::clock::Clock;
use boss_core::gate_evidence::{Gate, GateEvidenceLog};
use boss_core::gate_window::{LiveRead, join_window};
use boss_core::machine_token;
use boss_policy_client::{AccessTier, CurrentUser, User};
use chrono::Duration;
use serde::Deserialize;
use serde_json::Value;

pub const PATH: &str = "/api/events/gate-window";
pub const MAX_HOURS: u32 = 168;
pub const POLICY_REFUSALS_PATH: &str = "/api/policy/check/refusals";

/// All reads are explicit and replaceable, including failed reads.
#[async_trait]
pub trait LiveTallies: Send + Sync {
    fn required_services(&self, gate: Gate) -> Vec<String>;
    async fn read(&self, gate: Gate, service: &str) -> Result<Value, String>;
}

/// Existing stamped machine client, at the registry's local ports.
pub struct LocalTallies {
    http: machine_token::Client,
    services: Vec<(String, String)>,
}

impl LocalTallies {
    pub fn from_ports() -> Result<Self, reqwest::Error> {
        let http = machine_token::Client::build(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(4)),
        )?;
        Ok(Self::at_declared_ports(http))
    }

    /// Same production roster with an explicitly supplied client. It
    /// lets the roster/transport tests exercise this definition without
    /// reading an installed token or sending to a real service.
    pub fn at_declared_ports(http: machine_token::Client) -> Self {
        Self::new(
            http,
            boss_ports::all()
                .filter(|s| boss_core::machine_gate::is_gated(s.name))
                .map(|s| (s.name.to_string(), boss_ports::url(s.name)))
                .collect(),
        )
    }

    /// Explicit private ports and client for transport tests. Production
    /// uses only from_ports; no request field selects these targets.
    pub fn new(http: machine_token::Client, services: Vec<(String, String)>) -> Self {
        Self { http, services }
    }
}

#[async_trait]
impl LiveTallies for LocalTallies {
    fn required_services(&self, gate: Gate) -> Vec<String> {
        match gate {
            Gate::MachineGate => self.services.iter().map(|(name, _)| name.clone()).collect(),
            Gate::PolicyCheck => vec!["policy".into()],
        }
    }

    async fn read(&self, gate: Gate, service: &str) -> Result<Value, String> {
        let targets: Vec<&str> = self
            .services
            .iter()
            .filter(|(s, _)| s == service)
            .map(|(_, url)| url.as_str())
            .collect();
        if targets.len() != 1 {
            return Err(format!(
                "{service}: {} declared upstreams, expected one",
                targets.len()
            ));
        }
        let path = match gate {
            Gate::MachineGate => boss_core::machine_gate::MISSES_PATH,
            Gate::PolicyCheck => POLICY_REFUSALS_PATH,
        };
        let url = format!("{}{path}", targets[0].trim_end_matches('/'));
        let actor = serde_json::to_string(&User::service("events"))
            .map_err(|e| format!("events reader identity: {e}"))?;
        let response = self
            .http
            .get(&url)
            .header("x-boss-user", actor)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("GET {url} body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "GET {url} answered {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        serde_json::from_slice(&body)
            .map_err(|e| format!("GET {url} is not one JSON document: {e}"))
    }
}

#[derive(Clone)]
struct ReaderState {
    log: Arc<dyn GateEvidenceLog>,
    live: Arc<dyn LiveTallies>,
    clock: Arc<dyn Clock>,
}

pub fn gate_window_router(
    log: Arc<dyn GateEvidenceLog>,
    live: Arc<dyn LiveTallies>,
    clock: Arc<dyn Clock>,
) -> Router {
    Router::new()
        .route(PATH, get(read_window))
        .with_state(ReaderState { log, live, clock })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowQuery {
    gate: Gate,
    hours: Option<u32>,
}

async fn read_window(
    State(state): State<ReaderState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<WindowQuery>,
) -> Response {
    // Exactly the existing audit-log read door, not a new policy grant.
    if !(matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor)
        || boss_core::roles::has_global_read(&user.role))
    {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }
    let hours = query.hours.unwrap_or(72);
    if !(1..=MAX_HOURS).contains(&hours) {
        return (
            StatusCode::BAD_REQUEST,
            format!("hours must be 1..={MAX_HOURS}"),
        )
            .into_response();
    }
    // Gate lifetimes and audit timestamps are on the record's wall
    // clock, not the company's simulated business timeline. Production
    // injects WallClock; fixed clocks exercise the same read in tests.
    let now = state.clock.now();
    let from = now - Duration::hours(i64::from(hours));
    let required = state.live.required_services(query.gate);
    let reads = futures::future::join_all(required.iter().map(|service| async {
        LiveRead {
            service: service.clone(),
            answer: state.live.read(query.gate, service).await,
        }
    }))
    .await;
    // Read the log after the live half. A restart/mode move that passes
    // between the reads cannot retain the old instance's clean answer:
    // the join demands the exact latest sourced start. This is an
    // observation, not an atomic authorization or a future safety grant.
    let facts = state.log.facts(query.gate, from).await;
    Json(join_window(query.gate, &required, from, now, facts, reads)).into_response()
}
