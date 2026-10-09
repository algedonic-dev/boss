//! Protected, report-only joined gate evidence in Apps (design 21946380).
//! No caller supplies a service subset or an upstream URL. The port
//! registry names every machine gate there could be, and the launcher's
//! record says which of them this pod started: those are required, and
//! an unavailable or non-recording one stays a named gap, never a
//! silent exemption. A gate the launcher recorded as skipped is excused
//! only while its port refuses a connection and the log holds nothing
//! from it; a record that cannot be read excuses nothing (backlog
//! 93e0814a, `boss_core::gate_window::LaunchRoster`). And an excuse holds
//! only for hours watched under the selection that makes it: every gated
//! process states the generation of its launch record with its start,
//! and a window whose hours span two of them — or that this reader's own
//! record does not digest to — is not clean and names both and the
//! instant (design 3cc6152a, backlog 14fe115c; `judged_generations`).

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::clock::Clock;
use boss_core::gate_evidence::{Gate, GateEvidenceLog};
use boss_core::gate_window::{
    LaunchRoster, LiveRead, PortProbe, join_launched_window, launch_roster,
};
use boss_core::machine_token;
use boss_policy_client::{CurrentUser, User};
use chrono::Duration;
use serde::Deserialize;
use serde_json::Value;

pub const PATH: &str = boss_core::gate_window::PATH;
pub const MAX_HOURS: u32 = 168;
pub const POLICY_REFUSALS_PATH: &str = "/api/policy/check/refusals";

/// All reads are explicit and replaceable, including failed reads.
#[async_trait]
pub trait LiveTallies: Send + Sync {
    fn required_services(&self, gate: Gate) -> Vec<String>;
    async fn read(&self, gate: Gate, service: &str) -> Result<Value, String>;
    /// Which of the gate's services this deployment started. The default
    /// consults no record and requires every one of them.
    fn roster(&self, gate: Gate) -> LaunchRoster {
        LaunchRoster::all_required(self.required_services(gate))
    }
    /// Whether anything accepts a connection at the service's port:
    /// `Ok(false)` only for a refused connection. The default cannot
    /// say, which holds an excuse rather than granting it.
    async fn listening(&self, service: &str) -> Result<bool, String> {
        Err(format!("this reader has no port probe for {service}"))
    }
}

/// The variable the launcher exports with the path of the record it
/// wrote (`services-launcher.sh`). Read once, at construction. The one
/// spelling every gated process reads its own roster stamp through.
pub const LAUNCH_RECORD_ENV: &str = boss_core::gate_window::LAUNCH_RECORD_ENV;

/// Existing stamped machine client, at the registry's local ports.
pub struct LocalTallies {
    http: machine_token::Client,
    /// Every service given, gated or not: each mounts an actor-role
    /// report, so the `actor-role` window requires each (backlog
    /// e0bdba74). The machine gate's window requires the gated ones.
    services: Vec<(String, String)>,
    /// `Some` only on the production path: `Ok(path)` of the launcher's
    /// record, or why no path is known. `None` (explicit test rosters)
    /// consults no record and requires every service.
    launch_record: Option<Result<std::path::PathBuf, String>>,
}

impl LocalTallies {
    pub fn from_ports() -> Result<Self, reqwest::Error> {
        let http = machine_token::Client::build(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .timeout(std::time::Duration::from_secs(4)),
        )?;
        // The launcher exports the path of the record it wrote; a
        // process it did not start has none and can excuse nothing.
        // The variable can aim this reader at another file, and that
        // buys nothing: a record only excuses, and every excuse is
        // checked against the port and the log before it counts.
        let record = std::env::var(LAUNCH_RECORD_ENV)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                format!(
                    "{LAUNCH_RECORD_ENV} is unset: this process was not started by the launcher, so what is deployed is unknown"
                )
            });
        Ok(Self::at_declared_ports(http).with_launch_record(record))
    }

    /// The same reader, deriving its required services from the
    /// launcher's record at `record` — or, given why there is no path,
    /// requiring every service and saying so.
    pub fn with_launch_record(mut self, record: Result<std::path::PathBuf, String>) -> Self {
        self.launch_record = Some(record);
        self
    }

    /// Same production roster with an explicitly supplied client. It
    /// lets the roster/transport tests exercise this definition without
    /// reading an installed token or sending to a real service.
    pub fn at_declared_ports(http: machine_token::Client) -> Self {
        Self::new(
            http,
            boss_ports::all()
                .map(|s| (s.name.to_string(), boss_ports::url(s.name)))
                .collect(),
        )
    }

    /// Explicit private ports and client for transport tests. Production
    /// uses only from_ports; no request field selects these targets.
    pub fn new(http: machine_token::Client, services: Vec<(String, String)>) -> Self {
        Self {
            http,
            services,
            launch_record: None,
        }
    }
}

