//! Hexagonal port: what the service needs from persistence.

use async_trait::async_trait;

use crate::types::{
    DesignDoc, DesignQuestion, FlushJob, FlushJobPayload, JobStatus, JobStatusUpdate,
    PendingDecision, PendingDecisionInput,
};

#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

#[async_trait]
pub trait DocsRepository: Send + Sync {
    // ----- Docs and questions (read-cache) -----

    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError>;

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError>;

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError>;

    /// Replace a doc's metadata + questions atomically. Used by the
    /// reindexer after re-parsing a file. Deletes any existing
    /// questions for the doc and inserts the new set in one txn.
    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError>;

    /// Remove a doc (and its questions + pending decisions) from the
    /// cache. Used when a file disappears from disk.
    async fn delete_doc(&self, path: &str) -> Result<(), DocsError>;

    // ----- Pending decisions -----

    /// Upsert a pending decision. Keyed on `(doc_path, anchor)`, so
    /// clicking twice on the same question overwrites the first.
    async fn upsert_pending_decision(
        &self,
        input: &PendingDecisionInput,
        decided_by: &str,
    ) -> Result<PendingDecision, DocsError>;

    async fn delete_pending_decision(&self, doc_path: &str, anchor: &str) -> Result<(), DocsError>;

    async fn pending_decisions_for_doc(
        &self,
        doc_path: &str,
    ) -> Result<Vec<PendingDecision>, DocsError>;

    // ----- Flush jobs -----

    /// Atomically snapshot pending decisions into a new flush job and
    /// delete them. Returns the new job. Errors with
    /// `DocsError::BadRequest` if there are zero pending decisions
    /// for the doc (D8 from the design doc).
    async fn create_flush_job(
        &self,
        payload: &FlushJobPayload,
        requested_by: &str,
    ) -> Result<FlushJob, DocsError>;

    async fn flush_job_by_id(&self, id: &str) -> Result<Option<FlushJob>, DocsError>;

    async fn flush_jobs_by_status(&self, status: JobStatus) -> Result<Vec<FlushJob>, DocsError>;

    async fn update_flush_job_status(
        &self,
        id: &str,
        update: &JobStatusUpdate,
    ) -> Result<FlushJob, DocsError>;

    /// Retry a failed job by setting status back to `queued` and
    /// calling the dispatch_worker hook (handled by the HTTP layer,
    /// not the repository).
    async fn retry_flush_job(&self, id: &str) -> Result<FlushJob, DocsError>;

    async fn recent_flush_jobs(&self, limit: i64) -> Result<Vec<FlushJob>, DocsError>;
}
