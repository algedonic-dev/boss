//! In-memory adapter. Used in tests and as a fallback when the
//! service is started without `--features postgres`.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::Utc;
use uuid::Uuid;

use crate::port::{DocsError, DocsRepository};
use crate::types::{
    DesignDoc, DesignQuestion, FlushJob, FlushJobPayload, JobStatus, JobStatusUpdate,
    PendingDecision, PendingDecisionInput,
};

#[derive(Default)]
struct Inner {
    docs: HashMap<String, DesignDoc>,
    questions: HashMap<String, Vec<DesignQuestion>>, // keyed by doc_path
    pending: Vec<PendingDecision>,
    jobs: Vec<FlushJob>,
}

pub struct InMemoryDocsRepo {
    inner: RwLock<Inner>,
}

impl Default for InMemoryDocsRepo {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl InMemoryDocsRepo {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DocsRepository for InMemoryDocsRepo {
    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError> {
        let inner = self.inner.read().unwrap();
        let mut docs: Vec<_> = inner.docs.values().cloned().collect();
        docs.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(docs)
    }

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError> {
        Ok(self.inner.read().unwrap().docs.get(path).cloned())
    }

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .questions
            .get(path)
            .cloned()
            .unwrap_or_default())
    }

    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError> {
        let mut inner = self.inner.write().unwrap();
        inner.docs.insert(doc.path.clone(), doc.clone());
        inner.questions.insert(doc.path.clone(), questions.to_vec());
        Ok(())
    }

    async fn delete_doc(&self, path: &str) -> Result<(), DocsError> {
        let mut inner = self.inner.write().unwrap();
        inner.docs.remove(path);
        inner.questions.remove(path);
        inner.pending.retain(|p| p.doc_path != path);
        Ok(())
    }

    async fn upsert_pending_decision(
        &self,
        input: &PendingDecisionInput,
        decided_by: &str,
    ) -> Result<PendingDecision, DocsError> {
        let mut inner = self.inner.write().unwrap();
        // Overwrite any existing row for (doc_path, anchor).
        inner
            .pending
            .retain(|p| !(p.doc_path == input.doc_path && p.anchor == input.anchor));
        let row = PendingDecision {
            id: format!("pd-{}", Uuid::new_v4().simple()),
            doc_path: input.doc_path.clone(),
            anchor: input.anchor.clone(),
            kind: input.kind,
            resolution: input.resolution.clone(),
            rationale: input.rationale.clone(),
            decided_by: decided_by.to_string(),
            decided_at: Utc::now(),
        };
        inner.pending.push(row.clone());
        // Bump pending_count on the doc row if present.
        let pending_for_doc = inner
            .pending
            .iter()
            .filter(|p| p.doc_path == input.doc_path)
            .count() as i32;
        if let Some(d) = inner.docs.get_mut(&input.doc_path) {
            d.pending_count = pending_for_doc;
        }
        Ok(row)
    }

    async fn delete_pending_decision(&self, doc_path: &str, anchor: &str) -> Result<(), DocsError> {
        let mut inner = self.inner.write().unwrap();
        let before = inner.pending.len();
        inner
            .pending
            .retain(|p| !(p.doc_path == doc_path && p.anchor == anchor));
        if inner.pending.len() == before {
            return Err(DocsError::NotFound(format!("{doc_path}#{anchor}")));
        }
        let pending_for_doc = inner
            .pending
            .iter()
            .filter(|p| p.doc_path == doc_path)
            .count() as i32;
        if let Some(d) = inner.docs.get_mut(doc_path) {
            d.pending_count = pending_for_doc;
        }
        Ok(())
    }

