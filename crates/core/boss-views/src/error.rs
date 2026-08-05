//! Error type for the Views port.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ViewsError {
    #[error("view not found: {0}")]
    NotFound(String),
    /// The `filter` expression did not parse. Reported at save time,
    /// not at read time — a View that only fails when someone opens it
    /// is a trap laid for whoever opens it.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
    #[error("invalid view: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
}
