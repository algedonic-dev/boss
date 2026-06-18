//! boss-docs — design decision tracker.
//!
//! Indexes markdown files under `docs/design/*.md` and exposes a
//! read + decision-collection API over them. Git is the source of
//! truth for doc prose; this crate holds ephemeral working memory
//! (pending human clicks + queued flush jobs) and read-caches
//! refreshed on demand.

pub mod config;
pub mod http;
pub mod in_memory;
pub mod parser;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod reindex;
pub mod types;

pub use in_memory::InMemoryDocsRepo;
pub use parser::{ParsedDoc, parse_doc};
pub use port::{DocsError, DocsRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgDocsRepo;
pub use types::*;
