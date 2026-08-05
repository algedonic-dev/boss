//! Search errors.

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl SearchError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        SearchError::Storage(e.to_string())
    }
}

impl SearchError {
    /// A storage-shaped error from a non-sqlx source (the policy
    /// call). Same class to the caller: the query could not be
    /// answered, and it is not their input that is wrong.
    pub fn storage_msg(msg: impl Into<String>) -> Self {
        SearchError::Storage(msg.into())
    }
}
