//! HTTP introspection API for the observability dashboard.
//!
//! Read-only snapshots of Cybernetics state. All endpoints return JSON.
//! The dashboard polls these alongside subscribing to S3 telemetry events.
//!
//! | Route                 | Body                                 |
//! |-----------------------|--------------------------------------|
//! | `GET /health`         | `{vm_id, status, timestamp}`         |
//! | `GET /agents`         | `[AgentSpec]`                        |
//! | `GET /queues`         | `[{agent, depth}]`                   |
//! | `GET /runs`           | `[RunHandle]`                        |
//! | `GET /costs?window=…` | `[{agent, cost, window}]`            |

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use boss_clock_client::ClockClient;
use boss_core::agent::{AgentId, AgentSpec, Cost, RunHandle, Window};
use boss_core::port::{AgentDispatcher, AgentRegistry, CostLedger, MessageQueue};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Shared state injected into every handler.
#[derive(Clone)]
pub struct HttpState {
    pub vm_id: Arc<str>,
    pub queue: Arc<dyn MessageQueue>,
    pub dispatcher: Arc<dyn AgentDispatcher>,
    pub ledger: Arc<dyn CostLedger>,
    pub registry: Arc<dyn AgentRegistry>,
    /// Authoritative clock — health, findings, message ids all
    /// stamp from here so sim mode produces sim-aligned timestamps.
    pub clock: Arc<dyn ClockClient>,
}

/// Build the router. Mount onto any axum server.
pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/agents", get(agents))
        .route("/queues", get(queues))
        .route("/runs", get(runs))
        .route("/costs", get(costs))
        .route("/findings", post(record_finding))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Response bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HealthBody {
    pub vm_id: String,
    pub status: &'static str,
    pub timestamp: DateTime<Utc>,
    pub capabilities: boss_core::startup::Capabilities,
}

#[derive(Debug, Serialize)]
pub struct QueueEntry {
    pub agent: AgentId,
    pub depth: usize,
}

#[derive(Debug, Serialize)]
pub struct CostEntry {
    pub agent: AgentId,
    pub cost: Cost,
    pub window: WindowName,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowName {
    #[default]
    Hour,
    Day,
}

impl WindowName {
    fn to_window(self) -> Window {
        match self {
            WindowName::Hour => Window::LastHour,
            WindowName::Day => Window::LastDay,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CostsQuery {
    #[serde(default)]
    pub window: WindowName,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(s): State<HttpState>) -> impl IntoResponse {
    let now = s.clock.now().await.now;
    axum::Json(HealthBody {
        vm_id: s.vm_id.to_string(),
        status: "ok",
        timestamp: now,
        capabilities: boss_core::startup::Capabilities::new(
            "boss-cybernetics",
            env!("CARGO_PKG_VERSION"),
            "in-memory",
        ),
    })
}

async fn agents(State(s): State<HttpState>) -> Result<axum::Json<Vec<AgentSpec>>, PortError> {
    let mut list = s
        .registry
        .list()
        .await
        .map_err(|e| PortError::internal(e.to_string()))?;
    list.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    Ok(axum::Json(list))
}

async fn queues(State(s): State<HttpState>) -> Result<axum::Json<Vec<QueueEntry>>, PortError> {
    let depths = s
        .queue
        .depths()
        .await
        .map_err(|e| PortError::internal(e.to_string()))?;
    let mut body: Vec<QueueEntry> = depths
        .into_iter()
        .map(|(agent, depth)| QueueEntry { agent, depth })
        .collect();
    body.sort_by(|a, b| a.agent.as_str().cmp(b.agent.as_str()));
    Ok(axum::Json(body))
}

async fn runs(State(s): State<HttpState>) -> Result<axum::Json<Vec<RunHandle>>, PortError> {
    let r = s
        .dispatcher
        .running()
        .await
        .map_err(|e| PortError::internal(e.to_string()))?;
    Ok(axum::Json(r))
}

async fn costs(
    State(s): State<HttpState>,
    Query(q): Query<CostsQuery>,
) -> Result<axum::Json<Vec<CostEntry>>, PortError> {
    let window_name = q.window;
    let window = window_name.to_window();
    let specs = s
        .registry
        .list()
        .await
        .map_err(|e| PortError::internal(e.to_string()))?;
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let cost = s
            .ledger
            .spent(&spec.id, window)
            .await
            .map_err(|e| PortError::internal(e.to_string()))?;
        out.push(CostEntry {
            agent: spec.id,
            cost,
            window: window_name,
        });
    }
    out.sort_by(|a, b| a.agent.as_str().cmp(b.agent.as_str()));
    Ok(axum::Json(out))
}

// ---------------------------------------------------------------------------
// Agent findings — write-back endpoint for agent feedback
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Finding {
    /// Which agent produced this finding.
    pub agent_id: String,
    /// Finding severity: info, warning, critical.
    #[serde(default = "default_severity")]
    pub severity: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of the finding.
    pub body: String,
    /// Optional entity reference (e.g., device serial, ticket ID).
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub entity_id: Option<String>,
    /// Optional list of recommended actions.
    #[serde(default)]
    pub actions: Vec<String>,
}

fn default_severity() -> String {
    "info".to_string()
}

#[derive(Debug, Serialize)]
struct FindingResponse {
    ok: bool,
    message_id: String,
}

/// Record an agent finding as telemetry. Severity sets the escalation
/// audience: critical/warning flag the executive audience (resolved from
/// the `is_executive` Class registry by the escalation router, not a
/// hardcoded org chart), info is recorded only. Per-tenant routing
/// (which manager handles which finding) belongs in a future
/// `escalation_rules` table, not core code.
async fn record_finding(
    State(s): State<HttpState>,
    axum::Json(finding): axum::Json<Finding>,
) -> Result<impl IntoResponse, PortError> {
    let audience = match finding.severity.as_str() {
        "critical" | "warning" => "executive",
        _ => "telemetry-only",
    };

    let now = s.clock.now().await.now;
    let message_id = format!("finding-{}-{}", finding.agent_id, now.timestamp_millis());

    // Publish as a telemetry event for the audit trail.
    let payload = serde_json::json!({
        "agent_id": finding.agent_id,
        "severity": finding.severity,
        "title": finding.title,
        "body": finding.body,
        "entity_type": finding.entity_type,
        "entity_id": finding.entity_id,
        "actions": finding.actions,
        "audience": audience,
        "message_id": message_id,
    });

    // Emit telemetry event (picked up by NATS subscribers + SSE dashboard).
    let event = boss_core::event::Event::new(
        format!("cybernetics.{}", finding.agent_id),
        "cybernetics.finding.recorded",
        payload,
        now,
    );
    // Best-effort publish — don't fail the finding if NATS is down.
    // The HttpState doesn't hold an EventBus directly, so we log and continue.
    tracing::info!(
        agent = %finding.agent_id,
        severity = %finding.severity,
        title = %finding.title,
        audience = %audience,
        event_id = %event.id,
        "agent finding recorded"
    );

    Ok((
        StatusCode::CREATED,
        axum::Json(FindingResponse {
            ok: true,
            message_id,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PortError {
    message: String,
}

impl PortError {
    fn internal(message: String) -> Self {
        Self { message }
    }
}

impl IntoResponse for PortError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}
