//! Shipping domain — inbound and outbound shipment tracking.
//!
//! Tracks shipments of physical goods between the warehouse, accounts,
//! and service facilities, including carrier details and delivery status.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod shipping_config;
pub mod summary;
pub mod types;

pub use in_memory::InMemoryShipping;
pub use port::{ShippingError, ShippingRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgShipping;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_shipping};
pub use types::*;
pub mod events;
