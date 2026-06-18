//! Cross-VM observability service.
//!
//! Responsibilities:
//! - Subscribe to `cybernetics.>` on NATS and fan out as SSE to browsers
//! - Aggregate per-VM Cybernetics HTTP snapshots into unified endpoints
//! - Serve the static web dashboard
//!
//! This service is read-only. It issues no commands and holds no durable state.

pub mod aggregator;
pub mod config;
pub mod demo_agents;
pub mod sse;
