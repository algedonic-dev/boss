//! Content-service errors.

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("validation failure: {0}")]
    Validation(String),

    #[error("storage failure: {0}")]
    Storage(String),
}
