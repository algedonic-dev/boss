//! Request timing middleware — logs method, path, status, and duration
//! for every request passing through the gateway, and records into the
//! shared `PerfCollector` for the `/api/gateway/perf` endpoint.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;

pub async fn request_timer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status().as_u16();
    let ms = duration.as_secs_f64() * 1000.0;

    state.perf.record(method.as_str(), &path, ms, status);

    tracing::info!(
        method = %method,
        path = %path,
        status = status,
        duration_ms = format!("{ms:.1}"),
        "request"
    );

    response
}
