//! Subject Kind registry — the data-driven source of truth for the
//! Subject-kind vocabulary. Core enumerates no kinds itself.
//!
//! Each row of the `subject_kinds` table names a Subject discriminator
//! (system, account, vendor, recipe, equipment, …). Tenants extend
//! the alphabet by inserting rows; Boss core ships system-owned rows
//! for the platform kinds.

pub mod http;
pub mod in_memory;
pub mod port;
pub mod subject_kinds_config;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use http::{SubjectKindsApiState, router};
pub use in_memory::InMemorySubjectKinds;
pub use port::{SubjectKind, SubjectKindError, SubjectKindRepository};
pub use subject_kinds_config::SubjectKindsApiConfig;

#[cfg(feature = "postgres")]
pub use postgres::PgSubjectKinds;
