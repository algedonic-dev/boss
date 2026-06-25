//! boss-simulator — hosts the Simulator UX.
//!
//! Serves the `apps/simulator` SPA under `/simulator` (the gateway
//! reverse-proxies `/simulator/*` here WITHOUT stripping the prefix, so
//! we nest the whole sub-app under `/simulator`) plus a small
//! `/simulator/api/*` control/status surface.
//!
//! Phase B is a skeleton: a health endpoint + a stub page when no built
//! bundle is present. Phase C drops the real `apps/simulator` bundle into
//! `BOSS_SIM_STATIC_DIR` and fills in the control/status API.

use std::{net::SocketAddr, path::Path};

use anyhow::{Context, Result};
use axum::{response::Html, routing::get, Json, Router};
use tokio::net::TcpListener;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

const STUB_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>BOSS Simulator</title></head>\
<body style=\"font-family:system-ui;padding:40px;color:#1c1917\">\
<h1>BOSS Simulator</h1>\
<p>The simulator UI bundle is not installed yet. Build <code>apps/simulator</code> \
and deploy it to <code>BOSS_SIM_STATIC_DIR</code>.</p></body></html>";

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "boss-simulator" }))
}

async fn stub() -> Html<&'static str> {
    Html(STUB_HTML)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .compact()
        .init();

    let bind = std::env::var("BOSS_SIM_BIND")
        .unwrap_or_else(|_| format!("0.0.0.0:{}", boss_ports::prod("simulator")));
    let static_dir = std::env::var("BOSS_SIM_STATIC_DIR")
        .unwrap_or_else(|_| "/var/lib/boss-simulator/dist".to_string());

    // The /simulator sub-app: control/status API routes + the SPA (or a
    // stub until apps/simulator is built + deployed). Nested under
    // /simulator because the gateway forwards that prefix unchanged.
    let api = Router::new().route("/api/health", get(health));
    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let sim = if Path::new(&index).exists() {
        // SPA fallback: unknown paths serve index.html so client-side
        // routes survive a reload.
        let serve = ServeDir::new(&static_dir).fallback(ServeFile::new(index));
        api.fallback_service(serve)
    } else {
        info!(dir = %static_dir, "no SPA bundle found — serving stub page");
        api.fallback(stub)
    };

    let app = Router::new()
        .nest("/simulator", sim)
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = bind.parse().with_context(|| format!("invalid bind `{bind}`"))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    info!(addr = %addr, static_dir = %static_dir, "boss-simulator listening");
    axum::serve(listener, app).await.context("serving HTTP")?;
    Ok(())
}
