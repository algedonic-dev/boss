//! Step UX Plugin Registry — see `docs/architecture-decisions.md`
//! §Step UX & frontend.
//!
//! Third-party step kinds ship as plugins. v1 kinds defined in
//! `step_registry::v1()` are implicit — the in-tree catalog stays
//! the canonical source for those. The DB table only stores
//! plugins.
//!
//! Shape mirrors `JobKindRegistry`: append-only versioning + a
//! status lifecycle (draft → active → retired), with a partial
//! unique index enforcing at most one active row per kind.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::registry::JobKindStatus;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A full plugin row. Serializes directly to the `step_plugins`
/// JSONB columns with the same names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepPluginSpec {
    pub kind: String,
    pub version: i32,
    pub status: JobKindStatus,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: String,
    pub metadata_schema: serde_json::Value,
    /// Relative path under `/var/lib/boss/step-plugins/` that the
    /// gateway serves from `/plugins/<path>`. v1 is always a static
    /// JS bundle path (Q2).
    pub frontend_url: String,
    pub owning_team: String,
    #[serde(default)]
    pub authoring_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl StepPluginSpec {
    /// Convenience constructor for tests + seeds.
    pub fn draft(
        kind: impl Into<String>,
        label: impl Into<String>,
        category: impl Into<String>,
        frontend_url: impl Into<String>,
        metadata_schema: serde_json::Value,
    ) -> Self {
        Self {
            kind: kind.into(),
            version: 1,
            status: JobKindStatus::Draft,
            label: label.into(),
            description: None,
            category: category.into(),
            metadata_schema,
            frontend_url: frontend_url.into(),
            owning_team: "authoring".to_string(),
            authoring_job_id: None,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StepPluginError {
    #[error("step plugin not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

#[async_trait]
pub trait StepPluginRegistry: Send + Sync {
    /// Currently-active spec for `kind`.
    async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError>;

    /// Specific historical version.
    async fn get_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<StepPluginSpec, StepPluginError>;

    /// Every active spec, optionally filtered by category.
    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<StepPluginSpec>, StepPluginError>;

    /// Every version of one kind (oldest first). Includes drafts + retired.
    async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError>;

    /// Append a new draft row. Version = max(version)+1 or 1.
    async fn create_draft(&self, spec: StepPluginSpec) -> Result<StepPluginSpec, StepPluginError>;

    /// Flip the latest draft to active and demote the previous active.
    async fn publish(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError>;

    /// Flip the active row to retired. Idempotent.
    async fn retire(&self, kind: &str) -> Result<(), StepPluginError>;
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

pub struct InMemoryStepPlugins {
    rows: Arc<Mutex<HashMap<(String, i32), StepPluginSpec>>>,
}

impl Default for InMemoryStepPlugins {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStepPlugins {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Seed helper for tests. Inserts as-is without versioning logic.
    pub fn seed(&self, spec: StepPluginSpec) -> Result<(), StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.kind.clone(), spec.version)) {
            return Err(StepPluginError::Conflict(format!(
                "row already exists: {}@{}",
                spec.kind, spec.version
            )));
        }
        rows.insert((spec.kind.clone(), spec.version), spec);
        Ok(())
    }

    fn snapshot(&self) -> Vec<StepPluginSpec> {
        self.rows.lock().unwrap().values().cloned().collect()
    }

    fn max_version(&self, kind: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(k, _)| k == kind)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl StepPluginRegistry for InMemoryStepPlugins {
    async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
        self.snapshot()
            .into_iter()
            .find(|r| r.kind == kind && r.status == JobKindStatus::Active)
            .ok_or_else(|| StepPluginError::NotFound(format!("no active plugin: {kind}")))
    }

    async fn get_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<StepPluginSpec, StepPluginError> {
        self.rows
            .lock()
            .unwrap()
            .get(&(kind.to_string(), version))
            .cloned()
            .ok_or_else(|| StepPluginError::NotFound(format!("{kind}@v{version}")))
    }

    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        let mut rows: Vec<StepPluginSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == JobKindStatus::Active)
            .filter(|r| category.is_none_or(|c| r.category == c))
            .collect();
        rows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(rows)
    }

    async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        let mut rows: Vec<StepPluginSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn create_draft(
        &self,
        mut spec: StepPluginSpec,
    ) -> Result<StepPluginSpec, StepPluginError> {
        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = JobKindStatus::Draft;
        spec.created_at = Utc::now();
        self.rows
            .lock()
            .unwrap()
            .insert((spec.kind.clone(), spec.version), spec.clone());
        Ok(spec)
    }

    async fn publish(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        let latest_draft = rows
            .values()
            .filter(|r| r.kind == kind && r.status == JobKindStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                StepPluginError::NotFound(format!("no draft to publish for plugin: {kind}"))
            })?;

        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == JobKindStatus::Active {
                row.status = JobKindStatus::Retired;
            }
        }
        let key = (latest_draft.kind.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = JobKindStatus::Active;
        Ok(row.clone())
    }

    async fn retire(&self, kind: &str) -> Result<(), StepPluginError> {
        let mut rows = self.rows.lock().unwrap();
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == JobKindStatus::Active {
                row.status = JobKindStatus::Retired;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    pub struct PgStepPlugins {
        pool: PgPool,
    }

    impl PgStepPlugins {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        version: i32,
        status: String,
        label: String,
        description: Option<String>,
        category: String,
        metadata_schema: serde_json::Value,
        frontend_url: String,
        owning_team: String,
        authoring_job_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<StepPluginSpec, StepPluginError> {
        let status = r
            .status
            .parse::<JobKindStatus>()
            .map_err(StepPluginError::Storage)?;
        Ok(StepPluginSpec {
            kind: r.kind,
            version: r.version,
            status,
            label: r.label,
            description: r.description,
            category: r.category,
            metadata_schema: r.metadata_schema,
            frontend_url: r.frontend_url,
            owning_team: r.owning_team,
            authoring_job_id: r.authoring_job_id,
            created_at: r.created_at,
        })
    }

    const SELECT: &str = "SELECT kind, version, status, label, description, category, \
                          metadata_schema, frontend_url, owning_team, authoring_job_id, created_at \
                          FROM step_plugins";

    #[async_trait]
    impl StepPluginRegistry for PgStepPlugins {
        async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 AND status = 'active'"))
                    .bind(kind)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StepPluginError::NotFound(format!("no active plugin: {kind}")))
        }

        async fn get_version(
            &self,
            kind: &str,
            version: i32,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 AND version = $2"))
                    .bind(kind)
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StepPluginError::NotFound(format!("{kind}@v{version}")))
        }

        async fn list_active(
            &self,
            category: Option<&str>,
        ) -> Result<Vec<StepPluginSpec>, StepPluginError> {
            let rows: Vec<Row> = match category {
                Some(c) => {
                    sqlx::query_as(&format!(
                        "{SELECT} WHERE status = 'active' AND category = $1 ORDER BY kind"
                    ))
                    .bind(c)
                    .fetch_all(&self.pool)
                    .await
                }
                None => {
                    sqlx::query_as(&format!("{SELECT} WHERE status = 'active' ORDER BY kind"))
                        .fetch_all(&self.pool)
                        .await
                }
            }
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError> {
            let rows: Vec<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE kind = $1 ORDER BY version"))
                    .bind(kind)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn create_draft(
            &self,
            mut spec: StepPluginSpec,
        ) -> Result<StepPluginSpec, StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM step_plugins WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            spec.version = max.0.map(|v| v + 1).unwrap_or(1);
            spec.status = JobKindStatus::Draft;
            spec.created_at = Utc::now();

            sqlx::query(
                "INSERT INTO step_plugins
                    (kind, version, status, label, description, category,
                     metadata_schema, frontend_url, owning_team, authoring_job_id, created_at)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&spec.metadata_schema)
            .bind(&spec.frontend_url)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            let draft_version: Option<(i32,)> = sqlx::query_as(
                "SELECT version FROM step_plugins
                 WHERE kind = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1",
            )
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            let draft_version = draft_version
                .map(|(v,)| v)
                .ok_or_else(|| StepPluginError::NotFound(format!("no draft to publish: {kind}")))?;

            sqlx::query(
                "UPDATE step_plugins SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            sqlx::query(
                "UPDATE step_plugins SET status = 'active'
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(draft_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| StepPluginError::Storage(e.to_string()))?;

            self.get_version(kind, draft_version).await
        }

        async fn retire(&self, kind: &str) -> Result<(), StepPluginError> {
            sqlx::query(
                "UPDATE step_plugins SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&self.pool)
            .await
            .map_err(|e| StepPluginError::Storage(e.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgStepPlugins;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: &str) -> StepPluginSpec {
        StepPluginSpec::draft(
            kind,
            format!("Test {kind}"),
            "qa",
            format!("{kind}.js"),
            serde_json::json!({}),
        )
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryStepPlugins::new();
        let v1 = reg
            .create_draft(sample("emerald-inspection"))
            .await
            .unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, JobKindStatus::Draft);
        let v2 = reg
            .create_draft(sample("emerald-inspection"))
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
    }

    #[tokio::test]
    async fn publish_retires_previous_active() {
        let reg = InMemoryStepPlugins::new();
        reg.create_draft(sample("kk")).await.unwrap();
        let active = reg.publish("kk").await.unwrap();
        assert_eq!(active.status, JobKindStatus::Active);

        reg.create_draft(sample("kk")).await.unwrap();
        reg.publish("kk").await.unwrap();

        let v1 = reg.get_version("kk", 1).await.unwrap();
        assert_eq!(v1.status, JobKindStatus::Retired);
        let cur = reg.get_active("kk").await.unwrap();
        assert_eq!(cur.version, 2);
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryStepPlugins::new();
        reg.retire("never-existed").await.unwrap();
        reg.retire("never-existed").await.unwrap();
    }

    #[tokio::test]
    async fn list_active_filters_by_category() {
        let reg = InMemoryStepPlugins::new();
        let mut q = sample("qa-plugin");
        q.category = "qa".into();
        q.status = JobKindStatus::Active;
        reg.seed(q).unwrap();

        let mut s = sample("sales-plugin");
        s.category = "sales".into();
        s.status = JobKindStatus::Active;
        reg.seed(s).unwrap();

        let qa_only = reg.list_active(Some("qa")).await.unwrap();
        assert_eq!(qa_only.len(), 1);
        assert_eq!(qa_only[0].kind, "qa-plugin");

        let all = reg.list_active(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }
}
