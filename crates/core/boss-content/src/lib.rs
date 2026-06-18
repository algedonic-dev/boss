//! HR-authored content: bulletin board + company manual.
//!
//! Decision record: `docs/architecture-decisions.md` §Content,
//! files, knowledge. This crate
//! owns the primitive and exposes it as an HTTP surface. Consumers
//! (My Day, /manual, /admin/content) render from these endpoints.
//!
//! v1a ships bulletin board surfaces (list/create/update/dismiss);
//! v1c adds the company manual (tree + section + history + seed).

pub mod error;
pub mod events;
pub mod files;
pub mod in_memory;
pub mod port;
pub mod seed;
pub mod types;

#[cfg(feature = "postgres")]
pub mod config;
#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub use error::ContentError;
pub use in_memory::InMemoryContent;
pub use port::ContentRepository;
pub use types::{
    Audience, Bulletin, BulletinDraft, BulletinPatch, BulletinPriority, ManualPatch, ManualSection,
    ManualSectionDraft, ManualSectionVersion, UserContext,
};

#[cfg(feature = "postgres")]
pub use postgres::PgContent;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_content};