#[async_trait]
impl LiveTallies for LocalTallies {
    fn required_services(&self, gate: Gate) -> Vec<String> {
        match gate {
            Gate::MachineGate => self
                .services
                .iter()
                .filter(|(name, _)| boss_core::machine_gate::is_gated(name))
                .map(|(name, _)| name.clone())
                .collect(),
            Gate::PolicyCheck => vec!["policy".into()],
            // Every port in the registry serves a report
            // (`role_service::report_path`; the pin
            // `every_registered_service_has_a_production_role_report_owner`
            // holds each binary to it).
            Gate::ActorRole => self.services.iter().map(|(name, _)| name.clone()).collect(),
        }
    }

    fn roster(&self, gate: Gate) -> LaunchRoster {
        let all = self.required_services(gate);
        // A service the launcher did not start mounts no machine gate
        // and no actor-role report: the record excuses it from either
        // window on the same checks. The generation rule is the machine
        // gate's alone (`join_selected`): only its starts state a
        // roster stamp, so an actor-role window names no generation.
        let (Gate::MachineGate | Gate::ActorRole, Some(record)) = (gate, &self.launch_record)
        else {
            return LaunchRoster::all_required(all);
        };
        let gated: Vec<(String, Vec<String>)> = all
            .into_iter()
            .map(|service| {
                let binaries = boss_ports::launcher_binaries(&service);
                (service, binaries)
            })
            .collect();
        // Read on every request: the file is small (and the read is
        // bounded), and a record that
        // disappears or changes under a running pod must be seen.
        // Through the one bounded reader every gated process stamps
        // from, so a reader and a process cannot disagree about what
        // the file holds.
        let text = record
            .clone()
            .and_then(|path| boss_core::gate_window::read_launch_record_file(&path));
        launch_roster(&gated, text.as_deref().map_err(Clone::clone))
    }

    async fn listening(&self, service: &str) -> Result<bool, String> {
        let targets: Vec<&str> = self
            .services
            .iter()
            .filter(|(s, _)| s == service)
            .map(|(_, url)| url.as_str())
            .collect();
        let [target] = targets.as_slice() else {
            return Err(format!(
                "{service}: {} declared upstreams, expected one",
                targets.len()
            ));
        };
        let url = reqwest::Url::parse(target).map_err(|e| format!("{target}: {e}"))?;
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return Err(format!("{target} names no host and port"));
        };
        let connect = tokio::net::TcpStream::connect((host, port));
        match tokio::time::timeout(std::time::Duration::from_secs(2), connect).await {
            Ok(Ok(_)) => Ok(true),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => Ok(false),
            Ok(Err(e)) => Err(format!("connect {host}:{port}: {e}")),
            Err(_) => Err(format!("connect {host}:{port}: no answer in 2s")),
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
            Gate::MachineGate => boss_core::machine_gate::MISSES_PATH.to_string(),
            Gate::PolicyCheck => POLICY_REFUSALS_PATH.to_string(),
            Gate::ActorRole => boss_policy_client::role_service::report_path(service),
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
    role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
}

pub fn gate_window_router(
    log: Arc<dyn GateEvidenceLog>,
    live: Arc<dyn LiveTallies>,
    clock: Arc<dyn Clock>,
) -> Router {
    gate_window_router_with_reports(log, live, clock, None)
}

pub fn gate_window_router_with_reports(
    log: Arc<dyn GateEvidenceLog>,
    live: Arc<dyn LiveTallies>,
    clock: Arc<dyn Clock>,
    role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
) -> Router {
    Router::new()
        .route(PATH, get(read_window))
        .with_state(ReaderState {
            log,
            live,
            clock,
            role_guards,
        })
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
    if !boss_events::tail_http::observed_audit_read(
        &user,
        state.role_guards.as_deref(),
        "gate-window-read",
    ) {
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
    // What this pod started, from the launcher's record. An excused
    // service is still read and its port probed: the excuse is a claim
    // this observation checks, and a consumer with its own roster (the
    // rotation's drain) is owed every port's answer.
    let roster = state.live.roster(query.gate);
    let excused: Vec<String> = roster
        .not_launched
        .iter()
        .map(|n| n.service.clone())
        .collect();
    let read_all =
        futures::future::join_all(roster.required.iter().chain(&excused).map(|service| async {
            LiveRead {
                service: service.clone(),
                answer: state.live.read(query.gate, service).await,
            }
        }));
    let probe_all = futures::future::join_all(excused.iter().map(|service| async {
        PortProbe {
            service: service.clone(),
            listening: state.live.listening(service).await,
        }
    }));
    let (reads, probes) = futures::future::join(read_all, probe_all).await;
    // Read the log after the live half. A restart/mode move that passes
    // between the reads cannot retain the old instance's clean answer:
    // the join demands the exact latest sourced start. This is an
    // observation, not an atomic authorization or a future safety grant.
    let facts = state.log.facts(query.gate, from).await;
    Json(join_launched_window(
        query.gate, &roster, &probes, from, now, facts, reads,
    ))
    .into_response()
}
