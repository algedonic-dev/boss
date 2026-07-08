//! Postgres adapter for `DocsRepository`.

#![cfg(feature = "postgres")]

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::port::{DocsError, DocsRepository};
use crate::types::{
    DecisionKind, DesignDoc, DesignQuestion, DocStatus, FlushJob, FlushJobPayload, JobStatus,
    JobStatusUpdate, PendingDecision, PendingDecisionInput,
};

pub struct PgDocsRepo {
    pool: PgPool,
}

impl PgDocsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn doc_from_row(row: &PgRow) -> Result<DesignDoc, DocsError> {
    let status_str: String = row
        .try_get("status")
        .map_err(|e| DocsError::Storage(e.to_string()))?;
    let status = match DocStatus::from_db_str(&status_str) {
        Some(s) => s,
        None => return Err(DocsError::Storage(format!("unknown status {status_str}"))),
    };
    Ok(DesignDoc {
        path: row.try_get("path").map_err(storage)?,
        title: row.try_get("title").map_err(storage)?,
        status,
        pending_count: row.try_get("pending_count").map_err(storage)?,
        word_count: row.try_get("word_count").map_err(storage)?,
        last_modified: row.try_get("last_modified").map_err(storage)?,
        last_author: row.try_get("last_author").map_err(storage)?,
        last_indexed_at: row.try_get("last_indexed_at").map_err(storage)?,
        last_commit_sha: row.try_get("last_commit_sha").map_err(storage)?,
        content_html: row.try_get("content_html").map_err(storage)?,
    })
}

fn question_from_row(row: &PgRow) -> Result<DesignQuestion, DocsError> {
    Ok(DesignQuestion {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        anchor: row.try_get("anchor").map_err(storage)?,
        ordinal: row.try_get("ordinal").map_err(storage)?,
        title: row.try_get("title").map_err(storage)?,
        body_md: row.try_get("body_md").map_err(storage)?,
        proposal: row.try_get("proposal").map_err(storage)?,
        context_md: row.try_get("context_md").map_err(storage)?,
    })
}

fn pending_from_row(row: &PgRow) -> Result<PendingDecision, DocsError> {
    let kind_str: String = row.try_get("kind").map_err(storage)?;
    let kind = match kind_str.as_str() {
        "accept" => DecisionKind::Accept,
        "override" => DecisionKind::Override,
        other => return Err(DocsError::Storage(format!("unknown kind {other}"))),
    };
    Ok(PendingDecision {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        anchor: row.try_get("anchor").map_err(storage)?,
        kind,
        resolution: row.try_get("resolution").map_err(storage)?,
        rationale: row.try_get("rationale").map_err(storage)?,
        decided_by: row.try_get("decided_by").map_err(storage)?,
        decided_at: row.try_get("decided_at").map_err(storage)?,
    })
}

fn job_from_row(row: &PgRow) -> Result<FlushJob, DocsError> {
    let status_str: String = row.try_get("status").map_err(storage)?;
    let status = match status_str.as_str() {
        "queued" => JobStatus::Queued,
        "running" => JobStatus::Running,
        "succeeded" => JobStatus::Succeeded,
        "failed" => JobStatus::Failed,
        other => return Err(DocsError::Storage(format!("unknown status {other}"))),
    };
    let payload_json: JsonValue = row.try_get("payload").map_err(storage)?;
    let payload: FlushJobPayload = serde_json::from_value(payload_json)
        .map_err(|e| DocsError::Storage(format!("bad payload json: {e}")))?;
    Ok(FlushJob {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        status,
        requested_by: row.try_get("requested_by").map_err(storage)?,
        queued_at: row.try_get("queued_at").map_err(storage)?,
        started_at: row.try_get("started_at").map_err(storage)?,
        completed_at: row.try_get("completed_at").map_err(storage)?,
        payload,
        commit_sha: row.try_get("commit_sha").map_err(storage)?,
        error: row.try_get("error").map_err(storage)?,
    })
}

fn storage<E: std::fmt::Display>(e: E) -> DocsError {
    DocsError::Storage(e.to_string())
}

