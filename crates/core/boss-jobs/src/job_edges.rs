//! The job_edges registry, read side — which Job metadata fields
//! reference other Jobs (department-flow-dashboards Q1; migration
//! 104 owns the table + write-path guard, 105 the abort default).
//!
//! Read-only by design: edges are declared in migrations the way
//! subject_edges' are, because an edge declaration changes what the
//! write path refuses — that is schema-shaped change, not runtime
//! authoring. Instruments (the Job page's Links panel first) fetch
//! the declarations and resolve a Job's fields against them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// `Deserialize` as well as `Serialize`: `GET /api/jobs/job-edges`
/// serves these, and the census reader (`boss packet census`) parses
/// them back into the same type rather than keeping a second copy of
/// the edge shape (CLAUDE.md §9a).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobEdgeSpec {
    pub source_kind: String,
    pub field_path: String,
    /// `job_id` | `job_id_list`.
    pub field_kind: String,
    /// `warn` | `abort`.
    pub on_missing: String,
    pub description: String,
}

#[async_trait]
pub trait JobEdgesRegistry: Send + Sync {
    async fn list(&self) -> Result<Vec<JobEdgeSpec>, String>;
}

/// The seeded defaults, for tests and pg-less deployments — kept in
/// deliberate agreement with migration 104's seeds (the pg test
/// asserts the table matches this shape).
pub struct InMemoryJobEdges;

#[async_trait]
impl JobEdgesRegistry for InMemoryJobEdges {
    async fn list(&self) -> Result<Vec<JobEdgeSpec>, String> {
        let mk = |sk: &str, fp: &str, fk: &str, d: &str| JobEdgeSpec {
            source_kind: sk.into(),
            field_path: fp.into(),
            field_kind: fk.into(),
            on_missing: "abort".into(),
            description: d.into(),
        };
        Ok(vec![
            mk(
                "*",
                "waiting_on",
                "job_id",
                "The Job whose closure this Job waits on",
            ),
            mk(
                "pr-train",
                "boarded_jobs",
                "job_id_list",
                "The ship-a-change passengers this train carried",
            ),
            mk(
                "ship-a-change",
                "backlog_item",
                "job_id",
                "The backlog/feedback Job this change answers",
            ),
            mk(
                "ship-a-change",
                "train",
                "job_id",
                "The pr-train Job this change boarded",
            ),
        ])
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgJobEdges;

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    pub struct PgJobEdges {
        pool: PgPool,
    }

    impl PgJobEdges {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[async_trait]
    impl JobEdgesRegistry for PgJobEdges {
        async fn list(&self) -> Result<Vec<JobEdgeSpec>, String> {
            let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
                "SELECT source_kind, field_path, field_kind, on_missing, description \
                 FROM job_edges ORDER BY source_kind, field_path",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
            Ok(rows
                .into_iter()
                .map(
                    |(source_kind, field_path, field_kind, on_missing, description)| JobEdgeSpec {
                        source_kind,
                        field_path,
                        field_kind,
                        on_missing,
                        description,
                    },
                )
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape the Links panel reads — a rename is breaking.
    #[tokio::test]
    async fn in_memory_serialises_with_the_field_names_the_panel_reads() {
        let edges = InMemoryJobEdges.list().await.expect("list");
        let v = serde_json::to_value(&edges).expect("serialises");
        assert_eq!(v[0]["source_kind"], "*");
        assert_eq!(v[0]["field_path"], "waiting_on");
        assert_eq!(v[1]["source_kind"], "pr-train");
        assert_eq!(v[1]["field_kind"], "job_id_list");
        assert_eq!(v[2]["field_path"], "backlog_item");
        assert_eq!(v[2]["on_missing"], "abort");
    }
}
