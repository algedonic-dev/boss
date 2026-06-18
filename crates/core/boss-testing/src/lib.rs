//! Shared test utilities for Boss services.
//!
//! Provides:
//! - `TestRequest` builder for sending HTTP requests to an axum Router
//! - `TestResponse` wrapper with assertion helpers that produce
//!   useful error messages
//! - `RecordingEventBus` that captures published events for verification
//! - Custom assertion functions designed for agent-friendly failure messages

pub mod assertions;
pub mod recording_bus;
pub mod request;
#[cfg(feature = "postgres")]
pub mod test_db;

pub use assertions::*;
pub use recording_bus::RecordingEventBus;
pub use request::{TestRequest, TestResponse};
#[cfg(feature = "postgres")]
pub use test_db::TestDb;