#[async_trait]
impl DocsRepository for PgDocsRepo {
    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError> {
        let rows = sqlx::query("SELECT * FROM design_docs ORDER BY path")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(doc_from_row).collect()
    }

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError> {
        let row = sqlx::query("SELECT * FROM design_docs WHERE path = $1")
            .bind(path)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(doc_from_row).transpose()
    }

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError> {
        let rows =
            sqlx::query("SELECT * FROM design_questions WHERE doc_path = $1 ORDER BY ordinal")
                .bind(path)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter().map(question_from_row).collect()
    }

    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        sqlx::query(
            "INSERT INTO design_docs (
                path, title, status, pending_count, word_count,
                last_modified, last_author, last_indexed_at,
                last_commit_sha, content_html
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (path) DO UPDATE SET
                title = EXCLUDED.title,
                status = EXCLUDED.status,
                word_count = EXCLUDED.word_count,
                last_modified = EXCLUDED.last_modified,
                last_author = EXCLUDED.last_author,
                last_indexed_at = EXCLUDED.last_indexed_at,
                last_commit_sha = EXCLUDED.last_commit_sha,
                content_html = EXCLUDED.content_html",
        )
        .bind(&doc.path)
        .bind(&doc.title)
        .bind(doc.status.as_str())
        .bind(doc.pending_count)
        .bind(doc.word_count)
        .bind(doc.last_modified)
        .bind(&doc.last_author)
        .bind(doc.last_indexed_at)
        .bind(&doc.last_commit_sha)
        .bind(&doc.content_html)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // Delete-and-reinsert the question set for this doc.
        sqlx::query("DELETE FROM design_questions WHERE doc_path = $1")
            .bind(&doc.path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

        for q in questions {
            sqlx::query(
                "INSERT INTO design_questions (
                    id, doc_path, anchor, ordinal, title, body_md,
                    proposal, context_md
                 ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(&q.id)
            .bind(&q.doc_path)
            .bind(&q.anchor)
            .bind(q.ordinal)
            .bind(&q.title)
            .bind(&q.body_md)
            .bind(&q.proposal)
            .bind(&q.context_md)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }

        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn delete_doc(&self, path: &str) -> Result<(), DocsError> {
        // Cascade handles design_questions. Pending decisions have no
        // FK to design_docs (they're keyed by (doc_path, anchor) as
        // free text), so delete them explicitly.
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("DELETE FROM design_pending_decisions WHERE doc_path = $1")
            .bind(path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM design_docs WHERE path = $1")
            .bind(path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn upsert_pending_decision(
        &self,
        input: &PendingDecisionInput,
        decided_by: &str,
    ) -> Result<PendingDecision, DocsError> {
        let id = format!("pd-{}", Uuid::new_v4().simple());
        let now: DateTime<Utc> = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;

        sqlx::query(
            "INSERT INTO design_pending_decisions (
                id, doc_path, anchor, kind, resolution, rationale,
                decided_by, decided_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT (doc_path, anchor) DO UPDATE SET
                kind = EXCLUDED.kind,
                resolution = EXCLUDED.resolution,
                rationale = EXCLUDED.rationale,
                decided_by = EXCLUDED.decided_by,
                decided_at = EXCLUDED.decided_at",
        )
        .bind(&id)
        .bind(&input.doc_path)
        .bind(&input.anchor)
        .bind(input.kind.as_str())
        .bind(&input.resolution)
        .bind(&input.rationale)
        .bind(decided_by)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // Refresh pending_count on the doc row.
        sqlx::query(
            "UPDATE design_docs SET pending_count = (
                SELECT COUNT(*) FROM design_pending_decisions WHERE doc_path = $1
             ) WHERE path = $1",
        )
        .bind(&input.doc_path)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // Re-fetch the row so we return the canonical state (ON
        // CONFLICT may have kept an older id).
        let row = sqlx::query(
            "SELECT * FROM design_pending_decisions WHERE doc_path = $1 AND anchor = $2",
        )
        .bind(&input.doc_path)
        .bind(&input.anchor)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let pending = pending_from_row(&row)?;

        tx.commit().await.map_err(storage)?;
        Ok(pending)
    }

    async fn delete_pending_decision(&self, doc_path: &str, anchor: &str) -> Result<(), DocsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let result =
            sqlx::query("DELETE FROM design_pending_decisions WHERE doc_path = $1 AND anchor = $2")
                .bind(doc_path)
                .bind(anchor)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(DocsError::NotFound(format!("{doc_path}#{anchor}")));
        }
        sqlx::query(
            "UPDATE design_docs SET pending_count = (
                SELECT COUNT(*) FROM design_pending_decisions WHERE doc_path = $1
             ) WHERE path = $1",
        )
        .bind(doc_path)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn pending_decisions_for_doc(
        &self,
        doc_path: &str,
    ) -> Result<Vec<PendingDecision>, DocsError> {
        let rows = sqlx::query(
            "SELECT * FROM design_pending_decisions
             WHERE doc_path = $1 ORDER BY decided_at",
        )
        .bind(doc_path)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(pending_from_row).collect()
    }

    async fn create_flush_job(
        &self,
        payload: &FlushJobPayload,
        requested_by: &str,
    ) -> Result<FlushJob, DocsError> {
        if payload.decisions.is_empty() {
            return Err(DocsError::BadRequest(format!(
                "no pending decisions for {}",
                payload.doc_path
            )));
        }

        let mut tx = self.pool.begin().await.map_err(storage)?;

        // Defensive: verify the pending rows still exist in the DB.
        // If the payload has N decisions but there are zero rows,
        // the caller lied and we should fail with BadRequest so the
        // UI can refetch.
        let pending_rows: Vec<PgRow> =
            sqlx::query("SELECT * FROM design_pending_decisions WHERE doc_path = $1")
                .bind(&payload.doc_path)
                .fetch_all(&mut *tx)
                .await
                .map_err(storage)?;
        if pending_rows.is_empty() {
            return Err(DocsError::BadRequest(format!(
                "no pending decisions for {}",
                payload.doc_path
            )));
        }

        let id = format!("fj-{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let payload_json = serde_json::to_value(payload)
            .map_err(|e| DocsError::Storage(format!("serializing payload: {e}")))?;

        sqlx::query(
            "INSERT INTO design_flush_jobs (
                id, doc_path, status, requested_by, queued_at, payload
             ) VALUES ($1,$2,'queued',$3,$4,$5)",
        )
        .bind(&id)
        .bind(&payload.doc_path)
        .bind(requested_by)
        .bind(now)
        .bind(&payload_json)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // Delete pending rows AFTER inserting the job so they're
        // captured in the payload snapshot.
        sqlx::query("DELETE FROM design_pending_decisions WHERE doc_path = $1")
            .bind(&payload.doc_path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

        // Reset pending_count on the doc.
        sqlx::query("UPDATE design_docs SET pending_count = 0 WHERE path = $1")
            .bind(&payload.doc_path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

        let row = sqlx::query("SELECT * FROM design_flush_jobs WHERE id = $1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
        let job = job_from_row(&row)?;

        tx.commit().await.map_err(storage)?;
        Ok(job)
    }

    async fn flush_job_by_id(&self, id: &str) -> Result<Option<FlushJob>, DocsError> {
        let row = sqlx::query("SELECT * FROM design_flush_jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(job_from_row).transpose()
    }

    async fn flush_jobs_by_status(&self, status: JobStatus) -> Result<Vec<FlushJob>, DocsError> {
        let rows =
            sqlx::query("SELECT * FROM design_flush_jobs WHERE status = $1 ORDER BY queued_at")
                .bind(status.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter().map(job_from_row).collect()
    }

    async fn update_flush_job_status(
        &self,
        id: &str,
        update: &JobStatusUpdate,
    ) -> Result<FlushJob, DocsError> {
        let now = Utc::now();
        let (started_at, completed_at) = match update.status {
            JobStatus::Running => (Some(now), None),
            JobStatus::Succeeded | JobStatus::Failed => (None, Some(now)),
            JobStatus::Queued => (None, None),
        };

        // Use a CASE to preserve existing started_at when setting
        // terminal states, and to clear completion when going back
        // to queued.
        let row = sqlx::query(
            "UPDATE design_flush_jobs SET
                status = $2,
                started_at = CASE
                    WHEN $2 = 'running' AND started_at IS NULL THEN $3
                    WHEN $2 = 'queued' THEN started_at
                    ELSE started_at
                END,
                completed_at = CASE
                    WHEN $2 IN ('succeeded','failed') THEN $4
                    WHEN $2 = 'queued' THEN NULL
                    ELSE completed_at
                END,
                commit_sha = COALESCE($5, commit_sha),
                error = CASE
                    WHEN $2 = 'queued' THEN NULL
                    ELSE COALESCE($6, error)
                END
             WHERE id = $1
             RETURNING *",
        )
        .bind(id)
        .bind(update.status.as_str())
        .bind(started_at)
        .bind(completed_at)
        .bind(&update.commit_sha)
        .bind(&update.error)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;

        match row {
            Some(r) => job_from_row(&r),
            None => Err(DocsError::NotFound(id.to_string())),
        }
    }

    async fn retry_flush_job(&self, id: &str) -> Result<FlushJob, DocsError> {
        // Read current status first; if already succeeded, it's a no-op.
        let existing = self.flush_job_by_id(id).await?;
        let Some(job) = existing else {
            return Err(DocsError::NotFound(id.to_string()));
        };
        if job.status == JobStatus::Succeeded {
            return Ok(job);
        }
        self.update_flush_job_status(
            id,
            &JobStatusUpdate {
                status: JobStatus::Queued,
                commit_sha: None,
                error: None,
            },
        )
        .await
    }

    async fn recent_flush_jobs(&self, limit: i64) -> Result<Vec<FlushJob>, DocsError> {
        let rows = sqlx::query("SELECT * FROM design_flush_jobs ORDER BY queued_at DESC LIMIT $1")
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(job_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DecisionKind, DocStatus, FlushDecision};
    use boss_testing::TestDb;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_and_list_docs() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_doc(&sample_doc("docs/design/b.md"), &[])
            .await
            .unwrap();
        let docs = repo.all_docs().await.unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_doc_replaces_questions() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_decisions_overwrite_same_anchor() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "first".to_string(),
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
                resolution: "better".to_string(),
                rationale: Some("changed mind".to_string()),
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
        assert_eq!(pending[0].resolution, "better");
        assert_eq!(pending[0].kind, DecisionKind::Override);
        let doc = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc.pending_count, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_flush_job_is_atomic() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        for i in 0..3 {
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
        let doc_before = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc_before.pending_count, 3);

        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: (0..3)
                .map(|i| FlushDecision {
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                })
                .collect(),
        };
        let job = repo.create_flush_job(&payload, "alice").await.unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.payload.decisions.len(), 3);

        // Pending rows cleared.
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert!(pending.is_empty());
        let doc_after = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc_after.pending_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_flush_job_with_no_pending_errors() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn update_flush_job_status_lifecycle() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_failed_resets_to_queued() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_succeeded_is_noop() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
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
