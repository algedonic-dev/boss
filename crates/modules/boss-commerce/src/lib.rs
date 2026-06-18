//! Commerce domain — opportunities, invoices, and revenue tracking.
//!
//! Tracks the sales pipeline from lead through close, invoicing,
//! and revenue recognition across product and service categories.

#[cfg(feature = "postgres")]
pub mod agreements;
pub mod commerce_config;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

pub use in_memory::InMemoryCommerce;
pub use port::{CommerceError, CommerceRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgCommerce;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_commerce};
pub use types::*;
pub mod events;
