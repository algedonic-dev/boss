//! Equipment Knowledge Base — the authoritative description of every
//! system model the tenant tracks (devices, vehicles, machines,
//! brewing equipment, printers — anything serial-tracked).
//!
//! One knowledge-base entry feeds three audiences:
//!
//! 1. **Sales & commerce** — marketing copy, price, indications, hero imagery.
//! 2. **Service technicians** — specifications, common failure modes, preventive maintenance
//!    schedules, calibration requirements, manuals.
//! 3. **Repair / refurbishment** — parts lists, consumables, teardown references.
//!
//! The knowledge base is slow-changing reference data. Serial-tracked
//! units (in `boss-assets`) reference the knowledge base by SKU.

#[cfg(feature = "postgres")]
pub mod extras_schema;
pub mod http;
pub mod in_memory;
pub mod kb_config;
pub mod marketing_assets;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

pub use in_memory::InMemoryKb;
pub use port::{KbError, KbRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgKb;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_catalog};
pub use types::*;
pub mod events;
