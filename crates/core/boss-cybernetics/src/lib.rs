//! Per-VM Cybernetics coordinator (VSM S2/S3).
//!
//! Responsibilities:
//! - Enqueue incoming S1 messages to a per-agent durable inbox
//! - Enforce per-agent budget caps before dispatching
//! - Dispatch one message at a time via the agent dispatcher
//! - Chain dispatches on run completion
//! - Emit structured S3 telemetry for every lifecycle event

pub mod config;
pub mod http;
pub mod ingress;
pub mod runtime;
pub mod scheduler;
pub mod telemetry;

pub use runtime::{Cybernetics, CyberneticsError};
