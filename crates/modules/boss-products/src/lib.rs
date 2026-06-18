//! Finished-product catalog + per-location on-hand inventory.
//!
//! Sibling to `boss-catalog`'s `parts` (raw inputs) but tracks
//! output the tenant produces and sells. Brewery: 1/2-BBL kegs of
//! Pale Ale, 12oz cases of seasonal release. Used-device-shop:
//! refurbished routers ready to ship.
//!
//! Decision record: `docs/architecture-decisions.md` §Finance &
//! ledger (finished products are tracked per-location with cost
//! basis). This crate is the HTTP API + the read/write surface.

pub mod events;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod config;
#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub mod in_memory;

pub use in_memory::InMemoryProducts;
pub use port::{GlMove, InventoryDeltaResult, ProductsError, ProductsRepository};
pub use types::{Product, ProductDetail, ProductInventory};

#[cfg(feature = "postgres")]
pub use postgres::PgProducts;

#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_products};