    async fn pending_decisions_for_doc(
        &self,
        doc_path: &str,
    ) -> Result<Vec<PendingDecision>, DocsError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .pending
            .iter()
            .filter(|p| p.doc_path == doc_path)
            .cloned()
            .collect())
    }

    async fn create_flush_job(
        &self,
        payload: &FlushJobPayload,
        requested_by: &str,
    ) -> Result<FlushJob, DocsError> {
        let mut inner = self.inner.write().unwrap();
        // Atomically: snapshot pending → job, delete pending rows.
        let pending: Vec<_> = inner
            .pending
            .iter()
            .filter(|p| p.doc_path == payload.doc_path)
            .cloned()
            .collect();
        if pending.is_empty() {
            return Err(DocsError::BadRequest(format!(
                "no pending decisions for {}",
                payload.doc_path
            )));
        }
        let job = FlushJob {
            id: format!("fj-{}", Uuid::new_v4().simple()),
            doc_path: payload.doc_path.clone(),
            status: JobStatus::Queued,
            requested_by: requested_by.to_string(),
            queued_at: Utc::now(),
            started_at: None,
            completed_at: None,
            payload: payload.clone(),
            commit_sha: None,
            error: None,
        };
        inner.jobs.push(job.clone());
        inner.pending.retain(|p| p.doc_path != payload.doc_path);
        if let Some(d) = inner.docs.get_mut(&payload.doc_path) {
            d.pending_count = 0;
        }
        Ok(job)
    }

    async fn flush_job_by_id(&self, id: &str) -> Result<Option<FlushJob>, DocsError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned())
    }

    async fn flush_jobs_by_status(&self, status: JobStatus) -> Result<Vec<FlushJob>, DocsError> {
        let mut jobs: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .jobs
            .iter()
            .filter(|j| j.status == status)
            .cloned()
            .collect();
        jobs.sort_by_key(|j| j.queued_at);
        Ok(jobs)
    }

    async fn update_flush_job_status(
        &self,
        id: &str,
        update: &JobStatusUpdate,
    ) -> Result<FlushJob, DocsError> {
        let mut inner = self.inner.write().unwrap();
        let job = inner
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| DocsError::NotFound(id.to_string()))?;
        job.status = update.status;
        let now = Utc::now();
        match update.status {
            JobStatus::Running => {
                if job.started_at.is_none() {
                    job.started_at = Some(now);
                }
            }
            JobStatus::Succeeded | JobStatus::Failed => {
                job.completed_at = Some(now);
            }
            JobStatus::Queued => {
                // Retry path: clear completion but keep started_at for audit.
                job.completed_at = None;
                job.error = None;
            }
        }
        if let Some(sha) = &update.commit_sha {
            job.commit_sha = Some(sha.clone());
        }
        if let Some(err) = &update.error {
            job.error = Some(err.clone());
        }
        Ok(job.clone())
    }

    async fn retry_flush_job(&self, id: &str) -> Result<FlushJob, DocsError> {
        let mut inner = self.inner.write().unwrap();
        let job = inner
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| DocsError::NotFound(id.to_string()))?;
        if job.status == JobStatus::Succeeded {
            // Idempotent: retry of a succeeded job is a no-op.
            return Ok(job.clone());
        }
        job.status = JobStatus::Queued;
        job.started_at = None;
        job.completed_at = None;
        job.error = None;
        job.commit_sha = None;
        Ok(job.clone())
    }

    async fn recent_flush_jobs(&self, limit: i64) -> Result<Vec<FlushJob>, DocsError> {
        let mut jobs = self.inner.read().unwrap().jobs.clone();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.queued_at));
        jobs.truncate(limit as usize);
        Ok(jobs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DecisionKind, DocStatus, FlushDecision};
    use chrono::TimeZone;

    fn sample_doc(path: &str) -> DesignDoc {
        DesignDoc {
            path: path.to_string(),
            title: "Test Doc".to_string(),
            status: DocStatus::InReview,
            pending_count: 0,
            word_count: 100,
            last_modified: Utc.with_ymd_and_hms(2026, 4, 13, 12, 0, 0).unwrap(),
            last_author: "alice".to_string(),
            last_indexed_at: Utc::now(),
            last_commit_sha: "abc123".to_string(),
            content_html: "<h1>Test</h1>".to_string(),
        }
    }

    fn sample_question(doc_path: &str, anchor: &str, ordinal: i32) -> DesignQuestion {
        DesignQuestion {
            id: format!("{doc_path}#{anchor}"),
            doc_path: doc_path.to_string(),
            anchor: anchor.to_string(),
            ordinal,
            title: format!("Question {anchor}"),
            body_md: "body".to_string(),
            proposal: Some("proposal".to_string()),
            context_md: None,
        }
    }

    #[tokio::test]
    async fn upsert_and_list_docs() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_doc(&sample_doc("docs/design/b.md"), &[])
            .await
            .unwrap();
        let docs = repo.all_docs().await.unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].path, "docs/design/a.md");
        assert_eq!(docs[1].path, "docs/design/b.md");
    }

    #[tokio::test]
    async fn upsert_doc_replaces_questions() {
        let repo = InMemoryDocsRepo::new();
        let doc = sample_doc("docs/design/a.md");
        repo.upsert_doc(
            &doc,
            &[
                sample_question("docs/design/a.md", "Q1", 0),
                sample_question("docs/design/a.md", "Q2", 1),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            repo.questions_for_doc("docs/design/a.md")
                .await
                .unwrap()
                .len(),
            2
        );
        // Reindex with one question.
        repo.upsert_doc(&doc, &[sample_question("docs/design/a.md", "Q1", 0)])
            .await
            .unwrap();
        assert_eq!(
            repo.questions_for_doc("docs/design/a.md")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn pending_decisions_overwrite_same_anchor() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "first answer".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Override,
                resolution: "better answer".to_string(),
                rationale: Some("I changed my mind".to_string()),
            },
            "alice",
        )
        .await
        .unwrap();
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resolution, "better answer");
        assert_eq!(pending[0].kind, DecisionKind::Override);
    }

    #[tokio::test]
    async fn pending_count_tracks_clicks() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        for i in 0..3 {
            repo.upsert_pending_decision(
                &PendingDecisionInput {
                    doc_path: "docs/design/a.md".to_string(),
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: "ok".to_string(),
                    rationale: None,
                },
                "alice",
            )
            .await
            .unwrap();
        }
        let doc = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc.pending_count, 3);
    }

    #[tokio::test]
    async fn create_flush_job_snapshots_and_clears_pending() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        for i in 0..2 {
            repo.upsert_pending_decision(
                &PendingDecisionInput {
                    doc_path: "docs/design/a.md".to_string(),
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                },
                "alice",
            )
            .await
            .unwrap();
        }
        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: vec![
                FlushDecision {
                    anchor: "Q0".to_string(),
                    kind: DecisionKind::Accept,
                    resolution: "answer 0".to_string(),
                    rationale: None,
                },
                FlushDecision {
                    anchor: "Q1".to_string(),
                    kind: DecisionKind::Accept,
                    resolution: "answer 1".to_string(),
                    rationale: None,
                },
            ],
        };
        let job = repo.create_flush_job(&payload, "alice").await.unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.payload.decisions.len(), 2);
        // Pending rows cleared.
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert!(pending.is_empty());
        // pending_count on doc zeroed.
        let doc = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc.pending_count, 0);
    }

    #[tokio::test]
    async fn create_flush_job_with_no_pending_errors() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: vec![],
        };
        let result = repo.create_flush_job(&payload, "alice").await;
        assert!(matches!(result, Err(DocsError::BadRequest(_))));
    }

    #[tokio::test]
    async fn update_flush_job_status_tracks_lifecycle() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();
        // Running.
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Running,
                commit_sha: None,
                error: None,
            },
        )
        .await
        .unwrap();
        let fetched = repo.flush_job_by_id(&job.id).await.unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Running);
        assert!(fetched.started_at.is_some());
        // Succeeded.
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Succeeded,
                commit_sha: Some("def456".to_string()),
                error: None,
            },
        )
        .await
        .unwrap();
        let fetched = repo.flush_job_by_id(&job.id).await.unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Succeeded);
        assert_eq!(fetched.commit_sha.as_deref(), Some("def456"));
        assert!(fetched.completed_at.is_some());
    }

    #[tokio::test]
    async fn retry_failed_job_resets_to_queued() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Failed,
                commit_sha: None,
                error: Some("boom".to_string()),
            },
        )
        .await
        .unwrap();
        let retried = repo.retry_flush_job(&job.id).await.unwrap();
        assert_eq!(retried.status, JobStatus::Queued);
        assert!(retried.error.is_none());
        assert!(retried.completed_at.is_none());
    }

    #[tokio::test]
    async fn retry_succeeded_job_is_noop() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Succeeded,
                commit_sha: Some("def".to_string()),
                error: None,
            },
        )
        .await
        .unwrap();
        let retried = repo.retry_flush_job(&job.id).await.unwrap();
        assert_eq!(retried.status, JobStatus::Succeeded);
        assert_eq!(retried.commit_sha.as_deref(), Some("def"));
    }
}
