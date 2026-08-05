//! Global search — one log-rooted index over Subjects, Jobs and events.
//!
//! An account is a Subject, the work about it is Jobs, what happened to
//! it is the audit log. In a suite assembled from separate systems,
//! searching those means querying three systems and hoping the
//! identifiers line up. Here they are three projections of one log,
//! joined on identity the system issued — so one query answers with the
//! Subject, its Jobs, and the events behind them.
//!
//! Design + decision history: docs/design/global-search.md.

pub mod error;
pub mod types;

#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod query;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub use error::SearchError;
pub use types::{RefKind, SearchResults, SearchRow, SubjectHit};

#[cfg(feature = "postgres")]
pub use query::search;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildSearchReport, rebuild_search};
