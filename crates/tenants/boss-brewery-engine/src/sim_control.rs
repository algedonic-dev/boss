//! Control + telemetry HTTP server for the `boss-brewery-sim` daemon.
//!
//! The daemon is a pure public-API client (it never touches the DB or the
//! bus). This localhost-only server lets `boss-simulator` OBSERVE how the
//! daemon is engaging the public API (the Cockpit) and — Phase 2 — GOVERN
//! its behavior config. It shares the daemon's in-process state via
//! `Arc<Mutex<…>>` and is reached only by `boss-simulator` on localhost
//! (NOT gateway-proxied). Inert unless the daemon runs (demo mode).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use boss_sim::output::live::LiveApiStats;
use boss_sim::workforce::WorkforceStats;

/// How many recent per-tick activity rows to retain in the ring buffer.
const RECENT_TICKS: usize = 60;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Clock + speed of the simulator's engagement — the cadence it drives the
/// public API at.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Cadence {
    pub sim_date: Option<String>,
    pub paused: bool,
    pub epoch_start: Option<String>,
    pub epoch_end: Option<String>,
    pub warp_factor: Option<f64>,
    pub days_per_tick: Option<i32>,
    pub tick_interval_seconds: Option<i32>,
}

/// One tick's worth of public-API engagement — the deltas the sim drove
/// that tick. The Cockpit renders these as the recent-activity feed.
#[derive(Debug, Clone, Serialize)]
pub struct TickActivity {
    pub tick: u64,
    pub sim_date: Option<String>,
    pub claimed: u64,
    pub completed: u64,
    pub deferred: u64,
    pub errors: u64,
}

/// A snapshot of how the simulator is engaging the public API. Served at
/// `GET /telemetry`; the Cockpit is oriented around it.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SimTelemetry {
    // --- identity: the sim drives the company AS the workforce, over the
    // public API (x-boss-user: automation:sim + x-sim-origin) ---
    pub actor: String,
    pub role: String,
    pub api_base: String,
    pub started_unix: i64,

    // --- clock / cadence ---
    pub cadence: Cadence,
    pub tick_count: u64,
    pub last_tick_unix: Option<i64>,

    // --- cumulative engagement since process start ---
    /// Workforce step transitions (checkins/claimed/completed/deferred/
    /// in_progress/errors) — the PUT /api/jobs/{}/steps engagement.
    pub workforce: WorkforceStats,
    /// Per-domain API writes (invoices/shipments/jobs/payments/…) — the
    /// step.done side-effect POSTs to the domain services.
    pub api_writes: LiveApiStats,

    // --- recent per-tick activity (ring buffer) ---
    pub recent: VecDeque<TickActivity>,
}

impl SimTelemetry {
    pub fn new(actor: String, role: String, api_base: String) -> Self {
        Self {
            actor,
            role,
            api_base,
            started_unix: now_unix(),
            ..Default::default()
        }
    }

    /// Refresh from the tick loop after a working tick: push a per-tick
    /// activity row (deltas vs the previous cumulative workforce
    /// snapshot), then replace the cumulative stats + cadence.
    pub fn record_tick(
        &mut self,
        tick: u64,
        cadence: Cadence,
        workforce: &WorkforceStats,
        api_writes: &LiveApiStats,
    ) {
        // Deltas vs the previous cumulative snapshot (read before we
        // overwrite self.workforce below).
        let claimed = workforce.claimed.saturating_sub(self.workforce.claimed);
        let completed = workforce.completed.saturating_sub(self.workforce.completed);
        let deferred = workforce.deferred.saturating_sub(self.workforce.deferred);
        let errors = workforce.errors.saturating_sub(self.workforce.errors);
        self.recent.push_back(TickActivity {
            tick,
            sim_date: cadence.sim_date.clone(),
            claimed,
            completed,
            deferred,
            errors,
        });
        while self.recent.len() > RECENT_TICKS {
            self.recent.pop_front();
        }

        self.cadence = cadence;
        self.tick_count = tick;
        self.last_tick_unix = Some(now_unix());
        self.workforce = workforce.clone();
        self.api_writes = api_writes.clone();
    }

    /// Update just the cadence (paused / restarting branches, where no
    /// work was done so no activity row is pushed).
    pub fn note_cadence(&mut self, cadence: Cadence) {
        self.cadence = cadence;
        self.last_tick_unix = Some(now_unix());
    }
}

/// Shared handle held by both the tick loop (writer) and the control
/// server (reader).
pub type SharedTelemetry = Arc<Mutex<SimTelemetry>>;

#[derive(Clone)]
struct ControlState {
    telemetry: SharedTelemetry,
}

async fn get_telemetry(State(st): State<ControlState>) -> Json<SimTelemetry> {
    let snapshot = st.telemetry.lock().map(|t| t.clone()).unwrap_or_default();
    Json(snapshot)
}

async fn health() -> &'static str {
    "ok"
}

/// Run the control + telemetry server. Binds `bind` (e.g.
/// `127.0.0.1:7011`); localhost-only. Spawned by the daemon as a
/// background task — returns only on listener error.
pub async fn serve(bind: String, telemetry: SharedTelemetry) -> anyhow::Result<()> {
    let state = ControlState { telemetry };
    let app = Router::new()
        .route("/telemetry", get(get_telemetry))
        .route("/health", get(health))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "sim control + telemetry server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
