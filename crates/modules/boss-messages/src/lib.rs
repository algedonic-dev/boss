//! Messages domain — direct messages and system signals.
//!
//! Handles internal communication between employees and
//! system-generated notifications tied to domain entities.

pub mod http;
pub mod in_memory;
pub mod messages_config;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

pub use in_memory::InMemoryMessages;
pub use port::{MessageError, MessageRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgMessages;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_messages};
pub use types::*;
pub mod events;
