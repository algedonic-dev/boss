//! Class registry — logical groupings of Subjects.
//!
//! The `Class` type itself lives in `boss-core::primitives`;
//! this crate
//! adds the persistence port, in-memory + Postgres adapters, and a
//! read-only HTTP API. Authoring (POST/PUT/DELETE) lands later when
//! the admin UI does.
//!
//! Roles ('ceo', 'service-tech', …) are Classes of `employee`-kind
//! Subjects. AccountType, system-model, and other taxonomies all
//! seat in this same registry once their domains are lifted.

pub mod classes_config;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use in_memory::InMemoryClasses;
pub use port::{ClassError, ClassRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgClasses;
